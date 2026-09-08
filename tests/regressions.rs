//! One test per defect found while evaluating Charon against real knowledge bases.
//!
//! Each test is named for the behaviour it pins and documents the failure it prevents, so a
//! regression reports *which* known bug came back rather than just that something broke.

mod common;

use std::fs;
use std::path::PathBuf;

use charon::prolog::KnowledgeBase;
use charon::service::{CatalogError, Config, Service, StartupError};
use common::{fixture, http_server};
use serde_json::{Value, json};

/// A scratch `.pl` file that removes itself.
struct Scratch(PathBuf);

impl Scratch {
    fn new(name: &str, source: &str) -> Self {
        let path = std::env::temp_dir().join(format!("charon_{name}_{}.pl", std::process::id()));
        fs::write(&path, source).unwrap();
        Scratch(path)
    }

    fn rewrite(&self, source: &str) {
        fs::write(&self.0, source).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

const GREETING: &str = "%! greeting(-Text:atom) is det.\n%\n%   A greeting.\n%\n%   @arg Text The greeting.\ngreeting(hello).\n";

// ---------------------------------------------------------------------------------------------
// D1 — a failed consult must not look like a successful one.
// ---------------------------------------------------------------------------------------------

#[test]
fn broken_clause_fails_startup_and_names_the_missing_predicate() {
    // `broken.pl` contains one clause whose arithmetic cannot be evaluated. That aborts scryer's
    // whole consult, so even `healthy/1` — a plain fact declared above it — is left undefined.
    // Charon used to start and bind regardless, then answer every request with an existence
    // error; startup must fail instead, naming what went missing.
    let error = Service::open(fixture("broken.pl"), Config::default())
        .expect_err("a knowledge base whose consult failed must not start");

    let StartupError::Catalog(CatalogError::PredicatesMissing(missing)) = error else {
        panic!("expected a missing-predicate failure, got {error:?}");
    };
    assert!(
        missing.iter().any(|indicator| indicator == "healthy/1"),
        "expected healthy/1 to be reported missing, got {missing:?}"
    );
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
    raised.assert_status_bad_request();
    assert!(
        raised.json::<Value>()["error"]
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
            .assert_status_bad_request();
        server.get("/ready").await.assert_status_ok();
    }
}

#[tokio::test]
async fn prolog_errors_are_rendered_in_prolog_syntax() {
    let server = http_server("api_test.pl");

    let response = server.get("/raises").add_query_param("X", "1").await;
    let message = response.json::<Value>()["error"]
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
    let server = http_server("api_test.pl");

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
    let etag_before = before.etag().to_string();

    scratch.rewrite(&format!(
        "{GREETING}\n%! farewell(-Text:atom) is det.\n%\n%   A farewell.\n%\n%   @arg Text The farewell.\nfarewell(bye).\n"
    ));
    service.reload().expect("the edited file should load");

    let after = service.catalog();
    assert!(after.find("farewell").is_some(), "new predicate visible");
    // The cached descriptors are rebuilt, not stale.
    assert_ne!(after.etag(), etag_before);
    assert!(after.openapi_json().contains("/farewell"));
    assert!(after.tools_json().contains("farewell"));
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
