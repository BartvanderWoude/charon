//! The MCP adapter: a JSON-RPC 2.0 envelope around the same [`Service`] the HTTP interface uses.
//!
//! Hand-written rather than built on an SDK, because the tool list is discovered from a `.pl`
//! file at runtime and every macro-based router assumes tools known at compile time. The surface
//! is small: initialize, the tool list, a tool call, and the knowledge base source as a resource.
//!
//! Two transports share one dispatcher. Which of them a process speaks is chosen on the command
//! line: HTTP by default, stdio when a desktop client is launching the binary itself.

mod stdio;
mod transport;

use std::sync::Arc;

use serde_json::{Map, Value, json};

use crate::service::{CallError, Service, mcp_resources};

pub use stdio::serve_stdio;
pub use transport::routes;

/// The MCP revision this server implements.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

/// Every protocol revision this server knows how to speak, newest first. A client that asks for
/// one of these gets it back; anything else is answered with the newest, which is what the spec
/// asks a server to do rather than failing the handshake.
const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &[PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];

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
pub async fn handle(service: &Arc<Service>, request: &Value) -> Option<Value> {
    match request {
        Value::Array(messages) if messages.is_empty() => Some(error_response(
            Value::Null,
            code::INVALID_REQUEST,
            "an empty batch is not a request".to_string(),
        )),
        Value::Array(messages) => {
            let mut replies = Vec::new();
            for message in messages {
                if let Some(reply) = handle_one(service, message).await {
                    replies.push(reply);
                }
            }
            (!replies.is_empty()).then_some(Value::Array(replies))
        }
        message => handle_one(service, message).await,
    }
}

async fn handle_one(service: &Arc<Service>, request: &Value) -> Option<Value> {
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

    let result = match method {
        "initialize" => Ok(initialize(service, &params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools_list(service)),
        "tools/call" => tools_call(service, &params).await,
        "resources/list" => Ok(mcp_resources()),
        "resources/read" => resources_read(service, &params),
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

fn initialize(service: &Service, params: &Map<String, Value>) -> Value {
    let catalog = service.catalog();
    // Echo the client's revision when it is one this server speaks, so an older client is not
    // told to understand a newer protocol than it asked for.
    let requested = params.get("protocolVersion").and_then(Value::as_str);
    let version = requested
        .filter(|asked| SUPPORTED_PROTOCOL_VERSIONS.contains(asked))
        .unwrap_or(PROTOCOL_VERSION);

    json!({
        "protocolVersion": version,
        "capabilities": {
            "tools": { "listChanged": false },
            "resources": { "subscribe": false, "listChanged": false },
        },
        "serverInfo": {
            "name": catalog.title(),
            "version": env!("CARGO_PKG_VERSION"),
        },
        "instructions": catalog.description(),
    })
}

fn tools_list(service: &Service) -> Value {
    // Served from the cache built at startup rather than rebuilt per call: clients ask for this
    // at least once per session.
    serde_json::from_str(&service.catalog().tools_json()).expect("cached tool list is valid JSON")
}

async fn tools_call(
    service: &Arc<Service>,
    params: &Map<String, Value>,
) -> Result<Value, RpcError> {
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
        Ok(value) => Ok(tool_result(&value, false)),
        // A Prolog exception or a bad argument is a *tool* failure, not a protocol failure: the
        // call itself was well-formed, so it comes back as an error result the model can read
        // and react to rather than as a JSON-RPC error.
        Err(cause @ (CallError::Prolog(_) | CallError::Argument(_))) => {
            Ok(tool_result(&Value::String(cause.to_string()), true))
        }
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

fn tool_result(value: &Value, is_error: bool) -> Value {
    let text = match value {
        Value::String(text) => text.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_else(|_| other.to_string()),
    };
    let mut result = json!({
        "content": [{ "type": "text", "text": text }],
        "isError": is_error,
    });
    if !is_error {
        // Also handed over structured, so a client that wants the data rather than the prose
        // does not have to re-parse the text block.
        result["structuredContent"] = json!({ "solutions": value });
    }
    result
}

fn resources_read(service: &Service, params: &Map<String, Value>) -> Result<Value, RpcError> {
    let uri = params
        .get("uri")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if uri != crate::service::SOURCE_RESOURCE_URI {
        return Err(RpcError {
            code: code::INVALID_PARAMS,
            message: format!("unknown resource: {uri}"),
        });
    }
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": crate::service::SOURCE_RESOURCE_MIME_TYPE,
            "text": service.catalog().source(),
        }]
    }))
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
