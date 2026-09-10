//! Collections: an enumerating predicate exposed as a table rather than as a call.
//!
//! A predicate whose arguments are all outputs is data rather than an action — `tag(-Name)` is a
//! set, `book(-Title, -Author, -Year)` is a table. Both protocols Charon speaks have a
//! first-class way to express data: HTTP has sub-resources, MCP has resources. One `@collection`
//! tag reaches both:
//!
//! ```prolog
//! %! book(-Title:atom) is multi.
//! %
//! %   @collection books key(Title)
//! ```
//!
//! yields `GET /books`, `GET|PUT|DELETE /books/{Title}`, and the MCP resources
//! `charon://collection/books` and `.../{Title}`. The write half exists only when the predicate
//! is `dynamic`; whether it is *exposed* is then the ordinary rule for changing operations,
//! since PUT and DELETE are changes like any other.
//!
//! The table is what the predicate is exposed *as*: `book` is not also a route or a tool of its
//! own, since the list route asks the same goal under the name the tag chose. That name may be
//! the predicate's own — `@collection book key(Title)` is a table at `/book`.

use std::fmt;

use serde_json::{Map, Value};

use crate::prolog::{CollectionTag, Determinism, Type};

use super::coerce;
use super::operation::{GoalShape, Operation, Route, Verb};

/// One column of a collection's rows.
#[derive(Debug, Clone)]
pub struct Column {
    pub name: String,
    pub declared_type: Type,
}

/// One declared collection.
#[derive(Debug, Clone)]
pub struct Collection {
    name: String,
    predicate: String,
    summary: String,
    /// Every column, in the order the predicate declares them.
    columns: Vec<Column>,
    /// Which of those columns identify a row, in the order they appear in a path.
    key: Vec<usize>,
    /// Whether members can be added and removed, i.e. whether the predicate is `dynamic`.
    writable: bool,
}

impl Collection {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn predicate(&self) -> &str {
        &self.predicate
    }

    pub fn summary(&self) -> &str {
        &self.summary
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    /// The columns identifying a row, in path order.
    pub fn key(&self) -> impl ExactSizeIterator<Item = &Column> {
        self.key.iter().map(|index| &self.columns[*index])
    }

    /// Where in `columns` the single key column sits, or `None` if the key has several.
    ///
    /// A multi-column key has no one column to draw values from, so only a single-column
    /// collection can serve as a `@domain`.
    pub(super) fn sole_key_position(&self) -> Option<usize> {
        match self.key.as_slice() {
            [only] => Some(*only),
            _ => None,
        }
    }

    /// The name of the operation answering for the whole table.
    pub fn list_operation(&self) -> String {
        route_name(&self.name, "list")
    }

    /// The name of the operation answering `verb` on one member, if there is one.
    pub fn item_operation(&self, verb: Verb) -> Option<String> {
        match verb {
            Verb::Get => Some(route_name(&self.name, "item")),
            Verb::Put if self.writable => Some(route_name(&self.name, "put")),
            Verb::Delete if self.writable => Some(route_name(&self.name, "delete")),
            _ => None,
        }
    }

    /// The verbs a member responds to, for an `Allow` header.
    pub fn item_verbs(&self) -> Vec<Verb> {
        match self.writable {
            true => vec![Verb::Get, Verb::Put, Verb::Delete],
            false => vec![Verb::Get],
        }
    }

    /// Reads a member's key, as it appears in a URL path or a `charon://` URI, into the
    /// arguments the item routes expect.
    ///
    /// Both transports address a member by the same text, so both read it here: `/x/null` and
    /// `charon://collection/x/null` cannot disagree about whether that is an atom or a JSON null.
    pub fn key_arguments(&self, key: &str) -> Result<Map<String, Value>, KeyError> {
        let segments: Vec<&str> = key.split('/').collect();
        if segments.len() != self.key.len() {
            return Err(KeyError::WrongLength {
                expected: self.key.len(),
                found: segments.len(),
            });
        }

        let mut arguments = Map::new();
        for (column, raw) in self.key().zip(segments) {
            let value = coerce::value_from_text(&column.declared_type, raw).map_err(|message| {
                KeyError::Invalid {
                    argument: column.name.clone(),
                    message,
                }
            })?;
            arguments.insert(column.name.clone(), value);
        }
        Ok(arguments)
    }
}

/// The name a generated route answers to.
///
/// Formulaic rather than stored, and separated by `__` so that it cannot collide with an
/// ordinary predicate name — a collision would be a startup error rather than a shadowing, but
/// one the author could do nothing about.
fn route_name(collection: &str, role: &str) -> String {
    format!("{collection}__{role}")
}

/// Why a member's key could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyError {
    WrongLength { expected: usize, found: usize },
    Invalid { argument: String, message: String },
}

impl fmt::Display for KeyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyError::WrongLength { expected, found } => write!(
                f,
                "this collection is keyed on {expected} column(s), but {found} were given"
            ),
            KeyError::Invalid { argument, message } => write!(f, "`{argument}`: {message}"),
        }
    }
}

/// Builds a collection and its routes from the operation that enumerates it.
///
/// Every failure here is a startup error rather than a silently dropped tag: a `@collection`
/// that does nothing would be exactly the sort of mystery the rest of this codebase prevents.
pub fn build(
    source: &Operation,
    tag: &CollectionTag,
    dynamic: bool,
) -> Result<(Collection, Vec<Operation>), CollectionError> {
    if source.inputs().next().is_some() {
        return Err(CollectionError::NotEnumerable {
            collection: tag.name.clone(),
            predicate: source.name().to_string(),
        });
    }

    let columns: Vec<Column> = source
        .arguments()
        .iter()
        .map(|param| Column {
            name: param.name.clone(),
            declared_type: param.declared_type.clone(),
        })
        .collect();

    // The key columns, resolved against the predicate's arguments in the order the tag lists
    // them.
    let mut key: Vec<usize> = Vec::with_capacity(tag.key.len());
    for name in &tag.key {
        let position = columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| CollectionError::NoSuchKey {
                collection: tag.name.clone(),
                predicate: source.name().to_string(),
                argument: name.clone(),
            })?;
        if key.contains(&position) {
            return Err(CollectionError::RepeatedKey {
                collection: tag.name.clone(),
                argument: columns[position].name.clone(),
            });
        }
        key.push(position);
    }

    let key_names: Vec<String> = key
        .iter()
        .map(|index| columns[*index].name.clone())
        .collect();
    let all_names: Vec<String> = columns.iter().map(|column| column.name.clone()).collect();
    let item_path = {
        let segments: Vec<String> = key_names.iter().map(|name| format!("{{{name}}}")).collect();
        format!("{}/{}", tag.name, segments.join("/"))
    };

    let item_route = |verb| Route {
        verb,
        path: item_path.clone(),
        in_path: key_names.clone(),
    };
    let derive = |role: &str, route, shape, bound: &[String]| {
        source
            .bind(route_name(&tag.name, role), route, shape, bound)
            .owned_by(&tag.name)
    };

    let mut operations = vec![
        // The whole table: the same answers as calling the predicate directly, under the name
        // the author chose for the collection, so its declared determinism carries over.
        derive(
            "list",
            Route {
                verb: Verb::Get,
                path: tag.name.clone(),
                in_path: Vec::new(),
            },
            GoalShape::Call,
            &[],
        ),
        // One member: the key bound from the path, the rest of the row answered. `semidet`, so
        // a member that is not there answers `null`, which the adapters turn into a 404.
        derive("item", item_route(Verb::Get), GoalShape::Call, &key_names)
            .with_determinism(Determinism::Semidet),
    ];

    // The write half exists only if the facts can change at all: `assertz` against a predicate
    // that is not `dynamic` raises a permission error, so offering the route would be a promise
    // Charon cannot keep.
    if dynamic {
        // PUT takes every column — the key from the path, the rest from the body.
        operations.push(
            derive("put", item_route(Verb::Put), GoalShape::Assert, &all_names)
                .with_determinism(Determinism::Det),
        );
        // DELETE takes only the key; the other columns stay free so that `retract` matches the
        // row whatever it holds, and they come back as the row that was removed.
        operations.push(
            derive(
                "delete",
                item_route(Verb::Delete),
                GoalShape::Retract,
                &key_names,
            )
            .with_determinism(Determinism::Semidet),
        );
    }

    Ok((
        Collection {
            name: tag.name.clone(),
            predicate: source.predicate_name().to_string(),
            summary: source.summary(),
            columns,
            key,
            writable: dynamic,
        },
        operations,
    ))
}

/// Why a `@collection` declaration could not be turned into routes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CollectionError {
    NotEnumerable {
        collection: String,
        predicate: String,
    },
    NoSuchKey {
        collection: String,
        predicate: String,
        argument: String,
    },
    RepeatedKey {
        collection: String,
        argument: String,
    },
}

impl fmt::Display for CollectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CollectionError::NotEnumerable {
                collection,
                predicate,
            } => write!(
                f,
                "`{predicate}` declares the collection `{collection}` but takes input arguments; \
                 a collection is enumerated, so every argument must be an output (`-`)."
            ),
            CollectionError::NoSuchKey {
                collection,
                predicate,
                argument,
            } => write!(
                f,
                "the collection `{collection}` is keyed on `{argument}`, which is not an output \
                 of `{predicate}`."
            ),
            CollectionError::RepeatedKey {
                collection,
                argument,
            } => write!(
                f,
                "the collection `{collection}` names `{argument}` twice in its key."
            ),
        }
    }
}

impl std::error::Error for CollectionError {}
