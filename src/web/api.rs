use std::collections::HashMap;
use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use utoipa::openapi::{Info, InfoBuilder, OpenApiBuilder, PathsBuilder};

use crate::prolog::{Interpreter, ModuleDoc, Predicate};

use super::Endpoint;

/// Used for `Info.title`/`Info.description` when the knowledge base has no `/** <module> ... */`
/// comment (see `build_info`).
const DEFAULT_TITLE: &str = "Charon Knowledge Base API";
const DEFAULT_DESCRIPTION: &str = "Generated from a Prolog knowledge base: every endpoint \
     corresponds to a predicate documented with a PlDoc `%!` comment. GET the input arguments as \
     query parameters to run the query; the response is the bare result if the query has \
     exactly one solution, otherwise a JSON array with one entry per solution.";

pub struct Api {
    endpoints: Vec<Endpoint>,
    interpreter: Interpreter,
    module_doc: Option<ModuleDoc>,
}

#[derive(Clone)]
struct AppState {
    interpreter: Interpreter,
    endpoints: Arc<Vec<Endpoint>>,
    module_doc: Arc<Option<ModuleDoc>>,
}

impl Api {
    pub fn new(
        predicates: impl IntoIterator<Item = Predicate>,
        interpreter: Interpreter,
        module_doc: Option<ModuleDoc>,
    ) -> Self {
        Self {
            endpoints: predicates
                .into_iter()
                .filter_map(|predicate| Endpoint::try_from(predicate).ok())
                .collect(),
            interpreter,
            module_doc,
        }
    }

    pub fn routes(self) -> axum::Router {
        let state = AppState {
            interpreter: self.interpreter,
            endpoints: Arc::new(self.endpoints),
            module_doc: Arc::new(self.module_doc),
        };

        axum::Router::new()
            .route("/openapi.json", axum::routing::get(get_openapi_json))
            .route("/{predicate}", axum::routing::get(call_predicate))
            .with_state(state)
    }

    /// Generate an OpenAPI JSON for the given endpoints.
    pub fn openapi_json(&self) -> String {
        build_openapi_json(&self.endpoints, self.module_doc.as_ref())
    }
}

/// Builds the document's `Info`: `title` and `description` come from the knowledge base's
/// `/** <module> Title ... */` comment when it has one (see
/// <https://www.swi-prolog.org/pldoc/man?section=sectioncomments>), falling back to a generic
/// default otherwise.
fn build_info(module_doc: Option<&ModuleDoc>) -> Info {
    let title = module_doc
        .and_then(|module_doc| module_doc.title.clone())
        .unwrap_or_else(|| DEFAULT_TITLE.to_string());
    let description = module_doc
        .and_then(|module_doc| module_doc.doc.render())
        .unwrap_or_else(|| DEFAULT_DESCRIPTION.to_string());

    InfoBuilder::new()
        .title(title)
        .description(Some(description))
        .version(env!("CARGO_PKG_VERSION"))
        .build()
}

fn build_openapi_json(endpoints: &[Endpoint], module_doc: Option<&ModuleDoc>) -> String {
    let mut paths = PathsBuilder::new();
    for endpoint in endpoints.iter() {
        paths = endpoint.add(paths);
    }

    OpenApiBuilder::new()
        .info(build_info(module_doc))
        .paths(paths)
        .build()
        .to_json()
        .expect("valid json")
}

async fn get_openapi_json(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        build_openapi_json(&state.endpoints, state.module_doc.as_ref().as_ref()),
    )
}

/// Calls the Prolog predicate named by the path, feeding its input arguments from the request's
/// query parameters. Returns the query's solutions: the bare value/object directly if there is
/// exactly one, otherwise a JSON array with one element per solution (see
/// `Endpoint::finalize_response`).
async fn call_predicate(
    State(state): State<AppState>,
    Path(predicate): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Response {
    let Some(endpoint) = state
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name() == predicate)
    else {
        return json_error(
            StatusCode::NOT_FOUND,
            format!("unknown predicate: {predicate}"),
        );
    };

    let query = match endpoint.build_query(&params) {
        Ok(query) => query,
        Err(error) => return json_error(StatusCode::BAD_REQUEST, error.to_string()),
    };

    let mut values = Vec::new();
    for answer in state.interpreter.query(&query) {
        match answer {
            Ok(value) => values.push(endpoint.extract_outputs(value)),
            Err(error) => return json_error(StatusCode::BAD_REQUEST, error.dump()),
        }
    }

    (
        [(header::CONTENT_TYPE, "application/json")],
        Endpoint::finalize_response(values).dump(),
    )
        .into_response()
}

/// Builds a `{"error": message}` JSON response, matching the error schema documented for every
/// endpoint's non-200 responses (see `Endpoint::error_response`).
fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    let body = json::object! { error: message.into() };
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.dump(),
    )
        .into_response()
}
