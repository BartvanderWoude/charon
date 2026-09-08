//! Fixtures shared by the integration tests.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;

use axum_test::TestServer;
use charon::service::{Config, Service};

/// The path of a fixture knowledge base.
pub fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A service over a fixture. Each call gets its own interpreter, so tests that assert facts do
/// not leak into one another.
pub fn service(name: &str) -> Arc<Service> {
    Arc::new(
        Service::open(fixture(name), Config::default())
            .unwrap_or_else(|error| panic!("fixture `{name}` should start: {error}")),
    )
}

/// The default fixture: `api_test.pl`.
pub fn api() -> Arc<Service> {
    service("api_test.pl")
}

/// A test server speaking the REST interface over a fixture.
pub fn http_server(name: &str) -> TestServer {
    TestServer::new(charon::http::routes(service(name)))
}

/// A test server speaking MCP over HTTP over a fixture.
pub fn mcp_server(name: &str) -> TestServer {
    TestServer::new(charon::mcp::routes(service(name)))
}
