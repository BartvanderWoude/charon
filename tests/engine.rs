//! The engine underneath both interfaces: how many machines answer, and what a term can carry.

mod common;

use std::sync::Arc;

use charon::prolog::KnowledgeBase;
use charon::service::{Config, Service};
use common::{Persisting, Scratch, http_server};
use serde_json::{Value, json};

// ---------------------------------------------------------------------------------------------
// Several machines, but only where several machines can agree
// ---------------------------------------------------------------------------------------------

const PURE: &str = "%! double(+X:int, -Y:int) is det.\ndouble(X, Y) :- Y is X * 2.\n";
const SELF_MODIFYING: &str = ":- dynamic(seen/1).\n\
     %! get_seen(-X:atom) is nondet.\nget_seen(X) :- seen(X).\n\
     %! note_it(+X:atom) is det.\nnote_it(X) :- assertz(seen(X)).\n";

#[test]
fn a_knowledge_base_that_cannot_change_answers_from_several_machines() {
    // A machine is a mutex, so with one of them two callers asking unrelated questions of a
    // large rule base wait for each other for no reason at all.
    let scratch = Scratch::new("pool_pure", PURE);
    let service = Service::open(
        &scratch.0,
        Config {
            interpreters: 4,
            ..Config::default()
        },
    )
    .expect("should start");

    assert_eq!(service.catalog().width(), 4);
}

#[test]
fn a_knowledge_base_that_modifies_itself_keeps_one_machine() {
    // `assertz` commits to the machine that ran it and the others never hear about it, so a pool
    // here would mean two callers disagreeing about what is true. Decided from the source rather
    // than from a flag, so it is not something anyone can switch on incorrectly.
    let scratch = Scratch::new("pool_dynamic", SELF_MODIFYING);
    let service = Service::open(
        &scratch.0,
        Config {
            interpreters: 4,
            ..Config::default()
        },
    )
    .expect("should start");

    assert_eq!(service.catalog().width(), 1);
    assert!(KnowledgeBase::load(&scratch.0).unwrap().is_self_modifying());
    assert!(
        !KnowledgeBase::load(&Scratch::new("pool_pure_probe", PURE).0)
            .unwrap()
            .is_self_modifying()
    );
}

#[test]
fn persistence_forces_a_single_machine() {
    // Even for a knowledge base that looks pure: `--persist` mirrors one machine's facts to the
    // file, and there would be no answer to which machine that should be.
    let scratch = Scratch::new("pool_persist", PURE);
    let service = Service::open(
        &scratch.0,
        Config {
            interpreters: 4,
            persist: true,
            ..Config::default()
        },
    )
    .expect("should start");

    assert_eq!(service.catalog().width(), 1);
}

#[tokio::test]
async fn a_pool_answers_the_same_thing_every_machine_would() {
    // Every machine is consulted from the same source, so which one answers must not be
    // observable. Enough concurrent calls to be sure more than one was used.
    let scratch = Scratch::new("pool_answers", PURE);
    let service = Arc::new(
        Service::open(
            &scratch.0,
            Config {
                interpreters: 4,
                ..Config::default()
            },
        )
        .expect("should start"),
    );
    let catalog = service.catalog();

    let mut calls = Vec::new();
    for value in 0..32 {
        let arguments = json!({ "X": value }).as_object().unwrap().clone();
        calls.push(Arc::clone(&service).call(
            Arc::clone(&catalog),
            "double".to_string(),
            arguments,
        ));
    }

    for (value, outcome) in futures_join(calls).await.into_iter().enumerate() {
        assert_eq!(
            outcome.expect("each call should answer").value,
            json!(value * 2)
        );
    }
}

/// Awaits every future in order. A tiny local `join_all`, so the test needs no new dependency.
async fn futures_join<F: Future>(futures: Vec<F>) -> Vec<F::Output> {
    let mut results = Vec::with_capacity(futures.len());
    for future in futures {
        results.push(future.await);
    }
    results
}

// ---------------------------------------------------------------------------------------------
// Compound terms
// ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn a_compound_output_keeps_its_structure() {
    // Before this, every output was a scalar or a list of scalars, so a rule with something
    // structured to say had to flatten itself into parallel arguments.
    let server = http_server("api_test.pl");
    let response = server
        .get("/prescribe")
        .add_query_param("Drug", "aspirin")
        .await;

    response.assert_status_ok();
    response.assert_json(&json!({ "functor": "dose", "args": [100, "mg"] }));
}

#[tokio::test]
async fn a_compound_argument_round_trips() {
    // The encoding an answer comes back in is the encoding an argument goes out in, so a caller
    // can hand a term straight back without translating it.
    let server = http_server("api_test.pl");
    // `describe_term` carries no verb prefix, so it is a GET — and a structured argument
    // reaches a GET through the query string, as JSON text.
    let response = server
        .get("/describe_term")
        .add_query_param("Term", r#"{"functor":"dose","args":[100,"mg"]}"#)
        .await;

    response.assert_status_ok();
    response.assert_json(&json!({ "Functor": "dose", "Arity": 2 }));
}

#[tokio::test]
async fn a_compound_argument_is_documented_as_an_object() {
    let server = http_server("api_test.pl");
    let document: Value = server.get("/openapi.json").await.json();

    let schema = &document["paths"]["/prescribe"]["get"]["responses"]["200"]["content"]["application/json"]
        ["schema"];
    // `semidet`, so the schema admits the term or null.
    let alternatives = schema["oneOf"].as_array().unwrap();
    assert!(
        alternatives
            .iter()
            .any(|alternative| alternative["properties"]["functor"]["type"] == "string"),
        "{schema}"
    );
}

#[tokio::test]
async fn a_malformed_compound_argument_is_named() {
    let server = http_server("api_test.pl");
    let response = server
        .get("/describe_term")
        .add_query_param("Term", r#"{"args":[1]}"#)
        .await;

    response.assert_status(axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    let body: Value = response.json();
    assert_eq!(body["type"], "urn:charon:argument-invalid");
    assert_eq!(body["argument"], "Term");
}

#[tokio::test]
async fn a_persisted_compound_fact_survives_a_reload() {
    // Terms written back to the file have to be readable again, which is the whole point of
    // rendering them with `quoted(true)`.
    let persisting = Persisting::source(
        "compound_persist",
        ":- dynamic(shape/1).\n\n\
         %! get_shape(-S:compound) is nondet.\nget_shape(S) :- shape(S).\n\n\
         %! put_shape(+S:compound) is det.\nput_shape(S) :- assertz(shape(S)).\n",
    );
    let server = persisting.http();

    server
        .put("/shape")
        .json(&json!({ "S": { "functor": "circle", "args": [3] } }))
        .await
        .assert_status_ok();

    assert!(
        persisting.scratch.read().contains("shape(circle(3))"),
        "{}",
        persisting.scratch.read()
    );

    persisting
        .service
        .reload()
        .expect("the rewritten file should still load");
    server
        .get("/shape")
        .await
        .assert_json(&json!([{ "functor": "circle", "args": [3] }]));
}
