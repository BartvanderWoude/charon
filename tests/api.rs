use std::path::PathBuf;

use axum_test::TestServer;
use charon::prolog::KnowledgeBase;
use charon::web::Api;
use serde_json::json;

fn test_server() -> TestServer {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api_test.pl");
    let knowledge_base = KnowledgeBase::load(&path).expect("failed to load test knowledge base");
    let interpreter = knowledge_base.parse();
    let api = Api::new(
        knowledge_base.predicates(),
        interpreter,
        knowledge_base.module_doc(),
    );
    TestServer::new(api.routes())
}

#[tokio::test]
async fn call_predicate_with_one_solution_returns_bare_value() {
    let server = test_server();

    // `list_length` is `det` (always exactly one solution) with a single output argument, so
    // both unwraps apply: no wrapper object, and no single-element array.
    let response = server
        .get("/list_length")
        .add_query_param("List", "[1, 2, 3]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!(3));
}

#[tokio::test]
async fn call_predicate_with_one_solution_and_multiple_outputs_returns_bare_object() {
    let server = test_server();

    let response = server
        .get("/split_pair")
        .add_query_param("Pair", "[1, 2]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!({ "First": 1, "Second": 2 }));
}

#[tokio::test]
async fn call_predicate_with_no_outputs_and_one_solution_returns_bare_boolean() {
    let server = test_server();

    let response = server
        .get("/is_pair")
        .add_query_param("Pair", "[1, 2]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!(true));
}

#[tokio::test]
async fn call_predicate_with_several_solutions_returns_array() {
    let server = test_server();

    let response = server.get("/color").await;

    response.assert_status_ok();
    response.assert_json(&json!(["red", "green", "blue"]));
}

#[tokio::test]
async fn call_predicate_with_missing_argument_returns_bad_request() {
    let server = test_server();

    let response = server.get("/list_length").await;

    response.assert_status_bad_request();
    let body: serde_json::Value = response.json();
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn call_unknown_predicate_returns_not_found() {
    let server = test_server();

    let response = server.get("/does_not_exist").await;

    response.assert_status_not_found();
    let body: serde_json::Value = response.json();
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn openapi_json_documents_exposed_predicates() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["paths"]["/list_length"].is_object());
}

#[tokio::test]
async fn openapi_json_info_comes_from_module_doc_comment() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    let body: serde_json::Value = response.json();
    // Sourced from the `/** <module> API Test Fixture ... */` comment at the top of
    // tests/fixtures/api_test.pl, rather than the generic fallback title/description.
    assert_eq!(body["info"]["title"], "API Test Fixture");
    let description = body["info"]["description"].as_str().unwrap();
    assert!(description.contains("Fixture predicates used by the `web::Api` integration tests."));
    assert!(description.contains("**Author(s):** Test Suite"));
}

#[tokio::test]
async fn openapi_json_documents_summary_and_argument_descriptions_as_query_params() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    let body: serde_json::Value = response.json();
    let operation = &body["paths"]["/list_length"]["get"];

    assert_eq!(operation["operationId"], "list_length");
    assert_eq!(
        operation["summary"],
        "True if Length is the number of elements in List."
    );

    let parameters = operation["parameters"].as_array().unwrap();
    let list_param = parameters
        .iter()
        .find(|param| param["name"] == "List")
        .expect("List is documented as a query parameter");
    assert_eq!(list_param["in"], "query");
    assert_eq!(list_param["required"], true);
    assert_eq!(list_param["description"], "The list to measure.");
}

#[tokio::test]
async fn openapi_json_marks_deprecated_predicates() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    let body: serde_json::Value = response.json();
    let operation = &body["paths"]["/old_pred"]["get"];

    assert_eq!(operation["deprecated"], true);
    let description = operation["description"].as_str().unwrap();
    assert!(description.contains("Use list_length/2 instead."));
}

#[tokio::test]
async fn openapi_json_response_is_bare_value_for_deterministic_predicates() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    let body: serde_json::Value = response.json();
    let response_schema = &body["paths"]["/list_length"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];

    // `list_length` is `det`: always exactly one solution, so the documented response is the
    // bare item schema, not an array.
    assert_eq!(response_schema["type"], "integer");
    assert_eq!(response_schema["description"], "The resulting length.");
}

#[tokio::test]
async fn verb_prefixed_predicates_share_one_path_with_different_methods() {
    let server = test_server();

    // `get_fruit`/`put_fruit`/`delete_fruit` all collapse to the `/fruit` path, dispatched by
    // HTTP method rather than by predicate name.
    let response = server.get("/fruit").await;
    response.assert_status_ok();
    response.assert_json(&json!("apple"));

    let response = server
        .put("/fruit")
        .add_query_param("Fruit", "banana")
        .await;
    response.assert_status_ok();

    let response = server.get("/fruit").await;
    response.assert_status_ok();
    response.assert_json(&json!(["apple", "banana"]));

    let response = server
        .delete("/fruit")
        .add_query_param("Fruit", "apple")
        .await;
    response.assert_status_ok();
    response.assert_json(&json!(true));

    let response = server.get("/fruit").await;
    response.assert_status_ok();
    response.assert_json(&json!("banana"));
}

#[tokio::test]
async fn verb_prefixed_predicate_rejects_wrong_method() {
    let server = test_server();

    // `/fruit` exists (as GET/PUT/DELETE) but not as POST.
    let response = server.post("/fruit").await;

    response.assert_status(axum::http::StatusCode::METHOD_NOT_ALLOWED);
    let body: serde_json::Value = response.json();
    assert!(body["error"].is_string());
}

#[tokio::test]
async fn openapi_json_documents_verb_prefixed_predicates_under_shared_path() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    let body: serde_json::Value = response.json();
    let path = &body["paths"]["/fruit"];
    assert!(path["get"].is_object());
    assert!(path["put"].is_object());
    assert!(path["delete"].is_object());
    assert_eq!(path["get"]["operationId"], "get_fruit");
    assert_eq!(path["put"]["operationId"], "put_fruit");
    assert_eq!(path["delete"]["operationId"], "delete_fruit");
}

#[tokio::test]
async fn openapi_json_response_allows_either_shape_for_non_deterministic_predicates() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    let body: serde_json::Value = response.json();
    let response_schema = &body["paths"]["/color"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];

    // `color` is `multi`: it can have one or several solutions, so the documented response is
    // either the bare item or an array of them.
    let alternatives = response_schema["oneOf"].as_array().unwrap();
    assert!(alternatives.iter().any(|schema| schema["type"] == "string"));
    assert!(alternatives.iter().any(|schema| schema["type"] == "array"));
}
