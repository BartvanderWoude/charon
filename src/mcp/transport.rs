//! MCP over HTTP: the Streamable HTTP transport, as a router of its own.

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing,
};
use serde_json::Value;

use super::Server;

/// The header a client uses to pin the protocol revision after `initialize`.
pub const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";
/// The header carrying the session this request belongs to.
pub const SESSION_HEADER: &str = "mcp-session-id";

/// Builds the MCP router. `POST /mcp` is the whole interface — the REST routes are deliberately
/// absent, because which interface a process serves is chosen when it is started.
pub fn routes(server: Arc<Server>) -> Router {
    Router::new()
        .route("/mcp", routing::post(endpoint).get(no_stream))
        .with_state(server)
}

/// `GET /mcp` is where a client would open a server-sent-event stream.
///
/// Charon has nothing to push: the tool list is fixed for the life of a process, and a reload is
/// signalled by SIGHUP rather than discovered by a client. Answering 405 with `Allow: POST` says
/// so plainly; leaving the method unrouted would look like a broken deployment instead.
async fn no_stream() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::ALLOW, "POST"),
        ],
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": {
                "code": -32600,
                "message": "this server has no event stream; send requests with POST /mcp",
            },
        })
        .to_string(),
    )
        .into_response()
}

async fn endpoint(State(server): State<Arc<Server>>, headers: HeaderMap, body: String) -> Response {
    // A browser on another origin can be made to POST to a loopback port it could never read
    // from — unless the page is allowed to read the response, which is what a permissive server
    // grants by accident. Charon has no authentication at all, so refusing a cross-origin
    // request outright is the only thing standing between a knowledge base bound to localhost
    // and any web page the user happens to have open.
    if let Some(origin) = headers
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        && !is_local_origin(origin)
    {
        return refuse(
            StatusCode::FORBIDDEN,
            format!("cross-origin requests are refused; `{origin}` is not a local origin"),
        );
    }

    // A revision the server does not speak is refused rather than answered in a dialect the
    // client will misread.
    if let Some(version) = headers
        .get(PROTOCOL_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
        && !super::speaks(version)
    {
        return refuse(
            StatusCode::BAD_REQUEST,
            format!("unsupported MCP protocol version `{version}`"),
        );
    }

    let Ok(request) = serde_json::from_str::<Value>(&body) else {
        return json(StatusCode::BAD_REQUEST, &super::parse_error(), None);
    };

    // A session id is issued on `initialize` and echoed thereafter. Charon keeps no per-session
    // state — every answer is a function of the knowledge base — so this identifies a
    // conversation for a client's own bookkeeping rather than unlocking anything here.
    let session = is_initialize(&request).then(new_session_id);

    match super::handle(&server, &request).await {
        Some(response) => json(StatusCode::OK, &response, session),
        // A notification is acknowledged with 202 and an empty body.
        None => StatusCode::ACCEPTED.into_response(),
    }
}

/// Whether an `Origin` may reach a server that has no authentication.
///
/// Only loopback. A knowledge base served with `--host 0.0.0.0` is readable by anyone who can
/// reach the port anyway, but that is a decision its operator made explicitly; a browser being
/// steered into making the request is not.
fn is_local_origin(origin: &str) -> bool {
    let Some(rest) = origin
        .strip_prefix("http://")
        .or_else(|| origin.strip_prefix("https://"))
    else {
        // `null` and non-HTTP origins are exactly the opaque cases worth refusing.
        return false;
    };

    // An IPv6 literal is bracketed, so the colon that separates the port is the one *after* the
    // closing bracket. Splitting on the first colon would read `http://[::1]:3000` as the host
    // `[` and refuse a request from the local machine.
    let host = match rest.strip_prefix('[') {
        Some(inside) => match inside.split_once(']') {
            Some((host, _)) => host,
            None => return false,
        },
        None => rest.split([':', '/']).next().unwrap_or_default(),
    };

    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    // Parsed rather than compared, so the whole loopback range counts and a lookalike such as
    // `127.0.0.1.example.com` does not.
    host.parse::<std::net::IpAddr>()
        .is_ok_and(|address| address.is_loopback())
}

fn is_initialize(request: &Value) -> bool {
    request.get("method").and_then(Value::as_str) == Some("initialize")
}

/// A session identifier: printable ASCII, unique per handshake, and carrying nothing secret,
/// since it authorises nothing.
fn new_session_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    format!("charon-{:x}-{:x}", std::process::id(), nanos)
}

fn refuse(status: StatusCode, message: String) -> Response {
    json(
        status,
        &serde_json::json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": { "code": -32600, "message": message },
        }),
        None,
    )
}

fn json(status: StatusCode, body: &Value, session: Option<String>) -> Response {
    let mut response = (
        status,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response();
    if let Some(session) = session
        && let Ok(value) = session.parse()
    {
        response.headers_mut().insert(SESSION_HEADER, value);
    }
    response
}

#[cfg(test)]
mod tests {
    use super::is_local_origin;

    #[test]
    fn only_the_local_machine_counts_as_a_local_origin() {
        for origin in [
            "http://localhost",
            "http://localhost:3000",
            "http://127.0.0.1:8080",
            "https://127.0.0.2",
            "http://[::1]:3000",
            "http://[::1]",
        ] {
            assert!(is_local_origin(origin), "{origin} should be local");
        }

        for origin in [
            "https://evil.example",
            "http://127.0.0.1.example.com",
            "http://例え.テスト",
            "null",
            "file://",
            "http://[::1",
            "",
        ] {
            assert!(!is_local_origin(origin), "{origin} should not be local");
        }
    }
}
