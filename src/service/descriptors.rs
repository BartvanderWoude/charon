//! The two descriptor documents Charon publishes: an OpenAPI 3.1 document and an MCP tool list.
//!
//! Both live here rather than in their transport adapters because both are pure functions of the
//! operation set — describing what can be called is core work, while serving it over a socket is
//! not. Keeping them together is also what makes it obvious that they are two renderings of one
//! thing: they share the schemas built by [`Operation`], so a change to how a solution is shaped
//! reaches both without being written twice.

use serde_json::{Map, Value, json};

use super::operation::Operation;

/// Renders the OpenAPI 3.1 document for `operations`.
///
/// Built as plain JSON rather than through a typed builder: everything here is discovered from a
/// `.pl` file at runtime, so a builder API buys no compile-time checking, and OpenAPI 3.1 is
/// itself JSON Schema — the same schemas the MCP descriptors use drop straight in.
pub fn openapi(operations: &[Operation], title: &str, description: &str) -> String {
    let mut paths: Map<String, Value> = Map::new();

    for operation in operations {
        let path = format!("/{}", operation.path());
        let entry = paths
            .entry(path)
            .or_insert_with(|| Value::Object(Map::new()));

        let mut item = json!({
            "operationId": operation.name(),
            "summary": operation.summary(),
            "responses": {
                "200": {
                    "description": response_description(operation),
                    "content": { "application/json": { "schema": operation.response_schema() } },
                },
                "400": error_response("A missing, unknown or invalid argument, or a Prolog exception."),
                "404": error_response("No predicate is exposed at this path."),
                "405": error_response("The path exists, but not for this method."),
            },
        });
        if let Some(description) = operation.description() {
            item["description"] = json!(description);
        }
        if operation.is_deprecated() {
            item["deprecated"] = json!(true);
        }

        if !operation.inputs().is_empty() {
            // Where the arguments go follows the method, exactly as the HTTP adapter reads it:
            // a body for the methods that conventionally carry one, the query string otherwise.
            if operation.verb().carries_a_body() {
                item["requestBody"] = json!({
                    "required": true,
                    "content": { "application/json": { "schema": operation.input_schema() } },
                });
            } else {
                item["parameters"] = json!(query_parameters(operation));
            }
        }

        entry[operation.verb().as_str()] = item;
    }

    let document = json!({
        "openapi": "3.1.0",
        "info": {
            "title": title,
            "description": description,
            "version": env!("CARGO_PKG_VERSION"),
        },
        "paths": paths,
    });
    serde_json::to_string_pretty(&document).expect("openapi document is serialisable")
}

fn query_parameters(operation: &Operation) -> Vec<Value> {
    operation
        .inputs()
        .iter()
        .map(|param| {
            let mut parameter = json!({
                "name": param.name,
                "in": "query",
                "required": true,
                "schema": param.schema(),
            });
            if let Some(description) = &param.description {
                parameter["description"] = json!(description);
            }
            parameter
        })
        .collect()
}

fn response_description(operation: &Operation) -> &'static str {
    if operation.is_singular() {
        "The predicate's solution, or null if it had none."
    } else {
        "An array with one entry per solution, empty if the query failed."
    }
}

fn error_response(description: &str) -> Value {
    json!({
        "description": description,
        "content": {
            "application/json": {
                "schema": {
                    "type": "object",
                    "properties": { "error": { "type": "string" } },
                    "required": ["error"],
                }
            }
        }
    })
}

/// Renders the MCP `tools/list` payload for `operations`.
///
/// A tool name is flat, so it is the predicate's own name — verb prefixes are *not* stripped.
/// Collapsing `get_example` and `put_example` onto one tool the way the HTTP router does would
/// lose the distinction entirely; instead the prefix becomes behavioural metadata, which is what
/// MCP clients actually want from it.
pub fn mcp_tools(operations: &[Operation]) -> String {
    let tools: Vec<Value> = operations.iter().map(tool).collect();
    serde_json::to_string(&json!({ "tools": tools })).expect("tool list is serialisable")
}

fn tool(operation: &Operation) -> Value {
    let mut description = operation.summary();
    if let Some(rest) = operation.description() {
        description.push_str("\n\n");
        description.push_str(&rest);
    }

    let verb = operation.verb();
    let mut annotations = json!({
        "title": operation.summary(),
        "readOnlyHint": verb.is_read_only(),
        "idempotentHint": verb.is_idempotent(),
        "destructiveHint": verb.is_destructive(),
        // Every answer comes from the local knowledge base; nothing reaches the network.
        "openWorldHint": false,
    });
    if operation.is_deprecated() {
        annotations["deprecated"] = json!(true);
    }

    json!({
        "name": operation.name(),
        "description": description,
        "inputSchema": operation.input_schema(),
        "outputSchema": {
            "type": "object",
            "properties": { "solutions": operation.response_schema() },
            "required": ["solutions"],
            "additionalProperties": false,
        },
        "annotations": annotations,
    })
}

/// The MCP `resources/list` payload: the knowledge base's own source, so a model can read the
/// rules it is calling rather than take the tool descriptions on faith.
pub fn mcp_resources() -> Value {
    json!({
        "resources": [{
            "uri": SOURCE_RESOURCE_URI,
            "name": "Knowledge base source",
            "description": "The Prolog source text this server was started with.",
            "mimeType": SOURCE_RESOURCE_MIME_TYPE,
        }]
    })
}

/// The one resource an MCP client can read.
pub const SOURCE_RESOURCE_URI: &str = "charon://source";
pub const SOURCE_RESOURCE_MIME_TYPE: &str = "text/x-prolog";
