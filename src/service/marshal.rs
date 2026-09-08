//! Building a Prolog goal out of JSON arguments.

use std::fmt;

use serde_json::Value;

use crate::prolog::{Type, quote_atom, quote_string};

/// The name Charon prefers for the variable a generated goal binds a caught exception to.
const ERROR_VARIABLE: &str = "CharonError__";

/// A variable name for the caught exception that none of `argument_variables` already uses.
///
/// An argument literally named `charonError__` would otherwise collide, since [`variable_name`]
/// upper-cases the first character — so rather than assume the base name is unique, take it and
/// widen it until it is.
pub fn error_variable(argument_variables: &[String]) -> String {
    let mut name = ERROR_VARIABLE.to_string();
    while argument_variables.contains(&name) {
        name.push('_');
    }
    name
}

/// Wraps a goal so that an exception is reported as a binding instead of propagating.
///
/// An exception that reaches scryer's top level leaves the machine unusable: every later query,
/// including ones against unrelated predicates, keeps returning that same stale error. Catching
/// it inside the query is what keeps a single bad request from poisoning the process — and it
/// means knowledge bases don't need defensive `catch/3` wrappers of their own.
pub fn wrap_goal(goal: &str, error_variable: &str) -> String {
    format!("catch({goal}, {error_variable}, true).")
}

/// Prolog variables must start with an upper-case letter or underscore; PlDoc argument names
/// don't have to, so this derives a valid, argument-specific variable name (`len` → `Len`).
pub fn variable_name(argument_name: &str) -> String {
    let mut characters = argument_name.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

/// A supplied argument that doesn't fit its declared type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeMismatch {
    pub expected: String,
    pub found: String,
}

impl fmt::Display for TypeMismatch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expected {}, found {}", self.expected, self.found)
    }
}

/// Renders a JSON value as Prolog source text for an argument of the given declared type.
pub fn term_for(value: &Value, declared: &Type) -> Result<String, TypeMismatch> {
    match declared {
        // An atom, so it unifies with ordinary facts like `drug(aspirin)`. Numbers and booleans
        // are accepted and rendered as their text: a caller that sends `1` where an atom is
        // wanted almost certainly means the atom `'1'`, and refusing it helps nobody.
        Type::Atom => match value {
            Value::String(text) => Ok(quote_atom(text)),
            Value::Number(number) => Ok(quote_atom(&number.to_string())),
            Value::Bool(boolean) => Ok(quote_atom(&boolean.to_string())),
            other => Err(mismatch("an atom", other)),
        },
        // A Prolog string, i.e. a character list under scryer's default `double_quotes` flag.
        Type::Text => match value {
            Value::String(text) => Ok(quote_string(text)),
            other => Err(mismatch("a string", other)),
        },
        Type::Integer => match value {
            // `as_i64` alone rejects integers above `i64::MAX`, which serde_json holds happily
            // as a `u64` and Prolog handles at arbitrary precision.
            Value::Number(number) if number.is_i64() || number.is_u64() => Ok(number.to_string()),
            other => Err(mismatch("an integer", other)),
        },
        Type::Float => match value.as_f64() {
            // Rendered with an explicit fractional part so Prolog reads it as a float rather
            // than an integer, which matters for `is/2` and for arithmetic comparison.
            Some(float) if float.is_finite() && float.fract() == 0.0 => Ok(format!("{float:.1}")),
            Some(float) if float.is_finite() => Ok(float.to_string()),
            _ => Err(mismatch("a finite number", value)),
        },
        Type::Boolean => match value {
            Value::Bool(boolean) => Ok(boolean.to_string()),
            other => Err(mismatch("a boolean", other)),
        },
        Type::List(element) => match value {
            Value::Array(items) => {
                let terms = items
                    .iter()
                    .map(|item| term_for(item, element))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("[{}]", terms.join(",")))
            }
            other => Err(mismatch("an array", other)),
        },
        // Undeclared: map each JSON type to its most idiomatic Prolog counterpart. Strings
        // become atoms, matching the scalar `atom` case, so an author who declares nothing gets
        // the representation they would have reached for anyway.
        Type::Any => match value {
            Value::String(text) => Ok(quote_atom(text)),
            Value::Number(number) => Ok(number.to_string()),
            Value::Bool(boolean) => Ok(boolean.to_string()),
            Value::Array(items) => {
                let terms = items
                    .iter()
                    .map(|item| term_for(item, &Type::Any))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(format!("[{}]", terms.join(",")))
            }
            other => Err(mismatch("a scalar or array", other)),
        },
        Type::Compound | Type::Stream | Type::Custom(_) => Err(TypeMismatch {
            expected: format!("a type Charon can marshal (got `{declared}`)"),
            found: describe(value),
        }),
    }
}

fn mismatch(expected: &str, value: &Value) -> TypeMismatch {
    TypeMismatch {
        expected: expected.to_string(),
        found: describe(value),
    }
}

fn describe(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(_) => "a boolean".to_string(),
        Value::Number(_) => "a number".to_string(),
        Value::String(_) => "a string".to_string(),
        Value::Array(_) => "an array".to_string(),
        Value::Object(_) => "an object".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list_of(element: Type) -> Type {
        Type::List(Box::new(element))
    }

    #[test]
    fn scalar_atoms_and_list_atoms_agree() {
        // The whole point of the type change: one JSON string yields the same Prolog term
        // whether it arrives on its own or inside a list.
        assert_eq!(
            term_for(&json!("diazepam"), &Type::Atom).unwrap(),
            "diazepam"
        );
        assert_eq!(
            term_for(&json!(["diazepam", "digoxin"]), &list_of(Type::Atom)).unwrap(),
            "[diazepam,digoxin]"
        );
    }

    #[test]
    fn strings_are_distinct_from_atoms() {
        assert_eq!(
            term_for(&json!("hello world"), &Type::Text).unwrap(),
            "\"hello world\""
        );
        assert_eq!(
            term_for(&json!("hello world"), &Type::Atom).unwrap(),
            "'hello world'"
        );
    }

    #[test]
    fn lists_nest() {
        assert_eq!(
            term_for(&json!([[1, 2], [3]]), &list_of(list_of(Type::Integer))).unwrap(),
            "[[1,2],[3]]"
        );
        assert_eq!(term_for(&json!([]), &list_of(Type::Integer)).unwrap(), "[]");
    }

    #[test]
    fn floats_keep_a_fractional_part() {
        assert_eq!(term_for(&json!(3), &Type::Float).unwrap(), "3.0");
        assert_eq!(term_for(&json!(2.5), &Type::Float).unwrap(), "2.5");
        assert_eq!(term_for(&json!(-0.5), &Type::Float).unwrap(), "-0.5");
    }

    #[test]
    fn integers_reject_fractions_but_keep_their_full_range() {
        assert!(term_for(&json!(2.5), &Type::Integer).is_err());
        assert_eq!(
            term_for(&json!(i64::MIN), &Type::Integer).unwrap(),
            i64::MIN.to_string()
        );
        // Above `i64::MAX` is still an integer to both serde_json and Prolog; rejecting it was
        // an artefact of reading every number through `as_i64`.
        assert_eq!(
            term_for(&json!(u64::MAX), &Type::Integer).unwrap(),
            u64::MAX.to_string()
        );
    }

    #[test]
    fn undeclared_strings_become_atoms() {
        assert_eq!(term_for(&json!("abc"), &Type::Any).unwrap(), "abc");
    }

    #[test]
    fn atoms_are_quoted_when_necessary() {
        assert_eq!(term_for(&json!("Rx"), &Type::Atom).unwrap(), "'Rx'");
    }

    #[test]
    fn a_value_cannot_escape_its_quotes_into_the_surrounding_goal() {
        // An argument is interpolated into generated Prolog source, so text that closes its own
        // quote and continues with a new goal has to come back escaped, not executed.
        let injected = "x'), assertz(owned(true)), foo('";
        assert_eq!(
            term_for(&json!(injected), &Type::Atom).unwrap(),
            r"'x\'), assertz(owned(true)), foo(\''"
        );

        let multiline = term_for(&json!("a\nfoo."), &Type::Text).unwrap();
        assert!(!multiline.contains('\n'), "got {multiline}");
    }

    #[test]
    fn types_with_no_json_equivalent_are_refused() {
        assert!(term_for(&json!("x"), &Type::Compound).is_err());
        assert!(term_for(&json!("x"), &Type::Stream).is_err());
        assert!(term_for(&json!("x"), &Type::Custom("widget".into())).is_err());
    }

    #[test]
    fn null_is_never_a_value() {
        // Prolog has no null; silently turning it into an atom `null` would let a caller's
        // "I don't know" unify with a fact.
        for declared in [
            Type::Atom,
            Type::Text,
            Type::Integer,
            Type::Boolean,
            Type::Any,
        ] {
            assert!(term_for(&Value::Null, &declared).is_err(), "{declared}");
        }
    }

    #[test]
    fn error_variable_steps_aside_for_a_colliding_argument() {
        assert_eq!(variable_name("len"), "Len");
        assert_eq!(variable_name(""), "");

        // An argument named `charonError__` upper-cases straight onto the preferred name.
        let colliding = vec![variable_name("charonError__")];
        assert_eq!(colliding[0], ERROR_VARIABLE);
        assert_eq!(error_variable(&colliding), "CharonError___");
        assert_eq!(error_variable(&[]), ERROR_VARIABLE);
    }
}
