//! Writing runtime changes back to the source file.
//!
//! With `--persist`, every `assertz`/`retract` a request performs has to be mirrored into the
//! `.pl` file, so that restarting the server does not silently roll the knowledge base back.
//! The mapping between "a fact in the machine" and "a line in the file" is the delicate part,
//! and it is deliberately narrow: only single-line ground facts of a `dynamic` predicate are
//! recognised. Anything else — a rule, a fact spanning two lines, a clause with a trailing
//! comment — is left exactly as the author wrote it.

use regex::Regex;
use serde_json::Value;

use super::Interpreter;
use super::term::quote_atom;

/// A predicate declared `dynamic` in a knowledge base's source (`:- dynamic(name/arity).`),
/// i.e. one whose facts can change at runtime and is a candidate for being written back.
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
        let name = quote_atom(&self.name);
        if self.arity == 0 {
            return name;
        }
        let variables: Vec<String> = (0..self.arity).map(|index| format!("V{index}")).collect();
        format!("{name}({})", variables.join(","))
    }
}

/// Scans a knowledge base's source text for `:- dynamic(...).` directives (also accepting the
/// unparenthesised `:- dynamic name/arity.` form, and comma-separated lists of indicators in
/// either form) and returns every `name/arity` indicator they declare. Only single-line
/// directives are recognised.
pub(super) fn parse_dynamic_directives(source: &str) -> Vec<DynamicPredicate> {
    let directive = Regex::new(r"^\s*:-\s*dynamic\b\s*(.*)\.\s*$").expect("static pattern");
    let indicator = Regex::new(r"^([a-z][a-zA-Z0-9_]*)\s*/\s*(\d+)$").expect("static pattern");

    let mut predicates = Vec::new();
    for line in source.lines() {
        let Some(captured) = directive.captures(line) else {
            continue;
        };
        let body = captured[1].trim();
        let body = body
            .strip_prefix('(')
            .and_then(|inner| inner.strip_suffix(')'))
            .unwrap_or(body);

        for part in body.split(',') {
            if let Some(captured) = indicator.captures(part.trim())
                && let Ok(arity) = captured[2].parse()
            {
                predicates.push(DynamicPredicate {
                    name: captured[1].to_string(),
                    arity,
                });
            }
        }
    }
    predicates
}

/// If `line` holds exactly one ground fact clause, its functor and arity.
///
/// The arity matters, and getting it wrong loses data: matching `:- dynamic(f/1).` against every
/// line beginning `f(` would claim a `f(a, b).` belonging to an entirely different `f/2` and then
/// delete it for not being among `f/1`'s live facts.
pub(super) fn fact_line_indicator(line: &str) -> Option<(&str, usize)> {
    let text = line.trim().strip_suffix('.')?.trim_end();

    // An unquoted-atom functor only. A quoted one would need the term reader to settle where it
    // ends, and a dynamic predicate whose name needs quoting is rare enough not to guess at.
    let name_end = text
        .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
        .unwrap_or(text.len());
    let (name, rest) = text.split_at(name_end);
    if !name.starts_with(|first: char| first.is_ascii_lowercase()) {
        return None;
    }
    if rest.is_empty() {
        return Some((name, 0));
    }

    // Everything after the argument list — `:- Body`, a trailing comment, a second clause — puts
    // this line outside what can be rewritten safely.
    let (arguments, tail) = split_at_closing_paren(rest.strip_prefix('(')?)?;
    if !tail.trim().is_empty() {
        return None;
    }
    Some((name, count_arguments(arguments)))
}

/// Whether `line` is a ground fact clause of `predicate` — the only shape of dynamic clause
/// [`crate::prolog::KnowledgeBase`] knows how to add to or remove from the source file.
pub(super) fn is_fact_line(line: &str, predicate: &DynamicPredicate) -> bool {
    fact_line_indicator(line) == Some((predicate.name.as_str(), predicate.arity))
}

/// Splits `text` at the `)` closing a group whose `(` has already been consumed, into the group's
/// contents and whatever follows it. Quoted text is skipped, so a `)` inside `'a)b'` does not
/// end the group.
fn split_at_closing_paren(text: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for (index, character) in text.char_indices() {
        if let Some(open_quote) = quote {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                character if character == open_quote => quote = None,
                _ => {}
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            '(' | '[' | '{' => depth += 1,
            ')' if depth == 0 => return Some((&text[..index], &text[index + 1..])),
            ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The number of arguments in an already-delimited argument list, counting only the commas that
/// separate them rather than the ones nested inside a term or a quoted atom.
fn count_arguments(arguments: &str) -> usize {
    if arguments.trim().is_empty() {
        return 0;
    }
    let mut count = 1;
    let mut depth = 0usize;
    let mut quote: Option<char> = None;
    let mut escaped = false;

    for character in arguments.chars() {
        if let Some(open_quote) = quote {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                character if character == open_quote => quote = None,
                _ => {}
            }
            continue;
        }
        match character {
            '\'' | '"' | '`' => quote = Some(character),
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => count += 1,
            _ => {}
        }
    }
    count
}

/// Asks the running `interpreter` for every fact currently satisfying `predicate`, each rendered
/// as quoted, `double_quotes(true)` Prolog source text (e.g. `example("a")`) — the same rendering
/// [`canonicalize`] uses for existing source lines, so the two are directly comparable.
pub(super) fn live_facts(interpreter: &Interpreter, predicate: &DynamicPredicate) -> Vec<String> {
    let goal = predicate.call_goal();
    let query = format!(
        "findall(S, ({goal}, charsio:write_term_to_chars({goal}, [quoted(true), double_quotes(true)], S)), Facts)."
    );

    let Ok(answers) = interpreter.query(&query) else {
        return Vec::new();
    };
    let Some(Value::Array(items)) = answers.first().and_then(|answer| answer.binding("Facts"))
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| item.as_str().map(String::from))
        .collect()
}

/// Renders a raw term of source text (a fact line minus its trailing `.`) the way [`live_facts`]
/// renders currently-true facts, so the two can be compared by string equality regardless of how
/// the original source spelled it. `None` if `term_text` is not parseable Prolog.
pub(super) fn canonicalize(interpreter: &Interpreter, term_text: &str) -> Option<String> {
    let query = format!(
        "Term = ({term_text}), charsio:write_term_to_chars(Term, [quoted(true), double_quotes(true)], S)."
    );
    let answers = interpreter.query(&query).ok()?;
    answers
        .first()
        .and_then(|answer| answer.binding("S"))
        .and_then(Value::as_str)
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn predicate(name: &str, arity: usize) -> DynamicPredicate {
        DynamicPredicate {
            name: name.to_string(),
            arity,
        }
    }

    #[test]
    fn parses_parenthesized_single_directive() {
        assert_eq!(
            parse_dynamic_directives(":- dynamic(role/1).\n"),
            vec![predicate("role", 1)]
        );
    }

    #[test]
    fn parses_parenthesized_list() {
        assert_eq!(
            parse_dynamic_directives(":- dynamic(a/1, b/2).\n"),
            vec![predicate("a", 1), predicate("b", 2)]
        );
    }

    #[test]
    fn parses_unparenthesized_form() {
        assert_eq!(
            parse_dynamic_directives(":- dynamic a/1, b/2.\n"),
            vec![predicate("a", 1), predicate("b", 2)]
        );
    }

    #[test]
    fn parses_zero_arity_and_ignores_malformed_indicators() {
        assert_eq!(
            parse_dynamic_directives(":- dynamic(ready/0, Bad/1, also/2).\n"),
            vec![predicate("ready", 0), predicate("also", 2)]
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
        let example = predicate("example", 1);
        assert!(is_fact_line("example(a).", &example));
        assert!(is_fact_line("  example(\"a\").  ", &example));
        assert!(!is_fact_line("example(X) :- foo(X).", &example));
        assert!(!is_fact_line("other(a).", &example));
        assert!(!is_fact_line("% example(a).", &example));
        assert!(!is_fact_line("", &example));
    }

    #[test]
    fn arity_is_part_of_the_match() {
        // Arity is part of the match: `:- dynamic(f/1).` must not claim `f(a, b).` — a clause of
        // the unrelated `f/2` — and then delete it for not being one of `f/1`'s live facts.
        assert!(!is_fact_line("example(a, b).", &predicate("example", 1)));
        assert!(is_fact_line("example(a, b).", &predicate("example", 2)));
        assert!(is_fact_line("ready.", &predicate("ready", 0)));
        assert!(!is_fact_line("ready.", &predicate("ready", 1)));
        assert!(!is_fact_line("readymade.", &predicate("ready", 0)));
    }

    #[test]
    fn commas_inside_a_term_do_not_inflate_the_arity() {
        assert_eq!(fact_line_indicator("f(g(a, b)) ."), Some(("f", 1)));
        assert_eq!(fact_line_indicator("f([a, b], c)."), Some(("f", 2)));
        assert_eq!(fact_line_indicator("f('a, b')."), Some(("f", 1)));
        assert_eq!(fact_line_indicator(r"f('it\'s, fine')."), Some(("f", 1)));
        assert_eq!(fact_line_indicator("f(\"a, b\", c)."), Some(("f", 2)));
    }

    #[test]
    fn a_line_carrying_more_than_one_clause_is_not_rewritable() {
        // Two facts on one line, or a fact plus a comment, cannot be edited line-by-line
        // without risking the part that was not meant to change.
        assert_eq!(fact_line_indicator("f(a). g(b)."), None);
        assert_eq!(fact_line_indicator("f(a). % a note"), None);
        assert_eq!(fact_line_indicator("f(a"), None);
    }
}
