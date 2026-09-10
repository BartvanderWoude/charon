//! The OpenAPI 3.1 document, and the cache that serves it.
//!
//! Built as plain JSON rather than through a typed builder: everything in it is discovered from
//! a `.pl` file at runtime, so a builder API buys no compile-time checking, and OpenAPI 3.1 is
//! itself JSON Schema — the schemas an [`Operation`] already carries drop straight in.

use parking_lot::RwLock;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::service::{Catalog, Operation, Origin, Param, Verb};

/// The document for everything `catalog` exposes, rendered fresh.
pub fn document(catalog: &Catalog) -> String {
    let mut paths: Map<String, Value> = Map::new();
    let mut tags: BTreeMap<String, String> = BTreeMap::new();

    for operation in catalog.available() {
        let entry = paths
            .entry(format!("/{}", operation.path()))
            .or_insert_with(|| Value::Object(Map::new()));

        let tag = match operation.origin() {
            Origin::Predicate => "predicates".to_string(),
            Origin::Collection(name) => name.clone(),
        };
        tags.entry(tag.clone())
            .or_insert_with(|| match operation.origin() {
                Origin::Predicate => "Documented predicates, called directly.".to_string(),
                Origin::Collection(name) => format!("The `{name}` collection."),
            });

        let mut item = json!({
            "operationId": operation.name(),
            "summary": operation.summary(),
            "tags": [tag],
            "responses": responses(operation),
        });
        if let Some(description) = operation.description() {
            item["description"] = json!(description);
        }
        if operation.is_deprecated() {
            item["deprecated"] = json!(true);
        }

        let parameters = parameters(operation);
        if !parameters.is_empty() {
            item["parameters"] = json!(parameters);
        }
        if let Some(body) = request_body(operation) {
            item["requestBody"] = body;
        }

        entry[operation.verb().as_str()] = item;
    }

    let document = json!({
        "openapi": "3.1.0",
        "info": {
            "title": catalog.title(),
            "description": catalog.description(),
            "version": env!("CARGO_PKG_VERSION"),
        },
        // Relative to wherever this document was fetched from: the bind address is a property of
        // how the process was started rather than of the knowledge base the document describes.
        "servers": [{ "url": "/" }],
        "tags": tags
            .into_iter()
            .map(|(name, description)| json!({ "name": name, "description": description }))
            .collect::<Vec<_>>(),
        "paths": paths,
        "components": { "schemas": { "Problem": problem_schema() } },
    });
    serde_json::to_string_pretty(&document).expect("openapi document is serialisable")
}

/// The rendered document and the `ETag` that identifies it.
#[derive(Clone)]
pub struct Document {
    pub json: Arc<str>,
    pub etag: String,
}

/// Holds the document for the catalog revision it was rendered from.
///
/// A reload moves the revision on, which is what invalidates this: the document is a pure
/// function of the catalog, so nothing else can make it stale.
#[derive(Default)]
pub struct Cache(RwLock<Option<(u64, Document)>>);

impl Cache {
    pub fn get(&self, catalog: &Catalog) -> Document {
        let revision = catalog.revision();
        if let Some((cached, document)) = self.0.read().as_ref()
            && *cached == revision
        {
            return document.clone();
        }

        let json: Arc<str> = document(catalog).into();
        // Hashed over the rendered document rather than the source, so that a Charon upgrade
        // changing how documents are rendered also invalidates a cached copy.
        let etag = format!("\"{:x}\"", fnv1a(&json));
        let rendered = Document { json, etag };
        *self.0.write() = Some((revision, rendered.clone()));
        rendered
    }
}

/// Whether an argument for this method is conventionally carried in a request body rather than
/// in the query string.
///
/// This only decides where an argument is *documented*; the adapter accepts both for every
/// method, so `curl -G` and `curl --json` are not gratuitously different.
fn carries_a_body(verb: Verb) -> bool {
    matches!(verb, Verb::Put | Verb::Post | Verb::Patch)
}

/// Where each argument goes: the ones naming a member are in the path, and of the rest, a body
/// for the methods that conventionally carry one and the query string otherwise.
fn parameters(operation: &Operation) -> Vec<Value> {
    operation
        .inputs()
        .filter_map(|param| {
            let in_path = operation.path_parameters().contains(&param.name);
            if !in_path && carries_a_body(operation.verb()) {
                return None;
            }
            let mut parameter = json!({
                "name": param.name,
                "in": if in_path { "path" } else { "query" },
                "required": true,
                "schema": param.schema,
            });
            if let Some(description) = &param.description {
                parameter["description"] = json!(description);
            }
            Some(parameter)
        })
        .collect()
}

fn request_body(operation: &Operation) -> Option<Value> {
    if !carries_a_body(operation.verb()) {
        return None;
    }
    let body: Vec<&Param> = operation
        .inputs()
        .filter(|param| !operation.path_parameters().contains(&param.name))
        .collect();
    if body.is_empty() {
        return None;
    }

    let properties: Map<String, Value> = body
        .iter()
        .map(|param| (param.name.clone(), param.schema.clone()))
        .collect();
    let required: Vec<&String> = properties.keys().collect();
    Some(json!({
        "required": true,
        "content": {
            "application/json": {
                "schema": {
                    "type": "object",
                    "properties": properties,
                    "required": required,
                    "additionalProperties": false,
                }
            }
        },
    }))
}

fn responses(operation: &Operation) -> Value {
    let description = match operation.is_singular() {
        true => "The predicate's solution, or null if it had none.",
        false => "An array with one entry per solution, empty if the query failed.",
    };
    let mut responses = json!({
        "200": {
            "description": description,
            "content": { "application/json": { "schema": operation.response_schema() } },
            "headers": {
                "X-Charon-Truncated": {
                    "description": "Present when the solution cap cut the answer short.",
                    "schema": { "type": "boolean" },
                },
            },
        },
        "400": error_response("The request could not be read."),
        "403": error_response(
            "The operation changes the knowledge base and this server was started without \
             `--persist`.",
        ),
        "404": error_response("No predicate or collection member is exposed here."),
        "405": error_response("The path exists, but not for this method."),
        "422": error_response(&refused_description(operation)),
        "500": error_response("The knowledge base is defective — it called something undefined."),
    });
    if operation.verb() == Verb::Get {
        responses["304"] = json!({ "description": "The descriptor has not changed." });
    }
    responses
}

/// The 422 description, extended with whatever the predicate's `@throws`/`@error` tags promised.
fn refused_description(operation: &Operation) -> String {
    let mut description =
        "A missing, unknown or invalid argument, or an exception the rules raised about this \
         request."
            .to_string();
    let errors = &operation.docs().errors;
    if !errors.is_empty() {
        description.push_str("\n\nThe knowledge base documents these conditions: ");
        description.push_str(&errors.join("; "));
        description.push('.');
    }
    description
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/problem+json": {
                "schema": { "$ref": "#/components/schemas/Problem" }
            }
        }
    })
}

/// RFC 9457 problem details, as JSON Schema. Charon's own extension members are named but not
/// required, since which ones appear depends on what went wrong.
fn problem_schema() -> Value {
    json!({
        "type": "object",
        "description": "An error, as RFC 9457 problem details.",
        "properties": {
            "type": { "type": "string", "description": "A stable URN identifying the problem." },
            "title": { "type": "string" },
            "status": { "type": "integer" },
            "detail": { "type": "string" },
            "argument": { "type": "string" },
            "arguments": { "type": "array", "items": { "type": "string" } },
            "operation": { "type": "string" },
            "allowed": { "type": "string" },
            "remedy": { "type": "string" },
            "path": { "type": "string" },
            "exception": { "description": "The Prolog exception term, when there was one." },
        },
        "required": ["type", "title", "status", "detail"],
    })
}

/// FNV-1a, used only to give the document a stable `ETag`.
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x1000_0000_01b3);
    }
    hash
}
