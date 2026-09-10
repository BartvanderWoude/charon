//! JSON Schema generation, for a declared type and for a whole operation.
//!
//! Both interfaces Charon speaks want JSON Schema: OpenAPI 3.1 *is* JSON Schema, and MCP tool
//! descriptors embed it directly. Schemas are built once here as plain `serde_json::Value` and
//! handed to whichever transport asked, so the two documents cannot describe the same predicate
//! differently.

use serde_json::{Map, Value, json};

use super::operation::Operation;
use crate::prolog::Type;

/// The JSON Schema of an operation's arguments: an object with every input required.
pub fn inputs_of(operation: &Operation) -> Value {
    object(
        operation
            .inputs()
            .map(|param| (param.name.clone(), param.schema.clone())),
    )
}

/// The JSON Schema of a single solution.
///
/// Zero outputs is just whether the query held; exactly one output is that value directly,
/// unwrapped; two or more is an object keyed by argument name.
pub fn solution_of(operation: &Operation) -> Value {
    let outputs: Vec<_> = operation.outputs().collect();
    match outputs.as_slice() {
        [] => json!({ "type": "boolean" }),
        [only] => only.schema.clone(),
        outputs => object(
            outputs
                .iter()
                .map(|param| (param.name.clone(), param.schema.clone())),
        ),
    }
}

/// The JSON Schema of a whole response.
///
/// Exact rather than a disjunction of every shape the runtime might reach:
/// `Operation::is_singular` settles the array-or-scalar question before a single answer exists.
pub fn response_of(operation: &Operation) -> Value {
    let solution = solution_of(operation);
    if operation.is_singular() {
        // A `semidet` predicate that fails answers `null`, or `false` when it has no outputs —
        // in which case the boolean schema already covers it.
        return match operation.may_fail() && operation.outputs().next().is_some() {
            true => nullable(solution),
            false => solution,
        };
    }

    let (min, max) = operation
        .determinism()
        .map_or((0, None), |determinism| determinism.solution_bounds());
    array(solution, Some(min), max)
}

/// The JSON Schema for a value of a declared PlDoc type, with its `@arg` description attached.
///
/// `None` for types that have no JSON equivalent — a caller should reject the predicate rather
/// than expose an argument it cannot marshal.
pub fn for_type(declared: &Type, description: Option<&str>) -> Option<Value> {
    let mut schema = match declared {
        Type::Any => Map::new(),
        Type::Atom | Type::Text => single("type", json!("string")),
        Type::Integer => single("type", json!("integer")),
        Type::Float => single("type", json!("number")),
        Type::Boolean => single("type", json!("boolean")),
        Type::List(element) => {
            let mut map = single("type", json!("array"));
            map.insert("items".to_string(), for_type(element, None)?);
            map
        }
        // A compound term, in the same `{functor, args}` encoding a term comes back as. This is
        // how a rule says something structured — `dose(500, mg)` — without flattening it into
        // parallel arguments.
        Type::Compound => {
            let mut map = single("type", json!("object"));
            map.insert(
                "properties".to_string(),
                json!({
                    "functor": {
                        "type": "string",
                        "description": "The term's functor.",
                    },
                    "args": {
                        "type": "array",
                        "description": "Its arguments, which may themselves be compound.",
                        "items": {},
                    },
                }),
            );
            map.insert("required".to_string(), json!(["functor", "args"]));
            map
        }
        Type::Stream | Type::Custom(_) => return None,
    };

    if let Some(description) = description.filter(|text| !text.is_empty()) {
        schema.insert("description".to_string(), json!(description));
    }
    Some(Value::Object(schema))
}

fn single(key: &str, value: Value) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert(key.to_string(), value);
    map
}

/// A JSON Schema `object` with the given properties, all of them required.
pub fn object(properties: impl IntoIterator<Item = (String, Value)>) -> Value {
    let properties: Map<String, Value> = properties.into_iter().collect();
    let required: Vec<Value> = properties.keys().map(|name| json!(name)).collect();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// A JSON Schema `array` of `items`, optionally bounded.
pub fn array(items: Value, min: Option<u64>, max: Option<u64>) -> Value {
    let mut schema = json!({ "type": "array", "items": items });
    if let Some(min) = min {
        schema["minItems"] = json!(min);
    }
    if let Some(max) = max {
        schema["maxItems"] = json!(max);
    }
    schema
}

/// `schema`, widened to admit `null` — how a predicate that is allowed to fail reports that it
/// did.
pub fn nullable(schema: Value) -> Value {
    // A bare `{}` (the schema for `any`) already admits null; widening it would only add noise.
    if schema.as_object().is_some_and(Map::is_empty) {
        return schema;
    }
    json!({ "oneOf": [schema, { "type": "null" }] })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prolog::Predicate;

    /// The one operation documented by `source`.
    fn operation(source: &str) -> Operation {
        let predicate = Predicate::parse(source)
            .into_iter()
            .next()
            .expect("the fixture documents a predicate");
        Operation::try_from(predicate).expect("the fixture is exposable")
    }

    #[test]
    fn the_response_schema_is_exact_rather_than_a_disjunction() {
        let det = operation("%! len(+L:list, -N:int) is det.\nlen(_, 0).\n");
        assert_eq!(response_of(&det), json!({ "type": "integer" }));

        let multi = operation("%! colour(-C:atom) is multi.\ncolour(red).\n");
        assert_eq!(
            response_of(&multi),
            json!({ "type": "array", "items": { "type": "string" }, "minItems": 1 })
        );

        let failure = operation("%! never(-X:int) is failure.\nnever(_) :- fail.\n");
        assert_eq!(response_of(&failure)["maxItems"], json!(0));
    }

    #[test]
    fn a_semidet_predicate_documents_the_null_it_can_answer_with() {
        let semidet = operation("%! maybe(-X:int) is semidet.\nmaybe(1).\n");
        let alternatives = response_of(&semidet)["oneOf"].as_array().unwrap().clone();
        assert!(alternatives.contains(&json!({ "type": "integer" })));
        assert!(alternatives.contains(&json!({ "type": "null" })));
    }

    #[test]
    fn an_undeclared_determinism_is_treated_as_unbounded() {
        let operation = operation("%! anything(-X:int).\nanything(1).\n");
        assert!(!operation.is_singular());
        assert_eq!(response_of(&operation)["type"], "array");
    }

    #[test]
    fn a_solution_is_unwrapped_only_when_there_is_one_output() {
        let none = operation("%! ready is semidet.\nready.\n");
        assert_eq!(solution_of(&none), json!({ "type": "boolean" }));

        let one = operation("%! only(-X:int) is det.\nonly(1).\n");
        assert_eq!(solution_of(&one), json!({ "type": "integer" }));

        let two = operation("%! pair(-Zeta:int, -Alpha:atom) is det.\npair(1, a).\n");
        assert_eq!(solution_of(&two)["required"], json!(["Zeta", "Alpha"]));
    }

    #[test]
    fn element_types_are_carried_into_arrays() {
        let schema = for_type(&Type::List(Box::new(Type::Integer)), None).unwrap();
        assert_eq!(schema["type"], "array");
        assert_eq!(schema["items"]["type"], "integer");
    }

    #[test]
    fn nested_arrays_keep_nesting() {
        let nested = Type::List(Box::new(Type::List(Box::new(Type::Atom))));
        let schema = for_type(&nested, None).unwrap();
        assert_eq!(schema["items"]["items"]["type"], "string");
    }

    #[test]
    fn any_is_the_empty_schema_rather_than_a_type() {
        // `{"type": "any"}` is not a thing in JSON Schema; the empty schema is what "anything"
        // is actually spelled.
        assert_eq!(for_type(&Type::Any, None).unwrap(), json!({}));
        assert_eq!(nullable(for_type(&Type::Any, None).unwrap()), json!({}));
    }

    #[test]
    fn types_without_a_json_equivalent_have_no_schema() {
        assert!(for_type(&Type::Stream, None).is_none());
        assert!(for_type(&Type::Custom("widget".into()), None).is_none());
        // …including when they are only reachable as an element type.
        assert!(for_type(&Type::List(Box::new(Type::Stream)), None).is_none());
    }

    #[test]
    fn a_compound_term_is_an_object_of_functor_and_arguments() {
        // The same encoding a term comes back in, so a structured answer can be handed straight
        // back as a structured argument.
        let schema = for_type(&Type::Compound, None).unwrap();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["functor"]["type"], "string");
        assert_eq!(schema["properties"]["args"]["type"], "array");
        assert_eq!(schema["required"], json!(["functor", "args"]));

        // And it nests, which is the point.
        let nested = for_type(&Type::List(Box::new(Type::Compound)), None).unwrap();
        assert_eq!(nested["items"]["properties"]["functor"]["type"], "string");
    }

    #[test]
    fn descriptions_are_attached_but_empty_ones_are_not() {
        let described = for_type(&Type::Atom, Some("The drug.")).unwrap();
        assert_eq!(described["description"], "The drug.");
        assert!(
            for_type(&Type::Atom, Some(""))
                .unwrap()
                .get("description")
                .is_none()
        );
    }

    #[test]
    fn objects_require_every_property_in_declaration_order() {
        let schema = object([
            ("Zeta".to_string(), json!({"type": "integer"})),
            ("Alpha".to_string(), json!({"type": "integer"})),
        ]);
        assert_eq!(schema["required"], json!(["Zeta", "Alpha"]));
        assert_eq!(schema["additionalProperties"], json!(false));
    }
}
