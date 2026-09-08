//! The HTTP adapter: routing, argument extraction, and status codes.

mod coerce;
mod serve;

use std::sync::Arc;

use axum::{
    Router,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use serde_json::{Map, Value, json};

use crate::service::{CallError, Operation, RouteError, Service, Verb};

pub use serve::{ServeError, serve};

/// Builds the REST router: one route per predicate path, plus the OpenAPI document.
///
/// The MCP endpoint is deliberately *not* mounted here. Which interface a process speaks is
/// chosen on the command line, so that what a given port answers is a property of how the server
/// was started rather than something a client discovers by probing.
pub fn routes(service: Arc<Service>) -> Router {
    Router::new()
        .route("/openapi.json", routing::get(openapi))
        .route(
            "/{predicate}",
            routing::get(call)
                .put(call)
                .post(call)
                .delete(call)
                .patch(call),
        )
        .with_state(service)
}

/// The [`Verb`] a request method stands for, or `None` for a method Charon does not route.
pub fn verb_for(method: &Method) -> Option<Verb> {
    match *method {
        Method::GET | Method::HEAD => Some(Verb::Get),
        Method::PUT => Some(Verb::Put),
        Method::POST => Some(Verb::Post),
        Method::DELETE => Some(Verb::Delete),
        Method::PATCH => Some(Verb::Patch),
        _ => None,
    }
}

/// Serves the cached OpenAPI document, with an ETag so a docs UI polling it gets a 304 rather
/// than the whole document again.
async fn openapi(State(service): State<Arc<Service>>, headers: HeaderMap) -> Response {
    let catalog = service.catalog();

    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == catalog.etag())
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ETAG, catalog.etag()),
        ],
        catalog.openapi_json().to_string(),
    )
        .into_response()
}

async fn call(
    State(service): State<Arc<Service>>,
    Path(path): Path<String>,
    method: Method,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let catalog = service.catalog();

    let Some(verb) = verb_for(&method) else {
        return error(StatusCode::METHOD_NOT_ALLOWED, "unsupported HTTP method");
    };

    let operation = match catalog.route(&path, verb) {
        Ok(operation) => operation,
        Err(RouteError::NoSuchPath) => {
            return error(StatusCode::NOT_FOUND, format!("no predicate at /{path}"));
        }
        Err(RouteError::WrongVerb { allowed }) => return method_not_allowed(&path, &allowed),
    };

    let arguments = match arguments_for(operation, &query, &headers, &body) {
        Ok(arguments) => arguments,
        Err((status, message)) => return error(status, message),
    };

    let name = operation.name().to_string();
    match Arc::clone(&service)
        .call(Arc::clone(&catalog), name, arguments)
        .await
    {
        Ok(value) => (
            [(header::CONTENT_TYPE, "application/json")],
            value.to_string(),
        )
            .into_response(),
        Err(cause) => call_error(&cause),
    }
}

/// Collects an operation's arguments from wherever this request put them.
///
/// A JSON body carries values that are already typed; the query string carries text that has to
/// be read as the declared type. Both are accepted for every method — the OpenAPI document
/// picks one per method to *document*, but refusing the other would make `curl -G` and a plain
/// `--json` post gratuitously different — and an argument given twice is an error rather than a
/// silent precedence rule.
fn arguments_for(
    operation: &Operation,
    query: &[(String, String)],
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<Map<String, Value>, (StatusCode, String)> {
    let mut arguments = json_body(headers, body)?;

    for (name, raw) in query {
        if arguments.contains_key(name) {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("argument `{name}` was given both in the query string and in the body"),
            ));
        }
        // An unknown name is left in place rather than coerced: the service reports it, so a
        // misspelling is named once, in the same words, for both transports.
        let value = match operation.inputs().iter().find(|param| param.name == *name) {
            Some(param) => coerce::from_query(&param.declared_type, raw)
                .map_err(|message| (StatusCode::BAD_REQUEST, format!("`{name}`: {message}")))?,
            None => Value::String(raw.clone()),
        };
        arguments.insert(name.clone(), value);
    }

    Ok(arguments)
}

/// Reads the request body as a JSON object of arguments. An empty body is simply no arguments.
fn json_body(
    headers: &HeaderMap,
    body: &Bytes,
) -> Result<Map<String, Value>, (StatusCode, String)> {
    if body.is_empty() {
        return Ok(Map::new());
    }

    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("application/json");
    if !content_type
        .split(';')
        .next()
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("application/json"))
    {
        return Err((
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            format!("expected `application/json`, got `{content_type}`"),
        ));
    }

    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err((
            StatusCode::BAD_REQUEST,
            "the request body must be a JSON object of arguments".to_string(),
        )),
        Err(cause) => Err((
            StatusCode::BAD_REQUEST,
            format!("the request body is not valid JSON: {cause}"),
        )),
    }
}

fn method_not_allowed(path: &str, allowed: &[Verb]) -> Response {
    let allow = allowed
        .iter()
        .flat_map(|verb| match verb {
            // A route answering GET answers HEAD too, and `Allow` is meant to be exhaustive.
            Verb::Get => vec!["GET".to_string(), "HEAD".to_string()],
            other => vec![other.as_str().to_uppercase()],
        })
        .collect::<Vec<_>>()
        .join(", ");

    (
        StatusCode::METHOD_NOT_ALLOWED,
        [
            (header::CONTENT_TYPE, "application/json".to_string()),
            (header::ALLOW, allow.clone()),
        ],
        json!({ "error": format!("/{path} does not answer this method; try {allow}") }).to_string(),
    )
        .into_response()
}

fn call_error(cause: &CallError) -> Response {
    let status = match cause {
        CallError::UnknownOperation { .. } => StatusCode::NOT_FOUND,
        CallError::Argument(_) | CallError::Prolog(_) => StatusCode::BAD_REQUEST,
        CallError::Interrupted => StatusCode::INTERNAL_SERVER_ERROR,
    };
    error(status, cause.to_string())
}

fn error(status: StatusCode, message: impl Into<String>) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        json!({ "error": message.into() }).to_string(),
    )
        .into_response()
}
