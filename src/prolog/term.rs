//! Prolog terms: how they become JSON, and how they are rendered back as Prolog source text.

use scryer_prolog::Term;
use serde_json::{Map, Value};

/// Converts a term to JSON, the representation Charon speaks on both of its interfaces.
///
/// Integers stay integers: routing them through `f64` would turn every count and identifier in a
/// response into a float, which is both ugly and lossy for large values.
///
/// The mapping is deliberately lossy in one direction — an atom and a string both become a JSON
/// string — because a caller reading a response wants the text rather than the term's
/// provenance. What an argument *becomes* on the way in is decided by its declared type instead
/// (see [`crate::service`]).
pub(crate) fn to_json(term: Term) -> Value {
    match term {
        Term::Var(variable) => Value::String(variable),
        Term::Atom(atom) => Value::String(atom),
        Term::String(string) => Value::String(string),
        Term::Integer(integer) => {
            // The arbitrary-precision integer is rendered as text and re-read, so values that
            // fit an i64 stay exact instead of being rounded through f64.
            match integer.to_string().parse::<i64>() {
                Ok(value) => Value::from(value),
                Err(_) => json_number(integer.to_f64().value()),
            }
        }
        Term::Float(float) => json_number(float),
        Term::Rational(rational) => json_number(rational.to_f64().value()),
        Term::List(items) => Value::Array(items.into_iter().map(to_json).collect()),
        Term::Compound(functor, arguments) => {
            let mut object = Map::new();
            object.insert("functor".to_string(), Value::String(functor));
            object.insert(
                "args".to_string(),
                Value::Array(arguments.into_iter().map(to_json).collect()),
            );
            Value::Object(object)
        }
        // `Term` is `#[non_exhaustive]`. Anything scryer adds later is surfaced as text rather
        // than silently dropped, so a caller can at least see what came back.
        other => Value::String(format!("{other:?}")),
    }
}

fn json_number(value: f64) -> Value {
    serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
}

/// Renders a term in Prolog syntax, e.g. `type_error(evaluable,foo/0)`.
///
/// Used for the human-facing half of a [`crate::prolog::PrologError`]: the structured
/// `{"functor": …, "args": […]}` encoding is unreadable once error terms nest three deep, which
/// is exactly when someone needs to read them.
pub fn render(term: &Term) -> String {
    match term {
        Term::Var(variable) => format!("_{variable}"),
        Term::Atom(atom) => quote_atom(atom),
        Term::String(string) => quote_string(string),
        Term::Integer(integer) => integer.to_string(),
        Term::Float(float) => float.to_string(),
        Term::Rational(rational) => rational.to_string(),
        Term::List(items) => format!("[{}]", render_all(items)),
        // Predicate indicators (`foo/1`) and module qualifications (`lists:member`) are far more
        // legible infix, and both are common inside error terms.
        Term::Compound(functor, arguments) if is_infix(functor, arguments.len()) => format!(
            "{}{functor}{}",
            render(&arguments[0]),
            render(&arguments[1])
        ),
        Term::Compound(functor, arguments) => {
            format!("{}({})", quote_atom(functor), render_all(arguments))
        }
        other => format!("{other:?}"),
    }
}

fn render_all(terms: &[Term]) -> String {
    terms.iter().map(render).collect::<Vec<_>>().join(",")
}

/// Renders a term that has already been through [`to_json`], in the same Prolog syntax
/// [`render`] produces.
///
/// This is the path an exception takes when it comes back as a `catch/3` binding rather than as
/// a propagated scryer error. The round trip through JSON has forgotten whether a piece of text
/// was an atom or a string, so it is rendered as an atom — which is what error terms are built
/// from in practice.
pub fn render_json(value: &Value) -> String {
    match value {
        Value::Object(object) => match (object.get("functor"), object.get("args")) {
            (Some(Value::String(functor)), Some(Value::Array(arguments))) => {
                let rendered: Vec<String> = arguments.iter().map(render_json).collect();
                if is_infix(functor, rendered.len()) {
                    format!("{}{functor}{}", rendered[0], rendered[1])
                } else {
                    format!("{}({})", quote_atom(functor), rendered.join(","))
                }
            }
            _ => value.to_string(),
        },
        Value::String(text) => quote_atom(text),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(render_json).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

/// Whether a compound term reads better written between its arguments than in front of them.
fn is_infix(functor: &str, arity: usize) -> bool {
    arity == 2 && matches!(functor, "/" | ":")
}

/// Renders an atom, quoting it only when Prolog's unquoted-atom syntax wouldn't accept it.
pub fn quote_atom(atom: &str) -> String {
    if is_unquoted_atom(atom) {
        return atom.to_string();
    }
    let mut quoted = String::with_capacity(atom.len() + 2);
    quoted.push('\'');
    escape_into(&mut quoted, atom, '\'');
    quoted.push('\'');
    quoted
}

/// Renders text as a double-quoted Prolog string, which under scryer's default `double_quotes`
/// flag reads back as a list of characters.
pub fn quote_string(text: &str) -> String {
    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');
    escape_into(&mut quoted, text, '"');
    quoted.push('"');
    quoted
}

/// Whether Prolog would read `atom` as an atom without quotes: a lower-case letter followed by
/// alphanumerics and underscores. Deliberately conservative — quoting something that did not
/// need it is harmless, failing to quote something that did is a syntax error at best.
fn is_unquoted_atom(atom: &str) -> bool {
    let mut characters = atom.chars();
    matches!(characters.next(), Some(first) if first.is_ascii_lowercase())
        && characters.all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// Escapes the characters a quoted Prolog token cannot carry literally.
///
/// Control characters matter here even though they look exotic: a knowledge base is often asked
/// about text a user typed, and an unescaped newline inside `'…'` ends the term mid-goal, which
/// turns a mundane argument into a syntax error — or, worse, into injected Prolog.
fn escape_into(out: &mut String, text: &str, quote: char) {
    for character in text.chars() {
        match character {
            '\\' => out.push_str("\\\\"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            '\u{7}' => out.push_str("\\a"),
            '\u{8}' => out.push_str("\\b"),
            '\u{b}' => out.push_str("\\v"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:x}\\", c as u32));
            }
            c => out.push(c),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_atoms_only_when_needed() {
        assert_eq!(quote_atom("foo"), "foo");
        assert_eq!(quote_atom("foo_bar1"), "foo_bar1");
        assert_eq!(quote_atom("Foo"), "'Foo'");
        assert_eq!(quote_atom("with space"), "'with space'");
        assert_eq!(quote_atom(""), "''");
        assert_eq!(quote_atom("it's"), r"'it\'s'");
    }

    #[test]
    fn control_characters_are_escaped_rather_than_ending_the_term() {
        // An unescaped newline closes nothing and breaks the generated goal; worse, a caller
        // could end a quote and continue with Prolog of their own.
        assert_eq!(quote_atom("a\nb"), r"'a\nb'");
        assert_eq!(quote_string("a\nb"), r#""a\nb""#);
        assert_eq!(quote_atom("tab\there"), r"'tab\there'");
        assert_eq!(quote_atom("nul\0"), r"'nul\x0\'");
        assert_eq!(quote_string("quote\"and\\slash"), r#""quote\"and\\slash""#);
    }

    #[test]
    fn renders_error_terms_in_prolog_syntax() {
        let term = Term::Compound(
            "error".to_string(),
            vec![
                Term::Compound(
                    "existence_error".to_string(),
                    vec![
                        Term::Atom("procedure".to_string()),
                        Term::Compound(
                            "/".to_string(),
                            vec![Term::Atom("after".to_string()), Term::integer(1)],
                        ),
                    ],
                ),
                Term::Atom("after".to_string()),
            ],
        );
        assert_eq!(
            render(&term),
            "error(existence_error(procedure,after/1),after)"
        );
    }

    #[test]
    fn the_two_renderings_agree() {
        // The same term reached through `catch/3` (JSON) and through a propagated scryer error
        // (a `Term`) must read the same, or the transport a request happened to take would
        // change the wording of its error message.
        let term = Term::Compound(
            "error".to_string(),
            vec![
                Term::Compound(
                    "type_error".to_string(),
                    vec![
                        Term::Atom("evaluable".to_string()),
                        Term::Compound(
                            "/".to_string(),
                            vec![Term::Atom("foo".to_string()), Term::integer(0)],
                        ),
                    ],
                ),
                Term::Atom("is".to_string()),
            ],
        );
        let rendered = render(&term);
        assert_eq!(rendered, "error(type_error(evaluable,foo/0),is)");
        assert_eq!(render_json(&to_json(term)), rendered);
    }

    #[test]
    fn integers_stay_integers() {
        assert_eq!(to_json(Term::integer(3)), Value::from(3));
        assert_eq!(to_json(Term::integer(-7)), Value::from(-7));
    }
}
