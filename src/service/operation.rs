//! One callable predicate, described independently of any transport.
//!
//! An `Operation` turns JSON arguments into a Prolog goal and the resulting answers back into
//! JSON. Everything a descriptor needs — the argument list, the schemas, the prose, the verb —
//! is settled when the operation is built, so rendering a document never has to consult the
//! knowledge base or handle a case that construction already ruled out.

use std::fmt;

use serde_json::{Map, Value};

use super::marshal::{self, TypeMismatch};
use super::schema;
use crate::prolog::{
    Answer, Determinism, DomainSource, Mode, Predicate, PrologError, Type, quote_atom,
};

/// The intent a predicate's name prefix declares.
///
/// HTTP reads it as a method and strips the prefix from the path; MCP keeps the full predicate
/// name and reads the prefix as tool annotations. One convention, two readings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Verb {
    Get,
    Put,
    Post,
    Delete,
    Patch,
}

impl Verb {
    /// Every verb, in the order a descriptor should list them.
    pub const ALL: [Verb; 5] = [Verb::Get, Verb::Put, Verb::Post, Verb::Delete, Verb::Patch];

    /// Whether calling this leaves the knowledge base unchanged.
    ///
    /// This is the whole definition of "changing": an operation that is not read-only is
    /// withheld unless the server was started with `--persist`. Widening it would expose
    /// operations, narrowing it would hide them.
    pub fn is_read_only(self) -> bool {
        self == Verb::Get
    }

    /// Whether calling it twice has the same effect as calling it once.
    pub fn is_idempotent(self) -> bool {
        self != Verb::Post
    }

    /// Whether it may remove knowledge.
    pub fn is_destructive(self) -> bool {
        self == Verb::Delete
    }

    /// The lower-case name.
    pub fn as_str(self) -> &'static str {
        match self {
            Verb::Get => "get",
            Verb::Put => "put",
            Verb::Post => "post",
            Verb::Delete => "delete",
            Verb::Patch => "patch",
        }
    }

    /// The predicate-name prefix that declares this verb.
    fn prefix(self) -> &'static str {
        match self {
            Verb::Get => "get_",
            Verb::Put => "put_",
            Verb::Post => "post_",
            Verb::Delete => "delete_",
            Verb::Patch => "patch_",
        }
    }
}

impl fmt::Display for Verb {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Whether an argument is supplied by the caller or bound by the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    In,
    Out,
}

/// How a generated goal invokes its predicate.
///
/// A collection's write routes are not plain calls, so keeping the three shapes here lets them
/// share marshalling, escaping, the `catch/3` wrapper and answer shaping with everything else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalShape {
    /// `p(A, B)` — ask the predicate.
    Call,
    /// `( p(A, B) -> true ; assertz(p(A, B)) )` — make it true, idempotently, as PUT promises.
    Assert,
    /// `retract(p(A, B))` — make it false, failing if it was not true, so that a DELETE of an
    /// absent member can answer 404.
    Retract,
}

/// Where an operation came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// A documented predicate, exposed under its own name.
    Predicate,
    /// Generated from a `@collection` declaration, and named after it. These are the only way
    /// the predicate behind them is reachable.
    Collection(String),
}

/// Where an argument's admissible values come from, once a `@domain` tag has been matched to the
/// predicate that holds them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Domain {
    /// The predicate to ask.
    pub predicate: String,
    pub arity: usize,
    /// Which of its arguments supplies the values.
    pub position: usize,
    /// Whether those facts can change while the server runs.
    pub dynamic: bool,
}

impl Domain {
    /// A goal collecting every value in this domain into `Values`.
    pub fn goal(&self) -> String {
        let arguments: Vec<&str> = (0..self.arity)
            .map(|index| if index == self.position { "V" } else { "_" })
            .collect();
        let call = match self.arity {
            0 => quote_atom(&self.predicate),
            _ => format!("{}({})", quote_atom(&self.predicate), arguments.join(", ")),
        };
        // Wrapped like every other generated goal, so a domain predicate that throws cannot
        // leave the machine unusable.
        format!("catch(findall(V, {call}, Values), _, Values = []).")
    }
}

/// A `@domain` tag, before and after the catalog matches it to a predicate.
///
/// Resolution needs the whole operation set — a tag may name a collection — so it happens one
/// stage after the operations are built, and this records which stage a param has reached.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum DomainState {
    #[default]
    None,
    Declared(DomainSource),
    Resolved(Domain),
}

/// One argument, in the order the mode line declared it.
///
/// Declaration order is what makes response keys, OpenAPI parameters and MCP schema properties
/// come out the same way every time, so arguments live in a `Vec` and are never hashed.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub declared_type: Type,
    pub description: Option<String>,
    pub direction: Direction,
    /// The Prolog variable this argument becomes in a generated goal.
    pub variable: String,
    /// This argument's JSON Schema, settled when the operation was built.
    pub schema: Value,
    pub domain: DomainState,
}

impl Param {
    pub fn is_input(&self) -> bool {
        self.direction == Direction::In
    }

    /// The `@domain` tag as written, if it has not been resolved yet.
    pub fn declared_domain(&self) -> Option<&DomainSource> {
        match &self.domain {
            DomainState::Declared(source) => Some(source),
            _ => None,
        }
    }

    /// The domain this argument ranges over, once resolved.
    pub fn domain(&self) -> Option<&Domain> {
        match &self.domain {
            DomainState::Resolved(domain) => Some(domain),
            _ => None,
        }
    }

    /// Records the resolved domain and, when the values cannot change under the server's feet,
    /// narrows the schema to exactly them.
    ///
    /// A `dynamic` domain is deliberately not baked in: descriptor documents are rendered once,
    /// so an enum over facts that `assertz` can add to would go stale on the first write.
    /// Completion still answers such a domain from the live machine.
    pub(super) fn resolve_domain(&mut self, domain: Domain, values: Vec<Value>) {
        if !domain.dynamic
            && !values.is_empty()
            && let Some(schema) = self.schema.as_object_mut()
        {
            schema.insert("enum".to_string(), Value::Array(values));
        }
        self.domain = DomainState::Resolved(domain);
    }
}

/// The predicate a generated goal calls, and how it calls it.
#[derive(Debug, Clone)]
pub struct Goal {
    predicate: String,
    shape: GoalShape,
    /// The variable `catch/3` binds a raised exception to, chosen not to collide with any
    /// argument variable.
    error_variable: String,
}

/// Where HTTP exposes an operation.
#[derive(Debug, Clone)]
pub struct Route {
    pub verb: Verb,
    /// The path, with any verb prefix stripped and no leading slash.
    pub path: String,
    /// Names of inputs carried in the URL path rather than the query string or a body. Empty
    /// for everything but a collection's item routes.
    pub in_path: Vec<String>,
}

/// The prose a descriptor quotes, flattened off the PlDoc comment.
#[derive(Debug, Clone, Default)]
pub struct Docs {
    pub summary: Option<String>,
    pub description: Option<String>,
    /// The PlDoc tags with no more specific home, already rendered as markdown lines.
    pub tags: Vec<String>,
    /// What `@throws`/`@error` promised this predicate can raise.
    pub errors: Vec<String>,
    pub deprecated: bool,
}

/// A documented predicate, exposed as something callable.
#[derive(Debug, Clone)]
pub struct Operation {
    name: String,
    goal: Goal,
    /// Every argument in declaration order, inputs and outputs interleaved as the mode line
    /// wrote them. Building a goal and shaping an answer read this one list.
    arguments: Vec<Param>,
    determinism: Option<Determinism>,
    docs: Docs,
    route: Route,
    origin: Origin,
}

impl TryFrom<Predicate> for Operation {
    type Error = UnsupportedPredicate;

    fn try_from(predicate: Predicate) -> Result<Self, UnsupportedPredicate> {
        if !predicate.name.is_ascii() {
            return Err(UnsupportedPredicate::NonAsciiName {
                name: predicate.name.clone(),
            });
        }

        let mut arguments = Vec::with_capacity(predicate.arguments.len());
        for argument in &predicate.arguments {
            let Some(schema) =
                schema::for_type(&argument.param_type, argument.description.as_deref())
            else {
                return Err(UnsupportedPredicate::UnsupportedType {
                    argument: argument.name.clone(),
                    declared: argument.param_type.to_string(),
                });
            };
            let direction = match argument.mode {
                Some(Mode::Input) => Direction::In,
                Some(Mode::Output) => Direction::Out,
                mode => {
                    return Err(UnsupportedPredicate::UnsupportedMode {
                        argument: argument.name.clone(),
                        mode: mode.map_or_else(|| "none".to_string(), |mode| mode.to_string()),
                    });
                }
            };

            arguments.push(Param {
                name: argument.name.clone(),
                declared_type: argument.param_type.clone(),
                description: argument.description.clone(),
                direction,
                variable: capitalise(&argument.name),
                schema,
                domain: predicate
                    .doc
                    .domains
                    .iter()
                    .find(|domain| domain.argument.eq_ignore_ascii_case(&argument.name))
                    .map_or(DomainState::None, |domain| {
                        DomainState::Declared(domain.source.clone())
                    }),
            });
        }

        // Two arguments sharing a name would share one Prolog variable, quietly unifying values
        // the caller passed separately.
        if let Some(duplicate) = first_duplicate(arguments.iter().map(|param| &param.variable)) {
            return Err(UnsupportedPredicate::DuplicateArgument {
                argument: duplicate,
            });
        }

        let variables: Vec<&str> = arguments
            .iter()
            .map(|param| param.variable.as_str())
            .collect();
        let (verb, path) = parse_route(&predicate.name);

        Ok(Operation {
            goal: Goal {
                predicate: predicate.name.clone(),
                shape: GoalShape::Call,
                error_variable: marshal::error_variable(&variables),
            },
            arguments,
            determinism: predicate.determinism,
            docs: Docs {
                summary: predicate.doc.summary.clone(),
                description: predicate.doc.description.clone(),
                tags: predicate.doc.tag_lines(),
                errors: predicate.doc.errors.clone(),
                deprecated: predicate.doc.deprecated.is_some(),
            },
            route: Route {
                verb,
                path,
                in_path: Vec::new(),
            },
            origin: Origin::Predicate,
            name: predicate.name,
        })
    }
}

impl Operation {
    /// The name this operation answers to: the predicate's own name, or the name a generated
    /// collection route was given.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The predicate this operation calls, whatever it is exposed as.
    pub fn predicate_name(&self) -> &str {
        &self.goal.predicate
    }

    pub fn origin(&self) -> &Origin {
        &self.origin
    }

    /// The path HTTP exposes it under, with any verb prefix stripped.
    pub fn path(&self) -> &str {
        &self.route.path
    }

    pub fn verb(&self) -> Verb {
        self.route.verb
    }

    /// Names of inputs taken from the URL path rather than the query string or a body.
    pub fn path_parameters(&self) -> &[String] {
        &self.route.in_path
    }

    pub fn docs(&self) -> &Docs {
        &self.docs
    }

    /// Re-points a copy of this operation at a different route, goal shape and name, binding
    /// `bound` arguments as inputs.
    ///
    /// This is how a collection's routes are derived from the predicate that enumerates it.
    /// Flipping a direction changes neither an argument's schema nor its variable, so nothing
    /// here has to be re-derived and no second construction path exists to drift from the first.
    pub(super) fn bind(
        &self,
        name: String,
        route: Route,
        shape: GoalShape,
        bound: &[String],
    ) -> Self {
        let mut derived = self.clone();
        derived.name = name;
        derived.goal.shape = shape;
        derived.route = route;
        for param in &mut derived.arguments {
            param.direction = match bound.contains(&param.name) {
                true => Direction::In,
                false => Direction::Out,
            };
        }
        derived
    }

    /// Marks this operation as generated for `collection`.
    pub(super) fn owned_by(mut self, collection: &str) -> Self {
        self.origin = Origin::Collection(collection.to_string());
        self
    }

    /// Overrides the declared determinism. A generated item route is `semidet` however the
    /// predicate that enumerates the collection was declared.
    pub(super) fn with_determinism(mut self, determinism: Determinism) -> Self {
        self.determinism = Some(determinism);
        self
    }

    /// Whether calling this can change the knowledge base.
    ///
    /// Read off the verb, the only declaration of intent Charon has: `get_` promises a question,
    /// every other prefix promises a change. Named here so the catalog, the descriptors and the
    /// startup report cannot drift apart on what "changing" means.
    pub fn is_mutating(&self) -> bool {
        !self.route.verb.is_read_only()
    }

    pub fn determinism(&self) -> Option<Determinism> {
        self.determinism
    }

    /// Every argument, in declaration order.
    pub fn arguments(&self) -> &[Param] {
        &self.arguments
    }

    pub(super) fn arguments_mut(&mut self) -> &mut [Param] {
        &mut self.arguments
    }

    pub fn inputs(&self) -> impl Iterator<Item = &Param> {
        self.arguments.iter().filter(|param| param.is_input())
    }

    pub fn outputs(&self) -> impl Iterator<Item = &Param> {
        self.arguments.iter().filter(|param| !param.is_input())
    }

    /// The input called `name`, if this operation has one.
    pub fn input(&self, name: &str) -> Option<&Param> {
        self.inputs().find(|param| param.name == name)
    }

    pub fn is_deprecated(&self) -> bool {
        self.docs.deprecated
    }

    /// A short human name, for a client that renders a list of operations.
    ///
    /// Distinct from [`Operation::summary`], which is a sentence: a label slot showing a whole
    /// sentence reads as noise in a tool picker.
    pub fn title(&self) -> String {
        capitalise(&self.name.replace('_', " "))
    }

    pub fn summary(&self) -> String {
        self.docs
            .summary
            .clone()
            .unwrap_or_else(|| format!("Call the `{}` predicate.", self.goal.predicate))
    }

    /// The long-form description: the doc comment's later paragraphs, then the determinism and
    /// whatever PlDoc tags have no more specific home.
    pub fn description(&self) -> Option<String> {
        let mut sections = Vec::new();
        if let Some(description) = &self.docs.description {
            sections.push(description.clone());
        }

        let mut meta = Vec::new();
        if let Some(determinism) = self.determinism {
            meta.push(format!("**Determinism:** {determinism}"));
        }
        meta.extend(self.docs.tags.iter().cloned());
        if !meta.is_empty() {
            sections.push(meta.join("  \n"));
        }

        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }

    /// Whether the declared determinism promises at most one solution.
    ///
    /// This single fact decides the response shape, and it is read from the declaration rather
    /// than from how many answers came back. That is what keeps the shape predictable: a
    /// `nondet` predicate that found one solution is still an array, so one solution binding
    /// `[1, 2]` cannot be confused with two solutions binding `1` and `2`.
    pub fn is_singular(&self) -> bool {
        matches!(
            self.determinism,
            Some(Determinism::Det | Determinism::Semidet)
        )
    }

    /// Whether the declared determinism allows the predicate to fail.
    pub(super) fn may_fail(&self) -> bool {
        !matches!(
            self.determinism,
            Some(Determinism::Det | Determinism::Multi)
        )
    }

    /// The JSON Schema of this operation's arguments, as an object with every input required.
    pub fn input_schema(&self) -> Value {
        schema::inputs_of(self)
    }

    /// The JSON Schema of a whole response.
    pub fn response_schema(&self) -> Value {
        schema::response_of(self)
    }

    /// Builds the Prolog goal for this operation, given its arguments as typed JSON.
    ///
    /// The goal is wrapped in `catch/3` so that an exception in the knowledge base comes back as
    /// a binding rather than escaping to scryer's top level, where it would leave the machine
    /// returning that same stale error for every later query.
    pub fn build_goal(&self, arguments: &Map<String, Value>) -> Result<String, ArgumentError> {
        let mut terms = Vec::with_capacity(self.arguments.len());
        for param in &self.arguments {
            if !param.is_input() {
                terms.push(param.variable.clone());
                continue;
            }
            let value = arguments
                .get(&param.name)
                .ok_or_else(|| ArgumentError::Missing {
                    argument: param.name.clone(),
                })?;
            terms.push(
                marshal::term_for(value, &param.declared_type).map_err(|cause| {
                    ArgumentError::Invalid {
                        argument: param.name.clone(),
                        cause,
                    }
                })?,
            );
        }

        let predicate = quote_atom(&self.goal.predicate);
        let call = match terms.is_empty() {
            true => predicate,
            false => format!("{predicate}({})", terms.join(", ")),
        };
        let goal = match self.goal.shape {
            GoalShape::Call => call,
            // Idempotent by construction: adding a member that is already there succeeds and
            // changes nothing, which is what PUT promises and what stops `--persist` writing a
            // duplicate fact on every retry.
            GoalShape::Assert => format!("( {call} -> true ; assertz({call}) )"),
            GoalShape::Retract => format!("retract({call})"),
        };
        Ok(marshal::wrap_goal(&goal, &self.goal.error_variable))
    }

    /// The names of arguments this operation does not accept, out of the ones supplied.
    ///
    /// A caller who misspells an argument otherwise gets "missing required argument `Author`"
    /// while staring at the `Auther` they typed.
    pub fn unexpected_arguments<'a>(
        &self,
        supplied: impl IntoIterator<Item = &'a String>,
    ) -> Vec<&'a String> {
        supplied
            .into_iter()
            .filter(|name| !self.inputs().any(|param| param.name == **name))
            .collect()
    }

    /// Turns the answers to a generated goal into the response body.
    ///
    /// An answer that bound the error variable is the `catch/3` wrapper reporting an exception,
    /// not a solution: it becomes an error even though Prolog technically succeeded.
    pub fn shape_answers(&self, answers: Vec<Answer>) -> Result<Value, PrologError> {
        let mut solutions = Vec::with_capacity(answers.len());
        for answer in answers {
            if let Some(raised) = answer.binding(&self.goal.error_variable)
                && !raised.is_null()
            {
                return Err(PrologError::from_json(raised));
            }
            solutions.push(self.shape_solution(&answer));
        }

        if !self.is_singular() {
            return Ok(Value::Array(solutions));
        }
        Ok(solutions.into_iter().next().unwrap_or({
            // At most one solution was promised and there was none. With no outputs the natural
            // report is `false`; otherwise there is no value to give back.
            match self.outputs().next().is_some() {
                true => Value::Null,
                false => Value::Bool(false),
            }
        }))
    }

    /// One solution: nothing to bind is just whether the query held, a single output is that
    /// value unwrapped, and several are an object keyed by argument name.
    fn shape_solution(&self, answer: &Answer) -> Value {
        // Read the binding by the argument's declared type rather than only by the term's shape:
        // `bool` is published as a JSON boolean and Prolog has only the atoms `true`/`false`.
        let bound = |param: &Param| {
            marshal::json_for(
                answer
                    .binding(&param.variable)
                    .cloned()
                    .unwrap_or(Value::Null),
                &param.declared_type,
            )
        };
        let mut outputs = self.outputs();
        let Some(first) = outputs.next() else {
            return Value::Bool(true);
        };
        if outputs.next().is_none() {
            return bound(first);
        }
        Value::Object(
            self.outputs()
                .map(|param| (param.name.clone(), bound(param)))
                .collect(),
        )
    }
}

/// Splits a predicate name into the intent its prefix declares and the name that remains.
fn parse_route(name: &str) -> (Verb, String) {
    for verb in Verb::ALL {
        if let Some(rest) = name.strip_prefix(verb.prefix())
            && !rest.is_empty()
        {
            return (verb, rest.to_string());
        }
    }
    (Verb::Get, name.to_string())
}

/// `len` → `Len`. Prolog variables must start with an upper-case letter or underscore, and PlDoc
/// argument names need not; the same rule gives an operation its display title.
pub(super) fn capitalise(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn first_duplicate<'a>(values: impl IntoIterator<Item = &'a String>) -> Option<String> {
    let mut seen: Vec<&str> = Vec::new();
    for value in values {
        if seen.contains(&value.as_str()) {
            return Some(value.clone());
        }
        seen.push(value);
    }
    None
}

/// Why a documented predicate could not be exposed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UnsupportedPredicate {
    NonAsciiName { name: String },
    UnsupportedMode { argument: String, mode: String },
    UnsupportedType { argument: String, declared: String },
    DuplicateArgument { argument: String },
}

impl fmt::Display for UnsupportedPredicate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnsupportedPredicate::NonAsciiName { name } => {
                write!(f, "predicate name `{name}` is not ASCII")
            }
            UnsupportedPredicate::UnsupportedMode { argument, mode } => write!(
                f,
                "argument `{argument}` has mode {mode}; only `+` and `-` can be exposed"
            ),
            UnsupportedPredicate::UnsupportedType { argument, declared } => write!(
                f,
                "argument `{argument}` has type `{declared}`, which has no JSON equivalent"
            ),
            UnsupportedPredicate::DuplicateArgument { argument } => write!(
                f,
                "two arguments share the name `{argument}`, so they would share one variable"
            ),
        }
    }
}

/// A problem with the arguments supplied by a caller.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgumentError {
    Missing {
        argument: String,
    },
    Unexpected {
        arguments: Vec<String>,
    },
    Invalid {
        argument: String,
        cause: TypeMismatch,
    },
}

impl fmt::Display for ArgumentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ArgumentError::Missing { argument } => {
                write!(f, "missing required argument `{argument}`")
            }
            ArgumentError::Unexpected { arguments } => {
                write!(f, "unknown argument(s): {}", arguments.join(", "))
            }
            ArgumentError::Invalid { argument, cause } => {
                write!(f, "invalid argument `{argument}`: {cause}")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prolog::Predicate;
    use serde_json::json;
    use std::collections::BTreeMap;

    /// Builds the one operation documented by `source`.
    fn operation(source: &str) -> Operation {
        let predicate = Predicate::parse(source)
            .into_iter()
            .next()
            .expect("the fixture documents a predicate");
        Operation::try_from(predicate).expect("the fixture is exposable")
    }

    fn rejected(source: &str) -> UnsupportedPredicate {
        let predicate = Predicate::parse(source).into_iter().next().unwrap();
        Operation::try_from(predicate).expect_err("the fixture is not exposable")
    }

    fn bindings(pairs: &[(&str, Value)]) -> Answer {
        Answer::Bindings(
            pairs
                .iter()
                .map(|(name, value)| ((*name).to_string(), value.clone()))
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn a_verb_prefix_becomes_a_verb_and_leaves_the_path() {
        assert_eq!(parse_route("get_fruit"), (Verb::Get, "fruit".to_string()));
        assert_eq!(
            parse_route("delete_fruit"),
            (Verb::Delete, "fruit".to_string())
        );
        // No prefix at all reads as a plain lookup.
        assert_eq!(parse_route("fruit"), (Verb::Get, "fruit".to_string()));
        // A bare prefix is treated as a name: `get_` would otherwise route to the empty path.
        assert_eq!(parse_route("get_"), (Verb::Get, "get_".to_string()));
        // Only the documented prefixes count; `getter` keeps its name.
        assert_eq!(parse_route("getter"), (Verb::Get, "getter".to_string()));
    }

    #[test]
    fn arguments_keep_declaration_order_whatever_their_direction() {
        let operation = operation("%! f(-A:int, +B:int, -C:int) is det.\nf(_, _, _).\n");
        let names: Vec<&str> = operation
            .arguments()
            .iter()
            .map(|param| param.name.as_str())
            .collect();
        assert_eq!(names, ["A", "B", "C"]);
        assert_eq!(operation.inputs().count(), 1);
        assert_eq!(operation.outputs().count(), 2);
    }

    #[test]
    fn declared_determinism_decides_the_shape_not_the_answer_count() {
        let singular = operation("%! only(-X:int) is semidet.\nonly(1).\n");
        let plural = operation("%! many(-X:int) is nondet.\nmany(1).\n");
        let answer = vec![bindings(&[("X", json!(1))])];

        assert_eq!(singular.shape_answers(answer.clone()).unwrap(), json!(1));
        assert_eq!(plural.shape_answers(answer).unwrap(), json!([1]));
    }

    #[test]
    fn a_single_list_solution_is_distinguishable_from_several_scalar_ones() {
        let operation = operation("%! parts(-Xs:list(int)) is nondet.\nparts([1,2]).\n");

        let one_list = vec![bindings(&[("Xs", json!([1, 2]))])];
        let two_lists = vec![
            bindings(&[("Xs", json!([1]))]),
            bindings(&[("Xs", json!([2]))]),
        ];
        assert_eq!(operation.shape_answers(one_list).unwrap(), json!([[1, 2]]));
        assert_eq!(
            operation.shape_answers(two_lists).unwrap(),
            json!([[1], [2]])
        );
    }

    #[test]
    fn a_failing_singular_predicate_reports_that_it_failed() {
        let with_output = operation("%! maybe(-X:int) is semidet.\nmaybe(1).\n");
        assert_eq!(with_output.shape_answers(vec![]).unwrap(), Value::Null);

        // With nothing to bind, "it did not hold" is exactly `false`.
        let without_output = operation("%! ready is semidet.\nready.\n");
        assert_eq!(without_output.shape_answers(vec![]).unwrap(), json!(false));
        assert_eq!(
            without_output.shape_answers(vec![Answer::True]).unwrap(),
            json!(true)
        );
    }

    #[test]
    fn several_outputs_come_back_keyed_in_declaration_order() {
        let operation = operation("%! pair(-Zeta:int, -Alpha:int) is det.\npair(1, 2).\n");
        let shaped = operation
            .shape_answers(vec![bindings(&[("Zeta", json!(1)), ("Alpha", json!(2))])])
            .unwrap();
        assert_eq!(shaped.to_string(), r#"{"Zeta":1,"Alpha":2}"#);
    }

    #[test]
    fn a_goal_names_its_outputs_and_marshals_its_inputs() {
        let operation =
            operation("%! hits(+Drugs:list(atom), -Drug:atom) is nondet.\nhits(_, a).\n");
        let arguments = json!({ "Drugs": ["diazepam"] })
            .as_object()
            .unwrap()
            .clone();
        assert_eq!(
            operation.build_goal(&arguments).unwrap(),
            "catch(hits([diazepam], Drug), CharonError__, true)."
        );
    }

    #[test]
    fn a_zero_arity_goal_has_no_argument_list() {
        let operation = operation("%! ready is semidet.\nready.\n");
        assert_eq!(
            operation.build_goal(&Map::new()).unwrap(),
            "catch(ready, CharonError__, true)."
        );
    }

    #[test]
    fn a_missing_or_ill_typed_argument_names_itself() {
        let operation = operation("%! total(+Values:list(int), -Sum:int) is det.\ntotal(_, 0).\n");
        assert_eq!(
            operation.build_goal(&Map::new()).unwrap_err(),
            ArgumentError::Missing {
                argument: "Values".to_string()
            }
        );

        let wrong = json!({ "Values": ["not", "numbers"] })
            .as_object()
            .unwrap()
            .clone();
        let error = operation.build_goal(&wrong).unwrap_err();
        assert!(error.to_string().contains("Values"), "got {error}");
    }

    #[test]
    fn misspelled_arguments_are_named_rather_than_ignored() {
        let operation = operation("%! total(+Values:list(int), -Sum:int) is det.\ntotal(_, 0).\n");
        let supplied = ["Value".to_string(), "Values".to_string()];
        assert_eq!(
            operation.unexpected_arguments(&supplied),
            vec![&"Value".to_string()]
        );
    }

    #[test]
    fn an_exception_binding_becomes_an_error_rather_than_a_solution() {
        let operation = operation("%! raises(-X:int) is det.\nraises(_) :- throw(boom).\n");
        let answer = vec![bindings(&[("CharonError__", json!("boom"))])];
        assert_eq!(operation.shape_answers(answer).unwrap_err().message, "boom");
    }

    #[test]
    fn predicates_that_cannot_be_exposed_say_why() {
        assert_eq!(
            rejected("%! f(?X:int) is det.\nf(_).\n"),
            UnsupportedPredicate::UnsupportedMode {
                argument: "X".to_string(),
                mode: "Input/Output (?)".to_string(),
            }
        );
        assert!(matches!(
            rejected("%! f(+S:stream) is det.\nf(_).\n"),
            UnsupportedPredicate::UnsupportedType { .. }
        ));
        // Two arguments with one name would collapse onto one Prolog variable, silently
        // unifying values the caller passed separately.
        assert_eq!(
            rejected("%! f(+X:int, -X:int) is det.\nf(_, _).\n"),
            UnsupportedPredicate::DuplicateArgument {
                argument: "X".to_string()
            }
        );
    }

    #[test]
    fn every_verb_but_get_is_a_change() {
        // The gate `--persist` opens. `post_` counts: a POST is not safe in HTTP terms, and a
        // knowledge base that spells `post_event(X) :- assertz(...)` must not slip through.
        assert!(!operation("%! get_x(-X:int) is det.\nget_x(1).\n").is_mutating());
        assert!(!operation("%! x(-X:int) is det.\nx(1).\n").is_mutating());
        for source in [
            "%! put_x(+X:int) is det.\nput_x(_).\n",
            "%! post_x(+X:int) is det.\npost_x(_).\n",
            "%! patch_x(+X:int) is det.\npatch_x(_).\n",
            "%! delete_x(+X:int) is det.\ndelete_x(_).\n",
        ] {
            assert!(operation(source).is_mutating(), "{source}");
        }
    }

    #[test]
    fn verb_hints_follow_http_semantics() {
        assert!(Verb::Get.is_read_only() && Verb::Get.is_idempotent());
        assert!(!Verb::Post.is_idempotent());
        assert!(Verb::Put.is_idempotent() && !Verb::Put.is_destructive());
        assert!(Verb::Delete.is_destructive() && Verb::Delete.is_idempotent());
    }
}
