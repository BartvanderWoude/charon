//! The HTTP adapter: routing, argument extraction, and status codes.

mod cors;
pub mod openapi;
mod origin;
mod problem;
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

use crate::service::{KeyError, Operation, Service, Verb, value_from_text};

use problem::{Kind, Problem};

pub use origin::is_local_origin;
pub use problem::PROBLEM_JSON;
pub use serve::{ServeError, serve, serve_reloadable};

/// Set when the solution cap cut an answer short, so a caller can tell "these are all the
/// solutions" from "these are the first N". A header rather than a body field: the response
/// schema promised by the OpenAPI document is exact, and wrapping the body in an envelope only
/// when truncation happened would make it a `oneOf` again.
pub const TRUNCATED_HEADER: &str = "x-charon-truncated";

/// Builds the REST router: one route per predicate path, plus the OpenAPI document.
///
/// The MCP endpoint is deliberately *not* mounted here. Which interface a process speaks is
/// chosen on the command line, so that what a given port answers is a property of how the server
/// was started rather than something a client discovers by probing.
pub fn routes(service: Arc<Service>) -> Router {
    Router::new()
        .route("/", routing::get(index))
        .route("/openapi.json", routing::get(descriptor))
        .route(
            "/{predicate}",
            routing::get(call)
                .put(call)
                .post(call)
                .delete(call)
                .patch(call),
        )
        // One member of a collection. A wildcard because a key may have several columns
        // (`/pairs/{Left}/{Right}`); a key value containing a `/` cannot be addressed, which is
        // the ordinary limitation of putting an identifier in a path.
        .route(
            "/{collection}/{*key}",
            routing::get(member).put(member).delete(member),
        )
        .with_state(Api {
            service,
            openapi: Arc::new(openapi::Cache::default()),
        })
        // Outside the routes, so that it also sees `OPTIONS` — a verb no predicate is exposed
        // under, and which the router would therefore answer with 405.
        .layer(axum::middleware::from_fn(cors::allow_local_pages))
}

/// What every handler is given: the service, and the document rendered from its catalog.
#[derive(Clone)]
struct Api {
    service: Arc<Service>,
    openapi: Arc<openapi::Cache>,
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

/// A plain index of what this server answers.
///
/// Exists so that opening the root in a browser explains the server rather than 404ing. It is
/// also the only place that says out loud which operations are being *withheld* for want of
/// `--persist` — the OpenAPI document deliberately does not mention them, because it describes
/// what can be called.
async fn index(State(state): State<Api>) -> Response {
    let catalog = state.service.catalog();

    let operations: Vec<Value> = catalog
        .available()
        .map(|operation| {
            json!({
                "operation": operation.name(),
                "method": operation.verb().as_str().to_uppercase(),
                "path": format!("/{}", operation.path()),
                "summary": operation.summary(),
            })
        })
        .collect();
    let withheld: Vec<Value> = catalog
        .withheld()
        .map(|operation| {
            json!({
                "operation": operation.name(),
                "reason": "changes the knowledge base; start charon with --persist to expose it",
            })
        })
        .collect();

    let mut body = json!({
        "title": catalog.title(),
        "description": catalog.description(),
        "version": env!("CARGO_PKG_VERSION"),
        "openapi": "/openapi.json",
        "operations": operations,
    });
    if !withheld.is_empty() {
        body["withheld"] = json!(withheld);
    }

    (
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// Serves the OpenAPI document, with an ETag so a docs UI polling it gets a 304 rather than the
/// whole document again.
async fn descriptor(State(state): State<Api>, headers: HeaderMap) -> Response {
    let document = state.openapi.get(&state.service.catalog());

    if headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value == document.etag)
    {
        return StatusCode::NOT_MODIFIED.into_response();
    }

    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ETAG, document.etag.as_str()),
        ],
        document.json.to_string(),
    )
        .into_response()
}

async fn call(
    State(Api { service, .. }): State<Api>,
    Path(path): Path<String>,
    method: Method,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let catalog = service.catalog();

    let Some(verb) = verb_for(&method) else {
        return Problem::new(
            Kind::UnsupportedMethod,
            format!("charon does not route {method} requests"),
        )
        .into_response();
    };

    // A HEAD request runs the query, exactly as a GET would. That is deliberate rather than
    // wasteful: HEAD promises the headers a GET would return, and answering it without asking
    // the knowledge base would report success for a query that in fact fails.
    let operation = match catalog.route(&path, verb) {
        Ok(operation) => operation,
        Err(cause) => return problem::for_route(&path, cause).into_response(),
    };

    let arguments = match arguments_for(operation, &query, &headers, &body) {
        Ok(arguments) => arguments,
        Err(problem) => return problem.into_response(),
    };

    let name = operation.name().to_string();
    match Arc::clone(&service)
        .call(Arc::clone(&catalog), name, arguments)
        .await
    {
        Ok(outcome) => {
            let mut response = (
                [(header::CONTENT_TYPE, "application/json")],
                outcome.value.to_string(),
            )
                .into_response();
            if outcome.truncated {
                response
                    .headers_mut()
                    .insert(TRUNCATED_HEADER, header::HeaderValue::from_static("true"));
            }
            response
        }
        Err(cause) => problem::for_call(&cause).into_response(),
    }
}

/// One member of a collection, addressed by its key.
async fn member(
    State(Api { service, .. }): State<Api>,
    Path((name, key)): Path<(String, String)>,
    method: Method,
    Query(query): Query<Vec<(String, String)>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let catalog = service.catalog();

    let Some(verb) = verb_for(&method) else {
        return Problem::new(
            Kind::UnsupportedMethod,
            format!("charon does not route {method} requests"),
        )
        .into_response();
    };

    let Some(collection) = catalog.collection(&name) else {
        return Problem::new(
            Kind::NoSuchPath,
            format!("no collection is exposed at /{name}"),
        )
        .with("path", json!(format!("/{name}")))
        .into_response();
    };

    // Which verbs this member answers depends both on whether the facts can change at all and
    // on whether this process was started to allow changes.
    let allowed: Vec<Verb> = collection
        .item_verbs()
        .into_iter()
        .filter(|verb| {
            collection
                .item_operation(*verb)
                .and_then(|name| catalog.find(&name))
                .is_some_and(|operation| catalog.is_available(operation))
        })
        .collect();

    let operation = match collection
        .item_operation(verb)
        .and_then(|name| catalog.find(&name))
    {
        Some(operation) if catalog.is_available(operation) => operation,
        // The route exists but this process declines to run it: say so, rather than pretending
        // the member is not addressable at all.
        Some(operation) => {
            return problem::for_call(&crate::service::CallError::Withheld {
                name: operation.name().to_string(),
            })
            .into_response();
        }
        None => {
            let allow = problem::allow_header(&allowed);
            return Problem::new(
                Kind::MethodNotAllowed,
                format!("/{name} members do not answer this method; try {allow}"),
            )
            .with("allowed", json!(allow))
            .with_header(header::ALLOW, allow)
            .into_response();
        }
    };

    // A path segment is text, exactly like a query-string value, and both are read by the same
    // code — as is an MCP collection URI, so the three cannot disagree about what `null` is.
    let key_arguments = match collection.key_arguments(&key) {
        Ok(arguments) => arguments,
        // The key names a different number of columns than this collection has, so it does not
        // address a member at all.
        Err(KeyError::WrongLength { expected, found }) => {
            return Problem::new(
                Kind::NoSuchPath,
                format!("/{name} is keyed on {expected} column(s), but {found} were given"),
            )
            .into_response();
        }
        Err(KeyError::Invalid { argument, message }) => {
            return Problem::new(Kind::ArgumentInvalid, format!("`{argument}`: {message}"))
                .with("argument", json!(argument))
                .into_response();
        }
    };

    let mut arguments = match arguments_for(operation, &query, &headers, &body) {
        Ok(arguments) => arguments,
        Err(problem) => return problem.into_response(),
    };
    for (name, value) in key_arguments {
        if arguments.contains_key(&name) {
            return Problem::new(
                Kind::ArgumentDuplicated,
                format!("`{name}` identifies the member and cannot also be given as an argument"),
            )
            .with("argument", json!(name))
            .into_response();
        }
        arguments.insert(name, value);
    }

    let operation_name = operation.name().to_string();
    match Arc::clone(&service)
        .call(Arc::clone(&catalog), operation_name, arguments)
        .await
    {
        // Every member route is `semidet` or `det`, so "no solution" means "no such member" —
        // which over HTTP is a 404 rather than a 200 carrying `null`.
        Ok(outcome) if is_absent(&outcome.value) => {
            Problem::new(Kind::NoSuchPath, format!("/{name} has no member {key}")).into_response()
        }
        Ok(outcome) => (
            [(header::CONTENT_TYPE, "application/json")],
            outcome.value.to_string(),
        )
            .into_response(),
        Err(cause) => problem::for_call(&cause).into_response(),
    }
}

/// Whether a member route's answer means "there is no such member".
///
/// A `semidet` operation with outputs answers `null` when it failed, and one without outputs
/// answers `false`. Neither can be confused with a found member: an atom column comes back as
/// the string `"false"`, never as the boolean.
fn is_absent(value: &Value) -> bool {
    matches!(value, Value::Null | Value::Bool(false))
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
) -> Result<Map<String, Value>, Problem> {
    let mut arguments = json_body(headers, body)?;

    for (name, raw) in query {
        if arguments.contains_key(name) {
            return Err(Problem::new(
                Kind::ArgumentDuplicated,
                format!("argument `{name}` was given both in the query string and in the body"),
            )
            .with("argument", json!(name)));
        }
        // An unknown name is left in place rather than coerced: the service reports it, so a
        // misspelling is named once, in the same words, for both transports.
        let value = match operation.inputs().find(|param| param.name == *name) {
            Some(param) => value_from_text(&param.declared_type, raw).map_err(|message| {
                Problem::new(Kind::ArgumentInvalid, format!("`{name}`: {message}"))
                    .with("argument", json!(name))
            })?,
            None => Value::String(raw.clone()),
        };
        arguments.insert(name.clone(), value);
    }

    Ok(arguments)
}

/// Reads the request body as a JSON object of arguments. An empty body is simply no arguments.
fn json_body(headers: &HeaderMap, body: &Bytes) -> Result<Map<String, Value>, Problem> {
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
        return Err(Problem::new(
            Kind::UnsupportedMediaType,
            format!("expected `application/json`, got `{content_type}`"),
        ));
    }

    match serde_json::from_slice::<Value>(body) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(Problem::new(
            Kind::MalformedRequest,
            "the request body must be a JSON object of arguments",
        )),
        Err(cause) => Err(Problem::new(
            Kind::MalformedRequest,
            format!("the request body is not valid JSON: {cause}"),
        )),
    }
}
