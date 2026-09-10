//! The embedded scryer machine, and the one lock that makes it usable from a server.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

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
/// There is deliberately no variant for "the goal has no (further) solutions". scryer reports
/// that as `LeafAnswer::False`, which terminates the answer stream rather than contributing to
/// it. Representing it as an answer would append a stray `false` to the JSON array of every
/// non-deterministic endpoint, so the type gives it nowhere to go.
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
/// reader of a tool result or an error body actually needs. `term` keeps the structured form for
/// callers that want to match on it.
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

    /// Whether this exception reports a defect in the knowledge base rather than a problem with
    /// the caller's data.
    ///
    /// This is what separates a 5xx from a 4xx. `existence_error(procedure, …)` means the rules
    /// call something nobody defined, and no change to the request can fix it; a `type_error`
    /// means the caller sent data the rules could not use, and a different request would work.
    /// A knowledge base that throws a term of its own — `throw(not_eligible)` — is signalling
    /// something about *this* request, so it counts as the caller's business too.
    pub fn is_knowledge_base_fault(&self) -> bool {
        // ISO exceptions are `error(Formal, Context)`; the formal part names the kind.
        let formal = match self.term.get("functor").and_then(Value::as_str) {
            Some("error") => self.term.get("args").and_then(|args| args.get(0)),
            _ => None,
        };
        let Some(formal) = formal else {
            return false;
        };
        // The formal part is either a compound (`existence_error(procedure, f/1)`) or a bare
        // atom (`instantiation_error`), which the term encoding renders as a plain string.
        let kind = formal
            .get("functor")
            .and_then(Value::as_str)
            .or_else(|| formal.as_str());
        matches!(
            kind,
            Some("existence_error" | "resource_error" | "system_error" | "permission_error")
        )
    }
}

impl fmt::Display for PrologError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PrologError {}

/// A fact consulted after the knowledge base, whose presence proves the consult reached the end
/// of the file. Named so that no plausible knowledge base defines it by accident.
const SENTINEL: &str = "'$charon_consult_reached_the_end'";

/// A loaded knowledge base, ready to answer queries.
///
/// Cloning is cheap and shares the same machines. Because a query can run for as long as the
/// knowledge base takes to answer it, callers on an async runtime must not call
/// [`Interpreter::query`] directly from a task — [`crate::service::Service`] moves it onto a
/// blocking thread.
///
/// There may be more than one machine. A machine is a mutex, so with one of them every query in
/// the process serialises behind every other, however read-only they are — two callers asking
/// unrelated questions of a large rule base wait for each other for no reason. Several machines,
/// each consulted from the same source, answer in parallel.
///
/// That is only sound while nothing changes: `assertz` commits to the machine that ran it, and
/// the others would never hear about it. [`Interpreter::load`] therefore builds exactly one, and
/// only [`Interpreter::pool`] builds more — called just where the knowledge base has been shown
/// not to modify itself.
#[derive(Debug, Clone)]
pub struct Interpreter {
    machines: Vec<Arc<Mutex<MachineHandle>>>,
    /// Where the next query starts looking. Round-robin rather than "first free" because it
    /// needs no coordination, and a fair split is what matters when every machine is identical.
    next: Arc<AtomicUsize>,
}

impl Interpreter {
    /// Consults `prolog_code` into a single fresh machine.
    ///
    /// A consult that fails is *silent*: scryer reports it by defining nothing at all, and
    /// `load_module_string` returns nothing to check. [`Interpreter::missing_predicates`] is
    /// what turns that into a diagnosable failure.
    pub fn load(prolog_code: &str) -> Self {
        Interpreter::pool(prolog_code, 1)
    }

    /// Consults `prolog_code` into `count` machines that answer in parallel.
    ///
    /// Only ever call this for a knowledge base that cannot change under itself — see
    /// [`crate::prolog::KnowledgeBase::is_self_modifying`]. Every machine holds a full copy of
    /// the rules, so the cost of a bigger pool is memory.
    pub fn pool(prolog_code: &str, count: usize) -> Self {
        // The sentinel is consulted last, so its presence afterwards means the consult ran to
        // the end. See [`Interpreter::consult_succeeded`] for why that is worth knowing.
        let source = format!("{prolog_code}\n{SENTINEL}.\n");
        let machines = (0..count.max(1))
            .map(|_| {
                let mut machine = MachineBuilder::default()
                    .with_toplevel(scryer_prolog::default_toplevel())
                    .build();
                machine.load_module_string("", &source);
                Arc::new(Mutex::new(MachineHandle(machine)))
            })
            .collect();
        Interpreter {
            machines,
            next: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// How many machines answer for this interpreter.
    pub fn width(&self) -> usize {
        self.machines.len()
    }

    /// Takes the next machine in the rotation, waiting for it if it is busy.
    fn machine(&self) -> parking_lot::MutexGuard<'_, MachineHandle> {
        // With one machine there is nothing to choose. With several, try each in turn without
        // blocking first, so a busy machine does not hold up a caller when a free one exists.
        if self.machines.len() > 1 {
            let start = self.next.fetch_add(1, Ordering::Relaxed);
            for offset in 0..self.machines.len() {
                let index = (start + offset) % self.machines.len();
                if let Some(guard) = self.machines[index].try_lock() {
                    return guard;
                }
            }
            return self.machines[start % self.machines.len()].lock();
        }
        self.machines[0].lock()
    }

    /// Runs `goal` and collects its answers.
    ///
    /// Callers are expected to have wrapped `goal` so that exceptions surface as a binding
    /// rather than propagating (see [`crate::service`]); an `Err` here therefore means something
    /// escaped that wrapper, which leaves the machine in an unreliable state.
    pub fn query(&self, goal: &str) -> Result<Vec<Answer>, PrologError> {
        self.query_bounded(goal, None).map(|(answers, _)| answers)
    }

    /// [`Interpreter::query`], stopping after `max_solutions`. Also reports whether it stopped
    /// early, so a caller can tell a complete answer from a partial one.
    ///
    /// The cap is enforced *here*, by abandoning scryer's answer stream, rather than by wrapping
    /// the goal in a Prolog-level limit. Two reasons: this scryer revision defines no
    /// `call_with_inference_limit/3`, `call_nth/2` or `limit/2` at all, and even where such a
    /// predicate exists it lives in a library the knowledge base would have to have imported
    /// itself. Abandoning the stream leaves the machine perfectly usable.
    ///
    /// This does not bound a goal that never yields a solution at all. Control only returns here
    /// between solutions, so `loop :- loop.` runs until the process ends. scryer
    /// offers no way to interrupt a running goal, so there is no honest way to cap that from
    /// outside; the cap answers the runaway that actually happens, which is a generator with no
    /// termination condition.
    pub fn query_bounded(
        &self,
        goal: &str,
        max_solutions: Option<usize>,
    ) -> Result<(Vec<Answer>, bool), PrologError> {
        let mut guard = self.machine();
        let stream = guard.0.run_query(goal);

        let mut answers = Vec::new();
        let mut truncated = false;
        for result in stream {
            match result {
                Ok(LeafAnswer::True) => answers.push(Answer::True),
                // The end of the answer stream rather than an answer.
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

            // One answer beyond the cap is drawn deliberately: a goal with exactly `max`
            // solutions must report itself complete rather than truncated, and the only way to
            // know which it is is to ask for one more.
            if max_solutions.is_some_and(|max| answers.len() > max) {
                answers.pop();
                truncated = true;
                break;
            }
        }
        Ok((answers, truncated))
    }

    /// Whether the consult ran all the way to the end of the source.
    ///
    /// scryer reports a failed consult by defining nothing further and returning nothing to
    /// check, so the only way to know is to ask what the machine ended up with. Probing the
    /// documented predicates is not enough on its own: a malformed clause *for a predicate that
    /// also has good clauses* leaves `current_predicate` answering true for it, so a file whose
    /// consult aborted half way could still pass every probe — and then throw an existence error
    /// on everything declared after the bad clause.
    ///
    /// A sentinel appended to the very end of the source has no such blind spot. Either the
    /// consult reached it or it did not, whatever went wrong and wherever.
    pub fn consult_succeeded(&self) -> bool {
        matches!(
            self.query(&format!("catch({SENTINEL}, _, fail).")),
            Ok(answers) if !answers.is_empty()
        )
    }

    /// Of the `name/arity` indicators given, those the loaded machine does not actually define.
    ///
    /// scryer reports a failed consult by simply not defining anything, so a knowledge base with
    /// one bad clause loads "successfully" and then answers every request with an existence
    /// error. This probe is what turns that into a startup failure naming what went missing.
    ///
    /// One query per indicator, deliberately. Asking them all at once — a single `findall` over a
    /// disjunction of `current_predicate` checks — reads better and is catastrophically slower:
    /// scryer's cost for that disjunction *doubles* with each branch, so a knowledge base
    /// documenting twenty predicates never finished starting up. Twenty small queries take
    /// milliseconds; one big one takes longer than anyone will wait.
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
    fn a_consult_that_aborted_is_caught_even_where_the_probe_is_blind() {
        // The blind spot: the bad clause belongs to `colour/1`, which already has a good clause,
        // so `current_predicate(colour/1)` answers true and every per-predicate probe passes —
        // while everything after the bad clause is undefined. Editing a served file and
        // reloading it is one way to produce this.
        const BROKEN: &str = "colour(red).\ncolour(X) :- X is bogus + 1.\n";

        // One query per machine: a machine left in this state by an aborted consult can panic
        // inside scryer on the *second* query, which is why the sentinel is asked first of all.
        assert!(
            Interpreter::load(BROKEN).defines("colour", 1),
            "the per-predicate probe cannot see this"
        );
        assert!(
            !Interpreter::load(BROKEN).consult_succeeded(),
            "but the sentinel can: the consult never reached the end of the file"
        );

        assert!(Interpreter::load("colour(red).\n").consult_succeeded());
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
        // An empty list, with no stray `False` element that would otherwise leak into every
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
    fn faults_are_attributed_to_the_knowledge_base_or_to_the_caller() {
        let interpreter = Interpreter::load("boom :- throw(my_error).\ncalls_nothing :- nope.\n");

        // The rules call something that does not exist. No request can fix that.
        let broken = interpreter.query("calls_nothing.").unwrap_err();
        assert!(broken.is_knowledge_base_fault(), "{}", broken.message);

        // A term the knowledge base threw on purpose is about this request rather than the file.
        let deliberate = interpreter.query("boom.").unwrap_err();
        assert!(
            !deliberate.is_knowledge_base_fault(),
            "{}",
            deliberate.message
        );

        // As is data the rules could not use.
        let mistyped = interpreter.query("X is foo + 1.").unwrap_err();
        assert!(!mistyped.is_knowledge_base_fault(), "{}", mistyped.message);
    }

    #[test]
    fn a_capped_query_leaves_the_machine_usable() {
        // The load-bearing fact behind `--max-solutions`: abandoning scryer's answer stream
        // part-way must leave the machine usable. Otherwise one capped request would break every
        // later one, the same class of defect the `catch/3` wrapper exists to prevent.
        let interpreter = Interpreter::load("nat(0).\nnat(N) :- nat(M), N is M+1.\ncolour(red).\n");

        let (answers, truncated) = interpreter.query_bounded("nat(X).", Some(3)).unwrap();
        assert_eq!(answers.len(), 3);
        assert!(
            truncated,
            "an endless generator must report itself cut short"
        );

        // Everything still works afterwards.
        assert_eq!(interpreter.query("colour(C).").unwrap().len(), 1);
        assert!(interpreter.defines("colour", 1));
        assert_eq!(
            interpreter
                .query_bounded("nat(X).", Some(2))
                .unwrap()
                .0
                .len(),
            2
        );
    }

    #[test]
    fn a_query_that_exactly_fills_the_cap_is_not_reported_as_truncated() {
        // Off-by-one that matters: reporting "there may be more" when the caller has in fact
        // seen everything would make every complete answer look partial.
        let interpreter = Interpreter::load("colour(red).\ncolour(green).\ncolour(blue).\n");

        assert!(!interpreter.query_bounded("colour(C).", Some(3)).unwrap().1);
        assert!(!interpreter.query_bounded("colour(C).", Some(10)).unwrap().1);
        assert!(interpreter.query_bounded("colour(C).", Some(2)).unwrap().1);
        // No cap at all collects every solution.
        assert_eq!(
            interpreter
                .query_bounded("colour(C).", None)
                .unwrap()
                .0
                .len(),
            3
        );
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
