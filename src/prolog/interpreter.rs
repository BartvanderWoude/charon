use std::sync::Arc;

use json::JsonValue;
use parking_lot::{ReentrantMutex, RwLock};
use scryer_prolog::{LeafAnswer, Machine, MachineBuilder, Term};

/// `Machine` holds raw pointers into its own arenas, so the compiler can't
/// see that it's safe to move between threads. It only ever gets touched
/// through the `ReentrantMutex<RwLock<_>>` below, which serializes access,
/// so asserting Send/Sync here is sound as long as that invariant holds.
#[derive(Debug)]
struct MachineHandle(Machine);

unsafe impl Send for MachineHandle {}
unsafe impl Sync for MachineHandle {}

impl std::ops::Deref for MachineHandle {
    type Target = Machine;
    fn deref(&self) -> &Machine {
        &self.0
    }
}

impl std::ops::DerefMut for MachineHandle {
    fn deref_mut(&mut self) -> &mut Machine {
        &mut self.0
    }
}

#[derive(Debug, Clone)]
pub struct Interpreter(Arc<ReentrantMutex<RwLock<MachineHandle>>>);

impl Interpreter {
    pub fn load(prolog_code: &str) -> Self {
        let mut wam = MachineBuilder::default()
            .with_toplevel(scryer_prolog::default_toplevel())
            .build();
        wam.load_module_string("", prolog_code);
        Interpreter(Arc::new(ReentrantMutex::new(RwLock::new(MachineHandle(
            wam,
        )))))
    }

    pub fn query(&self, query: &str) -> Vec<Result<JsonValue, JsonValue>> {
        let results: Vec<_> = self.0.lock().write().run_query(query).collect();
        results
            .into_iter()
            .map(|answer| match answer {
                Ok(LeafAnswer::True) => Ok(JsonValue::Boolean(true)),
                Ok(LeafAnswer::False) => Ok(JsonValue::Boolean(false)),
                Ok(LeafAnswer::LeafAnswer { bindings, .. }) => {
                    let mut result = json::object::Object::new();
                    for (var, term) in bindings {
                        result.insert(var.as_str(), Self::parse_term(term));
                    }
                    Ok(JsonValue::Object(result))
                }
                Ok(LeafAnswer::Exception(error)) => Err(Self::parse_term(error)),
                Err(error) => Err(Self::parse_term(error)),
            })
            .collect()
    }

    fn parse_term(term: Term) -> JsonValue {
        match term {
            Term::Var(var) => JsonValue::String(var.to_string()),
            Term::Atom(atom) => JsonValue::String(atom.to_string()),
            Term::Integer(int) => JsonValue::Number(int.to_f64().value().into()),
            Term::Float(float) => JsonValue::Number(float.into()),
            Term::Rational(rational) => JsonValue::Number(rational.to_f64().value().into()),
            Term::Compound(functor, args) => {
                json::object! {
                    functor: functor.to_string(),
                    args: JsonValue::Array(args.into_iter().map(Interpreter::parse_term).collect())
                }
            }
            Term::List(list) => {
                JsonValue::Array(list.into_iter().map(Interpreter::parse_term).collect())
            }
            Term::String(string) => JsonValue::String(string),
            _ => panic!("Unsupported term type: {:?}", term),
        }
    }
}
