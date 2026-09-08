//! The embedded scryer machine, and the one lock that makes it usable from a server.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use parking_lot::Mutex;
use scryer_prolog::{LeafAnswer, Machine, MachineBuilder, Term};
use serde_json::Value;

use super::term::{self, quote_atom};

/// `Machine` holds raw pointers into its own arenas, so the compiler cannot see that moving it
/// between threads is safe. It is only ever touched through the [`Mutex`] in [`Interpreter`],
/// which serialises every access, so asserting `Send`/`Sync` here is sound exactly as long as
/// that invariant holds. Never hand a `MachineHandle` out from behind the lock.
#[derive(Debug)]
struct MachineHandle(Machine);

unsafe impl Send for MachineHandle {}
unsafe impl Sync for MachineHandle {}

/// One answer to a query.
///
/// Note what is *not* here: a variant for "the goal has no (further) solutions". scryer reports
/// that as `LeafAnswer::False`, which terminates the answer stream rather than contributing to
/// it. Representing it as an answer is what used to append a stray `false` to the JSON array of
/// every non-deterministic endpoint, so the type deliberately gives it nowhere to go.
#[derive(Debug, Clone, PartialEq)]
pub enum Answer {
    /// A solution, as the bindings of the query's variables.
    Bindings(BTreeMap<String, Value>),
    /// The goal succeeded without binding anything, i.e. a predicate with no output arguments.
    True,
}

impl Answer {
    /// The value bound to `variable` in this answer, if any.
    pub fn binding(&self, variable: &str) -> Option<&Value> {
        match self {
            Answer::Bindings(bindings) => bindings.get(variable),
            Answer::True => None,
        }
    }
}

/// An exception raised by a Prolog query.
///
/// `message` is the term in Prolog syntax (`type_error(evaluable,foo/0)`), which is what a
/// human — or a language model reading a tool result — actually needs. `term` keeps the
/// structured form for callers that want to match on it.
#[derive(Debug, Clone, PartialEq)]
pub struct PrologError {
    pub message: String,
    pub term: Value,
}

impl PrologError {
    fn from_term(raised: Term) -> Self {
        PrologError {
            message: term::render(&raised),
            term: term::to_json(raised),
        }
    }

    /// The same error, reconstructed from the JSON encoding a `catch/3` binding delivers.
    pub fn from_json(raised: &Value) -> Self {
        PrologError {
            message: term::render_json(raised),
            term: raised.clone(),
        }
    }
}

impl fmt::Display for PrologError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PrologError {}

/// A loaded knowledge base, ready to answer queries.
///
/// Cloning is cheap and shares the same machine: there is exactly one interpreter per catalog,
/// and queries against it are serialised. Because a query can run for as long as the knowledge
/// base takes to answer it, callers on an async runtime must not call [`Interpreter::query`]
/// directly from a task — [`crate::service::Service`] moves it onto a blocking thread.
#[derive(Debug, Clone)]
pub struct Interpreter(Arc<Mutex<MachineHandle>>);

impl Interpreter {
    /// Consults `prolog_code` into a fresh machine.
    ///
    /// A consult that fails is *silent*: scryer reports it by defining nothing at all, and
    /// `load_module_string` returns nothing to check. [`Interpreter::missing_predicates`] is
    /// what turns that into a diagnosable failure.
    pub fn load(prolog_code: &str) -> Self {
        let mut machine = MachineBuilder::default()
            .with_toplevel(scryer_prolog::default_toplevel())
            .build();
        machine.load_module_string("", prolog_code);
        Interpreter(Arc::new(Mutex::new(MachineHandle(machine))))
    }

    /// Runs `goal` and collects its answers.
    ///
    /// Callers are expected to have wrapped `goal` so that exceptions surface as a binding
    /// rather than propagating (see [`crate::service`]); an `Err` here therefore means something
    /// escaped that wrapper, which leaves the machine in an unreliable state.
    pub fn query(&self, goal: &str) -> Result<Vec<Answer>, PrologError> {
        let results: Vec<_> = self.0.lock().0.run_query(goal).collect();

        let mut answers = Vec::new();
        for result in results {
            match result {
                Ok(LeafAnswer::True) => answers.push(Answer::True),
                // End of the answer stream, not an answer.
                Ok(LeafAnswer::False) => break,
                Ok(LeafAnswer::LeafAnswer { bindings, .. }) => {
                    let bindings = bindings
                        .into_iter()
                        .map(|(variable, bound)| (variable.to_string(), term::to_json(bound)))
                        .collect();
                    answers.push(Answer::Bindings(bindings));
                }
                Ok(LeafAnswer::Exception(raised)) | Err(raised) => {
                    return Err(PrologError::from_term(raised));
                }
            }
        }
        Ok(answers)
    }

    /// Of the `name/arity` indicators given, those the loaded machine does not actually define.
    ///
    /// scryer reports a failed consult by simply not defining anything, so a knowledge base with
    /// one bad clause loads "successfully" and then answers every request with an existence
    /// error. This probe is what turns that into a startup failure naming what went missing.
    pub fn missing_predicates<'a>(
        &self,
        indicators: impl IntoIterator<Item = (&'a str, usize)>,
    ) -> Vec<String> {
        indicators
            .into_iter()
            .filter(|(name, arity)| !self.defines(name, *arity))
            .map(|(name, arity)| format!("{name}/{arity}"))
            .collect()
    }

    /// Whether `name/arity` is defined in the loaded machine.
    pub fn defines(&self, name: &str, arity: usize) -> bool {
        // `catch/3` keeps a malformed indicator from escaping as an exception, which would
        // poison the machine before it has served a single request.
        let goal = format!(
            "catch(current_predicate({}/{arity}), _, fail).",
            quote_atom(name)
        );
        matches!(self.query(&goal), Ok(answers) if !answers.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_consult_is_visible_only_through_the_probe() {
        // `broken/2` cannot be compiled, which aborts the whole consult — `healthy/1` above it
        // is left undefined even though nothing is wrong with it. Loading still "succeeds".
        let interpreter = Interpreter::load("healthy(1).\nbroken(_, X) :- X is foo + 1.\n");
        assert_eq!(
            interpreter.missing_predicates([("healthy", 1)]),
            vec!["healthy/1".to_string()]
        );
    }

    #[test]
    fn a_healthy_consult_defines_what_it_declares() {
        let interpreter = Interpreter::load("healthy(1).\n");
        assert!(interpreter.defines("healthy", 1));
        assert!(!interpreter.defines("healthy", 2));
        assert!(interpreter.missing_predicates([("healthy", 1)]).is_empty());
    }

    #[test]
    fn a_goal_with_no_solutions_yields_no_answers() {
        // Not one `False` answer — that stray element is what used to leak into every
        // non-deterministic response.
        let interpreter = Interpreter::load("colour(red).\n");
        assert!(interpreter.query("colour(blue).").unwrap().is_empty());
    }

    #[test]
    fn an_escaping_exception_is_rendered_in_prolog_syntax() {
        let interpreter = Interpreter::load("boom :- throw(my_error).\n");
        let error = interpreter.query("boom.").unwrap_err();
        assert_eq!(error.message, "my_error");
    }

    #[test]
    fn a_malformed_indicator_does_not_poison_the_machine() {
        let interpreter = Interpreter::load("healthy(1).\n");
        // A name that needs quoting would be a syntax error unquoted; `defines` must survive it
        // and leave the machine able to answer the next question.
        assert!(!interpreter.defines("no such predicate", 1));
        assert!(interpreter.defines("healthy", 1));
    }
}
