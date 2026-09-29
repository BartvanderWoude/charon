//! Letting a page on the local machine read the REST interface.
//!
//! A dashboard is a browser page, and a browser page served from anywhere other than this exact
//! port is a different origin — `file://` included, which sends `Origin: null`. Without a
//! `Access-Control-Allow-Origin` header the request still *reaches* Charon and still runs the
//! query; the browser simply refuses to hand the answer to the script that asked. So the choice
//! is not whether the knowledge base is reachable, only whether a local page may read what it
//! already caused to happen.
//!
//! The policy is the one [`super::is_local_origin`] already states for the MCP transport, and
//! deliberately not a wider one: Charon has no authentication, so a page on the open web must
//! not be able to read a knowledge base bound to this machine.
//!
//! Requests carrying no `Origin` at all — curl, a script, another server — are untouched. CORS
//! is a browser mechanism, and adding headers for a client that never asked would only obscure
//! what the policy is for.

use axum::{
    extract::Request,
    http::{HeaderValue, Method, StatusCode, header},
    middleware::Next,
    response::Response,
};

use super::is_local_origin;

/// Methods a local page may use. The REST router answers all of them for some route or other, so
/// naming a narrower set here would refuse a request the server is otherwise willing to serve.
const ALLOWED_METHODS: &str = "GET, POST, PUT, PATCH, DELETE, OPTIONS";

/// Headers a local page may send. `content-type` is what makes a JSON body possible; a preflight
/// asking for anything else is answered without it and the browser stops there.
const ALLOWED_HEADERS: &str = "content-type";

/// Headers a local page may *read* off the response. Only the one Charon adds itself: without it
/// a script cannot tell a complete answer from one the solution cap cut short.
const EXPOSED_HEADERS: &str = super::TRUNCATED_HEADER;

/// How long a browser may cache a preflight, in seconds.
const PREFLIGHT_MAX_AGE: &str = "600";

/// Answers preflights and marks responses a local page is allowed to read.
///
/// A preflight is answered here rather than routed, because `OPTIONS` is not a verb any
/// predicate is exposed under: letting it reach the router would turn every preflight into a
/// 405 and every cross-origin POST into a failure the page could not explain.
pub async fn allow_local_pages(request: Request, next: Next) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|value| value.to_str().ok())
        .filter(|origin| is_local_origin(origin))
        .map(str::to_owned);

    let is_preflight = request.method() == Method::OPTIONS;

    // A preflight is never dispatched: it asks whether a request would be allowed, and the
    // router has no answer to that question for any route.
    let mut response = if is_preflight {
        let mut response = Response::new(axum::body::Body::empty());
        *response.status_mut() = if origin.is_some() {
            StatusCode::NO_CONTENT
        } else {
            // Not an error: the request was understood and refused. Saying so with a status the
            // browser will not treat as a successful preflight is the whole of the refusal.
            StatusCode::FORBIDDEN
        };
        response
    } else {
        next.run(request).await
    };

    // `Vary` goes on every response, refused ones included: what Charon answers depends on the
    // request's `Origin`, so a shared cache must not replay one origin's response to another.
    let headers = response.headers_mut();
    headers.append(header::VARY, HeaderValue::from_static("origin"));

    let Some(origin) = origin else {
        return response;
    };

    // Echoed rather than `*`, so the header stays correct if credentials are ever allowed, and
    // so that the response names the one origin it was actually prepared for.
    if let Ok(value) = HeaderValue::from_str(&origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, value);
    }
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static(EXPOSED_HEADERS),
    );

    if is_preflight {
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static(ALLOWED_METHODS),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(ALLOWED_HEADERS),
        );
        headers.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static(PREFLIGHT_MAX_AGE),
        );
    }

    response
}
