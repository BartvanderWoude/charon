//! Reading a query-string value as the typed JSON the service expects.

use serde_json::Value;

use crate::prolog::Type;

/// Reads `raw` as a value of the declared type. `Err` carries a message naming what was wanted.
pub fn from_query(declared: &Type, raw: &str) -> Result<Value, String> {
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
        // Structured values arrive as JSON text, e.g. `?Drugs=["diazepam"]`. A value that is not
        // JSON falls back to being the text itself, so `?Xs=hello` is still a usable mistake to
        // make: the type check downstream names the argument.
        Type::List(_) | Type::Any => {
            serde_json::from_str(raw).unwrap_or_else(|_| Value::String(raw.to_string()))
        }
        other => return Err(format!("cannot read a `{other}` from a query string")),
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
        assert_eq!(from_query(&Type::Atom, "null").unwrap(), json!("null"));
        assert_eq!(from_query(&Type::Atom, "1").unwrap(), json!("1"));
        assert_eq!(from_query(&Type::Text, "true").unwrap(), json!("true"));
        assert_eq!(from_query(&Type::Atom, "").unwrap(), json!(""));
    }

    #[test]
    fn numbers_are_read_as_numbers() {
        assert_eq!(from_query(&Type::Integer, " 82 ").unwrap(), json!(82));
        assert_eq!(from_query(&Type::Integer, "-3").unwrap(), json!(-3));
        assert_eq!(from_query(&Type::Float, "2.5").unwrap(), json!(2.5));
        assert!(from_query(&Type::Integer, "2.5").is_err());
        assert!(from_query(&Type::Integer, "eighty-two").is_err());
        // JSON cannot carry these, and neither can Prolog arithmetic usefully.
        assert!(from_query(&Type::Float, "inf").is_err());
        assert!(from_query(&Type::Float, "NaN").is_err());
    }

    #[test]
    fn booleans_accept_the_spellings_a_query_string_invites() {
        assert_eq!(from_query(&Type::Boolean, "true").unwrap(), json!(true));
        assert_eq!(from_query(&Type::Boolean, "1").unwrap(), json!(true));
        assert_eq!(from_query(&Type::Boolean, "no").unwrap(), json!(false));
        assert!(from_query(&Type::Boolean, "maybe").is_err());
    }

    #[test]
    fn structured_values_arrive_as_json_text() {
        assert_eq!(
            from_query(&list_of(Type::Atom), r#"["diazepam"]"#).unwrap(),
            json!(["diazepam"])
        );
        assert_eq!(
            from_query(&list_of(Type::Integer), "[1,2]").unwrap(),
            json!([1, 2])
        );
        // Not JSON: kept as text so the type check downstream can name the argument.
        assert_eq!(
            from_query(&list_of(Type::Atom), "diazepam").unwrap(),
            json!("diazepam")
        );
    }

    #[test]
    fn types_that_cannot_come_from_a_query_string_say_so() {
        assert!(from_query(&Type::Stream, "x").is_err());
        assert!(from_query(&Type::Compound, "x").is_err());
    }
}
