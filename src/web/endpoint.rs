use json::JsonValue;
use utoipa::openapi::{
    ContentBuilder, Object, ObjectBuilder, PathsBuilder, Required, ResponseBuilder, Schema,
    path::PathItemBuilder,
    request_body::RequestBodyBuilder,
    schema::{Array, SchemaType},
};

use crate::prolog::{Mode, Predicate, Type as PrologType};

use std::collections::HashMap;

pub struct Endpoint {
    raw: Predicate,
    inputs: HashMap<String, SchemaType>,
    outputs: HashMap<String, SchemaType>,
}

impl TryFrom<Predicate> for Endpoint {
    type Error = Error;

    fn try_from(raw: Predicate) -> Result<Self, Error> {
        if !raw.name.is_ascii() {
            return Err(Error::InvalidName);
        }

        let mut inputs = HashMap::new();
        let mut outputs = HashMap::new();
        for argument in raw.arguments.iter() {
            // Derive the schema equivalent
            let schema_type = Self::infer_schema_type(&argument.param_type).ok_or_else(|| {
                Error::UnsupportedArgument {
                    argument: argument.name.to_string(),
                }
            })?;

            // Split the values into inputs and outputs
            match argument.mode {
                Some(Mode::Input) => inputs.insert(argument.name.clone(), schema_type),
                Some(Mode::Output) => outputs.insert(argument.name.clone(), schema_type),
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
            inputs,
            outputs,
        })
    }
}

impl Endpoint {
    pub fn name(&self) -> &str {
        &self.raw.name
    }

    pub fn add(&self, builder: PathsBuilder) -> PathsBuilder {
        let path = format!("/{}", self.raw.name);

        let mut operation_builder = utoipa::openapi::path::OperationBuilder::new();

        // Build the input schema and, if there are any inputs, require them as a JSON body.
        if !self.inputs.is_empty() {
            let mut input_schema_builder =
                ObjectBuilder::new().schema_type(utoipa::openapi::Type::Object);
            for (input_name, input_type) in self.inputs.iter() {
                input_schema_builder = input_schema_builder.property(
                    input_name,
                    Schema::Object(Object::with_type(input_type.clone())),
                );
            }

            operation_builder = operation_builder.request_body(Some(
                RequestBodyBuilder::new()
                    .required(Some(Required::True))
                    .content(
                        "application/json",
                        ContentBuilder::new()
                            .schema(Some(Schema::Object(input_schema_builder.build())))
                            .build(),
                    )
                    .build(),
            ));
        }

        // Build the output schema: one object per solution the query produces.
        let mut output_schema_builder =
            ObjectBuilder::new().schema_type(utoipa::openapi::Type::Object);
        for (output_name, output) in self.outputs.iter() {
            output_schema_builder = output_schema_builder.property(
                output_name,
                Schema::Object(Object::with_type(output.clone())),
            )
        }

        // A predicate can succeed with several distinct bindings, so the response is a
        // list of all of them rather than just the first one.
        let results_schema = Schema::Array(Array::new(Schema::Object(
            output_schema_builder.build(),
        )));

        operation_builder = operation_builder.response(
            "200",
            ResponseBuilder::new().content(
                "application/json",
                ContentBuilder::new().schema(Some(results_schema)).build(),
            ),
        );

        let path_builder =
            PathItemBuilder::new().operation(utoipa::openapi::HttpMethod::Post, operation_builder);

        builder.path(path, path_builder.build())
    }

    /// Builds the Prolog query text for this predicate given the JSON object of input values,
    /// e.g. `my_length([1,2,3], Length).`.
    pub fn build_query(&self, input: &JsonValue) -> Result<String, QueryError> {
        let mut terms = Vec::with_capacity(self.raw.arguments.len());
        for argument in self.raw.arguments.iter() {
            match argument.mode {
                Some(Mode::Input) => {
                    let value = &input[argument.name.as_str()];
                    if value.is_null() {
                        return Err(QueryError::MissingArgument(argument.name.clone()));
                    }
                    terms.push(Self::json_to_term(value).ok_or_else(|| {
                        QueryError::UnsupportedValue(argument.name.clone())
                    })?);
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

    /// Picks the output bindings back out of a query answer, keyed by the original
    /// (undocumented-case) argument names rather than the generated Prolog variables.
    pub fn extract_outputs(&self, answer: JsonValue) -> JsonValue {
        if self.outputs.is_empty() {
            return answer;
        }

        match answer {
            JsonValue::Object(bindings) => {
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
            other => other,
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

    fn json_to_term(value: &JsonValue) -> Option<String> {
        Some(match value {
            JsonValue::String(_) | JsonValue::Short(_) => {
                format!("\"{}\"", value.as_str()?.replace('\\', "\\\\").replace('"', "\\\""))
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
        Some(match prolog_type {
            &PrologType::Any => SchemaType::AnyValue,
            &PrologType::Atom => SchemaType::Type(utoipa::openapi::Type::String),
            &PrologType::Float => SchemaType::Type(utoipa::openapi::Type::Number),
            &PrologType::Integer => SchemaType::Type(utoipa::openapi::Type::Integer),
            &PrologType::List => SchemaType::AnyValue,
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
