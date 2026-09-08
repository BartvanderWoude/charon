//! The engine layer: a `.pl` file, what its PlDoc comments say about it, and a machine to run it.
//!
//! Nothing here knows about JSON transport, HTTP, or MCP. What it exposes is a knowledge base
//! ([`KnowledgeBase`]), the signatures its documentation declares ([`Predicate`]), and a way to
//! ask questions of it ([`Interpreter`]).

mod interpreter;
mod knowledge_base;
mod persistence;
mod predicate;
mod term;

pub use self::interpreter::{Answer, Interpreter, PrologError};
pub use self::knowledge_base::{Error, KnowledgeBase};
pub use self::persistence::DynamicPredicate;
pub use self::predicate::{Argument, Determinism, Doc, Mode, ModuleDoc, Predicate, Type};
pub use self::term::{quote_atom, quote_string};
