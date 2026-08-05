use regex::Regex;

use super::Interpreter;
use json::JsonValue;

/// A predicate declared `dynamic` in a knowledge base's source (`:- dynamic(name/arity).`),
/// i.e. one whose facts can change at runtime via `assertz`/`retract` and, when `--persist` is
/// enabled, is a candidate for being written back to the source file. See `KnowledgeBase::sync`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DynamicPredicate {
    pub name: String,
    pub arity: usize,
}

impl DynamicPredicate {
    /// A callable goal for this predicate applied to fresh variables `V0`..`V{arity-1}`, e.g.
    /// `example(V0)`. Reused verbatim within a single query so repeated occurrences of the same
    /// variable name refer to the same binding.
    fn call_goal(&self) -> String {
        if self.arity == 0 {
            self.name.clone()
        } else {
            let vars = (0..self.arity)
                .map(|i| format!("V{i}"))
                .collect::<Vec<_>>()
                .join(",");
            format!("{}({})", self.name, vars)
        }
    }
}

/// Scans a knowledge base's source text for `:- dynamic(...).` directives (also accepting the
/// unparenthesized `:- dynamic name/arity.` form, and comma-separated lists of indicators in
/// either form) and returns every `name/arity` indicator they declare. Only single-line
/// directives are recognized.
pub(super) fn parse_dynamic_directives(source: &str) -> Vec<DynamicPredicate> {
    let directive_re = Regex::new(r"^\s*:-\s*dynamic\b\s*(.*)\.\s*$").unwrap();
    let indicator_re = Regex::new(r"^([a-zA-Z][a-zA-Z0-9_]*)\s*/\s*(\d+)$").unwrap();

    let mut predicates = Vec::new();
    for line in source.lines() {
        let Some(cap) = directive_re.captures(line) else {
            continue;
        };
        let body = cap[1].trim();
        let body = body
            .strip_prefix('(')
            .and_then(|inner| inner.strip_suffix(')'))
            .unwrap_or(body);

        for part in body.split(',') {
            if let Some(cap) = indicator_re.captures(part.trim()) {
                predicates.push(DynamicPredicate {
                    name: cap[1].to_string(),
                    arity: cap[2].parse().unwrap_or(0),
                });
            }
        }
    }
    predicates
}

/// Whether `line` is a simple ground fact clause of `predicate` — the only shape of dynamic
/// clause `KnowledgeBase::sync` knows how to add/remove from the source file. Rules (containing
/// `:-`) and anything spanning multiple lines aren't recognized.
pub(super) fn is_fact_line(line: &str, predicate: &DynamicPredicate) -> bool {
    let trimmed = line.trim();
    if predicate.arity == 0 {
        return trimmed == format!("{}.", predicate.name);
    }
    trimmed.starts_with(&format!("{}(", predicate.name))
        && trimmed.ends_with(").")
        && !trimmed.contains(":-")
}

/// Asks the running `interpreter` for every fact currently satisfying `predicate`, each rendered
/// as quoted, `double_quotes(true)` Prolog source text (e.g. `example("a")`) — the same rendering
/// `canonicalize` uses for existing source lines, so the two are directly comparable.
pub(super) fn live_facts(interpreter: &Interpreter, predicate: &DynamicPredicate) -> Vec<String> {
    let goal = predicate.call_goal();
    let query = format!(
        "findall(S, ({goal}, charsio:write_term_to_chars({goal}, [quoted(true), double_quotes(true)], S)), Facts)."
    );

    let Some(Ok(JsonValue::Object(bindings))) = interpreter.query(&query).into_iter().next() else {
        return Vec::new();
    };
    let Some(JsonValue::Array(items)) = bindings.get("Facts").cloned() else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_str().map(String::from))
        .collect()
}

/// Renders a raw term of source text (e.g. the contents of a fact line, minus its trailing `.`)
/// the same way `live_facts` renders currently-true facts, so the two can be compared by string
/// equality regardless of how the original source formatted it. Returns `None` if `term_text`
/// isn't parseable Prolog (e.g. it turned out not to be a fact line after all).
pub(super) fn canonicalize(interpreter: &Interpreter, term_text: &str) -> Option<String> {
    let query = format!(
        "Term = ({term_text}), charsio:write_term_to_chars(Term, [quoted(true), double_quotes(true)], S)."
    );
    let Some(Ok(JsonValue::Object(bindings))) = interpreter.query(&query).into_iter().next() else {
        return None;
    };
    bindings.get("S").and_then(|v| v.as_str()).map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_parenthesized_single_directive() {
        assert_eq!(
            parse_dynamic_directives(":- dynamic(role/1).\n"),
            vec![DynamicPredicate {
                name: "role".to_string(),
                arity: 1
            }]
        );
    }

    #[test]
    fn parses_parenthesized_list() {
        assert_eq!(
            parse_dynamic_directives(":- dynamic(a/1, b/2).\n"),
            vec![
                DynamicPredicate {
                    name: "a".to_string(),
                    arity: 1
                },
                DynamicPredicate {
                    name: "b".to_string(),
                    arity: 2
                }
            ]
        );
    }

    #[test]
    fn parses_unparenthesized_form() {
        assert_eq!(
            parse_dynamic_directives(":- dynamic a/1, b/2.\n"),
            vec![
                DynamicPredicate {
                    name: "a".to_string(),
                    arity: 1
                },
                DynamicPredicate {
                    name: "b".to_string(),
                    arity: 2
                }
            ]
        );
    }

    #[test]
    fn ignores_non_directive_lines() {
        assert_eq!(
            parse_dynamic_directives("example(a).\n% :- dynamic(role/1).\n"),
            vec![]
        );
    }

    #[test]
    fn fact_line_detection() {
        let predicate = DynamicPredicate {
            name: "example".to_string(),
            arity: 1,
        };
        assert!(is_fact_line("example(a).", &predicate));
        assert!(is_fact_line("  example(\"a\").  ", &predicate));
        assert!(!is_fact_line("example(X) :- foo(X).", &predicate));
        assert!(!is_fact_line("other(a).", &predicate));
    }
}
