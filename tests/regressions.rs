//! One test per defect found while evaluating Charon against real knowledge bases.
//!
//! Each test is named for the behaviour it pins and documents the failure it prevents, so a
//! regression reports *which* known bug came back rather than just that something broke.

mod common;

use std::fs;

use axum::http::StatusCode;
use charon::prolog::KnowledgeBase;
use charon::service::{CatalogError, Config, Service, StartupError};
use common::{Persisting, Scratch, fixture, http_server};
use serde_json::{Value, json};

const GREETING: &str = "%! greeting(-Text:atom) is det.\n%\n%   A greeting.\n%\n%   @arg Text The greeting.\ngreeting(hello).\n";

// ---------------------------------------------------------------------------------------------
// D1 — a failed consult must not look like a successful one.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_broken_clause_fails_startup_rather_than_binding_a_port() {
    // `broken.pl` contains one clause whose arithmetic cannot be evaluated. That aborts scryer's
    // whole consult, so even `healthy/1` — a plain fact declared above it — is left undefined.
    // Charon used to start and bind regardless, then answer every request with an existence
    // error; startup must fail instead.
    let error = Service::open(fixture("broken.pl"), Config::default())
        .expect_err("a knowledge base whose consult failed must not start");

    assert!(
        matches!(
            error,
            StartupError::Catalog(CatalogError::ConsultFailed)
                | StartupError::Catalog(CatalogError::PredicatesMissing(_))
        ),
        "expected a load failure, got {error:?}"
    );
    assert!(
        error.to_string().contains("did not load completely"),
        "{error}"
    );
}

#[test]
fn a_broken_clause_is_caught_even_where_probing_the_predicates_cannot_see_it() {
    // The blind spot behind the sentinel: the bad clause belongs to `colour/1`, which already has
    // a good clause, so `current_predicate(colour/1)` answers true and every per-predicate probe
    // passes. Found by editing a served knowledge base and reloading it — the server accepted the
    // edit and then answered `existence_error` to everything.
    let scratch = Scratch::new(
        "blind_spot",
        "%! colour(-C:atom) is multi.\ncolour(red).\ncolour(X) :- X is bogus + 1.\n",
    );

    let error = Service::open(&scratch.0, Config::default())
        .expect_err("an aborted consult must not start, however it aborted");
    assert!(
        matches!(error, StartupError::Catalog(CatalogError::ConsultFailed)),
        "got {error:?}"
    );
}

#[test]
fn a_reload_that_cannot_compile_leaves_the_previous_version_serving() {
    // The promise `SIGHUP` reloading depends on: a half-saved file must be an inconvenience, not
    // an outage.
    let scratch = Scratch::new("reload_broken", GREETING);
    let service = Service::open(&scratch.0, Config::default()).expect("should start");

    scratch.rewrite(&format!("{GREETING}broken(X) :- X is bogus + 1.\n"));
    assert!(service.reload().is_err());

    // Still answering, and still from the version that compiled.
    assert!(service.catalog().find("greeting").is_some());
}

#[test]
fn healthy_knowledge_base_starts() {
    // The counterpart to the test above: the probe must not produce false positives, or every
    // knowledge base would fail to start.
    assert!(Service::open(fixture("api_test.pl"), Config::default()).is_ok());
}

// ---------------------------------------------------------------------------------------------
// D2 — an exception must not poison the interpreter.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn exception_does_not_poison_later_requests() {
    let server = http_server("api_test.pl");

    // Healthy before.
    server.get("/ready").await.assert_status_ok();

    let raised = server.get("/raises").add_query_param("X", "1").await;
    // A term the knowledge base threw on purpose is about this request, not a defect in the
    // file, so it is a 422 rather than a 500.
    raised.assert_status(StatusCode::UNPROCESSABLE_ENTITY);
    assert!(
        raised.json::<Value>()["detail"]
            .as_str()
            .unwrap()
            .contains("deliberate_test_error"),
        "the exception should be reported"
    );

    // Healthy after. Before the `catch/3` wrapper, every subsequent request — including ones
    // against completely unrelated predicates — kept returning that same stale exception.
    let after = server.get("/ready").await;
    after.assert_status_ok();
    after.assert_json(&json!(true));

    let length = server
        .get("/list_length")
        .add_query_param("List", "[1,2,3]")
        .await;
    length.assert_status_ok();
    length.assert_json(&json!(3));
}

#[tokio::test]
async fn repeated_exceptions_stay_contained() {
    let server = http_server("api_test.pl");

    for _ in 0..5 {
        server
            .get("/raises")
            .add_query_param("X", "1")
            .await
            .assert_status(StatusCode::UNPROCESSABLE_ENTITY);
        server.get("/ready").await.assert_status_ok();
    }
}

#[tokio::test]
async fn prolog_errors_are_rendered_in_prolog_syntax() {
    let server = http_server("api_test.pl");

    let response = server.get("/raises").add_query_param("X", "1").await;
    let message = response.json::<Value>()["detail"]
        .as_str()
        .unwrap()
        .to_string();

    // Not the nested `{"functor": …, "args": […]}` encoding, which was unreadable by the time
    // an error term nested three deep.
    assert_eq!(message, "deliberate_test_error");
    assert!(!message.contains("functor"), "got {message}");
}

// ---------------------------------------------------------------------------------------------
// D3 — no trailing `false` in non-deterministic responses.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_nondet_predicate_with_one_solution_has_no_false_terminator() {
    let server = http_server("api_test.pl");

    // `pim_hits` leaves a choicepoint that ultimately fails, so scryer emits a final `False`
    // leaf answer. Counting that as a solution turned a single hit into `[{...}, false]`.
    let response = server
        .get("/pim_hits")
        .add_query_param("Drugs", "[\"diazepam\"]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!([{ "Drug": "diazepam", "Reason": "sedation" }]));
}

#[tokio::test]
async fn a_nondet_predicate_with_several_solutions_lists_only_them() {
    let server = http_server("api_test.pl");

    let response = server
        .get("/pim_hits")
        .add_query_param("Drugs", "[\"diazepam\",\"digoxin\"]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!([
        { "Drug": "diazepam", "Reason": "sedation" },
        { "Drug": "digoxin", "Reason": "toxicity" },
    ]));
}

#[tokio::test]
async fn a_failing_nondet_query_returns_an_empty_array() {
    let server = http_server("api_test.pl");

    let response = server
        .get("/pim_hits")
        .add_query_param("Drugs", "[\"aspirin\"]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!([]));
}

// ---------------------------------------------------------------------------------------------
// D4 — response key order follows the mode line, not a hash.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn response_key_order_is_stable_and_follows_declaration_order() {
    let server = http_server("api_test.pl");

    // Iterating a `HashMap` used to randomise this per process, which defeats byte-comparison
    // of responses and any claim that the same question gives the same answer.
    //
    // `ordered_outputs` declares Zeta before Alpha, so this distinguishes a genuinely
    // declaration-ordered response from one that merely comes out alphabetically.
    for _ in 0..5 {
        let response = server
            .get("/ordered_outputs")
            .add_query_param("X", "7")
            .await;
        assert_eq!(response.text(), r#"{"Zeta":7,"Alpha":7}"#);
    }
}

#[tokio::test]
async fn openapi_property_and_parameter_order_is_stable() {
    let server = http_server("api_test.pl");

    let first = server.get("/openapi.json").await.text();
    let second = server.get("/openapi.json").await.text();
    assert_eq!(first, second);

    let zeta = first.find("\"Zeta\"").expect("Zeta is documented");
    let alpha = first.find("\"Alpha\"").expect("Alpha is documented");
    assert!(
        zeta < alpha,
        "schema properties should follow the mode line"
    );
}

// ---------------------------------------------------------------------------------------------
// D5 — one JSON string, one Prolog term, whatever carried it.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn list_of_atoms_unifies_with_plain_atom_facts() {
    let server = http_server("api_test.pl");

    // The `key(Name, Key) :- atom(Name), !, atom_chars(Name, Key).` workaround existed because
    // a string inside a list arrived as a character list while the same string as a scalar
    // arrived as an atom. `list(atom)` now says which is meant.
    let response = server
        .get("/pim_hits")
        .add_query_param("Drugs", "[\"digoxin\"]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!([{ "Drug": "digoxin", "Reason": "toxicity" }]));
}

#[tokio::test]
async fn scalar_atom_matches_the_same_facts_as_a_list_element() {
    // Asserting needs a writable server: `put_fruit` changes the knowledge base, so a read-only
    // process withholds it entirely.
    let writable = common::writable_http("regress_scalar_atom");
    let server = &*writable;

    server
        .put("/fruit")
        .add_query_param("Fruit", "cherry")
        .await
        .assert_status_ok();

    let body: Value = server.get("/fruit").await.json();
    assert!(
        body.as_array().unwrap().contains(&json!("cherry")),
        "asserted atom should be readable back, got {body}"
    );
}

#[tokio::test]
async fn string_arguments_are_character_lists_not_atoms() {
    let server = http_server("api_test.pl");

    let response = server
        .get("/text_length")
        .add_query_param("Text", "hello")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!(5));
}

#[tokio::test]
async fn typed_list_elements_stay_numeric() {
    let server = http_server("api_test.pl");

    let response = server
        .get("/sum_all")
        .add_query_param("Values", "[1,2,3,4]")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!(10));
}

// ---------------------------------------------------------------------------------------------
// D6 — an argument cannot break out of the goal it is interpolated into.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_argument_that_closes_its_own_quote_is_escaped_not_executed() {
    let server = http_server("api_test.pl");

    // Generated goals are Prolog *source*, so an unescaped quote in an argument ends the term
    // and whatever follows is read as more Prolog.
    let hostile = "x'), assertz(fruit(pwned)), echo_atom('y";
    let response = server
        .get("/echo_atom")
        .add_query_param("Name", hostile)
        .await;

    response.assert_status_ok();
    response.assert_json(&json!(hostile));

    // Nothing was executed on the way past.
    let fruit: Value = server.get("/fruit").await.json();
    assert!(
        !fruit.as_array().unwrap().contains(&json!("pwned")),
        "got {fruit}"
    );
}

#[tokio::test]
async fn an_argument_containing_a_newline_does_not_end_the_term() {
    let server = http_server("api_test.pl");

    // A quoted Prolog atom cannot carry a literal newline; before it was escaped, any text with
    // one in it became a syntax error rather than an answer.
    let response = server
        .get("/echo_atom")
        .add_query_param("Name", "line one\nline two")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!("line one\nline two"));
}

// ---------------------------------------------------------------------------------------------
// D7 — a solution's shape does not depend on how many solutions there were.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_nondet_predicate_answers_with_an_array_even_for_one_solution() {
    let server = http_server("api_test.pl");

    // Unwrapping whenever exactly one solution came back made one solution binding a list
    // indistinguishable from several solutions binding its elements. The shape now follows the
    // declared determinism, so it is settled before any answer exists.
    let one: Value = server
        .get("/pim_hits")
        .add_query_param("Drugs", "[\"diazepam\"]")
        .await
        .json();
    assert!(one.is_array(), "got {one}");

    // …while a `det` predicate, which promises exactly one, is still unwrapped.
    let det: Value = server
        .get("/list_length")
        .add_query_param("List", "[1,2,3]")
        .await
        .json();
    assert_eq!(det, json!(3));
}

// ---------------------------------------------------------------------------------------------
// Reload: one path for hot-reload, cache invalidation, and machine recovery.
// ---------------------------------------------------------------------------------------------

#[test]
fn reload_picks_up_an_edited_file_and_rebuilds_the_cached_descriptors() {
    let scratch = Scratch::new("reload", GREETING);
    let service = Service::open(&scratch.0, Config::default()).expect("should start");

    let before = service.catalog();
    assert!(before.find("greeting").is_some());
    assert!(before.find("farewell").is_none());
    let revision_before = before.revision();

    scratch.rewrite(&format!(
        "{GREETING}\n%! farewell(-Text:atom) is det.\n%\n%   A farewell.\n%\n%   @arg Text The farewell.\nfarewell(bye).\n"
    ));
    service.reload().expect("the edited file should load");

    let after = service.catalog();
    assert!(after.find("farewell").is_some(), "new predicate visible");
    // The revision moved on, which is what makes each adapter re-render its descriptors.
    assert_ne!(after.revision(), revision_before);
    assert!(charon::http::openapi::document(&after).contains("/farewell"));
    assert!(charon::mcp::tools(&after).to_string().contains("farewell"));
}

#[test]
fn a_failed_reload_leaves_the_previous_catalog_serving() {
    let scratch = Scratch::new("reload_bad", GREETING);
    let service = Service::open(&scratch.0, Config::default()).expect("should start");

    // Same breakage as broken.pl: an unevaluable arithmetic expression aborts the consult.
    scratch.rewrite(&format!("{GREETING}\nbroken(_, X) :- X is foo + 1.\n"));

    assert!(
        service.reload().is_err(),
        "a broken edit must not be adopted"
    );
    // A bad edit degrades to "the old version keeps serving", not to a dead server.
    assert!(service.catalog().find("greeting").is_some());
}

#[test]
fn a_reload_that_cannot_read_the_file_leaves_the_previous_catalog_serving() {
    let scratch = Scratch::new("reload_gone", GREETING);
    let service = Service::open(&scratch.0, Config::default()).expect("should start");

    fs::remove_file(&scratch.0).unwrap();

    assert!(matches!(service.reload(), Err(StartupError::Io(_))));
    assert!(service.catalog().find("greeting").is_some());
}

// ---------------------------------------------------------------------------------------------
// Persistence through the HTTP interface.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn asserted_facts_reach_the_file_when_persistence_is_on() {
    let scratch = Scratch::new(
        "persist_http",
        ":- dynamic(fruit/1).\nfruit(apple).\n\n\
         %! get_fruit(-Fruit:atom) is multi.\nget_fruit(F) :- fruit(F).\n\n\
         %! put_fruit(+Fruit:atom) is det.\nput_fruit(F) :- assertz(fruit(F)).\n",
    );
    let service = std::sync::Arc::new(
        Service::open(&scratch.0, Config::persisting(true)).expect("should start"),
    );
    let server = axum_test::TestServer::new(charon::http::routes(service));

    server
        .put("/fruit")
        .json(&json!({ "Fruit": "banana" }))
        .await
        .assert_status_ok();

    let on_disk = fs::read_to_string(&scratch.0).unwrap();
    assert!(on_disk.contains("fruit(banana)."), "{on_disk}");
}

#[test]
fn persisting_to_a_file_that_cannot_be_written_fails_at_startup() {
    let scratch = Scratch::new("persist_readonly", "%! ready is semidet.\nready.\n");
    let mut permissions = fs::metadata(&scratch.0).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&scratch.0, permissions).unwrap();

    // Discovering this after the first `assertz` means the change is live in the machine and
    // lost on restart, with only a log line to say so.
    let error = Service::open(&scratch.0, Config::persisting(true))
        .expect_err("persisting to a read-only file cannot work");
    assert!(matches!(error, StartupError::Io(_)), "got {error:?}");

    // Serving it read-only is still perfectly fine.
    assert!(Service::open(&scratch.0, Config::default()).is_ok());

    let mut permissions = fs::metadata(&scratch.0).unwrap().permissions();
    #[allow(clippy::permissions_set_readonly_false)]
    permissions.set_readonly(false);
    fs::set_permissions(&scratch.0, permissions).unwrap();
}

// ---------------------------------------------------------------------------------------------
// Startup diagnostics.
// ---------------------------------------------------------------------------------------------

#[test]
fn a_predicate_that_cannot_be_exposed_is_reported_rather_than_missing() {
    let knowledge_base = KnowledgeBase::load(fixture("api_test.pl")).unwrap();
    let service = Service::new(knowledge_base, Config::default()).unwrap();

    let catalog = service.catalog();
    let (name, reason) = catalog
        .rejected()
        .iter()
        .find(|(name, _)| name == "hidden")
        .expect("`hidden` has a `?` mode, which cannot be exposed");
    assert_eq!(name, "hidden");
    assert!(reason.to_string().contains('?'), "{reason}");
}

// ---------------------------------------------------------------------------------------------
// Changing operations are exposed only when the server can write the change down.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_withheld_operation_is_refused_rather_than_run() {
    // The defect this pins: without `--persist`, `PUT /fruit` used to *succeed* — the `assertz`
    // committed to the running machine and nobody wrote it down, so the server quietly diverged
    // from the file it claims to serve until the next restart threw the change away.
    let server = http_server("api_test.pl");

    let refused = server
        .put("/fruit")
        .json(&json!({ "Fruit": "banana" }))
        .await;
    refused.assert_status(StatusCode::FORBIDDEN);

    let body: Value = refused.json();
    assert_eq!(body["type"], "urn:charon:withheld");
    assert!(
        body["detail"].as_str().unwrap().contains("--persist"),
        "the refusal must say what to change: {body}"
    );

    // And, the point of the whole exercise, nothing happened.
    server.get("/fruit").await.assert_json(&json!(["apple"]));
}

#[tokio::test]
async fn mutating_operations_are_absent_from_the_openapi_document_without_persistence() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    let fruit = &document["paths"]["/fruit"];
    assert!(fruit["get"].is_object(), "reading is always on offer");
    assert!(fruit.get("put").is_none(), "{fruit}");
    assert!(fruit.get("delete").is_none(), "{fruit}");
    // `post_note` has no read-only counterpart, so its path disappears entirely.
    assert!(document["paths"].get("/note").is_none());
}

#[tokio::test]
async fn mutating_tools_are_absent_from_the_tool_list_without_persistence() {
    let server = common::mcp_server("api_test.pl");
    let response = server
        .post("/mcp")
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
        .await;

    let listed: Value = response.json();
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();

    assert!(names.contains(&"get_fruit"));
    for withheld in ["put_fruit", "delete_fruit", "post_note"] {
        assert!(!names.contains(&withheld), "{withheld} should be withheld");
    }
}

#[tokio::test]
async fn a_withheld_tool_says_why_rather_than_looking_broken() {
    // A model that is told "restart with --persist" can relay that to a person. A bare
    // `-32602 unknown tool` just looks like a server that does not work.
    let server = common::mcp_server("api_test.pl");
    let response: Value = server
        .post("/mcp")
        .json(&json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": { "name": "put_fruit", "arguments": { "Fruit": "banana" } },
        }))
        .await
        .json();

    let result = &response["result"];
    assert_eq!(result["isError"], true, "{response}");
    let text = result["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("--persist"), "{text}");
}

#[tokio::test]
async fn the_allow_header_never_advertises_a_method_that_would_be_refused() {
    // `/fruit` answers PUT and DELETE on a writable server. On a read-only one it answers only
    // GET, and saying otherwise would send a caller straight into a second failure.
    let server = http_server("api_test.pl");
    let response = server.post("/fruit").await;

    response.assert_status(StatusCode::METHOD_NOT_ALLOWED);
    let allow = response.headers()["allow"].to_str().unwrap().to_string();
    assert_eq!(allow, "GET, HEAD", "{allow}");
}

#[test]
fn persistence_decides_what_is_reachable_never_whether_the_file_is_valid() {
    // Two changing predicates answering to one name is a startup failure. It must be one
    // *whether or not* `--persist` was passed: a flag that decides what is reachable must never
    // be able to turn a server that starts into one that refuses to, or adding the flag later
    // would surface a failure the author never saw.
    let scratch = Scratch::new(
        "collide_mutating",
        "%! put_fruit(+F:atom) is det.\nput_fruit(_).\n\n\
         %! put_fruit(+G:atom) is det.\nput_fruit(_).\n",
    );

    for config in [Config::default(), Config::persisting(true)] {
        let persist = config.persist;
        let error = Service::open(&scratch.0, config)
            .expect_err("a duplicate name is a startup failure either way");
        assert!(
            matches!(error, StartupError::Catalog(CatalogError::DuplicateName(_))),
            "with persist={persist}: {error:?}"
        );
    }
}

// ---------------------------------------------------------------------------------------------
// An endless relation is capped rather than allowed to exhaust the process.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn an_endless_relation_is_capped_and_says_so() {
    // Before the cap, `GET /endless` collected solutions into a `Vec` until the process died,
    // holding the machine's lock throughout — so one such request took every other request with
    // it, on both interfaces.
    let persisting = Persisting::source(
        "endless_cap",
        "%! endless(-N:int) is nondet.\nendless(0).\nendless(N) :- endless(M), N is M + 1.\n",
    );
    let service = std::sync::Arc::new(
        Service::open(
            &persisting.scratch.0,
            Config {
                max_solutions: Some(5),
                ..Config::default()
            },
        )
        .expect("should start"),
    );
    let server = axum_test::TestServer::new(charon::http::routes(std::sync::Arc::clone(&service)));

    let response = server.get("/endless").await;
    response.assert_status_ok();
    assert_eq!(response.json::<Value>(), json!([0, 1, 2, 3, 4]));
    assert_eq!(
        response.headers()["x-charon-truncated"].to_str().unwrap(),
        "true",
        "a capped answer must not look like a complete one"
    );

    // And the machine survived being cut off, which is the part that is not obvious.
    let again = server.get("/endless").await;
    again.assert_status_ok();
    assert_eq!(again.json::<Value>(), json!([0, 1, 2, 3, 4]));
}

#[tokio::test]
async fn a_complete_answer_is_not_labelled_truncated() {
    let server = http_server("api_test.pl");
    let response = server.get("/color").await;

    response.assert_status_ok();
    assert!(
        response.headers().get("x-charon-truncated").is_none(),
        "three colours under a cap of 1000 is the whole answer"
    );
}

// ---------------------------------------------------------------------------------------------
// D8 — a `bool` output is answered as a boolean, because that is what its schema promises.
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_bool_output_answers_with_a_json_boolean_and_not_the_word() {
    let server = http_server("api_test.pl");

    // Prolog has no boolean type: `true` and `false` are ordinary atoms, and reading an answer
    // by the term's own shape turned both into JSON *strings*. The published schema for the
    // same argument said `"type": "boolean"` all along, so Charon was generating responses that
    // failed validation against its own description of itself — and a client that branched on
    // the value got a truthy string either way.
    let document: Value = server.get("/openapi.json").await.json();
    let schema = &document["paths"]["/is_even"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    assert_eq!(schema["type"], "boolean", "the document promises a boolean");

    let even: Value = server
        .get("/is_even")
        .add_query_param("Value", "4")
        .await
        .json();
    assert_eq!(even, json!(true), "and the answer must be one");

    let odd: Value = server
        .get("/is_even")
        .add_query_param("Value", "7")
        .await
        .json();
    assert_eq!(odd, json!(false));

    // Alongside another output the solution is an object, so the conversion has to reach into
    // it rather than only handle the unwrapped case. `Sign` is an atom and stays a string.
    let classified: Value = server
        .get("/classify")
        .add_query_param("Value", "-4")
        .await
        .json();
    assert_eq!(classified, json!({"Even": true, "Sign": "negative"}));

    // And it recurses through `list(bool)`, the way argument marshalling does on the way in.
    let parities: Value = server
        .get("/parities")
        .add_query_param("Values", "[1,2,3,4]")
        .await
        .json();
    assert_eq!(parities, json!([false, true, false, true]));
}

#[tokio::test]
async fn a_bool_output_bound_to_something_else_is_left_alone() {
    let server = http_server("api_test.pl");

    // `undecided/1` is declared `bool` and binds `maybe`. That is a defect in the knowledge
    // base, and the repair above must not paper over it by picking a boolean: the value comes
    // back as it was so that the mismatch stays visible to whoever wrote the mode line.
    let answer: Value = server.get("/undecided").await.json();
    assert_eq!(answer, json!("maybe"));
}
