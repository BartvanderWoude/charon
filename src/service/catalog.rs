//! An immutable snapshot of everything derived from one revision of a knowledge base's source.

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

use crate::prolog::{Interpreter, KnowledgeBase, ModuleDoc};

use super::descriptors;
use super::operation::{Operation, UnsupportedPredicate, Verb};

pub const DEFAULT_TITLE: &str = "Charon Knowledge Base API";
pub const DEFAULT_DESCRIPTION: &str = "Generated from a Prolog knowledge base: every documented \
     predicate is callable. A predicate declared `det` or `semidet` answers with its solution \
     directly; any other predicate answers with an array holding one entry per solution, empty \
     if the query failed.";

/// Everything derived from one revision of the source text, rendered once.
///
/// The descriptor documents are built here rather than on every request: both are pure functions
/// of the operations, the module doc and the crate version, none of which change while the
/// server runs — `assertz` adds facts, not documented predicates.
pub struct Catalog {
    operations: Vec<Operation>,
    /// Predicate name → index. Also the uniqueness check: MCP addresses a tool by this name, so
    /// two operations answering to one name would make a call ambiguous.
    by_name: HashMap<String, usize>,
    /// (path, verb) → index. Two predicates colliding here would shadow each other over HTTP.
    by_route: HashMap<(String, Verb), usize>,
    module_doc: Option<ModuleDoc>,
    source: String,
    /// Predicates that were documented but could not be exposed, with the reason. Reported at
    /// startup so a missing endpoint is diagnosable instead of mysterious.
    rejected: Vec<(String, UnsupportedPredicate)>,
    interpreter: Interpreter,
    openapi_json: Arc<str>,
    tools_json: Arc<str>,
    etag: String,
}

impl Catalog {
    /// Builds everything derived from `knowledge_base`'s current source.
    ///
    /// Fails if the consult did not actually define the predicates the PlDoc comments promise.
    /// scryer reports a failed consult by defining nothing at all and returns nothing to check,
    /// so without this probe a knowledge base with one malformed clause starts up looking
    /// healthy and then answers every single request with an existence error.
    pub fn build(knowledge_base: &KnowledgeBase) -> Result<Self, CatalogError> {
        let interpreter = knowledge_base.parse();
        let predicates = knowledge_base.predicates();

        let indicators: Vec<(&str, usize)> = predicates
            .iter()
            .map(|predicate| (predicate.name.as_str(), predicate.arguments.len()))
            .collect();
        let missing = interpreter.missing_predicates(indicators);
        if !missing.is_empty() {
            return Err(CatalogError::PredicatesMissing(missing));
        }

        let mut operations = Vec::new();
        let mut rejected = Vec::new();
        for predicate in predicates {
            let name = predicate.name.clone();
            match Operation::try_from(predicate) {
                Ok(operation) => operations.push(operation),
                Err(reason) => rejected.push((name, reason)),
            }
        }

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

        let source = knowledge_base.source().to_string();
        let module_doc = knowledge_base.module_doc();
        let title = title_of(module_doc.as_ref());
        let description = description_of(module_doc.as_ref());

        let openapi_json: Arc<str> = descriptors::openapi(&operations, &title, &description).into();
        let tools_json: Arc<str> = descriptors::mcp_tools(&operations).into();
        // Hashed over the rendered document rather than the source, so that a Charon upgrade
        // that changes how documents are rendered also invalidates a cached copy.
        let etag = format!("\"{:x}\"", fnv1a(&openapi_json));

        Ok(Catalog {
            operations,
            by_name,
            by_route,
            module_doc,
            source,
            rejected,
            interpreter,
            openapi_json,
            tools_json,
            etag,
        })
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Predicates that were documented but could not be exposed, and why.
    pub fn rejected(&self) -> &[(String, UnsupportedPredicate)] {
        &self.rejected
    }

    /// The knowledge base's source text, served as an MCP resource.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub(super) fn interpreter(&self) -> &Interpreter {
        &self.interpreter
    }

    /// The operation exposing `name`, matched on the predicate's own name. This is the lookup
    /// MCP uses, since a tool name is flat and keeps its verb prefix.
    pub fn find(&self, name: &str) -> Option<&Operation> {
        self.by_name.get(name).map(|index| &self.operations[*index])
    }

    /// The operation at `path` for `verb`. HTTP splits the verb out of the predicate name, so
    /// several operations can share a path.
    pub fn route(&self, path: &str, verb: Verb) -> Result<&Operation, RouteError> {
        if let Some(index) = self.by_route.get(&(path.to_string(), verb)) {
            return Ok(&self.operations[*index]);
        }
        let elsewhere: Vec<Verb> = Verb::ALL
            .into_iter()
            .filter(|other| self.by_route.contains_key(&(path.to_string(), *other)))
            .collect();
        match elsewhere.is_empty() {
            true => Err(RouteError::NoSuchPath),
            false => Err(RouteError::WrongVerb { allowed: elsewhere }),
        }
    }

    pub fn openapi_json(&self) -> Arc<str> {
        Arc::clone(&self.openapi_json)
    }

    pub fn tools_json(&self) -> Arc<str> {
        Arc::clone(&self.tools_json)
    }

    pub fn etag(&self) -> &str {
        &self.etag
    }

    pub fn title(&self) -> String {
        title_of(self.module_doc.as_ref())
    }

    pub fn description(&self) -> String {
        description_of(self.module_doc.as_ref())
    }
}

impl fmt::Debug for Catalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Catalog")
            .field("operations", &self.operations.len())
            .field("rejected", &self.rejected.len())
            .field("etag", &self.etag)
            .finish_non_exhaustive()
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

/// FNV-1a, used only to give the descriptor documents a stable ETag.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}

/// Why a revision of a knowledge base could not be turned into a catalog.
#[derive(Debug)]
pub enum CatalogError {
    /// The consult did not define these predicates, even though they are documented. Almost
    /// always one malformed clause earlier in the file.
    PredicatesMissing(Vec<String>),
    /// Two documented predicates answer to one name, so a tool call would be ambiguous.
    DuplicateName(String),
    /// Two documented predicates land on one HTTP route, so one would shadow the other.
    CollidingRoute {
        first: String,
        second: String,
        verb: Verb,
        path: String,
    },
}

impl fmt::Display for CatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CatalogError::PredicatesMissing(missing) => write!(
                f,
                "the knowledge base loaded but does not define {}.\n\
                 This usually means a clause earlier in the file failed to compile, which \
                 aborts the whole consult — check for a malformed clause or an arithmetic \
                 expression that cannot be evaluated.",
                missing.join(", ")
            ),
            CatalogError::DuplicateName(name) => write!(
                f,
                "`{name}` is documented more than once; a predicate is addressed by name, so \
                 there is no way to say which was meant."
            ),
            CatalogError::CollidingRoute {
                first,
                second,
                verb,
                path,
            } => write!(
                f,
                "`{first}` and `{second}` both answer {} /{path}; rename one, or give them \
                 different verb prefixes.",
                verb.as_str().to_uppercase()
            ),
        }
    }
}

impl std::error::Error for CatalogError {}

/// Why a path and method did not resolve to an operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RouteError {
    NoSuchPath,
    WrongVerb { allowed: Vec<Verb> },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Writes `source` to a scratch file and builds a catalog from it.
    fn catalog_for(source: &str) -> Result<Catalog, CatalogError> {
        let path = std::env::temp_dir().join(format!(
            "charon_catalog_{}_{:p}.pl",
            std::process::id(),
            source
        ));
        let mut file = std::fs::File::create(&path).unwrap();
        file.write_all(source.as_bytes()).unwrap();
        drop(file);

        let knowledge_base = KnowledgeBase::load(&path).unwrap();
        let built = Catalog::build(&knowledge_base);
        let _ = std::fs::remove_file(&path);
        built
    }

    #[test]
    fn routes_resolve_by_path_and_verb() {
        let catalog = catalog_for(
            "%! get_fruit(-F:atom) is multi.\nget_fruit(apple).\n\
             %! delete_fruit(+F:atom) is semidet.\ndelete_fruit(_).\n",
        )
        .unwrap();

        assert_eq!(
            catalog.route("fruit", Verb::Get).unwrap().name(),
            "get_fruit"
        );
        assert_eq!(
            catalog.route("fruit", Verb::Delete).unwrap().name(),
            "delete_fruit"
        );
        // The path exists, but not for this verb — which is a different answer from "no such
        // path", and the two get different status codes.
        assert_eq!(
            catalog.route("fruit", Verb::Post).unwrap_err(),
            RouteError::WrongVerb {
                allowed: vec![Verb::Get, Verb::Delete]
            }
        );
        assert_eq!(
            catalog.route("vegetable", Verb::Get).unwrap_err(),
            RouteError::NoSuchPath
        );
    }

    #[test]
    fn two_predicates_answering_to_one_name_is_a_startup_failure() {
        // Silently keeping the first would make an MCP `tools/call` land somewhere the author
        // did not choose.
        let error = catalog_for("%! f(-X:int) is det.\nf(1).\n\n%! f(-X:int) is det.\nf(2).\n")
            .unwrap_err();
        assert!(matches!(error, CatalogError::DuplicateName(name) if name == "f"));
    }

    #[test]
    fn two_predicates_colliding_on_one_route_is_a_startup_failure() {
        // `get_fruit` and `fruit` both mean GET /fruit; one would shadow the other.
        let error = catalog_for(
            "%! get_fruit(-F:atom) is det.\nget_fruit(apple).\n\n\
             %! fruit(-F:atom) is det.\nfruit(pear).\n",
        )
        .unwrap_err();
        let message = error.to_string();
        assert!(
            message.contains("get_fruit") && message.contains("fruit"),
            "{message}"
        );
    }

    #[test]
    fn a_predicate_that_cannot_be_exposed_is_reported_rather_than_dropped() {
        let catalog = catalog_for("%! f(?X:int) is det.\nf(_).\n").unwrap();
        assert!(catalog.operations().is_empty());
        assert_eq!(catalog.rejected().len(), 1);
        assert_eq!(catalog.rejected()[0].0, "f");
    }

    #[test]
    fn the_etag_tracks_the_rendered_document() {
        let one = catalog_for("%! a(-X:int) is det.\na(1).\n").unwrap();
        let other = catalog_for("%! b(-X:int) is det.\nb(1).\n").unwrap();
        assert_ne!(one.etag(), other.etag());
        assert!(one.etag().starts_with('"') && one.etag().ends_with('"'));
    }

    #[test]
    fn a_file_without_a_module_comment_falls_back_to_the_generic_title() {
        let catalog = catalog_for("%! a(-X:int) is det.\na(1).\n").unwrap();
        assert_eq!(catalog.title(), DEFAULT_TITLE);
        assert_eq!(catalog.description(), DEFAULT_DESCRIPTION);
    }
}
