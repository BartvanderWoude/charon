//! The transport-neutral core: what a knowledge base makes callable, and how to call it.
//!
//! Both interfaces are built on exactly this — the operations, their schemas, the marshalling
//! and the catalog — but neither wire format lives here. The OpenAPI document belongs to
//! `http`, the MCP tool and resource lists to `mcp`, so that adding or changing one interface
//! cannot reach the other.

mod build;
mod catalog;
mod coerce;
mod collection;
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
pub use coerce::value_from_text;
pub use collection::{Collection, CollectionError, Column, KeyError};
pub use marshal::TypeMismatch;
pub use operation::{
    ArgumentError, Direction, Docs, Domain, Operation, Origin, Param, Route, UnsupportedPredicate,
    Verb,
};

/// A `Config` shared by the service and every catalog it builds, so the settings a process runs
/// under exist exactly once.
pub type SharedConfig = Arc<Config>;

/// The most solutions one call will collect before reporting itself cut short.
///
/// A bound exists because a knowledge base can describe an endless relation, and collecting one
/// would materialise solutions into a `Vec` forever while holding the machine's lock.
pub const DEFAULT_MAX_SOLUTIONS: usize = 1000;

/// How a [`Service`] should treat its knowledge base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Mirror `assertz`/`retract` changes back into the source file after every call.
    ///
    /// Also the gate on *changing* operations: with this off, a predicate whose name carries a
    /// `put_`/`post_`/`patch_`/`delete_` prefix is withheld entirely rather than being run
    /// against a machine whose changes nobody will write down.
    pub persist: bool,
    /// The most solutions one call may collect, or `None` for no bound.
    pub max_solutions: Option<usize>,
    /// How many machines to answer from, when the knowledge base allows more than one.
    ///
    /// A machine is a mutex, so one machine means every query in the process serialises behind
    /// every other. Ignored for a knowledge base that modifies itself, where a second machine
    /// would simply be wrong.
    pub interpreters: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            persist: false,
            max_solutions: Some(DEFAULT_MAX_SOLUTIONS),
            interpreters: default_interpreters(),
        }
    }
}

/// How many machines to run by default: enough to use the machine's cores, capped because each
/// one holds a full copy of the knowledge base.
fn default_interpreters() -> usize {
    std::thread::available_parallelism()
        .map(|parallelism| parallelism.get().min(4))
        .unwrap_or(1)
}

impl Config {
    /// How many machines this configuration actually wants.
    ///
    /// One whenever changes are being written down: `assertz` commits to the machine that ran
    /// it, so a pool would leave the rest of them disagreeing with both it and the file.
    pub fn interpreter_count(&self) -> usize {
        match self.persist {
            true => 1,
            false => self.interpreters.max(1),
        }
    }

    pub fn persisting(persist: bool) -> Self {
        Config {
            persist,
            ..Config::default()
        }
    }
}

/// What a call produced.
///
/// Carries the truncation flag beside the body rather than inside it, so that the exact response
/// schema [`Operation::is_singular`] guarantees is not disturbed by a field that only sometimes
/// applies. Each adapter reports it in its own idiom.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub value: Value,
    /// Whether the solution cap cut the answer short, i.e. there were more solutions to be had.
    pub truncated: bool,
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

/// Owns the interpreter and the current catalog, and answers calls against them.
pub struct Service {
    path: PathBuf,
    /// Bounds how many queries may be in flight at once.
    ///
    /// `spawn_blocking` will happily start hundreds of threads, and every one of them would then
    /// queue behind the same handful of machines. Holding the excess here keeps the waiting in
    /// the runtime, where it costs a task rather than a thread.
    queries: Arc<tokio::sync::Semaphore>,
    /// Shared with every catalog this service builds, so the settings exist once and a reload
    /// cannot rebuild under different ones.
    config: SharedConfig,
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
        let config = Arc::new(config);
        let catalog = Catalog::build(&knowledge_base, &config)?;

        let persistence = config.persist.then(|| {
            let dynamic_predicates = knowledge_base.dynamic_predicates();
            Mutex::new(Persistence {
                knowledge_base,
                dynamic_predicates,
            })
        });

        Ok(Service {
            path,
            queries: Arc::new(tokio::sync::Semaphore::new(config.interpreter_count())),
            catalog: RwLock::new(Arc::new(catalog)),
            config,
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
    /// One method covers three needs that reduce to the same thing — rebuild everything derived
    /// from the source: picking up an edited `.pl` file, moving the catalog's revision on so
    /// adapters re-render their descriptors, and replacing an interpreter whose machine has
    /// become unreliable.
    ///
    /// On failure the existing catalog is left in place, so a bad edit degrades to "the last good
    /// version keeps serving" rather than to a dead server.
    pub fn reload(&self) -> Result<(), StartupError> {
        let knowledge_base = KnowledgeBase::load(&self.path).map_err(StartupError::Io)?;
        let catalog = Catalog::build(&knowledge_base, &self.config)?;

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
    ) -> Result<Outcome, CallError> {
        let operation = catalog
            .find(name)
            .ok_or_else(|| CallError::UnknownOperation {
                name: name.to_string(),
            })?;

        // Checked here rather than in either adapter, so that HTTP and MCP refuse the same
        // operations in the same words; and checked before the arguments are validated, so a
        // withheld operation does not describe its own signature to a caller who may not run it.
        if !catalog.is_available(operation) {
            return Err(CallError::Withheld {
                name: name.to_string(),
            });
        }

        let unexpected = operation.unexpected_arguments(arguments.keys());
        if !unexpected.is_empty() {
            return Err(ArgumentError::Unexpected {
                arguments: unexpected.into_iter().cloned().collect(),
            }
            .into());
        }

        let goal = operation.build_goal(arguments)?;
        tracing::debug!(operation = name, %goal, "querying");
        let max_solutions = catalog.config().max_solutions;
        let answers = catalog.interpreter().query_bounded(&goal, max_solutions);

        // Runs regardless of the outcome: `assertz`/`retract` commit to the live machine as soon
        // as they run, even if a later goal in the same query fails or throws.
        if let Some(persistence) = &self.persistence {
            persistence.lock().sync(catalog.interpreter());
        }

        let (answers, truncated) = answers?;
        if truncated {
            tracing::warn!(
                operation = name,
                cap = max_solutions,
                "the solution cap cut this answer short"
            );
        }
        Ok(Outcome {
            value: operation.shape_answers(answers)?,
            truncated,
        })
    }

    /// Every value in `domain`, asked of the live knowledge base.
    ///
    /// Answered per request rather than from a cache because a `dynamic` domain changes as facts
    /// are asserted, and a completion list that is stale is worse than none.
    pub async fn domain_values(
        self: Arc<Self>,
        catalog: Arc<Catalog>,
        domain: Domain,
    ) -> Vec<Value> {
        let permit = Arc::clone(&self.queries).acquire_owned().await;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            catalog.domain_values(&domain)
        })
        .await
        .unwrap_or_default()
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
    ) -> Result<Outcome, CallError> {
        let permit = Arc::clone(&self.queries).acquire_owned().await;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            self.call_blocking(&catalog, &name, &arguments)
        })
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
    /// The operation exists and is well-formed, but this process declines to run it: it changes
    /// the knowledge base and was started without `--persist`.
    Withheld {
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
            // The one place this sentence is written. Both adapters quote it, so a caller reading
            // a tool error and a developer reading a 403 body are told the same thing.
            CallError::Withheld { name } => write!(
                f,
                "`{name}` changes the knowledge base, which this server was started without \
                 `--persist` to allow; restart it with `--persist` to expose it"
            ),
            CallError::Argument(error) => write!(f, "{error}"),
            CallError::Prolog(error) => write!(f, "{error}"),
            CallError::Interrupted => write!(f, "the query did not finish"),
        }
    }
}

impl std::error::Error for CallError {}
