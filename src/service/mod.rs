//! The transport-neutral core: everything both the HTTP/OpenAPI interface and the MCP interface
//! need, and nothing either of them needs alone.

mod catalog;
mod descriptors;
mod marshal;
mod operation;
mod schema;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::{Map, Value};

use crate::prolog::{DynamicPredicate, Interpreter, KnowledgeBase, PrologError};

pub use catalog::{Catalog, CatalogError, DEFAULT_DESCRIPTION, DEFAULT_TITLE, RouteError};
pub use descriptors::{SOURCE_RESOURCE_MIME_TYPE, SOURCE_RESOURCE_URI, mcp_resources};
pub use marshal::TypeMismatch;
pub use operation::{ArgumentError, Operation, Param, UnsupportedPredicate, Verb};

/// How a [`Service`] should treat its knowledge base.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Config {
    /// Mirror `assertz`/`retract` changes back into the source file after every call.
    pub persist: bool,
}

impl Config {
    pub fn persisting(persist: bool) -> Self {
        Config { persist }
    }
}

/// Bundles the knowledge base with the dynamic predicates to mirror back into its source file
/// after every request, when persistence is enabled.
struct Persistence {
    knowledge_base: KnowledgeBase,
    dynamic_predicates: Vec<DynamicPredicate>,
}

impl Persistence {
    fn sync(&mut self, interpreter: &Interpreter) {
        match self
            .knowledge_base
            .persist(interpreter, &self.dynamic_predicates)
        {
            Ok(true) => tracing::debug!(path = %self.knowledge_base.path().display(), "persisted"),
            Ok(false) => {}
            Err(error) => tracing::error!(%error, "failed to persist knowledge base"),
        }
    }
}

/// Owns the interpreter, the operations and the cached descriptors.
pub struct Service {
    path: PathBuf,
    catalog: RwLock<Arc<Catalog>>,
    persistence: Option<Mutex<Persistence>>,
}

impl Service {
    /// Loads a knowledge base and prepares it for serving.
    pub fn new(knowledge_base: KnowledgeBase, config: Config) -> Result<Self, StartupError> {
        let path = knowledge_base.path().to_path_buf();
        // Checked before anything else so that `--persist` against a file nobody may write
        // fails now, rather than after the first `assertz` has already happened.
        if config.persist {
            knowledge_base.ensure_writable().map_err(StartupError::Io)?;
        }
        let catalog = Catalog::build(&knowledge_base)?;

        let persistence = config.persist.then(|| {
            let dynamic_predicates = knowledge_base.dynamic_predicates();
            Mutex::new(Persistence {
                knowledge_base,
                dynamic_predicates,
            })
        });

        Ok(Service {
            path,
            catalog: RwLock::new(Arc::new(catalog)),
            persistence,
        })
    }

    /// Convenience for the common case: load `path`, then build the service.
    pub fn open(path: impl AsRef<Path>, config: Config) -> Result<Self, StartupError> {
        Service::new(KnowledgeBase::load(path).map_err(StartupError::Io)?, config)
    }

    /// The current snapshot. Cheap: callers clone an `Arc` and read from it for the whole of a
    /// request without holding a lock.
    pub fn catalog(&self) -> Arc<Catalog> {
        Arc::clone(&self.catalog.read())
    }

    /// The file this service was loaded from.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Whether changes are mirrored back to the source file.
    pub fn is_persisting(&self) -> bool {
        self.persistence.is_some()
    }

    /// Re-reads the knowledge base from disk and swaps in a fresh catalog.
    ///
    /// One method covers three needs that all reduce to the same thing — rebuild everything
    /// derived from the source: picking up an edited `.pl` file, invalidating the cached
    /// descriptor documents, and replacing an interpreter whose machine has become unreliable.
    ///
    /// On failure the existing catalog is left in place, so a bad edit degrades to "the old
    /// version keeps serving" rather than to a dead server.
    pub fn reload(&self) -> Result<(), StartupError> {
        let knowledge_base = KnowledgeBase::load(&self.path).map_err(StartupError::Io)?;
        let catalog = Catalog::build(&knowledge_base)?;

        if let Some(persistence) = &self.persistence {
            let mut persistence = persistence.lock();
            persistence.dynamic_predicates = knowledge_base.dynamic_predicates();
            persistence.knowledge_base = knowledge_base;
        }

        *self.catalog.write() = Arc::new(catalog);
        Ok(())
    }

    /// Runs the operation named `name` against `catalog`, with the given typed arguments.
    ///
    /// Blocking: a Prolog query runs for as long as the knowledge base takes, and there is one
    /// machine, so calls serialise. On an async runtime use [`Service::call`] instead, which
    /// moves this onto a blocking thread rather than stalling a worker.
    pub fn call_blocking(
        &self,
        catalog: &Catalog,
        name: &str,
        arguments: &Map<String, Value>,
    ) -> Result<Value, CallError> {
        let operation = catalog
            .find(name)
            .ok_or_else(|| CallError::UnknownOperation {
                name: name.to_string(),
            })?;

        let unexpected = operation.unexpected_arguments(arguments.keys());
        if !unexpected.is_empty() {
            return Err(ArgumentError::Unexpected {
                arguments: unexpected.into_iter().cloned().collect(),
            }
            .into());
        }

        let goal = operation.build_goal(arguments)?;
        tracing::debug!(operation = name, %goal, "querying");
        let answers = catalog.interpreter().query(&goal);

        // Runs regardless of the outcome: `assertz`/`retract` commit to the live machine as soon
        // as they run, even if a later goal in the same query fails or throws.
        if let Some(persistence) = &self.persistence {
            persistence.lock().sync(catalog.interpreter());
        }

        Ok(operation.shape_answers(answers?)?)
    }

    /// [`Service::call_blocking`], moved off the async runtime's worker threads.
    ///
    /// A knowledge base can take arbitrarily long to answer — recursion over a large fact base,
    /// a deep search — and running that inside a task blocks a tokio worker for the duration.
    /// With a small worker pool, a handful of slow queries is enough to stop the server
    /// answering anything at all, including `/openapi.json`.
    pub async fn call(
        self: Arc<Self>,
        catalog: Arc<Catalog>,
        name: String,
        arguments: Map<String, Value>,
    ) -> Result<Value, CallError> {
        tokio::task::spawn_blocking(move || self.call_blocking(&catalog, &name, &arguments))
            .await
            .unwrap_or_else(|error| {
                tracing::error!(%error, "the query task did not finish");
                Err(CallError::Interrupted)
            })
    }
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service")
            .field("path", &self.path)
            .field("persisting", &self.is_persisting())
            .finish_non_exhaustive()
    }
}

/// Why the server could not start.
#[derive(Debug)]
pub enum StartupError {
    /// The source file could not be read, or could not be written when `--persist` asked for it.
    Io(crate::prolog::Error),
    /// The source loaded, but does not describe a coherent set of operations.
    Catalog(CatalogError),
}

impl From<CatalogError> for StartupError {
    fn from(error: CatalogError) -> Self {
        StartupError::Catalog(error)
    }
}

impl fmt::Display for StartupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StartupError::Io(error) => write!(f, "{error}"),
            StartupError::Catalog(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for StartupError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            StartupError::Io(error) => Some(error),
            StartupError::Catalog(error) => Some(error),
        }
    }
}

/// Why a call could not be answered.
#[derive(Debug)]
pub enum CallError {
    UnknownOperation {
        name: String,
    },
    Argument(ArgumentError),
    Prolog(PrologError),
    /// The query task was cancelled or panicked. Not a caller's fault, and not retryable by
    /// changing the request.
    Interrupted,
}

impl From<ArgumentError> for CallError {
    fn from(error: ArgumentError) -> Self {
        CallError::Argument(error)
    }
}

impl From<PrologError> for CallError {
    fn from(error: PrologError) -> Self {
        CallError::Prolog(error)
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CallError::UnknownOperation { name } => write!(f, "unknown predicate: {name}"),
            CallError::Argument(error) => write!(f, "{error}"),
            CallError::Prolog(error) => write!(f, "{error}"),
            CallError::Interrupted => write!(f, "the query did not finish"),
        }
    }
}

impl std::error::Error for CallError {}
