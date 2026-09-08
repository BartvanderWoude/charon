//! One callable predicate, described independently of any transport.
//!
//! An `Operation` knows how to turn JSON arguments into a Prolog goal and the resulting answers
//! back into JSON, and it can describe its own input and output shapes as JSON Schema. The verb it
//! carries is a neutral hint that each adapter reads in its own way, rather than an OpenAPI type
//! used as the internal representation.

use std::fmt;

use serde_json::{Map, Value, json};

use super::marshal::{self, TypeMismatch};
use super::schema;
use crate::prolog::{Answer, Determinism, Mode, Predicate, PrologError, Type, quote_atom};

/// The intent a predicate's name prefix declares.
///
/// HTTP reads this as a method and strips the prefix from the path; MCP reads it as tool
/// annotations and keeps the full predicate name. One convention, two readings.
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

    /// Whether calling this can be assumed to leave the knowledge base unchanged.
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

    /// Whether an argument for it is conventionally carried in a request body rather than in the
    /// query string. Read by the HTTP adapter and by the OpenAPI renderer; both must agree.
    pub fn carries_a_body(self) -> bool {
        matches!(self, Verb::Put | Verb::Post | Verb::Patch)
    }

    /// The lower-case name, which is also how OpenAPI keys a path item.
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

/// One input or output argument, in the order the mode line declared it.
///
/// Declaration order is preserved rather than hashed, so response keys, OpenAPI parameters and
/// MCP schema properties all come out in a stable order. Iterating a `HashMap` here is what used
/// to make JSON object key order vary from run to run.
#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub declared_type: Type,
    pub description: Option<String>,
    /// The Prolog variable this argument becomes in a generated goal.
    pub variable: String,
    /// The JSON Schema for this argument, resolved once when the operation was accepted. Holding
    /// it here is what lets every descriptor be rendered without an `expect` for a case that
    /// construction already ruled out.
    schema: Value,
}

impl Param {
    pub fn schema(&self) -> &Value {
        &self.schema
    }
}

/// A documented predicate, exposed as something callable.
#[derive(Debug, Clone)]
pub struct Operation {
    predicate: Predicate,
    verb: Verb,
    path: String,
    inputs: Vec<Param>,
    outputs: Vec<Param>,
    error_variable: String,
}

impl TryFrom<Predicate> for Operation {
    type Error = UnsupportedPredicate;

    fn try_from(predicate: Predicate) -> Result<Self, UnsupportedPredicate> {
        if !predicate.name.is_ascii() {
            return Err(UnsupportedPredicate::NonAsciiName {
                name: predicate.name.clone(),
            });
        }

        let (verb, path) = parse_route(&predicate.name);

        let mut inputs = Vec::new();
        let mut outputs = Vec::new();
        for argument in predicate.arguments.iter() {
            let Some(schema) =
                schema::for_type(&argument.param_type, argument.description.as_deref())
            else {
                return Err(UnsupportedPredicate::UnsupportedType {
                    argument: argument.name.clone(),
                    declared: argument.param_type.to_string(),
                });
            };

            let param = Param {
                name: argument.name.clone(),
                declared_type: argument.param_type.clone(),
                description: argument.description.clone(),
                variable: marshal::variable_name(&argument.name),
                schema,
            };

            match argument.mode {
                Some(Mode::Input) => inputs.push(param),
                Some(Mode::Output) => outputs.push(param),
                mode => {
                    return Err(UnsupportedPredicate::UnsupportedMode {
                        argument: argument.name.clone(),
                        mode: mode.map_or_else(|| "none".to_string(), |mode| mode.to_string()),
                    });
                }
            }
        }

        // Two arguments sharing a name would share a Prolog variable, quietly unifying values a
        // caller passed separately.
        if let Some(duplicate) = first_duplicate(inputs.iter().chain(&outputs).map(|p| &p.variable))
        {
            return Err(UnsupportedPredicate::DuplicateArgument {
                argument: duplicate,
            });
        }

        let variables: Vec<String> = inputs
            .iter()
            .chain(outputs.iter())
            .map(|param| param.variable.clone())
            .collect();

        Ok(Operation {
            error_variable: marshal::error_variable(&variables),
            predicate,
            verb,
            path,
            inputs,
            outputs,
        })
    }
}

impl Operation {
    /// The predicate's own name — the identity MCP exposes as a tool name.
    pub fn name(&self) -> &str {
        &self.predicate.name
    }

    /// The path HTTP exposes it under, with any verb prefix stripped.
    pub fn path(&self) -> &str {
        &self.path
    }

    pub fn verb(&self) -> Verb {
        self.verb
    }

    pub fn determinism(&self) -> Option<Determinism> {
        self.predicate.determinism
    }

    pub fn inputs(&self) -> &[Param] {
        &self.inputs
    }

    pub fn outputs(&self) -> &[Param] {
        &self.outputs
    }

    pub fn is_deprecated(&self) -> bool {
        self.predicate.doc.deprecated.is_some()
    }

    pub fn summary(&self) -> String {
        self.predicate
            .doc
            .summary
            .clone()
            .unwrap_or_else(|| format!("Call the `{}` predicate.", self.predicate.name))
    }

    /// The long-form description: the doc comment's later paragraphs, plus a rendered appendix
    /// of the PlDoc tags that have no more specific home.
    pub fn description(&self) -> Option<String> {
        let doc = &self.predicate.doc;
        let mut sections = Vec::new();
        if let Some(description) = &doc.description {
            sections.push(description.clone());
        }

        let mut meta = Vec::new();
        if let Some(determinism) = self.predicate.determinism {
            meta.push(format!("**Determinism:** {determinism}"));
        }
        meta.extend(doc.tag_lines());
        if !meta.is_empty() {
            sections.push(meta.join("  \n"));
        }

        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }

    /// Whether the declared determinism promises at most one solution.
    ///
    /// This is the single fact the response shape turns on. Deciding it from the *declaration*
    /// rather than from how many answers happened to come back is what makes the shape
    /// predictable: a `nondet` predicate that found one solution is still an array, so a caller
    /// can no longer confuse one solution binding `[1, 2]` with two solutions binding `1` and
    /// `2` — which, when the answer decided the shape, were byte-identical.
    pub fn is_singular(&self) -> bool {
        matches!(
            self.predicate.determinism,
            Some(Determinism::Det | Determinism::Semidet)
        )
    }

    /// Whether the declared determinism allows the predicate to fail.
    fn may_fail(&self) -> bool {
        !matches!(
            self.predicate.determinism,
            Some(Determinism::Det | Determinism::Multi)
        )
    }

    /// The JSON Schema of this operation's arguments, as an object with every input required.
    pub fn input_schema(&self) -> Value {
        schema::object(
            self.inputs
                .iter()
                .map(|param| (param.name.clone(), param.schema.clone())),
        )
    }

    /// The JSON Schema of a single solution.
    ///
    /// Zero outputs is just whether the query held; exactly one output is that value directly,
    /// unwrapped; two or more is an object keyed by argument name.
    pub fn solution_schema(&self) -> Value {
        match self.outputs.as_slice() {
            [] => json!({ "type": "boolean" }),
            [only] => only.schema.clone(),
            outputs => schema::object(
                outputs
                    .iter()
                    .map(|param| (param.name.clone(), param.schema.clone())),
            ),
        }
    }

    /// The JSON Schema of a whole response.
    ///
    /// Exact, not a disjunction of every shape the runtime might reach: [`Self::is_singular`]
    /// settles array-or-not before a single answer exists.
    pub fn response_schema(&self) -> Value {
        let solution = self.solution_schema();
        if self.is_singular() {
            // A `semidet` predicate that fails answers `null`, or `false` when it has no outputs
            // (in which case the boolean schema already covers it).
            return if self.may_fail() && !self.outputs.is_empty() {
                schema::nullable(solution)
            } else {
                solution
            };
        }

        let (min, max) = self
            .predicate
            .determinism
            .map_or((0, None), |determinism| determinism.solution_bounds());
        schema::array(solution, Some(min), max)
    }

    /// Builds the Prolog goal for this operation, given its arguments as typed JSON.
    ///
    /// The goal is wrapped in `catch/3`, so an exception in the knowledge base comes back as a
    /// binding rather than escaping to the top level and poisoning the machine.
    pub fn build_goal(&self, arguments: &Map<String, Value>) -> Result<String, ArgumentError> {
        let mut terms = Vec::with_capacity(self.predicate.arguments.len());
        for argument in self.predicate.arguments.iter() {
            match argument.mode {
                Some(Mode::Output) => terms.push(marshal::variable_name(&argument.name)),
                _ => {
                    let value =
                        arguments
                            .get(&argument.name)
                            .ok_or_else(|| ArgumentError::Missing {
                                argument: argument.name.clone(),
                            })?;
                    let term = marshal::term_for(value, &argument.param_type).map_err(|cause| {
                        ArgumentError::Invalid {
                            argument: argument.name.clone(),
                            cause,
                        }
                    })?;
                    terms.push(term);
                }
            }
        }

        let goal = if terms.is_empty() {
            quote_atom(&self.predicate.name)
        } else {
            format!("{}({})", quote_atom(&self.predicate.name), terms.join(", "))
        };
        Ok(marshal::wrap_goal(&goal, &self.error_variable))
    }

    /// The names of arguments this operation does not accept, out of the ones supplied.
    ///
    /// A caller who misspells an argument otherwise gets "missing required argument `Drugs`"
    /// while staring at the `Drug` they typed.
    pub fn unexpected_arguments<'a>(
        &self,
        supplied: impl IntoIterator<Item = &'a String>,
    ) -> Vec<&'a String> {
        supplied
            .into_iter()
            .filter(|name| !self.inputs.iter().any(|param| param.name == **name))
            .collect()
    }

    /// Turns the answers to a generated goal into the response body.
    ///
    /// An answer that bound the error variable is the `catch/3` wrapper reporting an exception,
    /// not a solution — it is returned as an error even though Prolog technically succeeded.
    pub fn shape_answers(&self, answers: Vec<Answer>) -> Result<Value, PrologError> {
        let mut solutions = Vec::with_capacity(answers.len());
        for answer in answers {
            if let Some(raised) = answer.binding(&self.error_variable)
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
            // The declaration promised at most one solution and there was none. With no outputs
            // the natural report is `false`; otherwise there is no value to give back.
            match self.outputs.is_empty() {
                true => Value::Bool(false),
                false => Value::Null,
            }
        }))
    }

    fn shape_solution(&self, answer: &Answer) -> Value {
        match self.outputs.as_slice() {
            [] => Value::Bool(true),
            [only] => answer
                .binding(&only.variable)
                .cloned()
                .unwrap_or(Value::Null),
            outputs => Value::Object(
                outputs
                    .iter()
                    .map(|param| {
                        (
                            param.name.clone(),
                            answer
                                .binding(&param.variable)
                                .cloned()
                                .unwrap_or(Value::Null),
                        )
                    })
                    .collect(),
            ),
        }
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
        // A bare prefix is a name, not a prefix: `get_` would otherwise route to the empty path.
        assert_eq!(parse_route("get_"), (Verb::Get, "get_".to_string()));
        // Only the documented prefixes count; `getter` keeps its name.
        assert_eq!(parse_route("getter"), (Verb::Get, "getter".to_string()));
    }

    #[test]
    fn declared_determinism_decides_the_shape_not_the_answer_count() {
        let singular = operation("%! only(-X:int) is semidet.\nonly(1).\n");
        let plural = operation("%! many(-X:int) is nondet.\nmany(1).\n");
        let answer = vec![bindings(&[("X", json!(1))])];

        assert_eq!(singular.shape_answers(answer.clone()).unwrap(), json!(1));
        // One solution, but the declaration allows more, so it stays an array. This is the
        // ambiguity the old count-based unwrap had: a single `list` solution and several scalar
        // solutions produced exactly the same JSON.
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
    fn the_response_schema_is_exact_rather_than_a_disjunction() {
        let det = operation("%! len(+L:list, -N:int) is det.\nlen(_, 0).\n");
        assert_eq!(det.response_schema(), json!({ "type": "integer" }));

        let multi = operation("%! colour(-C:atom) is multi.\ncolour(red).\n");
        assert_eq!(
            multi.response_schema(),
            json!({ "type": "array", "items": { "type": "string" }, "minItems": 1 })
        );

        let failure = operation("%! never(-X:int) is failure.\nnever(_) :- fail.\n");
        assert_eq!(failure.response_schema()["maxItems"], json!(0));
    }

    #[test]
    fn a_semidet_predicate_documents_the_null_it_can_answer_with() {
        let semidet = operation("%! maybe(-X:int) is semidet.\nmaybe(1).\n");
        let alternatives = semidet.response_schema()["oneOf"]
            .as_array()
            .unwrap()
            .clone();
        assert!(alternatives.contains(&json!({ "type": "integer" })));
        assert!(alternatives.contains(&json!({ "type": "null" })));
    }

    #[test]
    fn an_undeclared_determinism_is_treated_as_unbounded() {
        let operation = operation("%! anything(-X:int).\nanything(1).\n");
        assert!(!operation.is_singular());
        assert_eq!(operation.response_schema()["type"], "array");
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
    fn verb_hints_follow_http_semantics() {
        assert!(Verb::Get.is_read_only() && Verb::Get.is_idempotent());
        assert!(!Verb::Post.is_idempotent());
        assert!(Verb::Put.is_idempotent() && !Verb::Put.is_destructive());
        assert!(Verb::Delete.is_destructive() && Verb::Delete.is_idempotent());
        assert!(!Verb::Get.carries_a_body() && !Verb::Delete.carries_a_body());
        assert!(Verb::Post.carries_a_body() && Verb::Patch.carries_a_body());
    }
}
