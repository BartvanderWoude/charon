//! MCP over HTTP: the Streamable HTTP transport, as a router of its own.

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use serde_json::Value;

use crate::service::Service;

/// Builds the MCP router. `POST /mcp` is the whole interface — the REST routes are deliberately
/// absent, because which interface a process serves is chosen when it is started.
pub fn routes(service: Arc<Service>) -> Router {
    Router::new()
        .route("/mcp", routing::post(endpoint))
        .with_state(service)
}

async fn endpoint(State(service): State<Arc<Service>>, body: String) -> Response {
    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return json(StatusCode::BAD_REQUEST, &super::parse_error());
    };

    match super::handle(&service, &request).await {
        Some(response) => json(StatusCode::OK, &response),
        // A notification is acknowledged with 202 and an empty body.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

fn json(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}
