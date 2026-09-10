//! Turning one revision of a knowledge base's source into a [`Catalog`].
//!
//! The stages run in a fixed order because each needs what the one before it produced:
//! operations come from the documented predicates, collections from the operations, `@domain`
//! tags from the collections, and the name and route indexes from all of them together.

use std::collections::HashMap;

use serde_json::Value;

use crate::prolog::{
    CollectionTag, DomainSource, DynamicPredicate, Interpreter, KnowledgeBase, ModuleDoc, Predicate,
};

use super::SharedConfig;
use super::catalog::{Catalog, CatalogError, DEFAULT_DESCRIPTION, DEFAULT_TITLE};
use super::collection::{self, Collection};
use super::operation::{Domain, Operation, UnsupportedPredicate, Verb};

/// Builds everything derived from `knowledge_base`'s current source.
pub fn catalog(
    knowledge_base: &KnowledgeBase,
    config: &SharedConfig,
) -> Result<Catalog, CatalogError> {
    let interpreter = knowledge_base.parse_pool(config.interpreter_count());
    let predicates = knowledge_base.predicates();
    check_consult(&interpreter, &predicates)?;

    let (operations, tags, rejected) = operations_from(predicates);
    let dynamic = knowledge_base.dynamic_predicates();
    let (mut operations, collections) = with_collections(operations, tags, &dynamic)?;
    resolve_domains(&mut operations, &collections, &dynamic, &interpreter)?;
    let (by_name, by_route) = index(&operations)?;

    let module_doc = knowledge_base.module_doc();
    let source = knowledge_base.source().to_string();
    Ok(Catalog {
        // What an adapter caches a rendered document against. Both inputs matter: the source
        // decides which operations exist, and `persist` decides which of them are reachable, so
        // a document rendered under one is not valid under the other.
        revision: revision_of(&source, config.persist),
        title: title_of(module_doc.as_ref()),
        description: description_of(module_doc.as_ref()),
        operations,
        by_name,
        by_route,
        collections,
        interpreter,
        config: config.clone(),
        source,
        rejected,
    })
}

/// Establishes that the machine actually holds the whole knowledge base rather than some prefix
/// of it.
///
/// scryer reports a failed consult by defining nothing at all and returning nothing to check, so
/// without this a file with one malformed clause starts up looking healthy and then answers
/// every request with an existence error.
fn check_consult(interpreter: &Interpreter, predicates: &[Predicate]) -> Result<(), CatalogError> {
    // Asked before any other question. A machine left in a bad state by an aborted consult can
    // panic inside scryer on its *second* query, so the one conclusive question must be first —
    // and must not be followed by the per-predicate probe, which is why this reports the failure
    // without naming which predicates went missing.
    if !interpreter.consult_succeeded() {
        return Err(CatalogError::ConsultFailed);
    }

    let indicators: Vec<(&str, usize)> = predicates
        .iter()
        .map(|predicate| (predicate.name.as_str(), predicate.arguments.len()))
        .collect();
    let missing = interpreter.missing_predicates(indicators);
    match missing.is_empty() {
        true => Ok(()),
        false => Err(CatalogError::PredicatesMissing(missing)),
    }
}

/// What the documented predicates yield: the operations, the `@collection` tags they carry
/// (paired with the operation each was declared on), and why each rejected predicate was.
type Documented = (
    Vec<Operation>,
    Vec<(usize, CollectionTag)>,
    Vec<(String, UnsupportedPredicate)>,
);

/// Turns every documented predicate into an operation, or records why it cannot be one.
///
/// The `@collection` tags are picked up here, against the predicates themselves, so an
/// `Operation` never has to carry a declaration it does not use.
fn operations_from(predicates: Vec<Predicate>) -> Documented {
    let mut operations = Vec::new();
    let mut tags = Vec::new();
    let mut rejected = Vec::new();
    for predicate in predicates {
        let name = predicate.name.clone();
        let tag = predicate.doc.collection.clone();
        match Operation::try_from(predicate) {
            Ok(operation) => {
                if let Some(tag) = tag {
                    tags.push((operations.len(), tag));
                }
                operations.push(operation);
            }
            Err(reason) => rejected.push((name, reason)),
        }
    }
    (operations, tags, rejected)
}

/// Replaces each operation carrying a `@collection` tag with the routes that tag declares.
///
/// A predicate that declares a collection is reachable through the collection and nowhere else:
/// the list route calls the same goal with the same determinism, so the predicate's own route
/// would be a second name for one set of facts. Dropping it also leaves that name free, which is
/// what lets a collection be called after the predicate that enumerates it.
///
/// The generated routes join `operations` before the indexes are built, so they face exactly the
/// same collision checks as everything else: a collection whose name is already a path is a
/// startup failure rather than a silent shadowing.
fn with_collections(
    operations: Vec<Operation>,
    tags: Vec<(usize, CollectionTag)>,
    dynamic: &[DynamicPredicate],
) -> Result<(Vec<Operation>, Vec<Collection>), CatalogError> {
    let mut collections = Vec::with_capacity(tags.len());
    let mut generated = Vec::new();
    let mut absorbed = vec![false; operations.len()];
    for (index, tag) in tags {
        let source = &operations[index];
        let is_dynamic = dynamic.iter().any(|candidate| {
            candidate.name == source.predicate_name() && candidate.arity == source.arguments().len()
        });
        let (collection, routes) =
            collection::build(source, &tag, is_dynamic).map_err(CatalogError::Collection)?;
        collections.push(collection);
        generated.extend(routes);
        absorbed[index] = true;
    }

    let mut operations: Vec<Operation> = operations
        .into_iter()
        .zip(absorbed)
        .filter_map(|(operation, absorbed)| (!absorbed).then_some(operation))
        .collect();
    operations.extend(generated);
    Ok((operations, collections))
}

/// Matches every `@domain` tag to the predicate that holds its values.
///
/// A domain over facts that cannot change is narrowed into the argument's schema, so an HTTP
/// client and an MCP client both reject a bad value before a goal is ever built.
///
/// Two passes: what to resolve is read from every operation first and only then written back,
/// because resolving needs the whole operation list while writing needs one operation
/// exclusively.
fn resolve_domains(
    operations: &mut [Operation],
    collections: &[Collection],
    dynamic: &[DynamicPredicate],
    interpreter: &Interpreter,
) -> Result<(), CatalogError> {
    let mut resolved: Vec<(usize, usize, Domain, Vec<Value>)> = Vec::new();
    for (index, operation) in operations.iter().enumerate() {
        for (position, param) in operation.arguments().iter().enumerate() {
            let Some(source) = param.declared_domain().filter(|_| param.is_input()) else {
                continue;
            };
            let unknown = || CatalogError::UnknownDomain {
                operation: operation.name().to_string(),
                argument: param.name.clone(),
                source: describe(source),
            };
            let domain = match_domain(source, collections, dynamic).ok_or_else(unknown)?;
            if !interpreter.defines(&domain.predicate, domain.arity) {
                return Err(unknown());
            }
            let values = domain_values(interpreter, &domain);
            resolved.push((index, position, domain, values));
        }
    }

    for (index, position, domain, values) in resolved {
        operations[index].arguments_mut()[position].resolve_domain(domain, values);
    }
    Ok(())
}

/// Where a `@domain` tag's values actually come from.
fn match_domain(
    source: &DomainSource,
    collections: &[Collection],
    dynamic: &[DynamicPredicate],
) -> Option<Domain> {
    let (predicate, arity, position) = match source {
        DomainSource::Predicate { name, arity } => (name.clone(), *arity, 0),
        // A collection's values are its key.
        DomainSource::Collection(name) => {
            let collection = collections
                .iter()
                .find(|candidate| candidate.name() == name)?;
            (
                collection.predicate().to_string(),
                collection.columns().len(),
                collection.sole_key_position()?,
            )
        }
    };

    Some(Domain {
        dynamic: dynamic
            .iter()
            .any(|candidate| candidate.name == predicate && candidate.arity == arity),
        predicate,
        arity,
        position,
    })
}

/// Every value in `domain`, asked of the live machine. Blocking.
pub(super) fn domain_values(interpreter: &Interpreter, domain: &Domain) -> Vec<Value> {
    let Ok(answers) = interpreter.query(&domain.goal()) else {
        return Vec::new();
    };
    match answers.first().and_then(|answer| answer.binding("Values")) {
        Some(Value::Array(values)) => {
            let mut values = values.clone();
            values.dedup();
            values
        }
        _ => Vec::new(),
    }
}

/// Builds the lookup tables, refusing any operation set that would be ambiguous.
///
/// Two operations sharing a name would make an MCP tool call land somewhere the author did not
/// choose; two sharing a `(path, verb)` would shadow each other over HTTP. Both are startup
/// errors rather than a first-wins rule.
type Indexes = (HashMap<String, usize>, HashMap<(String, Verb), usize>);
fn index(operations: &[Operation]) -> Result<Indexes, CatalogError> {
    let mut by_name = HashMap::with_capacity(operations.len());
    let mut by_route = HashMap::with_capacity(operations.len());
    for (index, operation) in operations.iter().enumerate() {
        if by_name
            .insert(operation.name().to_string(), index)
            .is_some()
        {
            return Err(CatalogError::DuplicateName(operation.name().to_string()));
        }
        let route = (operation.path().to_string(), operation.verb());
        if let Some(previous) = by_route.insert(route, index) {
            return Err(CatalogError::CollidingRoute {
                first: operations[previous].name().to_string(),
                second: operation.name().to_string(),
                verb: operation.verb(),
                path: operation.path().to_string(),
            });
        }
    }
    Ok((by_name, by_route))
}

fn describe(source: &DomainSource) -> String {
    match source {
        DomainSource::Collection(name) => name.clone(),
        DomainSource::Predicate { name, arity } => format!("{name}/{arity}"),
    }
}

fn title_of(module_doc: Option<&ModuleDoc>) -> String {
    module_doc
        .and_then(|doc| doc.title.clone())
        .unwrap_or_else(|| DEFAULT_TITLE.to_string())
}

fn description_of(module_doc: Option<&ModuleDoc>) -> String {
    module_doc
        .and_then(|doc| doc.doc.render())
        .unwrap_or_else(|| DEFAULT_DESCRIPTION.to_string())
}

/// The number an adapter caches a rendered document against.
fn revision_of(source: &str, persist: bool) -> u64 {
    fnv1a(source) ^ u64::from(persist)
}

/// FNV-1a. Used only to derive the revision above; nothing depends on it being a good hash.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
