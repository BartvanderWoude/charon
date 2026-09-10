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

/// The declared type of a predicate argument.
///
/// The distinction between `Atom` and `String` is what removes the need for knowledge bases to
/// normalise names by hand: a JSON string becomes a Prolog atom (`'aspirin'`) or a Prolog string
/// (`"aspirin"`) according to what was declared, rather than according to whether it arrived as a
/// scalar or inside a list. `List` carries its element type for the same reason — `list(atom)` is
/// what makes `["aspirin"]` unify with an ordinary `drug(aspirin, …)` fact.
#[derive(Debug, PartialEq, Eq, Clone)]
pub enum Type {
    Any,
    Atom,
    /// A Prolog string, i.e. a list of characters under scryer's default `double_quotes` flag.
    Text,
    Integer,
    Float,
    Boolean,
    /// A list, with the type of its elements. Bare `list` parses as `List(Box::new(Type::Any))`.
    List(Box<Type>),
    Compound,
    Stream,
    Custom(String),
}

impl FromStr for Type {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let s = s.trim();
        // A parameterised type: `list(atom)`, `list(list(int))`, ...
        if let Some(open) = s.find('(')
            && let Some(inner) = s.strip_suffix(')')
        {
            let (constructor, _) = s.split_at(open);
            let inner = &inner[open + 1..];
            return match constructor.trim().to_lowercase().as_str() {
                "list" => Ok(Type::List(Box::new(inner.parse()?))),
                _ => Ok(Type::Custom(s.to_string())),
            };
        }

        match s.to_lowercase().as_str() {
            "any" => Ok(Type::Any),
            "atom" => Ok(Type::Atom),
            "string" | "text" => Ok(Type::Text),
            "int" | "integer" => Ok(Type::Integer),
            "float" | "number" => Ok(Type::Float),
            "bool" | "boolean" => Ok(Type::Boolean),
            "list" => Ok(Type::List(Box::new(Type::Any))),
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
            Type::Text => write!(f, "string"),
            Type::Integer => write!(f, "int"),
            Type::Float => write!(f, "float"),
            Type::Boolean => write!(f, "boolean"),
            Type::List(element) if **element == Type::Any => write!(f, "list"),
            Type::List(element) => write!(f, "list({element})"),
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

/// A `@collection <name> key(<Arg>, ...)` tag.
///
/// Declares that an enumerating predicate is a *table* rather than an operation, and names the
/// arguments that identify a row. Charon derives the item routes and the MCP resources from it,
/// so one line turns `book(-Title)` into a collection addressable per member on both interfaces.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct CollectionTag {
    /// What to call the collection, e.g. `books`.
    pub name: String,
    /// The argument names that identify one row, in the order they appear in a path.
    pub key: Vec<String>,
}

/// A `@domain <Argument> <source>` tag: where an argument's admissible values come from.
///
/// The source is either a collection's name or a `name/arity` predicate indicator. This is what
/// lets Charon answer MCP argument completion from the knowledge base itself, and what turns a
/// bounded domain into a JSON Schema `enum` on both interfaces.
#[derive(Debug, PartialEq, Eq, Clone)]
pub struct DomainTag {
    /// The argument whose values are constrained.
    pub argument: String,
    /// The source of the values.
    pub source: DomainSource,
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub enum DomainSource {
    /// A collection declared elsewhere with `@collection`; its key supplies the values.
    Collection(String),
    /// A predicate, whose first argument supplies the values.
    Predicate { name: String, arity: usize },
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
    /// The `@collection` tag, if this predicate declares itself a table.
    pub collection: Option<CollectionTag>,
    /// `@domain` tags, one per constrained argument.
    pub domains: Vec<DomainTag>,
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

/// The three things a `%!` mode line states.
struct Signature {
    name: String,
    arguments: Vec<Argument>,
    determinism: Option<Determinism>,
}

/// Parses a `@collection` tag's content: `books key(Title)`.
///
/// Hand-written for the same reason [`parse_mode_line`] is: `key\(([^)]*)\)` stops at the first
/// closing parenthesis the moment a key argument nests, and the failure mode of a regex here is
/// a silently mangled key rather than an error.
fn parse_collection_tag(content: &str) -> Option<CollectionTag> {
    let content = content.trim();
    let name_end = content
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(content.len());
    let (name, rest) = content.split_at(name_end);
    if name.is_empty() || !name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_') {
        return None;
    }

    let rest = rest.trim_start();
    let after_open = rest.strip_prefix("key(")?;
    let close = matching_paren(after_open)?;
    if !after_open[close + 1..].trim().is_empty() {
        return None;
    }

    let key: Vec<String> = after_open[..close]
        .split(',')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    // A collection with no key has no addressable members, which is the whole point of one.
    (!key.is_empty()).then_some(CollectionTag {
        name: name.to_string(),
        key,
    })
}

/// Parses a `@domain` tag's content: `Title books`, or `Title book/1`.
fn parse_domain_tag(content: &str) -> Option<DomainTag> {
    let (argument, source) = content.trim().split_once(char::is_whitespace)?;
    let argument = argument.trim();
    let source = source.trim();
    if argument.is_empty() || source.is_empty() {
        return None;
    }

    let source = match source.split_once('/') {
        Some((name, arity)) => DomainSource::Predicate {
            name: name.trim().to_string(),
            arity: arity.trim().parse().ok()?,
        },
        None => DomainSource::Collection(source.to_string()),
    };
    Some(DomainTag {
        argument: argument.to_string(),
        source,
    })
}

/// Parses a PlDoc mode line: `%! name(+In:type, -Out:list(atom)) is det.`
///
/// This is a scanner rather than a regular expression because argument types nest: a pattern
/// like `\(([^)]*)\)` stops at the first closing parenthesis, so it truncates the argument list
/// of `%! f(+Xs:list(atom))` and silently mangles the whole line. The parenthesised argument
/// list is optional, so `%! ready is det.` documents a zero-arity predicate.
fn parse_mode_line(line: &str) -> Option<Signature> {
    let rest = line.trim_start().strip_prefix("%!")?;
    let rest = rest.strip_prefix(char::is_whitespace)?.trim_start();

    let name_end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    let (name, mut rest) = rest.split_at(name_end);
    if name.is_empty() {
        return None;
    }
    rest = rest.trim_start();

    let mut arguments = Vec::new();
    if let Some(after_open) = rest.strip_prefix('(') {
        let close = matching_paren(after_open)?;
        arguments = split_arguments(&after_open[..close])
            .into_iter()
            .map(parse_argument)
            .collect();
        rest = after_open[close + 1..].trim_start();
    }

    // `is <determinism>`, then an optional trailing full stop, then nothing else.
    let determinism = match rest.strip_prefix("is ") {
        Some(tail) => {
            let tail = tail.trim_start();
            let word_end = tail
                .find(|c: char| !c.is_ascii_alphabetic())
                .unwrap_or(tail.len());
            let (word, tail) = tail.split_at(word_end);
            rest = tail.trim_start();
            word.parse().ok()
        }
        None => None,
    };

    if !rest.trim_end_matches('.').trim().is_empty() {
        return None;
    }

    Some(Signature {
        name: name.to_string(),
        arguments,
        determinism,
    })
}

/// The index of the `)` closing a parenthesised group whose opening `(` has already been
/// consumed, or `None` if it is never closed.
fn matching_paren(text: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (index, character) in text.char_indices() {
        match character {
            '(' => depth += 1,
            ')' if depth == 0 => return Some(index),
            ')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// Splits an argument list on the commas that separate arguments, ignoring commas nested inside
/// a parameterised type such as `pair(int,atom)`.
fn split_arguments(list: &str) -> Vec<&str> {
    let mut arguments = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, character) in list.char_indices() {
        match character {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                arguments.push(&list[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    arguments.push(&list[start..]);
    arguments
        .into_iter()
        .map(str::trim)
        .filter(|argument| !argument.is_empty())
        .collect()
}

/// Parses one argument: an optional mode sigil, a name, and an optional `:type`.
fn parse_argument(argument: &str) -> Argument {
    let argument = argument.trim();
    let (mode, rest) = match argument.chars().next() {
        Some(sigil) if "+-?@".contains(sigil) => (
            sigil.to_string().parse::<Mode>().ok(),
            argument[sigil.len_utf8()..].trim_start(),
        ),
        _ => (None, argument),
    };

    let (name, param_type) = match rest.split_once(':') {
        Some((name, declared)) => (
            name.trim(),
            declared.trim().parse::<Type>().unwrap_or(Type::Any),
        ),
        None => (rest.trim(), Type::Any),
    };

    Argument {
        name: name.to_string(),
        mode,
        param_type,
        description: None,
    }
}

impl Predicate {
    /// Every predicate the source documents with a `%!` PlDoc comment, in file order.
    pub fn parse(source_code: &str) -> Vec<Predicate> {
        // Matches a "@tag rest-of-line" doc tag.
        let tag_re = Regex::new(r"^@(\w+)(?:\s+(.*))?$").unwrap();

        let lines: Vec<&str> = source_code.lines().collect();
        let mut predicates = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let Some(signature) = parse_mode_line(lines[i]) else {
                i += 1;
                continue;
            };

            let Signature {
                name,
                mut arguments,
                determinism,
            } = signature;

            i += 1;
            // PlDoc allows several mode lines (one per calling pattern) to document one
            // predicate; skip any further ones directly below the first.
            while i < lines.len() && parse_mode_line(lines[i]).is_some() {
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
                "collection" => match parse_collection_tag(&content) {
                    Some(collection) => doc.collection = Some(collection),
                    // Reported rather than silently dropped: a malformed tag that quietly does
                    // nothing is the same mystery as a missing endpoint.
                    None => tracing::warn!(
                        tag = %content,
                        "ignoring a malformed @collection tag; expected `name key(Arg, ...)`"
                    ),
                },
                "domain" => match parse_domain_tag(&content) {
                    Some(domain) => doc.domains.push(domain),
                    None => tracing::warn!(
                        tag = %content,
                        "ignoring a malformed @domain tag; expected `Argument collection` or \
                         `Argument name/arity`"
                    ),
                },
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
    /// The file-level `/** <module> … */` comment, if the source has one.
    pub fn parse(source_code: &str) -> Option<ModuleDoc> {
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
        assert_eq!("list".parse::<Type>(), Ok(Type::List(Box::new(Type::Any))));
        assert_eq!(
            "list(atom)".parse::<Type>(),
            Ok(Type::List(Box::new(Type::Atom)))
        );
        assert_eq!("string".parse::<Type>(), Ok(Type::Text));
        assert_eq!("boolean".parse::<Type>(), Ok(Type::Boolean));
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
                    param_type: Type::List(Box::new(Type::Any)),
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

    #[test]
    fn a_collection_tag_names_a_table_and_its_key() {
        let source = "%! consent_module(-Module:atom) is multi.
%
%   The consent modules.
%
%   @collection consent_modules key(Module)
%   @arg Module The module's short name.
consent_module(research).
";
        let predicates = Predicate::parse(source);
        let collection = predicates[0]
            .doc
            .collection
            .as_ref()
            .expect("the tag should be parsed");
        assert_eq!(collection.name, "consent_modules");
        assert_eq!(collection.key, vec!["Module".to_string()]);
    }

    #[test]
    fn a_collection_key_may_have_several_columns() {
        assert_eq!(
            parse_collection_tag("criteria key(Substance, Kind)"),
            Some(CollectionTag {
                name: "criteria".to_string(),
                key: vec!["Substance".to_string(), "Kind".to_string()],
            })
        );
    }

    #[test]
    fn a_malformed_collection_tag_is_rejected_rather_than_half_read() {
        // A half-read key would address the wrong member, which is worse than no collection.
        assert_eq!(parse_collection_tag("criteria"), None);
        assert_eq!(parse_collection_tag("criteria key()"), None);
        assert_eq!(parse_collection_tag("criteria key(Substance"), None);
        assert_eq!(parse_collection_tag("criteria key(Substance) extra"), None);
        assert_eq!(parse_collection_tag(""), None);
    }

    #[test]
    fn a_domain_tag_names_either_a_collection_or_a_predicate() {
        assert_eq!(
            parse_domain_tag("Module consent_modules"),
            Some(DomainTag {
                argument: "Module".to_string(),
                source: DomainSource::Collection("consent_modules".to_string()),
            })
        );
        assert_eq!(
            parse_domain_tag("Color color/1"),
            Some(DomainTag {
                argument: "Color".to_string(),
                source: DomainSource::Predicate {
                    name: "color".to_string(),
                    arity: 1,
                },
            })
        );
        assert_eq!(parse_domain_tag("Color"), None);
        assert_eq!(parse_domain_tag("Color color/x"), None);
    }
}
