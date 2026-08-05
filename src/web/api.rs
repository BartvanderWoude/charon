use std::sync::Arc;

use axum::{
    extract::{Path, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use json::JsonValue;
use utoipa::openapi::{OpenApiBuilder, PathsBuilder};

use crate::prolog::{Interpreter, Predicate};

use super::Endpoint;

pub struct Api {
    endpoints: Vec<Endpoint>,
    interpreter: Interpreter,
}

#[derive(Clone)]
struct AppState {
    interpreter: Interpreter,
    endpoints: Arc<Vec<Endpoint>>,
}

impl Api {
    pub fn new(predicates: impl IntoIterator<Item = Predicate>, interpreter: Interpreter) -> Self {
        Self {
            endpoints: predicates
                .into_iter()
                .filter_map(|predicate| Endpoint::try_from(predicate).ok())
                .collect(),
            interpreter,
        }
    }

    pub fn routes(self) -> axum::Router {
        let state = AppState {
            interpreter: self.interpreter,
            endpoints: Arc::new(self.endpoints),
        };

        axum::Router::new()
            .route("/openapi.json", axum::routing::get(get_openapi_json))
            .route("/{predicate}", axum::routing::post(call_predicate))
            .with_state(state)
    }

    /// Generate an OpenAPI JSON for the given endpoints.
    pub fn openapi_json(&self) -> String {
        build_openapi_json(&self.endpoints)
    }
}

fn build_openapi_json(endpoints: &[Endpoint]) -> String {
    let mut paths = PathsBuilder::new();
    for endpoint in endpoints.iter() {
        paths = endpoint.add(paths);
    }
    OpenApiBuilder::new()
        .paths(paths)
        .build()
        .to_json()
        .expect("valid json")
}

async fn get_openapi_json(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        build_openapi_json(&state.endpoints),
    )
}

/// Calls the Prolog predicate named by the path, feeding its input arguments from the JSON
/// request body and returning every solution the query has as a JSON array (list of values).
async fn call_predicate(
    State(state): State<AppState>,
    Path(predicate): Path<String>,
    body: String,
) -> Response {
    let Some(endpoint) = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name() == predicate)
    else {
        return (
            StatusCode::NOT_FOUND,
            format!("unknown predicate: {predicate}"),
        )
            .into_response();
    };

    let input = if body.trim().is_empty() {
        JsonValue::new_object()
    } else {
        match json::parse(&body) {
            Ok(value) => value,
            Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        }
    };

    let query = match endpoint.build_query(&input) {
        Ok(query) => query,
        Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
    };

    let mut values = Vec::new();
    for answer in state.interpreter.query(&query) {
        match answer {
            Ok(value) => values.push(endpoint.extract_outputs(value)),
            Err(error) => return (StatusCode::BAD_REQUEST, error.dump()).into_response(),
        }
    }

    (
        [(header::CONTENT_TYPE, "application/json")],
        JsonValue::Array(values).dump(),
    )
        .into_response()
}
