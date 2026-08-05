use regex::Regex;
use std::fmt;
use std::str::FromStr;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Mode {
    Input,       // +
    Output,      // -
    InputOutput, // ?
    ReadOnly,    // @
}

impl FromStr for Mode {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim() {
            "+" => Ok(Mode::Input),
            "-" => Ok(Mode::Output),
            "?" => Ok(Mode::InputOutput),
            "@" => Ok(Mode::ReadOnly),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Mode::Input => write!(f, "Input (+)"),
            Mode::Output => write!(f, "Output (-)"),
            Mode::InputOutput => write!(f, "Input/Output (?)"),
            Mode::ReadOnly => write!(f, "Read-Only (@)"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Type {
    Any,
    Atom,
    Integer,
    Float,
    List,
    Compound,
    Stream,
    Custom(String),
}

impl FromStr for Type {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "any" => Ok(Type::Any),
            "atom" => Ok(Type::Atom),
            "int" | "integer" => Ok(Type::Integer),
            "float" | "number" => Ok(Type::Float),
            "list" => Ok(Type::List),
            "compound" => Ok(Type::Compound),
            "stream" => Ok(Type::Stream),
            other => Ok(Type::Custom(other.to_string())),
        }
    }
}

impl fmt::Display for Type {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Type::Any => write!(f, "any"),
            Type::Atom => write!(f, "atom"),
            Type::Integer => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::List => write!(f, "list"),
            Type::Compound => write!(f, "compound"),
            Type::Stream => write!(f, "stream"),
            Type::Custom(custom) => write!(f, "{}", custom),
        }
    }
}

/// A predicate's declared determinism (the `is det`/`is semidet`/... suffix on a PlDoc mode
/// line). See <https://www.swi-prolog.org/pldoc/man?section=modes>.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Determinism {
    /// Succeeds exactly once, never fails.
    Det,
    /// Fails, or succeeds exactly once.
    Semidet,
    /// Fails, or succeeds one or more times.
    Nondet,
    /// Succeeds one or more times, never fails.
    Multi,
    /// Always fails.
    Failure,
    /// Determinism is not (yet) established.
    Undefined,
}

impl FromStr for Determinism {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "det" => Ok(Determinism::Det),
            "semidet" => Ok(Determinism::Semidet),
            "nondet" => Ok(Determinism::Nondet),
            "multi" => Ok(Determinism::Multi),
            "failure" => Ok(Determinism::Failure),
            "undefined" => Ok(Determinism::Undefined),
            _ => Err(()),
        }
    }
}

impl fmt::Display for Determinism {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Determinism::Det => write!(f, "det — succeeds exactly once"),
            Determinism::Semidet => write!(f, "semidet — fails, or succeeds exactly once"),
            Determinism::Nondet => write!(f, "nondet — fails, or succeeds one or more times"),
            Determinism::Multi => write!(f, "multi — succeeds one or more times"),
            Determinism::Failure => write!(f, "failure — always fails"),
            Determinism::Undefined => write!(f, "undefined"),
        }
    }
}

impl Determinism {
    /// The number of solutions this determinism guarantees, as `(min, max)`. `max: None` means
    /// unbounded. Useful for constraining the size of the JSON array returned for a query.
    pub fn solution_bounds(&self) -> (u64, Option<u64>) {
        match self {
            Determinism::Det => (1, Some(1)),
            Determinism::Semidet => (0, Some(1)),
            Determinism::Nondet => (0, None),
            Determinism::Multi => (1, None),
            Determinism::Failure => (0, Some(0)),
            Determinism::Undefined => (0, None),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Argument {
    pub name: String,
    pub mode: Option<Mode>,
    pub param_type: Type,
    /// Free-text description taken from this argument's `@arg`/`@param` PlDoc tag, if any.
    pub description: Option<String>,
}

/// Predicate-level documentation gathered from the free text and tags following a `%!` mode
/// line. See <https://www.swi-prolog.org/pldoc/man?section=tags> for the tag reference.
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct Doc {
    /// The first paragraph of the doc comment.
    pub summary: Option<String>,
    /// Any paragraphs after the summary.
    pub description: Option<String>,
    /// Present if an `@deprecated` tag was found; holds its (possibly empty) message.
    pub deprecated: Option<String>,
    /// `@see` entries.
    pub see_also: Vec<String>,
    /// `@author` entries.
    pub authors: Vec<String>,
    /// `@version` text.
    pub version: Option<String>,
    /// `@license` text.
    pub license: Option<String>,
    /// `@copyright` text.
    pub copyright: Option<String>,
    /// `@compat` entries.
    pub compat: Vec<String>,
    /// `@bug` entries.
    pub bugs: Vec<String>,
    /// `@tbd` entries.
    pub tbd: Vec<String>,
    /// `@error`/`@throws` entries.
    pub errors: Vec<String>,
}

impl Doc {
    /// Renders the tags that don't already have a dedicated home in a caller's output (i.e.
    /// everything but `summary`/`description`) as short markdown lines, e.g. for appending
    /// under a longer description. Empty if none of these tags were present.
    pub fn tag_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(message) = &self.deprecated {
            lines.push(if message.is_empty() {
                "**Deprecated.**".to_string()
            } else {
                format!("**Deprecated.** {message}")
            });
        }
        if !self.see_also.is_empty() {
            lines.push(format!("**See also:** {}", self.see_also.join(", ")));
        }
        if !self.errors.is_empty() {
            lines.push(format!("**Errors:** {}", self.errors.join("; ")));
        }
        if !self.authors.is_empty() {
            lines.push(format!("**Author(s):** {}", self.authors.join(", ")));
        }
        if let Some(version) = &self.version {
            lines.push(format!("**Version:** {version}"));
        }
        if !self.compat.is_empty() {
            lines.push(format!("**Compatibility:** {}", self.compat.join(", ")));
        }
        if let Some(license) = &self.license {
            lines.push(format!("**License:** {license}"));
        }
        if let Some(copyright) = &self.copyright {
            lines.push(format!("**Copyright:** {copyright}"));
        }
        if !self.bugs.is_empty() {
            lines.push(format!("**Known issues:** {}", self.bugs.join("; ")));
        }
        if !self.tbd.is_empty() {
            lines.push(format!("**To be done:** {}", self.tbd.join("; ")));
        }
        lines
    }

    /// Renders the full doc comment — summary, description, and tag appendix — as one markdown
    /// blob, for contexts (like an OpenAPI `Info.description`) that have no separate slot for a
    /// summary. Returns `None` if the doc comment was empty.
    pub fn render(&self) -> Option<String> {
        let mut sections: Vec<String> = self
            .summary
            .iter()
            .chain(self.description.iter())
            .cloned()
            .collect();

        let tag_lines = self.tag_lines();
        if !tag_lines.is_empty() {
            sections.push(tag_lines.join("  \n"));
        }

        (!sections.is_empty()).then(|| sections.join("\n\n"))
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct Predicate {
    pub name: String,
    pub arguments: Vec<Argument>,
    pub determinism: Option<Determinism>,
    pub doc: Doc,
}

impl Predicate {
    pub(super) fn parse(source_code: &str) -> Vec<Predicate> {
        // Matches a "%! name(args...) [is determinism]." mode line in full.
        let signature_re = Regex::new(
            r"(?x)
            ^\s*%!\s+
            ([a-zA-Z0-9_]+)\s*\(([^)]*)\)
            \s*(?:is\s+([a-zA-Z]+))?
            \s*\.?\s*$
            ",
        )
        .unwrap();

        // Matches mode (+/-/?/@), variable name, and optional :type
        let arg_re =
            Regex::new(r"([+\-?@])?\s*([a-zA-Z0-9_]+)(?:\s*:\s*([a-zA-Z0-9_]+))?").unwrap();

        // Matches a "@tag rest-of-line" doc tag.
        let tag_re = Regex::new(r"^@(\w+)(?:\s+(.*))?$").unwrap();

        let lines: Vec<&str> = source_code.lines().collect();
        let mut predicates = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let Some(cap) = signature_re.captures(lines[i]) else {
                i += 1;
                continue;
            };

            let name = cap[1].to_string();
            let args_str = cap[2].to_string();
            let determinism = cap.get(3).and_then(|m| m.as_str().parse().ok());

            let mut arguments: Vec<Argument> = arg_re
                .captures_iter(&args_str)
                .map(|arg_cap| {
                    let mode = arg_cap.get(1).and_then(|m| m.as_str().parse::<Mode>().ok());
                    let name = arg_cap[2].to_string();
                    let param_type = arg_cap
                        .get(3)
                        .and_then(|t| t.as_str().parse::<Type>().ok())
                        .unwrap_or(Type::Any);

                    Argument {
                        name,
                        mode,
                        param_type,
                        description: None,
                    }
                })
                .collect();

            i += 1;
            // PlDoc allows several mode lines (one per calling pattern) to document one
            // predicate; skip any further ones directly below the first.
            while i < lines.len() && signature_re.is_match(lines[i]) {
                i += 1;
            }

            // Consume the rest of the doc comment block: consecutive lines starting with `%`.
            let mut doc_lines: Vec<&str> = Vec::new();
            while i < lines.len() {
                let Some(rest) = lines[i].trim_start().strip_prefix('%') else {
                    break;
                };
                // A "%!" line here would start a new predicate's documentation.
                if rest.trim_start().starts_with('!') {
                    break;
                }
                doc_lines.push(rest.strip_prefix(' ').unwrap_or(rest));
                i += 1;
            }

            let doc = Self::parse_doc_block(&doc_lines, &tag_re, &mut arguments);

            predicates.push(Predicate {
                name,
                arguments,
                determinism,
                doc,
            });
        }
        predicates
    }

    /// Splits a doc comment block into a leading free-text description and a set of `@tag`
    /// sections, applying `@arg`/`@param` tags to the matching argument in-place.
    fn parse_doc_block(lines: &[&str], tag_re: &Regex, arguments: &mut [Argument]) -> Doc {
        let tag_start = lines.iter().position(|line| tag_re.is_match(line.trim()));
        let (prose, tag_lines) = match tag_start {
            Some(index) => (&lines[..index], &lines[index..]),
            None => (lines, &[][..]),
        };

        let mut paragraphs = Self::paragraphs(prose).into_iter();
        let summary = paragraphs.next();
        let description = {
            let rest: Vec<String> = paragraphs.collect();
            (!rest.is_empty()).then(|| rest.join("\n\n"))
        };

        let mut doc = Doc {
            summary,
            description,
            ..Doc::default()
        };

        let mut i = 0;
        while i < tag_lines.len() {
            let Some(cap) = tag_re.captures(tag_lines[i].trim()) else {
                i += 1;
                continue;
            };
            let tag = cap[1].to_lowercase();
            let mut content = cap
                .get(2)
                .map(|m| m.as_str().to_string())
                .unwrap_or_default();
            i += 1;

            // Continuation lines: everything up to the next tag or a blank separator line.
            while i < tag_lines.len() {
                let line = tag_lines[i].trim();
                if line.is_empty() || tag_re.is_match(line) {
                    break;
                }
                if !content.is_empty() {
                    content.push(' ');
                }
                content.push_str(line);
                i += 1;
            }
            let content = content.trim().to_string();

            match tag.as_str() {
                "arg" | "param" => {
                    if let Some((arg_name, arg_desc)) = content.split_once(char::is_whitespace)
                        && let Some(argument) = arguments
                            .iter_mut()
                            .find(|argument| argument.name.eq_ignore_ascii_case(arg_name))
                    {
                        argument.description = Some(arg_desc.trim().to_string());
                    }
                }
                "deprecated" => doc.deprecated = Some(content),
                "see" if !content.is_empty() => doc.see_also.push(content),
                "author" if !content.is_empty() => doc.authors.push(content),
                "version" if !content.is_empty() => doc.version = Some(content),
                "license" if !content.is_empty() => doc.license = Some(content),
                "copyright" if !content.is_empty() => doc.copyright = Some(content),
                "compat" if !content.is_empty() => doc.compat.push(content),
                "bug" if !content.is_empty() => doc.bugs.push(content),
                "tbd" if !content.is_empty() => doc.tbd.push(content),
                "error" | "throws" if !content.is_empty() => doc.errors.push(content),
                _ => {}
            }
        }

        doc
    }

    /// Groups doc-comment content lines into paragraphs, splitting on blank (`%`-only) lines.
    fn paragraphs(lines: &[&str]) -> Vec<String> {
        let mut paragraphs = Vec::new();
        let mut current: Vec<&str> = Vec::new();
        for line in lines {
            if line.trim().is_empty() {
                if !current.is_empty() {
                    paragraphs.push(current.join(" "));
                    current.clear();
                }
            } else {
                current.push(line.trim());
            }
        }
        if !current.is_empty() {
            paragraphs.push(current.join(" "));
        }
        paragraphs
    }
}

/// A file's PlDoc module comment: `/** <module> Title text ... */`, giving the whole knowledge
/// base a title, description and tags rather than a single predicate. See
/// <https://www.swi-prolog.org/pldoc/man?section=sectioncomments>.
#[derive(Debug, PartialEq, Eq, Clone, Default)]
pub struct ModuleDoc {
    /// The text following `<module>` on its opening line.
    pub title: Option<String>,
    /// The free text and tags in the rest of the comment, parsed exactly like a predicate's doc
    /// comment (`Doc::summary` is the first paragraph, etc.).
    pub doc: Doc,
}

impl ModuleDoc {
    pub(super) fn parse(source_code: &str) -> Option<ModuleDoc> {
        // Find the "/** <module>" opener, then the comment's closing "*/" (Prolog block
        // comments don't nest, so the first one found ends it).
        let marker_re = Regex::new(r"/\*\*[ \t]*<module>[ \t]*").unwrap();
        let after_marker = &source_code[marker_re.find(source_code)?.end()..];
        let body = &after_marker[..after_marker.find("*/")?];

        // The title is the rest of the opener's line; everything after that first newline (if
        // any) is free text and tags, parsed exactly like a predicate's doc comment.
        let (title_line, rest) = body.split_once('\n').unwrap_or((body, ""));
        let title = {
            let title = title_line.trim();
            (!title.is_empty()).then(|| title.to_string())
        };

        let tag_re = Regex::new(r"^@(\w+)(?:\s+(.*))?$").unwrap();
        let lines: Vec<&str> = rest.lines().map(str::trim).collect();
        let doc = Predicate::parse_doc_block(&lines, &tag_re, &mut []);

        Some(ModuleDoc { title, doc })
    }
}

// ============================================================================
// UNIT TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mode_from_str() {
        assert_eq!("+".parse::<Mode>(), Ok(Mode::Input));
        assert_eq!("-".parse::<Mode>(), Ok(Mode::Output));
        assert_eq!("?".parse::<Mode>(), Ok(Mode::InputOutput));
        assert_eq!("@".parse::<Mode>(), Ok(Mode::ReadOnly));
        assert!("invalid".parse::<Mode>().is_err());
    }

    #[test]
    fn test_type_from_str() {
        assert_eq!("int".parse::<Type>(), Ok(Type::Integer));
        assert_eq!("integer".parse::<Type>(), Ok(Type::Integer));
        assert_eq!("float".parse::<Type>(), Ok(Type::Float));
        assert_eq!("number".parse::<Type>(), Ok(Type::Float));
        assert_eq!("atom".parse::<Type>(), Ok(Type::Atom));
        assert_eq!("list".parse::<Type>(), Ok(Type::List));
        assert_eq!("any".parse::<Type>(), Ok(Type::Any));

        // Custom types
        assert_eq!(
            "my_custom_type".parse::<Type>(),
            Ok(Type::Custom("my_custom_type".to_string()))
        );
    }

    #[test]
    fn test_determinism_from_str() {
        assert_eq!("det".parse::<Determinism>(), Ok(Determinism::Det));
        assert_eq!("semidet".parse::<Determinism>(), Ok(Determinism::Semidet));
        assert_eq!("nondet".parse::<Determinism>(), Ok(Determinism::Nondet));
        assert_eq!("multi".parse::<Determinism>(), Ok(Determinism::Multi));
        assert_eq!("failure".parse::<Determinism>(), Ok(Determinism::Failure));
        assert_eq!(
            "undefined".parse::<Determinism>(),
            Ok(Determinism::Undefined)
        );
        assert!("bogus".parse::<Determinism>().is_err());
    }

    #[test]
    fn test_parse_basic_pldoc() {
        let code = "%! length(+List:list, -Length:int) is det.";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "length");
        assert_eq!(res[0].determinism, Some(Determinism::Det));
        assert_eq!(
            res[0].arguments,
            vec![
                Argument {
                    name: "List".to_string(),
                    mode: Some(Mode::Input),
                    param_type: Type::List,
                    description: None,
                },
                Argument {
                    name: "Length".to_string(),
                    mode: Some(Mode::Output),
                    param_type: Type::Integer,
                    description: None,
                }
            ]
        );
    }

    #[test]
    fn test_parse_predicate_zero_args() {
        let code = "%! true_pred()";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "true_pred");
        assert!(res[0].arguments.is_empty());
        assert_eq!(res[0].determinism, None);
    }

    #[test]
    fn test_parse_all_modes_and_unspecified_types() {
        let code = "%! mix_modes(+In, -Out, ?InOut, @Read)";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(
            res[0].arguments,
            vec![
                Argument {
                    name: "In".to_string(),
                    mode: Some(Mode::Input),
                    param_type: Type::Any,
                    description: None,
                },
                Argument {
                    name: "Out".to_string(),
                    mode: Some(Mode::Output),
                    param_type: Type::Any,
                    description: None,
                },
                Argument {
                    name: "InOut".to_string(),
                    mode: Some(Mode::InputOutput),
                    param_type: Type::Any,
                    description: None,
                },
                Argument {
                    name: "Read".to_string(),
                    mode: Some(Mode::ReadOnly),
                    param_type: Type::Any,
                    description: None,
                }
            ]
        );
    }

    #[test]
    fn test_ignore_false_positives_in_strings() {
        let code = r#"
%! valid(+Arg:atom)
valid(Arg) :-
    format("Not a docstring %! fake(+X:int)"),
    assertz(data("%! another_fake()")).
"#;
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "valid");
    }

    #[test]
    fn test_whitespace_and_formatting_variations() {
        let code = "  %!   spaced_out  (  +  Var  :  atom   ,   -  Out  :  int  )  ";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].name, "spaced_out");
        assert_eq!(
            res[0].arguments,
            vec![
                Argument {
                    name: "Var".to_string(),
                    mode: Some(Mode::Input),
                    param_type: Type::Atom,
                    description: None,
                },
                Argument {
                    name: "Out".to_string(),
                    mode: Some(Mode::Output),
                    param_type: Type::Integer,
                    description: None,
                }
            ]
        );
    }

    #[test]
    fn test_custom_domain_types() {
        let code = "%! process_order(+Order:order_struct, -Receipt:json_dict)";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(
            res[0].arguments[0],
            Argument {
                name: "Order".to_string(),
                mode: Some(Mode::Input),
                param_type: Type::Custom("order_struct".to_string()),
                description: None,
            }
        );
        assert_eq!(
            res[0].arguments[1],
            Argument {
                name: "Receipt".to_string(),
                mode: Some(Mode::Output),
                param_type: Type::Custom("json_dict".to_string()),
                description: None,
            }
        );
    }

    #[test]
    fn test_multiple_predicates_in_one_file() {
        let code = r#"
%! pred_one(+X:int)
pred_one(X).

% Standard Prolog comment
% pred_one is great!

%! pred_two(-Y:atom)
pred_two(Y).
"#;
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 2);
        assert_eq!(res[0].name, "pred_one");
        assert_eq!(res[1].name, "pred_two");
    }

    #[test]
    fn test_parse_summary_and_description() {
        let code = r#"
%! length(+List:list, -Length:int) is det.
%
%   True if Length is the number of elements in List.
%
%   This predicate is deterministic and works on proper lists
%   only.
length(List, Length).
"#;
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(
            res[0].doc.summary.as_deref(),
            Some("True if Length is the number of elements in List.")
        );
        assert_eq!(
            res[0].doc.description.as_deref(),
            Some("This predicate is deterministic and works on proper lists only.")
        );
    }

    #[test]
    fn test_parse_argument_tags() {
        let code = r#"
%! my_length(+List:list, -Length:int) is det.
%
%   Computes the length of a list.
%
%   @arg List The input list.
%   @param Length The resulting length.
%   @see length/2
%   @deprecated Use length/2 instead.
%   @author Jane Doe
%   @error type_error(list, List)
my_length(List, Length).
"#;
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        let predicate = &res[0];
        assert_eq!(
            predicate.doc.summary.as_deref(),
            Some("Computes the length of a list.")
        );
        assert_eq!(
            predicate.arguments[0].description.as_deref(),
            Some("The input list.")
        );
        assert_eq!(
            predicate.arguments[1].description.as_deref(),
            Some("The resulting length.")
        );
        assert_eq!(predicate.doc.see_also, vec!["length/2".to_string()]);
        assert_eq!(
            predicate.doc.deprecated.as_deref(),
            Some("Use length/2 instead.")
        );
        assert_eq!(predicate.doc.authors, vec!["Jane Doe".to_string()]);
        assert_eq!(
            predicate.doc.errors,
            vec!["type_error(list, List)".to_string()]
        );
    }

    #[test]
    fn test_parse_deprecated_without_message() {
        let code = r#"
%! old_pred(+X:int)
%
%   @deprecated
old_pred(X).
"#;
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].doc.deprecated.as_deref(), Some(""));
    }

    #[test]
    fn test_undocumented_predicate_has_empty_doc() {
        let code = "%! bare(+X:int)";
        let res = Predicate::parse(code);

        assert_eq!(res.len(), 1);
        assert_eq!(res[0].doc, Doc::default());
    }

    #[test]
    fn test_parse_module_doc() {
        let code = r#"
/** <module> Association Knowledge Base

This module tracks club membership and role history.

It supports point-in-time queries.

@author Jane Doe
@license MIT
*/

:- use_module(library(lists)).

%! role(-Role:atom)
role(member).
"#;
        let module_doc = ModuleDoc::parse(code).expect("module doc should be found");

        assert_eq!(
            module_doc.title.as_deref(),
            Some("Association Knowledge Base")
        );
        assert_eq!(
            module_doc.doc.summary.as_deref(),
            Some("This module tracks club membership and role history.")
        );
        assert_eq!(
            module_doc.doc.description.as_deref(),
            Some("It supports point-in-time queries.")
        );
        assert_eq!(module_doc.doc.authors, vec!["Jane Doe".to_string()]);
        assert_eq!(module_doc.doc.license.as_deref(), Some("MIT"));
    }

    #[test]
    fn test_parse_module_doc_title_only() {
        let code = "/** <module> Just A Title */\nfoo(bar).";
        let module_doc = ModuleDoc::parse(code).expect("module doc should be found");

        assert_eq!(module_doc.title.as_deref(), Some("Just A Title"));
        assert_eq!(module_doc.doc, Doc::default());
    }

    #[test]
    fn test_no_module_doc_returns_none() {
        let code = "%! role(-Role:atom)\nrole(member).";
        assert_eq!(ModuleDoc::parse(code), None);
    }

    #[test]
    fn test_doc_render_combines_summary_description_and_tags() {
        let doc = Doc {
            summary: Some("Summary sentence.".to_string()),
            description: Some("Extra detail.".to_string()),
            authors: vec!["Jane Doe".to_string()],
            ..Doc::default()
        };

        let rendered = doc.render().expect("non-empty doc should render");
        assert!(rendered.contains("Summary sentence."));
        assert!(rendered.contains("Extra detail."));
        assert!(rendered.contains("**Author(s):** Jane Doe"));
    }

    #[test]
    fn test_doc_render_empty_doc_is_none() {
        assert_eq!(Doc::default().render(), None);
    }
}
