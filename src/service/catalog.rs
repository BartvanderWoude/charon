//! An immutable snapshot of everything derived from one revision of a knowledge base's source.

use std::collections::HashMap;
use std::fmt;

use serde_json::Value;

use crate::prolog::{Interpreter, KnowledgeBase};

use super::build;
use super::collection::{Collection, CollectionError};
use super::operation::{Domain, Operation, UnsupportedPredicate, Verb};
use super::{Config, SharedConfig};

pub const DEFAULT_TITLE: &str = "Charon Knowledge Base API";
pub const DEFAULT_DESCRIPTION: &str = "Generated from a Prolog knowledge base: every documented \
     predicate is callable. A predicate declared `det` or `semidet` answers with its solution \
     directly; any other predicate answers with an array holding one entry per solution, empty \
     if the query failed.";

/// Everything derived from one revision of the source text.
///
/// Nothing here changes while the server runs: `assertz` adds facts rather than documented
/// predicates.
/// That is what lets each adapter render its descriptor document once and cache it against
/// [`Catalog::revision`].
pub struct Catalog {
    pub(super) operations: Vec<Operation>,
    /// Name → index. MCP addresses a tool by name, so this index is also the check that two
    /// operations never answer to one name.
    pub(super) by_name: HashMap<String, usize>,
    /// (path, verb) → index. Two operations colliding here would shadow each other over HTTP.
    pub(super) by_route: HashMap<(String, Verb), usize>,
    pub(super) collections: Vec<Collection>,
    pub(super) interpreter: Interpreter,
    pub(super) config: SharedConfig,
    pub(super) title: String,
    pub(super) description: String,
    pub(super) source: String,
    /// Predicates that were documented but could not be exposed, with the reason.
    pub(super) rejected: Vec<(String, UnsupportedPredicate)>,
    pub(super) revision: u64,
}

impl Catalog {
    /// Builds everything derived from `knowledge_base`'s current source.
    pub fn build(
        knowledge_base: &KnowledgeBase,
        config: &SharedConfig,
    ) -> Result<Self, CatalogError> {
        build::catalog(knowledge_base, config)
    }

    /// A number that changes whenever anything derived from the source does. Adapters cache
    /// their rendered documents against it.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }

    /// Whether this catalog will actually run `operation`, as opposed to merely knowing about it.
    ///
    /// A changing operation is withheld unless the server was started with `--persist`: a server
    /// that cannot write a change down has no business offering to make one, since the change
    /// would live only in the running machine and vanish on restart, leaving the file it claims
    /// to serve quietly wrong.
    pub fn is_available(&self, operation: &Operation) -> bool {
        self.config.persist || !operation.is_mutating()
    }

    /// The operations this process exposes. Every descriptor is rendered from exactly this set,
    /// so what is documented and what is callable cannot disagree.
    pub fn available(&self) -> impl Iterator<Item = &Operation> {
        self.operations
            .iter()
            .filter(|operation| self.is_available(operation))
    }

    /// Operations the source documents but this process withholds. Reported at startup beside
    /// [`Catalog::rejected`], so that a missing endpoint is diagnosable rather than mysterious.
    pub fn withheld(&self) -> impl Iterator<Item = &Operation> {
        self.operations
            .iter()
            .filter(|operation| !self.is_available(operation))
    }

    /// The tables declared with `@collection`.
    pub fn collections(&self) -> &[Collection] {
        &self.collections
    }

    /// The collection called `name`, if there is one.
    pub fn collection(&self, name: &str) -> Option<&Collection> {
        self.collections
            .iter()
            .find(|collection| collection.name() == name)
    }

    /// Every value in `domain`, asked of the live machine. Blocking.
    pub(super) fn domain_values(&self, domain: &Domain) -> Vec<Value> {
        build::domain_values(&self.interpreter, domain)
    }

    /// How many machines answer for this catalog.
    pub fn width(&self) -> usize {
        self.interpreter.width()
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// Predicates that were documented but could not be exposed, and why.
    pub fn rejected(&self) -> &[(String, UnsupportedPredicate)] {
        &self.rejected
    }

    /// The knowledge base's source text.
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn description(&self) -> &str {
        &self.description
    }

    pub(super) fn interpreter(&self) -> &Interpreter {
        &self.interpreter
    }

    /// The operation answering to `name`. This is the lookup MCP uses, since a tool name is flat
    /// and keeps its verb prefix.
    pub fn find(&self, name: &str) -> Option<&Operation> {
        self.by_name.get(name).map(|index| &self.operations[*index])
    }

    /// The operation at `path` for `verb`. HTTP splits the verb out of the predicate name, so
    /// several operations can share a path.
    pub fn route(&self, path: &str, verb: Verb) -> Result<&Operation, RouteError> {
        if let Some(index) = self.by_route.get(&(path.to_string(), verb)) {
            let operation = &self.operations[*index];
            return match self.is_available(operation) {
                true => Ok(operation),
                false => Err(RouteError::Withheld {
                    name: operation.name().to_string(),
                }),
            };
        }
        // Only verbs this process will actually answer belong in `Allow`; advertising one that
        // would be refused sends a caller to a second failure.
        let elsewhere: Vec<Verb> = Verb::ALL
            .into_iter()
            .filter(|other| {
                self.by_route
                    .get(&(path.to_string(), *other))
                    .is_some_and(|index| self.is_available(&self.operations[*index]))
            })
            .collect();
        match elsewhere.is_empty() {
            true => Err(RouteError::NoSuchPath),
            false => Err(RouteError::WrongVerb { allowed: elsewhere }),
        }
    }
}

impl fmt::Debug for Catalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Catalog")
            .field("operations", &self.operations.len())
            .field("rejected", &self.rejected.len())
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

/// Why a revision of a knowledge base could not be turned into a catalog.
#[derive(Debug)]
pub enum CatalogError {
    /// The consult stopped before the end of the file, so what the machine holds is some prefix
    /// of the knowledge base rather than the knowledge base.
    ConsultFailed,
    /// The consult did not define these predicates, even though they are documented. Almost
    /// always one malformed clause earlier in the file.
    PredicatesMissing(Vec<String>),
    /// Two operations answer to one name, so a tool call would be ambiguous.
    DuplicateName(String),
    /// A `@collection` declaration could not be turned into routes.
    Collection(CollectionError),
    /// A `@domain` tag names something the knowledge base does not enumerate.
    UnknownDomain {
        operation: String,
        argument: String,
        source: String,
    },
    /// Two operations land on one HTTP route, so one would shadow the other.
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
            CatalogError::ConsultFailed => write!(
                f,
                "the knowledge base did not load completely.\n\
                 A clause somewhere in the file failed to compile, which aborts scryer's consult \
                 at that point and leaves everything after it undefined — look for a malformed \
                 clause, or an arithmetic expression that cannot be evaluated, such as `X is \
                 foo + 1`."
            ),
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
            CatalogError::Collection(cause) => write!(f, "{cause}"),
            CatalogError::UnknownDomain {
                operation,
                argument,
                source,
            } => write!(
                f,
                "`{operation}` declares that `{argument}` ranges over `{source}`, which is \
                 neither a single-key collection nor a predicate this knowledge base defines."
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
    WrongVerb {
        allowed: Vec<Verb>,
    },
    /// The path and method resolve, but the operation changes the knowledge base and this
    /// process was started without `--persist`.
    Withheld {
        name: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Arc;

    /// Writes `source` to a scratch file and builds a catalog from it, with changing operations
    /// exposed.
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
        let built = Catalog::build(&knowledge_base, &Arc::new(Config::persisting(true)));
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
        // The path exists, but not for this verb — a different answer from "no such path", and
        // the two get different status codes.
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
    fn the_revision_tracks_the_source() {
        let one = catalog_for("%! a(-X:int) is det.\na(1).\n").unwrap();
        let other = catalog_for("%! b(-X:int) is det.\nb(1).\n").unwrap();
        assert_ne!(one.revision(), other.revision());
    }

    #[test]
    fn a_file_without_a_module_comment_falls_back_to_the_generic_title() {
        let catalog = catalog_for("%! a(-X:int) is det.\na(1).\n").unwrap();
        assert_eq!(catalog.title(), DEFAULT_TITLE);
        assert_eq!(catalog.description(), DEFAULT_DESCRIPTION);
    }
}
