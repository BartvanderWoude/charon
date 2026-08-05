mod interpreter;
mod knowledge_base;
mod persistence;
mod predicate;

pub use self::interpreter::Interpreter;
pub use self::knowledge_base::{Error, KnowledgeBase};
pub use self::persistence::DynamicPredicate;
pub use self::predicate::{Argument, Determinism, Doc, Mode, ModuleDoc, Predicate, Type};
