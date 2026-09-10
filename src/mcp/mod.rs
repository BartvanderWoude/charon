//! The MCP adapter: a JSON-RPC 2.0 envelope around the same [`Service`] the HTTP interface uses.
//!
//! Hand-written rather than built on an SDK, because the tool list is discovered from a `.pl`
//! file at runtime and every macro-based router assumes tools known at compile time. The surface
//! is small: initialize, the tool list, a tool call, and the knowledge base source as a resource.
//!
//! Two transports share one dispatcher. Which of them a process speaks is chosen on the command
//! line: HTTP by default, stdio when a desktop client is launching the binary itself.

mod descriptors;
mod stdio;
mod transport;

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::service::{CallError, KeyError, Service, Verb};

pub use descriptors::{
    COLLECTION_RESOURCE_PREFIX, OVERVIEW_PROMPT, SOURCE_RESOURCE_MIME_TYPE, SOURCE_RESOURCE_URI,
    collection_uri, tools,
};
pub use stdio::serve_stdio;
pub use transport::{PROTOCOL_VERSION_HEADER, SESSION_HEADER, routes};

/// The dispatcher and what it caches: one per process, shared by whichever transport is serving.
pub struct Server {
    service: Arc<Service>,
    tools: descriptors::Cache,
}

impl Server {
    pub fn new(service: Arc<Service>) -> Arc<Self> {
        Arc::new(Server {
            service,
            tools: descriptors::Cache::default(),
        })
    }

    pub fn service(&self) -> &Arc<Service> {
        &self.service
    }
}

/// The MCP revision this server implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Every protocol revision this server knows how to speak, newest first. A client that asks for
/// one of these gets it back; anything else is answered with the newest, which is what the spec
/// asks a server to do rather than failing the handshake.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

/// Whether this server speaks `version`.
pub fn speaks(version: &str) -> bool {
    SUPPORTED_PROTOCOL_VERSIONS.contains(&version)
}

/// Standard JSON-RPC error codes, plus the ones MCP leans on.
mod code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL_ERROR: i32 = -32603;
}

/// Handles one parsed JSON-RPC message and produces the response.
///
/// `None` means "say nothing", which is the correct answer to a notification — a message with no
/// `id`, which by JSON-RPC convention must not be replied to. A batch (a JSON array) is answered
/// with a batch of the replies its requests earned, or with nothing if it held only
/// notifications.
pub async fn handle(server: &Arc<Server>, request: &Value) -> Option<Value> {
    match request {
        Value::Array(messages) if messages.is_empty() => Some(error_response(
            Value::Null,
            code::INVALID_REQUEST,
            "an empty batch is not a request".to_string(),
        )),
        Value::Array(messages) => {
            let mut replies = Vec::new();
            for message in messages {
                if let Some(reply) = handle_one(server, message).await {
                    replies.push(reply);
                }
            }
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        message => handle_one(server, message).await,
    }
}

async fn handle_one(server: &Arc<Server>, request: &Value) -> Option<Value> {
    // Notifications carry no id and, by JSON-RPC convention, expect no reply.
    let id = request.get("id").cloned()?;
    let Some(method) = request.get("method").and_then(Value::as_str) else {
        return Some(error_response(
            id,
            code::INVALID_REQUEST,
            "`method` is required".to_string(),
        ));
    };
    let params = request
        .get("params")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();

    let catalog = server.service.catalog();
    let result = match method {
        "initialize" => Ok(initialize(&catalog, &params)),
        "ping" => Ok(json!({})),
        // Served from the cache rather than rebuilt per call: clients ask at least once per
        // session, and the answer only changes when the catalog does.
        "tools/list" => Ok((*server.tools.tools(&catalog)).clone()),
        "tools/call" => tools_call(server, &params).await,
        "resources/list" => Ok(descriptors::resources(&catalog)),
        "resources/templates/list" => Ok(descriptors::resource_templates(&catalog)),
        "resources/read" => resources_read(server, &params).await,
        "prompts/list" => Ok(descriptors::prompts()),
        "prompts/get" => prompts_get(&catalog, &params),
        "completion/complete" => completion_complete(server, &params).await,
        // Accepted and acknowledged rather than refused: a client that sets a level and gets
        // `method not found` has to special-case this server. Charon's diagnostics go to stderr
        // via `tracing`, which is where a process's logs belong.
        "logging/setLevel" => Ok(json!({})),
        other => Err(RpcError {
            code: code::METHOD_NOT_FOUND,
            message: format!("method not found: {other}"),
        }),
    };

    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err(error) => error_response(id, error.code, error.message),
    })
}

fn error_response(id: Value, code: i32, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": { "code": code, "message": message },
    })
}

fn initialize(catalog: &crate::service::Catalog, params: &Map<String, Value>) -> Value {
    // Echo the client's revision when it is one this server speaks, so an older client is not
    // told to understand a newer protocol than it asked for.
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|asked| speaks(asked))
        .unwrap_or(PROTOCOL_VERSION);

    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
            "prompts": { "listChanged": false },
            "logging": {},
            // Argument values can be completed from the knowledge base itself, which is the one
            // thing a Prolog-backed server can do that a hand-written one cannot without
            // duplicating the data.
            "completions": {},
        },
        "serverInfo": {
            "name": catalog.title(),
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": catalog.description(),
    })
}

fn prompts_get(
    catalog: &crate::service::Catalog,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if name != OVERVIEW_PROMPT {
        return Err(RpcError {
            code: code::INVALID_PARAMS,
            message: format!("unknown prompt: {name}"),
        });
    }

    let text = descriptors::overview(catalog);
    Ok(json!({
        "description": "An orientation for this knowledge base.",
        "messages": [{
            "role": "user",
            "content": { "type": "text", "text": text },
        }],
    }))
}

async fn tools_call(server: &Arc<Server>, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let service = &server.service;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| RpcError::invalid_params("`name` is required"))?;
    let arguments = match params.get("arguments") {
        Some(Value::Object(arguments)) => arguments.clone(),
        None | Some(Value::Null) => Map::new(),
        Some(_) => return Err(RpcError::invalid_params("`arguments` must be an object")),
    };

    let catalog = service.catalog();
    // A tool name is the predicate's own name, verb prefix and all.
    if catalog.find(name).is_none() {
        return Err(RpcError {
            code: code::INVALID_PARAMS,
            message: format!("unknown tool: {name}"),
        });
    }

    match Arc::clone(service)
        .call(catalog, name.to_string(), arguments)
        .await
    {
        Ok(outcome) => Ok(tool_result(&outcome.value, outcome.truncated, false)),
        // A Prolog exception, a bad argument, or an operation this server declines to run is a
        // *tool* failure rather than a protocol failure: the call itself was well-formed, so it
        // comes back as an error result the caller can read and react to rather than as a
        // JSON-RPC error. A caller told "restart charon with --persist" can relay that to a
        // person; `-32602 unknown tool` would just look like a broken server.
        Err(
            cause @ (CallError::Prolog(_) | CallError::Argument(_) | CallError::Withheld { .. }),
        ) => Ok(tool_result(&Value::String(cause.to_string()), false, true)),
        Err(cause @ CallError::Interrupted) => Err(RpcError {
            code: code::INTERNAL_ERROR,
            message: cause.to_string(),
        }),
        Err(cause) => Err(RpcError {
            code: code::INVALID_PARAMS,
            message: cause.to_string(),
        }),
    }
}

fn tool_result(value: &Value, truncated: bool, is_error: bool) -> Value {
    let mut text = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    // Said in the prose as well as in the structured half: a caller that reads only the text
    // block would otherwise present a capped answer as if it were the whole of it.
    if truncated {
        text.push_str("\n\n(Truncated: the solution cap was reached, so there are more solutions than these.)");
    }

    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    if !is_error {
        // Also handed over structured, so a client that wants the data rather than the prose
        // does not have to re-parse the text block.
        result["structuredContent"] = json!({ "solutions": value, "truncated": truncated });
    }
    result
}

async fn resources_read(
    server: &Arc<Server>,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    let service = &server.service;
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();

    if uri == SOURCE_RESOURCE_URI {
        return Ok(contents(
            &uri,
            SOURCE_RESOURCE_MIME_TYPE,
            service.catalog().source().to_string(),
        ));
    }

    let Some(rest) = uri.strip_prefix(COLLECTION_RESOURCE_PREFIX) else {
        return Err(RpcError {
            code: code::INVALID_PARAMS,
            message: format!("unknown resource: {uri}"),
        });
    };

    // `charon://collection/<name>` is the whole table; anything after it identifies one member.
    let (name, key) = match rest.split_once('/') {
        Some((name, key)) => (name, Some(key)),
        None => (rest, None),
    };

    let catalog = service.catalog();
    let collection = catalog.collection(name).ok_or_else(|| RpcError {
        code: code::INVALID_PARAMS,
        message: format!("unknown collection: {name}"),
    })?;

    // Reading a collection runs the same operation the HTTP route runs; there is no second way
    // to ask the knowledge base a question.
    let (operation_name, arguments) = match key {
        None => (collection.list_operation(), Map::new()),
        // A key in a URI is text, read exactly as a URL path segment is, so the two cannot
        // disagree about whether `null` is an atom or a JSON null.
        Some(key) => {
            let arguments = collection.key_arguments(key).map_err(|cause| match cause {
                KeyError::WrongLength { .. } => RpcError::invalid_params(
                    "the number of key segments does not match this collection's key",
                ),
                cause => RpcError {
                    code: code::INVALID_PARAMS,
                    message: cause.to_string(),
                },
            })?;
            (
                collection.item_operation(Verb::Get).unwrap_or_default(),
                arguments,
            )
        }
    };

    let outcome = Arc::clone(service)
        .call(Arc::clone(&catalog), operation_name, arguments)
        .await
        .map_err(|cause| RpcError {
            code: code::INTERNAL_ERROR,
            message: cause.to_string(),
        })?;

    let text =
        serde_json::to_string_pretty(&outcome.value).unwrap_or_else(|_| outcome.value.to_string());
    Ok(contents(&uri, "application/json", text))
}

/// Answers `completion/complete`: what values an argument could take.
///
/// This is the one thing a knowledge-base-backed MCP server can do that a hand-written one
/// cannot without duplicating the data — the admissible values of an argument are whatever the
/// predicate behind its `@domain` currently says they are rather than a separately maintained
/// list.
async fn completion_complete(
    server: &Arc<Server>,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
    let service = &server.service;
    let empty = json!({ "completion": { "values": [], "total": 0, "hasMore": false } });

    let reference = params.get("ref").and_then(Value::as_object);
    let kind = reference
        .and_then(|reference| reference.get("type"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    // Prompts here take no arguments, so there is nothing to complete for them.
    if kind != "ref/tool" {
        return Ok(empty);
    }
    let Some(tool) = reference
        .and_then(|reference| reference.get("name"))
        .and_then(Value::as_str)
    else {
        return Err(RpcError::invalid_params("`ref.name` is required"));
    };

    let argument = params.get("argument").and_then(Value::as_object);
    let Some(name) = argument
        .and_then(|argument| argument.get("name"))
        .and_then(Value::as_str)
    else {
        return Err(RpcError::invalid_params("`argument.name` is required"));
    };
    let prefix = argument
        .and_then(|argument| argument.get("value"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let catalog = service.catalog();
    // An argument with no declared domain is not an error — most have none — so the honest
    // answer is an empty completion rather than a failure.
    let Some(domain) = catalog
        .find(tool)
        .filter(|operation| catalog.is_available(operation))
        .and_then(|operation| operation.input(name))
        .and_then(|param| param.domain())
        .cloned()
    else {
        return Ok(empty);
    };

    let values = Arc::clone(service)
        .domain_values(Arc::clone(&catalog), domain)
        .await;
    let matching: Vec<Value> = values
        .into_iter()
        .filter(|value| match value.as_str() {
            Some(text) => text.starts_with(prefix),
            // A non-string value is matched on how it would be written.
            None => value.to_string().starts_with(prefix),
        })
        .collect();

    // The spec caps a completion page at 100 and asks the server to say whether it held back.
    let total = matching.len();
    let page: Vec<Value> = matching.into_iter().take(MAX_COMPLETIONS).collect();
    Ok(json!({
        "completion": {
            "values": page,
            "total": total,
            "hasMore": total > MAX_COMPLETIONS,
        }
    }))
}

/// The most completion values one response carries, as the MCP specification requires.
const MAX_COMPLETIONS: usize = 100;

fn contents(uri: &str, mime_type: &str, text: String) -> Value {
    json!({
        "contents": [{ "uri": uri, "mimeType": mime_type, "text": text }]
    })
}

#[derive(Debug)]
struct RpcError {
    code: i32,
    message: String,
}

impl RpcError {
    fn invalid_params(message: &str) -> Self {
        RpcError {
            code: code::INVALID_PARAMS,
            message: message.to_string(),
        }
    }
}

/// The reply to a message that could not be parsed as JSON at all.
fn parse_error() -> Value {
    error_response(Value::Null, code::PARSE_ERROR, "parse error".to_string())
}
