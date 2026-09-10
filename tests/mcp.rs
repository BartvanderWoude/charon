//! The MCP interface, exercised over the same `Service` the HTTP interface uses.
//!
//! The point these tests defend is that the two interfaces are two renderings of one thing: a
//! predicate exposed over HTTP is callable over MCP, with the same solution shaping, without a
//! second implementation of any of it.

mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use charon::mcp::Server;
use charon::service::Service;
use common::{api, mcp_server};
use serde_json::{Value, json};

/// The dispatcher over a fixture, which is what most of these tests drive directly.
fn server(service: &Arc<Service>) -> Arc<Server> {
    Server::new(Arc::clone(service))
}

async fn request(server: &Arc<Server>, method: &str, params: Value) -> Value {
    let response = charon::mcp::handle(
        server,
        &json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }),
    )
    .await
    .expect("a request with an id must be answered");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 1);
    response
}

async fn result(server: &Arc<Server>, method: &str, params: Value) -> Value {
    let response = request(server, method, params).await;
    assert!(
        response.get("error").is_none(),
        "unexpected error: {response}"
    );
    response["result"].clone()
}

// ---------------------------------------------------------------------------------------------
// The handshake
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn initialize_advertises_tools_and_resources() {
    let service = server(&api());
    let result = result(&service, "initialize", json!({})).await;

    assert_eq!(result["protocolVersion"], charon::mcp::PROTOCOL_VERSION);
    assert!(result["capabilities"]["tools"].is_object());
    assert!(result["capabilities"]["resources"].is_object());
    // Server identity comes from the knowledge base's own module comment.
    assert_eq!(result["serverInfo"]["name"], "API Test Fixture");
}

#[tokio::test]
async fn initialize_answers_in_the_revision_the_client_asked_for() {
    let service = server(&api());

    // Telling a 2024-11-05 client that the session is 2025-06-18 asks it to understand a
    // protocol it does not know.
    let known = result(
        &service,
        "initialize",
        json!({ "protocolVersion": "2024-11-05" }),
    )
    .await;
    assert_eq!(known["protocolVersion"], "2024-11-05");

    // A revision this server does not speak gets the newest one it does, rather than a failure.
    let unknown = result(
        &service,
        "initialize",
        json!({ "protocolVersion": "1999-01-01" }),
    )
    .await;
    assert_eq!(unknown["protocolVersion"], charon::mcp::PROTOCOL_VERSION);
}

#[tokio::test]
async fn ping_is_answered_with_an_empty_result() {
    let service = server(&api());
    assert_eq!(result(&service, "ping", json!({})).await, json!({}));
}

#[tokio::test]
async fn notifications_are_not_answered() {
    let service = server(&api());

    // No `id`, so by JSON-RPC convention there must be no response at all.
    let response = charon::mcp::handle(
        &service,
        &json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
    )
    .await;
    assert!(response.is_none());
}

#[tokio::test]
async fn a_batch_is_answered_with_a_batch() {
    let service = server(&api());

    // A JSON-RPC array used to fall through the `id` lookup and be treated as a notification, so
    // a batching client got silence.
    let response = charon::mcp::handle(
        &service,
        &json!([
            { "jsonrpc": "2.0", "id": 1, "method": "ping" },
            { "jsonrpc": "2.0", "method": "notifications/initialized" },
            { "jsonrpc": "2.0", "id": 2, "method": "ping" },
        ]),
    )
    .await
    .expect("a batch containing requests must be answered");

    let replies = response.as_array().expect("a batch answers with a batch");
    // Two requests and one notification in, two replies out.
    assert_eq!(replies.len(), 2);
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(replies[1]["id"], 2);
}

#[tokio::test]
async fn a_batch_of_only_notifications_is_answered_with_silence() {
    let service = server(&api());
    let response = charon::mcp::handle(
        &service,
        &json!([{ "jsonrpc": "2.0", "method": "notifications/initialized" }]),
    )
    .await;
    assert!(response.is_none());
}

#[tokio::test]
async fn malformed_messages_are_refused_by_code() {
    let service = server(&api());

    assert_eq!(
        request(&service, "nonsense/method", json!({})).await["error"]["code"],
        -32601
    );

    // A message with an id but no method is a request that cannot be dispatched.
    let response = charon::mcp::handle(&service, &json!({ "jsonrpc": "2.0", "id": 1 }))
        .await
        .expect("a message with an id must be answered");
    assert_eq!(response["error"]["code"], -32600);
}

// ---------------------------------------------------------------------------------------------
// Tools
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn tools_keep_their_predicate_names_including_verb_prefixes() {
    // Writable, so the changing operations are on the list at all to be named.
    let writable = common::Persisting::fixture("api_test.pl", "mcp_tool_names");
    let service = server(&writable.service);
    let result = result(&service, "tools/list", json!({})).await;
    let names: Vec<&str> = result["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();

    // HTTP collapses these three onto `/fruit` and distinguishes them by method. A tool name is
    // flat, so doing the same here would merge three distinct operations into one.
    assert!(names.contains(&"get_fruit"));
    assert!(names.contains(&"put_fruit"));
    assert!(names.contains(&"delete_fruit"));
    // A predicate that cannot be exposed is not a tool either.
    assert!(!names.contains(&"hidden"));
}

#[tokio::test]
async fn verb_prefixes_become_behavioural_annotations() {
    let writable = common::Persisting::fixture("api_test.pl", "mcp_annotations");
    let service = server(&writable.service);
    let result = result(&service, "tools/list", json!({})).await;
    let tools = result["tools"].as_array().unwrap().clone();
    let tool = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} should be exposed"))
            .clone()
    };

    assert_eq!(tool("get_fruit")["annotations"]["readOnlyHint"], true);
    assert_eq!(tool("put_fruit")["annotations"]["readOnlyHint"], false);
    assert_eq!(tool("put_fruit")["annotations"]["idempotentHint"], true);
    assert_eq!(tool("post_note")["annotations"]["idempotentHint"], false);
    assert_eq!(tool("delete_fruit")["annotations"]["destructiveHint"], true);
    // Nothing here reaches the network; every answer comes from the local knowledge base.
    assert_eq!(tool("get_fruit")["annotations"]["openWorldHint"], false);
    assert_eq!(tool("old_pred")["annotations"]["deprecated"], true);
}

#[tokio::test]
async fn tool_schemas_match_the_declared_types() {
    let service = server(&api());
    let result = result(&service, "tools/list", json!({})).await;
    let tools = result["tools"].as_array().unwrap().clone();
    let tool = tools
        .iter()
        .find(|tool| tool["name"] == "pim_hits")
        .unwrap();

    let input = &tool["inputSchema"];
    assert_eq!(input["type"], "object");
    assert_eq!(input["properties"]["Drugs"]["type"], "array");
    // The element type is carried through, which is what `list(atom)` buys.
    assert_eq!(input["properties"]["Drugs"]["items"]["type"], "string");
    assert_eq!(input["required"], json!(["Drugs"]));

    // The output schema describes the same shape the call actually returns.
    let solutions = &tool["outputSchema"]["properties"]["solutions"];
    assert_eq!(solutions["type"], "array");
    assert_eq!(solutions["items"]["properties"]["Drug"]["type"], "string");
}

#[tokio::test]
async fn calling_a_tool_returns_text_and_structured_content() {
    let service = server(&api());
    let result = result(
        &service,
        "tools/call",
        json!({ "name": "list_length", "arguments": { "List": [1, 2, 3] } }),
    )
    .await;

    assert_eq!(result["isError"], false);
    assert_eq!(result["content"][0]["type"], "text");
    assert_eq!(result["content"][0]["text"], "3");
    assert_eq!(result["structuredContent"]["solutions"], json!(3));
}

#[tokio::test]
async fn mcp_arguments_are_passed_through_already_typed() {
    let service = server(&api());

    // No query-string coercion is involved on this path: the array arrives as an array.
    let result = result(
        &service,
        "tools/call",
        json!({ "name": "pim_hits", "arguments": { "Drugs": ["diazepam", "digoxin"] } }),
    )
    .await;

    assert_eq!(
        result["structuredContent"]["solutions"],
        json!([
            { "Drug": "diazepam", "Reason": "sedation" },
            { "Drug": "digoxin", "Reason": "toxicity" },
        ])
    );
}

#[tokio::test]
async fn a_tool_taking_no_arguments_can_be_called_without_them() {
    let service = server(&api());

    for params in [
        json!({ "name": "ready" }),
        json!({ "name": "ready", "arguments": {} }),
        json!({ "name": "ready", "arguments": null }),
    ] {
        let result = result(&service, "tools/call", params.clone()).await;
        assert_eq!(
            result["structuredContent"]["solutions"],
            json!(true),
            "{params}"
        );
    }
}

#[tokio::test]
async fn a_prolog_exception_is_a_tool_error_not_a_protocol_error() {
    let service = server(&api());
    let response = request(
        &service,
        "tools/call",
        json!({ "name": "raises", "arguments": { "X": 1 } }),
    )
    .await;

    // The call itself was well-formed, so JSON-RPC succeeded; the failure belongs in the result
    // where the model can read it.
    assert!(response.get("error").is_none(), "got {response}");
    let result = &response["result"];
    assert_eq!(result["isError"], true);
    assert!(
        result["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("deliberate_test_error"),
        "got {result}"
    );
}

#[tokio::test]
async fn an_exception_over_mcp_does_not_poison_later_calls() {
    let service = server(&api());

    request(
        &service,
        "tools/call",
        json!({ "name": "raises", "arguments": { "X": 1 } }),
    )
    .await;

    let result = result(
        &service,
        "tools/call",
        json!({ "name": "list_length", "arguments": { "List": [1, 2] } }),
    )
    .await;
    assert_eq!(result["structuredContent"]["solutions"], json!(2));
}

#[tokio::test]
async fn a_bad_tool_call_is_refused_by_code() {
    let service = server(&api());

    let unknown = request(
        &service,
        "tools/call",
        json!({ "name": "no_such_tool", "arguments": {} }),
    )
    .await;
    assert_eq!(unknown["error"]["code"], -32602);

    let nameless = request(&service, "tools/call", json!({ "arguments": {} })).await;
    assert_eq!(nameless["error"]["code"], -32602);

    let ill_typed = request(
        &service,
        "tools/call",
        json!({ "name": "ready", "arguments": ["not", "an", "object"] }),
    )
    .await;
    assert_eq!(ill_typed["error"]["code"], -32602);
}

// ---------------------------------------------------------------------------------------------
// Resources
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn the_knowledge_base_source_is_readable_as_a_resource() {
    let service = server(&api());

    let listing = result(&service, "resources/list", json!({})).await;
    assert_eq!(listing["resources"][0]["uri"], "charon://source");

    let read = result(
        &service,
        "resources/read",
        json!({ "uri": "charon://source" }),
    )
    .await;
    let text = read["contents"][0]["text"].as_str().unwrap();
    // A model can read the rules it is calling rather than trust the descriptions.
    assert!(text.contains("pim(diazepam, sedation)."), "got {text:.200}");
}

#[tokio::test]
async fn an_unknown_resource_is_refused() {
    let service = server(&api());
    let response = request(
        &service,
        "resources/read",
        json!({ "uri": "charon://other" }),
    )
    .await;
    assert_eq!(response["error"]["code"], -32602);
}

// ---------------------------------------------------------------------------------------------
// The HTTP transport
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn mcp_over_http_answers_at_its_own_endpoint() {
    let server = mcp_server("api_test.pl");

    let response = server
        .post("/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .await;

    response.assert_status_ok();
    response.assert_json(&json!({ "jsonrpc": "2.0", "id": 1, "result": {} }));
}

#[tokio::test]
async fn mcp_over_http_acknowledges_a_notification_without_a_body() {
    let server = mcp_server("api_test.pl");

    let response = server
        .post("/mcp")
        .json(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }))
        .await;

    response.assert_status(StatusCode::ACCEPTED);
    assert!(response.text().is_empty());
}

#[tokio::test]
async fn mcp_over_http_reports_a_parse_error_as_json_rpc() {
    let server = mcp_server("api_test.pl");

    let response = server.post("/mcp").text("{not json").await;

    response.assert_status_bad_request();
    assert_eq!(response.json::<Value>()["error"]["code"], -32700);
}

#[tokio::test]
async fn the_rest_interface_is_absent_from_the_mcp_server() {
    let server = mcp_server("api_test.pl");

    // A process started as `charon mcp` speaks MCP and nothing else — including no OpenAPI
    // document, which would describe endpoints it does not serve.
    assert_ne!(
        server.get("/openapi.json").await.status_code(),
        StatusCode::OK
    );
    assert_ne!(
        server.get("/list_length").await.status_code(),
        StatusCode::OK
    );
}

// ---------------------------------------------------------------------------------------------
// One service behind both interfaces
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn both_interfaces_shape_solutions_identically() {
    let service = api();
    let server = server(&service);
    let catalog = service.catalog();

    // The same operation, called through the service the way each transport calls it, has to
    // produce the same JSON — that equality is what "no duplicated logic" actually means.
    let arguments = json!({ "Drugs": ["diazepam"] })
        .as_object()
        .unwrap()
        .clone();
    let direct = service
        .call_blocking(&catalog, "pim_hits", &arguments)
        .expect("call should succeed");

    let over_mcp = result(
        &server,
        "tools/call",
        json!({ "name": "pim_hits", "arguments": { "Drugs": ["diazepam"] } }),
    )
    .await;

    assert_eq!(direct.value, over_mcp["structuredContent"]["solutions"]);
}

// ---------------------------------------------------------------------------------------------
// The Streamable HTTP transport
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_cross_origin_request_is_refused() {
    // A web page cannot read from a loopback port, but it can be made to POST to one. Charon has
    // no authentication at all, so refusing the request outright is the only thing between a
    // knowledge base bound to localhost and any page the user happens to have open.
    let server = common::mcp_server("api_test.pl");

    let refused = server
        .post("/mcp")
        .add_header("origin", "https://evil.example")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await;
    refused.assert_status(StatusCode::FORBIDDEN);

    // A local origin, and no origin at all, are both fine.
    for origin in ["http://localhost:3000", "http://127.0.0.1:8080"] {
        server
            .post("/mcp")
            .add_header("origin", origin)
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
            .await
            .assert_status_ok();
    }
    server
        .post("/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn a_protocol_version_the_server_does_not_speak_is_refused() {
    // Better than answering in a dialect the client will misread.
    let server = common::mcp_server("api_test.pl");

    server
        .post("/mcp")
        .add_header("mcp-protocol-version", "1999-01-01")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .await
        .assert_status_bad_request();

    server
        .post("/mcp")
        .add_header("mcp-protocol-version", charon::mcp::PROTOCOL_VERSION)
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "ping" }))
        .await
        .assert_status_ok();
}

#[tokio::test]
async fn a_session_is_issued_on_the_handshake_and_not_before() {
    let server = common::mcp_server("api_test.pl");

    let handshake = server
        .post("/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
        .await;
    handshake.assert_status_ok();
    assert!(
        handshake.headers().get("mcp-session-id").is_some(),
        "initialize should issue a session"
    );

    let later = server
        .post("/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "ping" }))
        .await;
    assert!(later.headers().get("mcp-session-id").is_none());
}

#[tokio::test]
async fn there_is_no_event_stream_and_the_server_says_so() {
    // Leaving GET unrouted would look like a broken deployment; Charon simply has nothing to
    // push, since the tool list is fixed for the life of a process.
    let server = common::mcp_server("api_test.pl");
    let response = server.get("/mcp").await;

    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"].to_str().unwrap(), "POST");
}

#[tokio::test]
async fn the_handshake_advertises_what_this_server_can_actually_do() {
    let service = server(&api());
    let result = result(&service, "initialize", json!({})).await;
    let capabilities = &result["capabilities"];

    for capability in ["tools", "resources", "prompts", "logging", "completions"] {
        assert!(
            capabilities.get(capability).is_some(),
            "{capability} missing from {capabilities}"
        );
    }
}
