mod interpreter;
mod knowledge_base;
mod predicate;

pub use self::interpreter::Interpreter;
pub use self::knowledge_base::{Error, KnowledgeBase};
pub use self::predicate::{Argument, Determinism, Doc, Mode, ModuleDoc, Predicate, Type};
