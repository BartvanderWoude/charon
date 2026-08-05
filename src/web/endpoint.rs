use json::JsonValue;
use utoipa::openapi::{
    ContentBuilder, Deprecated, HttpMethod, Object, ObjectBuilder, PathsBuilder, RefOr, Required,
    ResponseBuilder, Schema,
    path::{Parameter, ParameterBuilder, ParameterIn, PathItemBuilder},
    schema::{ArrayBuilder, OneOfBuilder, SchemaType},
};

use crate::prolog::{Determinism, Mode, Predicate, Type as PrologType};

use std::collections::HashMap;

/// Predicate-name prefixes that select an HTTP method other than the default `GET`, e.g.
/// `put_example` is exposed as `PUT /example`. The remainder after the prefix becomes the path
/// (grouping `get_example`/`put_example`/`delete_example` under the same `/example` path, each
/// with its own method); a predicate with none of these prefixes keeps the old behaviour of a
/// `GET` at its own name.
const VERB_PREFIXES: &[(&str, HttpMethod)] = &[
    ("get_", HttpMethod::Get),
    ("put_", HttpMethod::Put),
    ("post_", HttpMethod::Post),
    ("delete_", HttpMethod::Delete),
    ("patch_", HttpMethod::Patch),
];

/// A JSON Schema type plus the free-text description carried over from the argument's
/// `@arg`/`@param` PlDoc tag, if any.
struct Field {
    schema_type: SchemaType,
    description: Option<String>,
}

pub struct Endpoint {
    raw: Predicate,
    method: HttpMethod,
    path_name: String,
    inputs: HashMap<String, Field>,
    outputs: HashMap<String, Field>,
}

impl TryFrom<Predicate> for Endpoint {
    type Error = Error;

    fn try_from(raw: Predicate) -> Result<Self, Error> {
        if !raw.name.is_ascii() {
            return Err(Error::InvalidName);
        }

        let (method, path_name) = Self::parse_route(&raw.name);

        let mut inputs = HashMap::new();
        let mut outputs = HashMap::new();
        for argument in raw.arguments.iter() {
            // Derive the schema equivalent
            let schema_type = Self::infer_schema_type(&argument.param_type).ok_or_else(|| {
                Error::UnsupportedArgument {
                    argument: argument.name.to_string(),
                }
            })?;
            let field = Field {
                schema_type,
                description: argument.description.clone(),
            };

            // Split the values into inputs and outputs
            match argument.mode {
                Some(Mode::Input) => inputs.insert(argument.name.clone(), field),
                Some(Mode::Output) => outputs.insert(argument.name.clone(), field),
                mode => {
                    return Err(Error::UnsupportedMode {
                        argument: argument.name.clone(),
                        mode,
                    });
                }
            };
        }

        Ok(Endpoint {
            raw,
            method,
            path_name,
            inputs,
            outputs,
        })
    }
}

impl Endpoint {
    /// The path this endpoint is exposed under (without the leading `/`), and the HTTP method
    /// it responds to. Several predicates can share a path by differing only in their verb
    /// prefix, e.g. `get_example`/`put_example`/`delete_example` all resolve to `example`.
    pub fn path_name(&self) -> &str {
        &self.path_name
    }

    pub fn method(&self) -> &HttpMethod {
        &self.method
    }

    /// Splits a predicate name into its HTTP method and path name, based on a recognised verb
    /// prefix (`get_`/`put_`/`post_`/`delete_`/`patch_`). Predicates without one of these
    /// prefixes (or where the prefix consumes the whole name) keep the old behaviour: `GET` at
    /// the predicate's own name.
    fn parse_route(name: &str) -> (HttpMethod, String) {
        for (prefix, method) in VERB_PREFIXES {
            if let Some(rest) = name.strip_prefix(prefix)
                && !rest.is_empty()
            {
                return (method.clone(), rest.to_string());
            }
        }
        (HttpMethod::Get, name.to_string())
    }

    pub fn add(&self, builder: PathsBuilder) -> PathsBuilder {
        let path = format!("/{}", self.path_name);

        let mut operation_builder = utoipa::openapi::path::OperationBuilder::new();
        operation_builder = operation_builder
            .operation_id(Some(self.raw.name.clone()))
            .summary(Some(self.raw.doc.summary.clone().unwrap_or_else(|| {
                format!("Call the `{}` predicate.", self.raw.name)
            })))
            .description(self.build_description())
            .deprecated(
                self.raw
                    .doc
                    .deprecated
                    .is_some()
                    .then_some(Deprecated::True),
            );

        // Every input argument becomes a required query parameter, since a bound value is
        // needed to run the query.
        if !self.inputs.is_empty() {
            let parameters: Vec<Parameter> = self
                .inputs
                .iter()
                .map(|(input_name, input_field)| {
                    ParameterBuilder::new()
                        .name(input_name.clone())
                        .parameter_in(ParameterIn::Query)
                        .required(Required::True)
                        .description(input_field.description.clone())
                        .schema(Some(Self::field_schema(input_field)))
                        .build()
                })
                .collect();
            operation_builder = operation_builder.parameters(Some(parameters));
        }

        // Build the schema of a single solution. A predicate can succeed with several distinct
        // bindings, so the response is a JSON array of these, one entry per solution — *unless*
        // there is exactly one, in which case that one solution is returned directly rather
        // than wrapped in a single-element array (see `extract_outputs`/`call_predicate`).
        //
        // - No output arguments: each solution is just whether the query held, i.e. `true`.
        // - Exactly one output argument: skip the wrapper object and use its value directly, so
        //   e.g. `-Length:int` yields `3` rather than `{"Length": 3}`.
        // - Multiple output arguments: an object keyed by argument name, as before.
        let solution_schema = match self.outputs.len() {
            0 => Schema::Object(Object::with_type(utoipa::openapi::Type::Boolean)),
            1 => {
                let field = self.outputs.values().next().expect("checked len() == 1");
                Self::field_schema(field)
            }
            _ => {
                let mut output_schema_builder =
                    ObjectBuilder::new().schema_type(utoipa::openapi::Type::Object);
                for (output_name, output_field) in self.outputs.iter() {
                    output_schema_builder = output_schema_builder
                        .property(output_name, Self::field_schema(output_field))
                        .required(output_name.clone());
                }
                Schema::Object(output_schema_builder.build())
            }
        };

        let mut results_array_builder =
            ArrayBuilder::new().items(RefOr::T(solution_schema.clone()));
        if let Some(determinism) = self.raw.determinism {
            let (min, max) = determinism.solution_bounds();
            results_array_builder = results_array_builder
                .min_items(Some(min as usize))
                .max_items(max.map(|max| max as usize));
        }
        let array_schema = Schema::Array(results_array_builder.build());

        // The response's actual shape depends on how many solutions the query has at runtime,
        // so document every shape that determinism leaves possible.
        let response_schema = match self.raw.determinism {
            // Always exactly one solution: always unwrapped, never an array.
            Some(Determinism::Det) => solution_schema,
            // Always zero solutions: always an (empty) array.
            Some(Determinism::Failure) => array_schema,
            // Zero, one, or many: could be either shape.
            _ => Schema::OneOf(
                OneOfBuilder::new()
                    .item(solution_schema)
                    .item(array_schema)
                    .build(),
            ),
        };

        operation_builder = operation_builder.response(
            "200",
            ResponseBuilder::new()
                .description(
                    "The query's solutions: the bare value/object if there is exactly one \
                     solution, otherwise a JSON array with one element per solution (empty if \
                     the query failed).",
                )
                .content(
                    "application/json",
                    ContentBuilder::new().schema(Some(response_schema)).build(),
                ),
        );
        operation_builder = operation_builder.response("400", Self::error_response());

        let path_builder = PathItemBuilder::new().operation(self.method.clone(), operation_builder);

        builder.path(path, path_builder.build())
    }

    /// Composes the operation description from the predicate's free-text doc and any PlDoc tags
    /// that don't have a more specific home in the OpenAPI operation (deprecation still also
    /// sets the dedicated `deprecated` flag; this just makes its message visible).
    fn build_description(&self) -> Option<String> {
        let doc = &self.raw.doc;
        let mut sections = Vec::new();

        if let Some(description) = &doc.description {
            sections.push(description.clone());
        }

        let mut meta = Vec::new();
        if let Some(determinism) = self.raw.determinism {
            meta.push(format!("**Determinism:** {determinism}"));
        }
        meta.extend(doc.tag_lines());

        if !meta.is_empty() {
            sections.push(meta.join("  \n"));
        }

        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }

    fn field_schema(field: &Field) -> Schema {
        Schema::Object(
            ObjectBuilder::new()
                .schema_type(field.schema_type.clone())
                .description(field.description.clone())
                .build(),
        )
    }

    /// The 400 response every endpoint can produce: a missing/invalid input argument, or a
    /// Prolog exception raised while running the query.
    fn error_response() -> ResponseBuilder {
        let schema = Schema::Object(
            ObjectBuilder::new()
                .schema_type(utoipa::openapi::Type::Object)
                .property(
                    "error",
                    Schema::Object(Object::with_type(utoipa::openapi::Type::String)),
                )
                .required("error")
                .build(),
        );

        ResponseBuilder::new()
            .description("Missing/invalid input argument, or the query raised a Prolog exception.")
            .content(
                "application/json",
                ContentBuilder::new().schema(Some(schema)).build(),
            )
    }

    /// Builds the Prolog query text for this predicate given the raw (string-valued) query
    /// parameters of the request, e.g. `my_length([1,2,3], Length).`.
    ///
    /// Each input's declared type controls how its query-string value is read: `int`/`float`
    /// parse as numbers, `atom` is taken verbatim as a quoted Prolog atom (so it unifies with
    /// plain atom facts, e.g. `example(a)`, rather than a double-quoted string), and `list`/`any`
    /// are parsed as JSON first (so `List=[1,2,3]` works) and fall back to a plain atom otherwise.
    pub fn build_query(&self, params: &HashMap<String, String>) -> Result<String, QueryError> {
        let mut terms = Vec::with_capacity(self.raw.arguments.len());
        for argument in self.raw.arguments.iter() {
            match argument.mode {
                Some(Mode::Input) => {
                    let raw = params
                        .get(argument.name.as_str())
                        .ok_or_else(|| QueryError::MissingArgument(argument.name.clone()))?;
                    let term = if argument.param_type == PrologType::Atom {
                        Self::atom_term(raw)
                    } else {
                        let value = Self::parse_query_value(&argument.param_type, raw)
                            .ok_or_else(|| QueryError::UnsupportedValue(argument.name.clone()))?;
                        Self::json_to_term(&value)
                            .ok_or_else(|| QueryError::UnsupportedValue(argument.name.clone()))?
                    };
                    terms.push(term);
                }
                Some(Mode::Output) => terms.push(Self::variable_name(&argument.name)),
                _ => unreachable!("Endpoint construction only allows Input/Output arguments"),
            }
        }

        Ok(if terms.is_empty() {
            format!("{}.", self.raw.name)
        } else {
            format!("{}({}).", self.raw.name, terms.join(", "))
        })
    }

    /// Interprets a raw query-string value according to the argument's declared PlDoc type.
    fn parse_query_value(param_type: &PrologType, raw: &str) -> Option<JsonValue> {
        Some(match param_type {
            PrologType::Integer => JsonValue::from(raw.trim().parse::<i64>().ok()?),
            PrologType::Float => JsonValue::from(raw.trim().parse::<f64>().ok()?),
            PrologType::Atom => JsonValue::String(raw.to_string()),
            PrologType::List | PrologType::Any => {
                json::parse(raw).unwrap_or_else(|_| JsonValue::String(raw.to_string()))
            }
            _ => return None,
        })
    }

    /// Picks the output bindings back out of a query answer, keyed by the original
    /// (undocumented-case) argument names rather than the generated Prolog variables.
    ///
    /// If there is exactly one output argument, the wrapper object is skipped and its value is
    /// returned directly (mirroring the single-value response schema built by `add`).
    pub fn extract_outputs(&self, answer: JsonValue) -> JsonValue {
        if self.outputs.is_empty() {
            return answer;
        }

        let JsonValue::Object(bindings) = answer else {
            return answer;
        };

        if self.outputs.len() == 1 {
            let name = self.outputs.keys().next().expect("checked len() == 1");
            return bindings
                .get(&Self::variable_name(name))
                .cloned()
                .unwrap_or(JsonValue::Null);
        }

        let mut result = json::object::Object::new();
        for name in self.outputs.keys() {
            let value = bindings
                .get(&Self::variable_name(name))
                .cloned()
                .unwrap_or(JsonValue::Null);
            result.insert(name, value);
        }
        JsonValue::Object(result)
    }

    /// Assembles the final response body from a query's (already `extract_outputs`-shaped)
    /// solutions: if there is exactly one, it's returned bare; otherwise every solution is
    /// returned as a JSON array. This mirrors the `response_schema` documented by `add`.
    pub fn finalize_response(mut solutions: Vec<JsonValue>) -> JsonValue {
        if solutions.len() == 1 {
            solutions.pop().expect("checked len() == 1")
        } else {
            JsonValue::Array(solutions)
        }
    }

    /// Prolog variables must start with an uppercase letter or underscore; PlDoc argument
    /// names don't have to, so this derives a valid, argument-specific variable name.
    fn variable_name(argument_name: &str) -> String {
        let mut chars = argument_name.chars();
        match chars.next() {
            Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            None => String::new(),
        }
    }

    /// Encodes a raw string as a quoted Prolog atom literal, e.g. `foo` becomes `'foo'`. Always
    /// quoting (rather than only when needed) sidesteps having to replicate Prolog's unquoted-atom
    /// grammar here.
    fn atom_term(raw: &str) -> String {
        format!("'{}'", raw.replace('\\', "\\\\").replace('\'', "\\'"))
    }

    fn json_to_term(value: &JsonValue) -> Option<String> {
        Some(match value {
            JsonValue::String(_) | JsonValue::Short(_) => {
                format!(
                    "\"{}\"",
                    value.as_str()?.replace('\\', "\\\\").replace('"', "\\\"")
                )
            }
            JsonValue::Number(_) => value.to_string(),
            JsonValue::Boolean(boolean) => boolean.to_string(),
            JsonValue::Array(items) => {
                let items: Option<Vec<String>> = items.iter().map(Self::json_to_term).collect();
                format!("[{}]", items?.join(", "))
            }
            JsonValue::Object(_) | JsonValue::Null => return None,
        })
    }

    fn infer_schema_type(prolog_type: &PrologType) -> Option<SchemaType> {
        Some(match *prolog_type {
            PrologType::Any => SchemaType::AnyValue,
            PrologType::Atom => SchemaType::Type(utoipa::openapi::Type::String),
            PrologType::Float => SchemaType::Type(utoipa::openapi::Type::Number),
            PrologType::Integer => SchemaType::Type(utoipa::openapi::Type::Integer),
            PrologType::List => SchemaType::AnyValue,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Error {
    InvalidName,
    UnsupportedMode {
        argument: String,
        mode: Option<Mode>,
    },
    UnsupportedArgument {
        argument: String,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryError {
    MissingArgument(String),
    UnsupportedValue(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::MissingArgument(argument) => {
                write!(f, "missing required argument: {}", argument)
            }
            QueryError::UnsupportedValue(argument) => {
                write!(f, "unsupported value for argument: {}", argument)
            }
        }
    }
}
