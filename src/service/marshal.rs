//! Carrying arguments between JSON and Prolog, in both directions.
//!
//! Conversion is driven by the *declared* type, recursively, so a JSON string yields the same
//! term whether it arrives on its own or inside a list: `atom` and `list(atom)` agree, and a
//! knowledge base matching plain facts needs no conversion clauses of its own.
//!
//! The way back is nearly all handled by [`crate::prolog::to_json`], which reads a term without
//! being told what it was declared to be — deliberately, because an `atom` and a `string` are
//! both just text to whoever reads the response. [`json_for`] handles the one case where that
//! is not enough.

use std::fmt;

use serde_json::{Map, Value};

use crate::prolog::{Type, quote_atom, quote_string};

/// Adjusts a term's JSON to what its declared type promised.
///
/// [`crate::prolog::to_json`] maps a term by its own shape, which is right for almost every
/// type: an atom and a string are both text, and a caller wants the text. `bool` is the
/// exception. Prolog has no boolean — it has the atoms `true` and `false` — so a term-shaped
/// mapping yields the JSON *string* `"true"` while the schema Charon publishes for that same
/// argument says `"type": "boolean"`. A client validating against the OpenAPI document then
/// rejects a response Charon generated from its own description of itself.
///
/// Only that mismatch is repaired, and only for values that are actually `true` or `false`. An
/// output declared `bool` and bound to something else is a defective knowledge base, and
/// guessing what it meant would hide the defect rather than report it; the value is passed
/// through as it came so that what is wrong stays visible.
pub fn json_for(value: Value, declared: &Type) -> Value {
    match declared {
        Type::Boolean => match value.as_str() {
            Some("true") => Value::Bool(true),
            Some("false") => Value::Bool(false),
            _ => value,
        },
        Type::List(element) => match value {
            Value::Array(items) => Value::Array(
                items
                    .into_iter()
                    .map(|item| json_for(item, element))
                    .collect(),
            ),
            other => other,
        },
        _ => value,
    }
}

/// The name Charon prefers for the variable a generated goal binds a caught exception to.
const ERROR_VARIABLE: &str = "CharonError__";

/// A variable name for the caught exception that none of `argument_variables` already uses.
///
/// An argument literally named `charonError__` becomes the variable `CharonError__`, so the base
/// name cannot simply be assumed unique: widen it until it is.
pub fn error_variable(argument_variables: &[&str]) -> String {
    let mut name = ERROR_VARIABLE.to_string();
    while argument_variables.contains(&name.as_str()) {
        name.push('_');
    }
    name
}

/// Wraps a goal so that an exception is reported as a binding instead of propagating.
///
/// An exception that reaches scryer's top level leaves the machine unusable: every later query,
/// including ones against unrelated predicates, keeps returning that same stale error. Catching
/// it inside the query keeps one bad request from poisoning the process, and means a knowledge
/// base needs no defensive `catch/3` wrappers of its own.
pub fn wrap_goal(goal: &str, error_variable: &str) -> String {
    format!("catch({goal}, {error_variable}, true).")
}

/// Renders a `{"functor": "dose", "args": [500, "mg"]}` object as the term `dose(500,mg)`.
///
/// Arguments are marshalled as `any`, so they nest: a compound argument of a compound term is
/// just another object. A functor with no arguments is an atom, which is what Prolog means by a
/// compound of arity zero.
fn compound_term(object: &Map<String, Value>) -> Result<String, TypeMismatch> {
    let Some(functor) = object.get("functor").and_then(Value::as_str) else {
        return Err(TypeMismatch {
            expected: "a compound term with a string `functor`".to_string(),
            found: describe(&Value::Object(object.clone())),
        });
    };

    let arguments = match object.get("args") {
        Some(Value::Array(arguments)) => arguments.as_slice(),
        // Absent is the same as none; anything else is a mistake worth naming.
        None | Some(Value::Null) => &[],
        Some(other) => {
            return Err(TypeMismatch {
                expected: "`args` to be an array".to_string(),
                found: describe(other),
            });
        }
    };

    if arguments.is_empty() {
        return Ok(quote_atom(functor));
    }
    let terms = arguments
        .iter()
        .map(|argument| term_for(argument, &Type::Any))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!("{}({})", quote_atom(functor), terms.join(",")))
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
        // become atoms, matching the scalar `atom` case.
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
            // An object in the `{functor, args}` encoding is a compound term. Accepted here as
            // well as under `compound`, so a structured answer can be handed straight back as an
            // argument without re-declaring its type.
            Value::Object(object) => compound_term(object),
            other => Err(mismatch("a scalar, array or compound term", other)),
        },
        Type::Compound => match value {
            Value::Object(object) => compound_term(object),
            other => Err(mismatch("a compound term", other)),
        },
        Type::Stream | Type::Custom(_) => Err(TypeMismatch {
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
        // One JSON string yields the same Prolog term whether it arrives on its own or inside
        // a list, which is what lets one knowledge base match both.
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
        // Above `i64::MAX` is still an integer to both serde_json and Prolog, so reading every
        // number through `as_i64` would reject it wrongly.
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
        // quote and continues with a new goal has to come back escaped rather than executed.
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
        // An argument named `charonError__` upper-cases straight onto the preferred name.
        assert_eq!(error_variable(&[ERROR_VARIABLE]), "CharonError___");
        assert_eq!(error_variable(&[]), ERROR_VARIABLE);
    }
}
