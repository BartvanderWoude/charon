use std::path::PathBuf;

use axum_test::TestServer;
use charon::prolog::KnowledgeBase;
use charon::web::Api;
use serde_json::json;

fn test_server() -> TestServer {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/api_test.pl");
    let knowledge_base = KnowledgeBase::load(&path).expect("failed to load test knowledge base");
    let interpreter = knowledge_base.parse();
    let api = Api::new(knowledge_base.predicates(), interpreter);
    TestServer::new(api.routes())
}

#[tokio::test]
async fn call_predicate_returns_all_solutions_as_json_array() {
    let server = test_server();

    let response = server
        .post("/list_length")
        .json(&json!({ "List": [1, 2, 3] }))
        .await;

    response.assert_status_ok();
    response.assert_json(&json!([{ "Length": 3 }]));
}

#[tokio::test]
async fn call_predicate_with_missing_argument_returns_bad_request() {
    let server = test_server();

    let response = server.post("/list_length").json(&json!({})).await;

    response.assert_status_bad_request();
}

#[tokio::test]
async fn call_unknown_predicate_returns_not_found() {
    let server = test_server();

    let response = server.post("/does_not_exist").text("").await;

    response.assert_status_not_found();
}

#[tokio::test]
async fn openapi_json_documents_exposed_predicates() {
    let server = test_server();

    let response = server.get("/openapi.json").await;

    response.assert_status_ok();
    let body: serde_json::Value = response.json();
    assert!(body["paths"]["/list_length"].is_object());
}
