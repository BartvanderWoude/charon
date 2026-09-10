//! Collections: an enumerating predicate exposed as a table on both interfaces.
//!
//! The point of these is that one `@collection` tag has to reach HTTP *and* MCP from one
//! declaration — REST sub-resources on the one, readable resources on the other — without either
//! adapter re-deriving what a collection is.

mod common;

use axum::http::StatusCode;
use common::{Persisting, http_server, mcp_server};
use serde_json::{Value, json};

/// Sends one JSON-RPC request to an MCP test server and returns its `result`.
async fn rpc(server: &axum_test::TestServer, method: &str, params: Value) -> Value {
    let response: Value = server
        .post("/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .await
        .json();
    assert!(
        response.get("error").is_none(),
        "{method} failed: {response}"
    );
    response["result"].clone()
}

// ---------------------------------------------------------------------------------------------
// Reading a collection over HTTP
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_collection_answers_the_whole_table() {
    let server = http_server("api_test.pl");
    let response = server.get("/criteria").await;

    response.assert_status_ok();
    response.assert_json(&json!([
        { "Substance": "diazepam", "Kind": "sedation", "Source": "PRISCUS" },
        { "Substance": "digoxin", "Kind": "toxicity", "Source": "PRISCUS" },
    ]));
}

#[tokio::test]
async fn a_collection_answers_one_member_without_repeating_its_key() {
    // The key is in the URL, so echoing it in the body would be saying the same thing twice.
    let server = http_server("api_test.pl");
    let response = server.get("/criteria/diazepam").await;

    response.assert_status_ok();
    response.assert_json(&json!({ "Kind": "sedation", "Source": "PRISCUS" }));
}

#[tokio::test]
async fn a_collection_predicate_is_not_a_route_of_its_own() {
    // The table is what `criterion/3` is exposed as. Answering the same facts a second time at
    // the predicate's own name would be two addresses for one set of facts, and an OpenAPI
    // document that describes the same rows twice.
    let server = http_server("api_test.pl");

    server.get("/criteria").await.assert_status_ok();
    server.get("/criterion").await.assert_status_not_found();

    let document: Value = server.get("/openapi.json").await.json();
    assert!(document["paths"]["/criteria"].is_object());
    assert!(
        document["paths"]["/criterion"].is_null(),
        "{}",
        document["paths"]
    );
    let ids: Vec<&str> = document["paths"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|item| item.as_object().unwrap().values())
        .map(|operation| operation["operationId"].as_str().unwrap())
        .collect();
    assert!(!ids.contains(&"criterion"), "{ids:?}");
}

#[tokio::test]
async fn a_collection_may_take_its_predicates_name() {
    // The predicate answers as its table and nowhere else, so the name it would have taken is
    // free for the table to use — which is what an author writing `role/1` as a table means.
    let kb = Persisting::source(
        "coll_same_name",
        "%! role(-Role:atom) is multi.\n\
         %\n\
         %   @collection role key(Role)\n\
         role(member).\n\
         role(treasurer).\n",
    );
    let server = kb.http();

    let members: Value = server.get("/role").await.json();
    assert_eq!(members, json!(["member", "treasurer"]));

    // A key-only row has nothing left to say once the key is in the path.
    server.get("/role/member").await.assert_json(&json!(true));
    server.get("/role/auditor").await.assert_status_not_found();
}

#[tokio::test]
async fn a_member_that_is_not_there_is_not_found() {
    // Not `200 null`: a collection member is a resource, and an absent one is a 404. This is the
    // whole reason the generated item route is `semidet` rather than inheriting `multi`.
    let server = http_server("api_test.pl");
    let response = server.get("/criteria/aspirin").await;

    response.assert_status_not_found();
    assert_eq!(response.json::<Value>()["type"], "urn:charon:no-such-path");
}

#[tokio::test]
async fn a_keys_only_member_answers_that_it_is_a_member() {
    // `consent_module/1` has nothing but its key, so there is no row to hand back — only the
    // fact that the member exists.
    let server = http_server("api_test.pl");

    server
        .get("/consent_modules/research")
        .await
        .assert_json(&json!(true));
    server
        .get("/consent_modules/nonsense")
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn a_static_collection_offers_no_write_routes_however_the_server_started() {
    // `criterion/3` is not `dynamic`, so `assertz` against it would raise a permission error.
    // Offering PUT would be a promise Charon cannot keep, so the route does not exist even with
    // `--persist`.
    let writable = common::writable_http("coll_static");
    let response = writable.put("/criteria/aspirin").await;

    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(response.headers()["allow"].to_str().unwrap(), "GET, HEAD");
}

// ---------------------------------------------------------------------------------------------
// Writing to a collection
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_writable_collection_takes_a_member_and_reads_it_back() {
    let writable = Persisting::fixture("api_test.pl", "coll_put");
    let server = writable.http();

    server
        .put("/consent_modules/genomics")
        .await
        .assert_status_ok();
    server
        .get("/consent_modules/genomics")
        .await
        .assert_json(&json!(true));

    // …and it reached the file, which is what `--persist` is for.
    assert!(
        writable.scratch.read().contains("consent_module(genomics)"),
        "{}",
        writable.scratch.read()
    );
}

#[tokio::test]
async fn putting_a_member_twice_changes_nothing() {
    // PUT promises idempotence, and `--persist` would otherwise write a duplicate fact on every
    // retry. The generated goal is `( p(K) -> true ; assertz(p(K)) )` for exactly this reason.
    let writable = Persisting::fixture("api_test.pl", "coll_idempotent");
    let server = writable.http();

    for _ in 0..3 {
        server
            .put("/consent_modules/genomics")
            .await
            .assert_status_ok();
    }

    let members: Value = server.get("/consent_modules").await.json();
    let count = members
        .as_array()
        .unwrap()
        .iter()
        .filter(|member| **member == json!("genomics"))
        .count();
    assert_eq!(count, 1, "{members}");
}

#[tokio::test]
async fn a_member_with_columns_is_written_from_the_body_and_returned_when_deleted() {
    let writable = Persisting::fixture("api_test.pl", "coll_memo");
    let server = writable.http();

    // The key comes from the path, the rest of the row from the body.
    server
        .put("/memos/greeting")
        .json(&json!({ "Body": "Hi there" }))
        .await
        .assert_status_ok();
    // One value column, so it unwraps — a member is shaped by the same rule as every other
    // solution in Charon, rather than always being an object. A collection with two value
    // columns answers with an object keyed by column, exactly as a two-output predicate does.
    server
        .get("/memos/greeting")
        .await
        .assert_json(&json!("Hi there"));

    // DELETE leaves the value columns free, so `retract` matches whatever the row held — and
    // hands it back, which is the only chance a caller gets to see what was removed.
    let deleted = server.delete("/memos/welcome").await;
    deleted.assert_status_ok();
    deleted.assert_json(&json!("Hello"));

    server.get("/memos/welcome").await.assert_status_not_found();
    server
        .delete("/memos/welcome")
        .await
        .assert_status_not_found();
}

#[tokio::test]
async fn a_writable_collection_is_read_only_without_persistence() {
    // Part 1's rule, reused rather than re-invented: a collection's write routes are PUT and
    // DELETE, which are changing operations like any other.
    let server = http_server("api_test.pl");

    let refused = server.put("/consent_modules/genomics").await;
    refused.assert_status(StatusCode::FORBIDDEN);
    assert_eq!(refused.json::<Value>()["type"], "urn:charon:withheld");

    // Reading is unaffected, and nothing was added.
    let members: Value = server.get("/consent_modules").await.json();
    assert_eq!(members, json!(["research", "biobank"]));
}

// ---------------------------------------------------------------------------------------------
// Collections over MCP: resources, not tools
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_collection_is_a_resource_rather_than_a_tool() {
    // A model is meant to read resources for context and call tools to act. Making it spend a
    // tool call to see a table it could have been handed gets that backwards.
    let server = mcp_server("api_test.pl");

    let resources = rpc(&server, "resources/list", json!({})).await;
    let uris: Vec<&str> = resources["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|resource| resource["uri"].as_str().unwrap())
        .collect();
    assert!(uris.contains(&"charon://source"));
    assert!(uris.contains(&"charon://collection/criteria"));
    assert!(uris.contains(&"charon://collection/consent_modules"));

    // Neither the generated routes nor the predicates they enumerate are tools: a table is
    // reachable one way, as the resource above.
    let tools = rpc(&server, "tools/list", json!({})).await;
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    for absent in [
        "criterion",
        "consent_module",
        "criteria__list",
        "criteria__item",
        "consent_modules__put",
    ] {
        assert!(!names.contains(&absent), "{absent} should not be a tool");
    }
}

#[tokio::test]
async fn a_collection_resource_reads_the_table_and_one_member() {
    let server = mcp_server("api_test.pl");

    let whole = rpc(
        &server,
        "resources/read",
        json!({ "uri": "charon://collection/criteria" }),
    )
    .await;
    let text = whole["contents"][0]["text"].as_str().unwrap();
    let table: Value = serde_json::from_str(text).unwrap();
    assert_eq!(table.as_array().unwrap().len(), 2, "{table}");
    assert_eq!(whole["contents"][0]["mimeType"], "application/json");

    let one = rpc(
        &server,
        "resources/read",
        json!({ "uri": "charon://collection/criteria/digoxin" }),
    )
    .await;
    let text = one["contents"][0]["text"].as_str().unwrap();
    let row: Value = serde_json::from_str(text).unwrap();
    assert_eq!(row, json!({ "Kind": "toxicity", "Source": "PRISCUS" }));
}

#[tokio::test]
async fn members_are_described_by_a_resource_template() {
    // So a model can read one row instead of pulling a whole table into its context.
    let server = mcp_server("api_test.pl");
    let templates = rpc(&server, "resources/templates/list", json!({})).await;

    let uris: Vec<&str> = templates["resourceTemplates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|template| template["uriTemplate"].as_str().unwrap())
        .collect();
    assert!(
        uris.contains(&"charon://collection/criteria/{Substance}"),
        "{uris:?}"
    );
    assert!(
        uris.contains(&"charon://collection/memos/{Slug}"),
        "{uris:?}"
    );
}

// ---------------------------------------------------------------------------------------------
// Domains: what an argument is allowed to be, answered from the knowledge base
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_domain_over_unchanging_facts_becomes_an_enum() {
    // `color/1` is not `dynamic`, so its values are settled at startup and can be promised in
    // the schema — which means a wrong colour is refused before a goal is ever built.
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    let parameter = &document["paths"]["/favourite"]["get"]["parameters"][0];
    assert_eq!(parameter["name"], "Color");
    assert_eq!(parameter["schema"]["enum"], json!(["red", "green", "blue"]));
}

#[tokio::test]
async fn a_domain_takes_its_values_from_the_key_column_wherever_it_is_declared() {
    // The key here is the *second* argument, so a domain over this collection must read
    // `shelf(_, V)` and not `shelf(V, _)`. Resolving the key by its position among the
    // predicate's arguments rather than among a reordered key-then-values list is what keeps
    // these apart; getting it wrong offers the shelf numbers as if they were book titles.
    let persisting = Persisting::source(
        "domain_late_key",
        "%! shelf(-Position:int, -Book:atom) is multi.\n\
         %\n\
         %   The shelf.\n\
         %\n\
         %   @collection books key(Book)\n\
         shelf(1, dune).\n\
         shelf(2, emma).\n\
         \n\
         %! lookup(+Book:atom, -Position:int) is semidet.\n\
         %\n\
         %   Where a book sits.\n\
         %\n\
         %   @domain Book books\n\
         lookup(B, P) :- shelf(P, B).\n",
    );
    let server = persisting.http();
    let document: Value = server.get("/openapi.json").await.json();

    let parameter = &document["paths"]["/lookup"]["get"]["parameters"][0];
    assert_eq!(parameter["name"], "Book");
    assert_eq!(parameter["schema"]["enum"], json!(["dune", "emma"]));
}

#[tokio::test]
async fn a_domain_over_dynamic_facts_is_not_frozen_into_a_schema() {
    // `consent_module/1` is `dynamic`, so an enum rendered once at startup would be a promise
    // that goes stale on the first write. Completion answers it live instead.
    let writable = common::writable_mcp("domain_dynamic");
    let tools = rpc(&writable, "tools/list", json!({})).await;

    let tool = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["name"] == "put_consent_module")
        .expect("put_consent_module should be a tool on a writable server");
    assert!(
        tool["inputSchema"]["properties"]["Module"]
            .get("enum")
            .is_none(),
        "{tool}"
    );
}

#[tokio::test]
async fn completion_answers_an_argument_from_the_knowledge_base() {
    // The distinctive thing a Prolog-backed MCP server can do: the admissible values of `Module`
    // are not a list somebody maintained, they are whatever `consent_module/1` currently says.
    let writable = common::writable_mcp("domain_completion");

    let completion = rpc(
        &writable,
        "completion/complete",
        json!({
            "ref": { "type": "ref/tool", "name": "put_consent_module" },
            "argument": { "name": "Module", "value": "" },
        }),
    )
    .await;
    assert_eq!(
        completion["completion"]["values"],
        json!(["research", "biobank"])
    );
    assert_eq!(completion["completion"]["hasMore"], false);

    // Prefix-filtered.
    let filtered = rpc(
        &writable,
        "completion/complete",
        json!({
            "ref": { "type": "ref/tool", "name": "put_consent_module" },
            "argument": { "name": "Module", "value": "res" },
        }),
    )
    .await;
    assert_eq!(filtered["completion"]["values"], json!(["research"]));

    // A newly added member is completable straight away, which is the point of asking live.
    rpc(
        &writable,
        "tools/call",
        json!({
            "name": "put_consent_module",
            "arguments": { "Module": "genomics" },
        }),
    )
    .await;
    let after = rpc(
        &writable,
        "completion/complete",
        json!({
            "ref": { "type": "ref/tool", "name": "put_consent_module" },
            "argument": { "name": "Module", "value": "gen" },
        }),
    )
    .await;
    assert_eq!(after["completion"]["values"], json!(["genomics"]));
}

#[tokio::test]
async fn an_argument_with_no_domain_completes_to_nothing_rather_than_failing() {
    let server = mcp_server("api_test.pl");
    let completion = rpc(
        &server,
        "completion/complete",
        json!({
            "ref": { "type": "ref/tool", "name": "list_length" },
            "argument": { "name": "List", "value": "" },
        }),
    )
    .await;

    assert_eq!(completion["completion"]["values"], json!([]));
    assert_eq!(completion["completion"]["total"], 0);
}

// ---------------------------------------------------------------------------------------------
// The orientation prompt
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_prompt_explains_what_the_knowledge_base_is_for() {
    let server = mcp_server("api_test.pl");

    let listed = rpc(&server, "prompts/list", json!({})).await;
    assert_eq!(listed["prompts"][0]["name"], "knowledge_base_overview");

    let prompt = rpc(
        &server,
        "prompts/get",
        json!({ "name": "knowledge_base_overview" }),
    )
    .await;
    let text = prompt["messages"][0]["content"]["text"]
        .as_str()
        .unwrap()
        .to_string();

    assert!(text.contains("API Test Fixture"), "{text}");
    // It names the collections and says to read them rather than call them.
    assert!(text.contains("charon://collection/criteria"), "{text}");
    assert!(text.contains("list_length"), "{text}");
}
