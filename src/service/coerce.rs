//! Reading text as the typed JSON an operation expects.
//!
//! This lives in the service layer rather than in the HTTP adapter because two transports need
//! it: a query-string value and a URL path segment are text, and so is the key inside an MCP
//! collection resource URI (`charon://collection/books/dune`). One copy means `?Title=null` and
//! `charon://collection/books/null` cannot disagree about whether that is an atom or a JSON null.

use serde_json::Value;

use crate::prolog::Type;

/// Reads `raw` as a value of the declared type. `Err` carries a message naming what was wanted.
pub fn value_from_text(declared: &Type, raw: &str) -> Result<Value, String> {
    Ok(match declared {
        // Taken verbatim: a name is not JSON, and parsing it as JSON would turn the perfectly
        // reasonable atom `null` into a null, and the atom `1` into a number.
        Type::Atom | Type::Text => Value::String(raw.to_string()),
        Type::Integer => raw
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| format!("expected an integer, got `{raw}`"))?,
        Type::Float => raw
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|number| number.is_finite())
            .map(Value::from)
            .ok_or_else(|| format!("expected a finite number, got `{raw}`"))?,
        Type::Boolean => match raw.trim() {
            "true" | "1" | "yes" => Value::Bool(true),
            "false" | "0" | "no" => Value::Bool(false),
            other => return Err(format!("expected true or false, got `{other}`")),
        },
        // Structured values arrive as JSON text, e.g. `?Tags=["fiction"]` or
        // `?Term={"functor":"dose","args":[100,"mg"]}`. A value that is not JSON falls back to
        // being the text itself, so `?Xs=hello` is still a usable mistake to make: the type
        // check downstream names the argument.
        Type::List(_) | Type::Any | Type::Compound => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
        }
        other => return Err(format!("cannot read a `{other}` from text")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list_of(element: Type) -> Type {
        Type::List(Box::new(element))
    }

    #[test]
    fn atoms_and_text_are_taken_verbatim() {
        // Not parsed as JSON: `null`, `true` and `1` are perfectly ordinary atoms, and a
        // knowledge base asking for an atom means the text it was sent.
        assert_eq!(value_from_text(&Type::Atom, "null").unwrap(), json!("null"));
        assert_eq!(value_from_text(&Type::Atom, "1").unwrap(), json!("1"));
        assert_eq!(value_from_text(&Type::Text, "true").unwrap(), json!("true"));
        assert_eq!(value_from_text(&Type::Atom, "").unwrap(), json!(""));
    }

    #[test]
    fn numbers_are_read_as_numbers() {
        assert_eq!(value_from_text(&Type::Integer, " 82 ").unwrap(), json!(82));
        assert_eq!(value_from_text(&Type::Integer, "-3").unwrap(), json!(-3));
        assert_eq!(value_from_text(&Type::Float, "2.5").unwrap(), json!(2.5));
        assert!(value_from_text(&Type::Integer, "2.5").is_err());
        assert!(value_from_text(&Type::Integer, "eighty-two").is_err());
        // JSON cannot carry these, and neither can Prolog arithmetic usefully.
        assert!(value_from_text(&Type::Float, "inf").is_err());
        assert!(value_from_text(&Type::Float, "NaN").is_err());
    }

    #[test]
    fn booleans_accept_the_spellings_a_query_string_invites() {
        assert_eq!(
            value_from_text(&Type::Boolean, "true").unwrap(),
            json!(true)
        );
        assert_eq!(value_from_text(&Type::Boolean, "1").unwrap(), json!(true));
        assert_eq!(value_from_text(&Type::Boolean, "no").unwrap(), json!(false));
        assert!(value_from_text(&Type::Boolean, "maybe").is_err());
    }

    #[test]
    fn structured_values_arrive_as_json_text() {
        assert_eq!(
            value_from_text(&list_of(Type::Atom), r#"["diazepam"]"#).unwrap(),
            json!(["diazepam"])
        );
        assert_eq!(
            value_from_text(&list_of(Type::Integer), "[1,2]").unwrap(),
            json!([1, 2])
        );
        // Not JSON: kept as text so the type check downstream can name the argument.
        assert_eq!(
            value_from_text(&list_of(Type::Atom), "diazepam").unwrap(),
            json!("diazepam")
        );
    }

    #[test]
    fn types_that_cannot_come_from_text_say_so() {
        assert!(value_from_text(&Type::Stream, "x").is_err());
        assert!(value_from_text(&Type::Custom("widget".into()), "x").is_err());
    }

    #[test]
    fn a_compound_term_arrives_as_json_text() {
        // Otherwise a `compound` argument would be reachable from a JSON body and from nowhere
        // else, which would make `curl -G` and `curl --json` disagree about what is callable.
        assert_eq!(
            value_from_text(&Type::Compound, r#"{"functor":"dose","args":[100,"mg"]}"#).unwrap(),
            json!({ "functor": "dose", "args": [100, "mg"] })
        );
    }
}
