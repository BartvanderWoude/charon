//! Fixtures shared by the integration tests.

#![allow(dead_code)]

use std::fs;
use std::ops::Deref;
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

/// A knowledge base written to a scratch file, removed when the test ends.
pub struct Scratch(pub PathBuf);

impl Scratch {
    pub fn new(name: &str, source: &str) -> Self {
        let path = std::env::temp_dir().join(format!("charon_{name}_{}.pl", std::process::id()));
        fs::write(&path, source).unwrap();
        Scratch(path)
    }

    /// A writable copy of a fixture, so a persisting test cannot edit the fixture itself.
    pub fn copy_of_fixture(name: &str, label: &str) -> Self {
        let source = fs::read_to_string(fixture(name))
            .unwrap_or_else(|error| panic!("fixture `{name}` should be readable: {error}"));
        Scratch::new(label, &source)
    }

    pub fn rewrite(&self, source: &str) {
        fs::write(&self.0, source).unwrap();
    }

    pub fn read(&self) -> String {
        fs::read_to_string(&self.0).unwrap()
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// A service over a fixture, read-only. Each call gets its own interpreter, so tests that assert
/// facts do not leak into one another.
///
/// Read-only means *changing operations are withheld*: `put_fruit`, `delete_fruit` and
/// `post_note` are not exposed here. A test that needs them wants [`persisting`].
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
    TestServer::new(charon::mcp::routes(charon::mcp::Server::new(service(name))))
}

/// A persisting service, its scratch file, and a server over it.
///
/// Owning the [`Scratch`] is the point: a persisting service rewrites the file it was given, so
/// every persisting test works on a copy that is deleted when the test ends. Pointing one at
/// `tests/fixtures/api_test.pl` would edit the fixture every other test reads.
pub struct Persisting {
    pub scratch: Scratch,
    pub service: Arc<Service>,
}

impl Persisting {
    /// A persisting service over a copy of `name`, with changing operations exposed.
    pub fn fixture(name: &str, label: &str) -> Self {
        let scratch = Scratch::copy_of_fixture(name, label);
        let service = Arc::new(
            Service::open(&scratch.0, Config::persisting(true))
                .unwrap_or_else(|error| panic!("fixture `{name}` should start: {error}")),
        );
        Persisting { scratch, service }
    }

    /// A persisting service over `source` written to a scratch file.
    pub fn source(label: &str, source: &str) -> Self {
        let scratch = Scratch::new(label, source);
        let service = Arc::new(
            Service::open(&scratch.0, Config::persisting(true))
                .unwrap_or_else(|error| panic!("scratch `{label}` should start: {error}")),
        );
        Persisting { scratch, service }
    }

    pub fn http(&self) -> TestServer {
        TestServer::new(charon::http::routes(Arc::clone(&self.service)))
    }

    pub fn mcp(&self) -> TestServer {
        TestServer::new(charon::mcp::routes(charon::mcp::Server::new(Arc::clone(
            &self.service,
        ))))
    }
}

/// A persisting REST server over a copy of `api_test.pl`, for tests about *transport* rather
/// than about persistence — argument handling, routing, the OpenAPI document — that happen to
/// touch a `put_`/`post_`/`delete_` operation.
pub fn writable_http(label: &str) -> WritableServer {
    let persisting = Persisting::fixture("api_test.pl", label);
    let server = persisting.http();
    WritableServer {
        _persisting: persisting,
        server,
    }
}

/// As [`writable_http`], speaking MCP.
pub fn writable_mcp(label: &str) -> WritableServer {
    let persisting = Persisting::fixture("api_test.pl", label);
    let server = persisting.mcp();
    WritableServer {
        _persisting: persisting,
        server,
    }
}

/// A [`TestServer`] that keeps its scratch knowledge base alive for as long as it is used.
pub struct WritableServer {
    _persisting: Persisting,
    server: TestServer,
}

impl Deref for WritableServer {
    type Target = TestServer;

    fn deref(&self) -> &TestServer {
        &self.server
    }
}
