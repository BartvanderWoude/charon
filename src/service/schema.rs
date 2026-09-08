//! JSON Schema generation.
//!
//! Both interfaces Charon speaks want JSON Schema: OpenAPI 3.1 *is* JSON Schema, and MCP tool
//! descriptors embed it directly. So schemas are built once here as plain `serde_json::Value`
//! and handed to whichever transport asked, rather than being built twice against two different
//! typed builder APIs.

use serde_json::{Map, Value, json};

use crate::prolog::Type;

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
        Type::Compound | Type::Stream | Type::Custom(_) => return None,
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
        assert!(for_type(&Type::Compound, None).is_none());
        assert!(for_type(&Type::Stream, None).is_none());
        assert!(for_type(&Type::Custom("widget".into()), None).is_none());
        // …including when they are only reachable as an element type.
        assert!(for_type(&Type::List(Box::new(Type::Stream)), None).is_none());
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
