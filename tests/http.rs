//! The HTTP/JSON interface: routing, argument extraction, response shaping, and the OpenAPI
//! document rendered from the same operation set.

mod common;

use axum::http::StatusCode;
use common::http_server;
use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------
// Calling predicates
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_det_predicate_answers_with_its_solution_directly() {
    let server = http_server("api_test.pl");

    // `list_length` is `det` with a single output, so both unwraps apply: no wrapper object and
    // no single-element array.
    let response = server
        .get("/list_length")
        .add_query_param("List", "[1, 2, 3]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!(3));
}

#[tokio::test]
async fn several_outputs_come_back_as_an_object() {
    let server = http_server("api_test.pl");

    let response = server
        .get("/split_pair")
        .add_query_param("Pair", "[1, 2]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!({ "First": 1, "Second": 2 }));
}

#[tokio::test]
async fn no_outputs_at_all_answers_with_whether_it_held() {
    let server = http_server("api_test.pl");

    let held = server
        .get("/is_pair")
        .add_query_param("Pair", "[1, 2]")
        .await;
    held.assert_status_ok();
    held.assert_json(&json!(true));

    // `is_pair` is `semidet`, so failing is a legitimate answer rather than an error — and with
    // nothing to bind, the honest report is `false`.
    let did_not = server.get("/is_pair").add_query_param("Pair", "[1]").await;
    did_not.assert_status_ok();
    did_not.assert_json(&json!(false));
}

#[tokio::test]
async fn a_failing_semidet_predicate_with_an_output_answers_null() {
    let server = http_server("api_test.pl");

    let response = server.get("/missing").await;

    response.assert_status_ok();
    response.assert_json(&Value::Null);
}

#[tokio::test]
async fn a_multi_predicate_always_answers_with_an_array() {
    let server = http_server("api_test.pl");

    let response = server.get("/color").await;

    response.assert_status_ok();
    response.assert_json(&json!(["red", "green", "blue"]));
}

#[tokio::test]
async fn a_zero_arity_predicate_is_reachable() {
    let server = http_server("api_test.pl");

    // `%! ready is semidet.` has no parenthesised argument list, which the old signature regex
    // required, so this endpoint simply did not exist.
    let response = server.get("/ready").await;

    response.assert_status_ok();
    response.assert_json(&json!(true));
}

#[tokio::test]
async fn a_head_request_is_answered_like_the_get_it_mirrors() {
    let server = http_server("api_test.pl");

    let response = server.method(axum::http::Method::HEAD, "/ready").await;

    response.assert_status_ok();
}

// ---------------------------------------------------------------------------------------------
// Where arguments come from
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn query_parameters_are_read_as_their_declared_type() {
    let server = http_server("api_test.pl");

    // A query string carries nothing but text; the declared type is what says whether `82` is a
    // number, `true` a boolean, and `[1,2]` a list.
    server
        .get("/sum_all")
        .add_query_param("Values", "[1,2,3,4]")
        .await
        .assert_json(&json!(10));
    server
        .get("/flagged")
        .add_query_param("On", "true")
        .await
        .assert_json(&json!("yes"));
    server
        .get("/scale")
        .add_query_param("Factor", "2")
        .add_query_param("Values", "[1,2]")
        .await
        .assert_json(&json!([2.0, 4.0]));
}

#[tokio::test]
async fn a_json_body_carries_already_typed_arguments() {
    let server = http_server("api_test.pl");

    // A `post_` predicate documents a request body, and sending one has to work without
    // url-encoding every argument into the query string first.
    let response = server.post("/note").json(&json!({ "Text": "hello" })).await;

    response.assert_status_ok();
    response.assert_json(&json!(5));
}

#[tokio::test]
async fn query_parameters_still_work_for_a_body_carrying_method() {
    let server = http_server("api_test.pl");

    // Documenting a body for POST should not make `curl -X POST '...?Text=hello'` stop working.
    let response = server.post("/note").add_query_param("Text", "hello").await;

    response.assert_status_ok();
    response.assert_json(&json!(5));
}

#[tokio::test]
async fn an_argument_given_twice_is_refused_rather_than_silently_preferred() {
    let server = http_server("api_test.pl");

    let response = server
        .post("/note")
        .add_query_param("Text", "hello")
        .json(&json!({ "Text": "goodbye" }))
        .await;

    response.assert_status_bad_request();
    let body: Value = response.json();
    assert!(
        body["error"].as_str().unwrap().contains("Text"),
        "got {body}"
    );
}

#[tokio::test]
async fn a_body_that_is_not_a_json_object_is_refused() {
    let server = http_server("api_test.pl");

    let response = server.post("/note").json(&json!(["hello"])).await;
    response.assert_status_bad_request();

    let response = server
        .post("/note")
        .content_type("text/plain")
        .text("Text=hello")
        .await;
    response.assert_status(StatusCode::UNSUPPORTED_MEDIA_TYPE);
}

#[tokio::test]
async fn a_missing_argument_is_named() {
    let server = http_server("api_test.pl");

    let response = server.get("/list_length").await;

    response.assert_status_bad_request();
    let body: Value = response.json();
    assert!(
        body["error"].as_str().unwrap().contains("List"),
        "got {body}"
    );
}

#[tokio::test]
async fn a_misspelled_argument_is_named_instead_of_ignored() {
    let server = http_server("api_test.pl");

    // Silently dropping `Value` and then complaining that `Values` is missing is technically
    // true and practically useless.
    let response = server
        .get("/sum_all")
        .add_query_param("Value", "[1,2]")
        .await;

    response.assert_status_bad_request();
    let message = response.json::<Value>()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("Value"), "got {message}");
}

#[tokio::test]
async fn an_ill_typed_argument_is_named() {
    let server = http_server("api_test.pl");

    let response = server
        .get("/sum_all")
        .add_query_param("Values", "[\"not\",\"numbers\"]")
        .await;

    response.assert_status_bad_request();
    let message = response.json::<Value>()["error"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(message.contains("Values"), "got {message}");
}

// ---------------------------------------------------------------------------------------------
// Routing and status codes
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn verb_prefixed_predicates_share_one_path() {
    let server = http_server("api_test.pl");

    // `get_fruit`/`put_fruit`/`delete_fruit` all collapse onto `/fruit`, dispatched by method.
    server.get("/fruit").await.assert_json(&json!(["apple"]));

    server
        .put("/fruit")
        .json(&json!({ "Fruit": "banana" }))
        .await
        .assert_status_ok();
    server
        .get("/fruit")
        .await
        .assert_json(&json!(["apple", "banana"]));

    server
        .delete("/fruit")
        .add_query_param("Fruit", "apple")
        .await
        .assert_json(&json!(true));
    server.get("/fruit").await.assert_json(&json!(["banana"]));
}

#[tokio::test]
async fn an_unknown_path_is_not_found() {
    let server = http_server("api_test.pl");

    let response = server.get("/does_not_exist").await;

    response.assert_status_not_found();
    assert!(response.json::<Value>()["error"].is_string());
}

#[tokio::test]
async fn a_path_that_does_not_answer_this_method_says_which_it_does() {
    let server = http_server("api_test.pl");

    // `/fruit` exists for GET, PUT and DELETE, but not for POST. "Not found" would send a
    // caller looking for a typo that is not there.
    let response = server.post("/fruit").await;

    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    let allow = response.headers()["allow"].to_str().unwrap().to_string();
    assert!(
        allow.contains("GET") && allow.contains("PUT") && allow.contains("DELETE"),
        "{allow}"
    );
    assert!(!allow.contains("POST"), "{allow}");
}

#[tokio::test]
async fn the_mcp_endpoint_is_absent_from_the_rest_interface() {
    let server = http_server("api_test.pl");

    // The two interfaces are no longer served together: a process started as `charon http`
    // answers REST and nothing else.
    let response = server.post("/mcp").json(&json!({})).await;

    assert_ne!(response.status_code(), StatusCode::OK);
}

// ---------------------------------------------------------------------------------------------
// The OpenAPI document
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn openapi_info_comes_from_the_module_comment() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    assert_eq!(document["info"]["title"], "API Test Fixture");
    let description = document["info"]["description"].as_str().unwrap();
    assert!(description.contains("Fixture predicates used by Charon's integration tests."));
    assert!(description.contains("**Author(s):** Test Suite"));
}

#[tokio::test]
async fn openapi_documents_arguments_where_the_method_carries_them() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    // GET takes its arguments in the query string…
    let get = &document["paths"]["/list_length"]["get"];
    assert_eq!(get["operationId"], "list_length");
    assert_eq!(
        get["summary"],
        "True if Length is the number of elements in List."
    );
    let parameters = get["parameters"].as_array().unwrap();
    let list = parameters.iter().find(|p| p["name"] == "List").unwrap();
    assert_eq!(list["in"], "query");
    assert_eq!(list["required"], true);
    assert_eq!(list["description"], "The list to measure.");
    assert!(get.get("requestBody").is_none());

    // …and POST in a body, which is what a JSON API caller expects to be able to send.
    let post = &document["paths"]["/note"]["post"];
    assert!(post.get("parameters").is_none());
    let schema = &post["requestBody"]["content"]["application/json"]["schema"];
    assert_eq!(post["requestBody"]["required"], true);
    assert_eq!(schema["properties"]["Text"]["type"], "string");
    assert_eq!(schema["required"], json!(["Text"]));
}

#[tokio::test]
async fn openapi_response_schemas_are_exact_rather_than_a_disjunction() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    let schema_at = |path: &str, verb: &str| {
        document["paths"][path][verb]["responses"]["200"]["content"]["application/json"]["schema"]
            .clone()
    };

    // `det`: exactly one solution, so the bare item schema.
    let det = schema_at("/list_length", "get");
    assert_eq!(det["type"], "integer");
    assert_eq!(det["description"], "The resulting length.");

    // `multi`: always an array, never "either shape". The old document had to say `oneOf`
    // because the response shape depended on how many solutions turned up at runtime.
    let multi = schema_at("/color", "get");
    assert_eq!(multi["type"], "array");
    assert_eq!(multi["items"]["type"], "string");
    assert!(multi.get("oneOf").is_none());

    // `semidet` with an output: the solution, or null.
    let semidet = schema_at("/missing", "get");
    let alternatives = semidet["oneOf"].as_array().unwrap();
    assert!(alternatives.iter().any(|s| s["type"] == "integer"));
    assert!(alternatives.iter().any(|s| s["type"] == "null"));
}

#[tokio::test]
async fn openapi_marks_deprecated_predicates() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    let operation = &document["paths"]["/old_pred"]["get"];
    assert_eq!(operation["deprecated"], true);
    assert!(
        operation["description"]
            .as_str()
            .unwrap()
            .contains("Use list_length/2 instead.")
    );
}

#[tokio::test]
async fn openapi_documents_verb_prefixed_predicates_under_one_path() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    let path = &document["paths"]["/fruit"];
    assert_eq!(path["get"]["operationId"], "get_fruit");
    assert_eq!(path["put"]["operationId"], "put_fruit");
    assert_eq!(path["delete"]["operationId"], "delete_fruit");
}

#[tokio::test]
async fn openapi_omits_predicates_that_cannot_be_exposed() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    // `hidden(?X:int)` has a mode with no JSON meaning. It is reported at startup, not exposed.
    assert!(document["paths"].get("/hidden").is_none());
}

#[tokio::test]
async fn openapi_is_cached_and_revalidates() {
    let server = http_server("api_test.pl");

    let response = server.get("/openapi.json").await;
    response.assert_status_ok();
    let etag = response.headers()["etag"].to_str().unwrap().to_string();

    let revalidated = server
        .get("/openapi.json")
        .add_header("if-none-match", &etag)
        .await;
    revalidated.assert_status(StatusCode::NOT_MODIFIED);
}
