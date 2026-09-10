//! Error responses, as RFC 9457 problem details.
//!
//! One shape for every failure, so a client can branch on a stable `type` rather than on prose
//! that changes whenever a message is reworded. The prose is still there — RFC 9457's `detail`
//! is exactly the sentence the service layer produced — so it is not the only thing a caller has
//! to work with.
//!
//! Status codes distinguish whose fault a failure is. A request that was well-formed but asked
//! for something the rules could not do is a 422; a knowledge base that calls a predicate nobody
//! defined is a 500. Reporting both as 400 would tell a caller to fix a request that no request
//! could fix.

use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::service::{ArgumentError, CallError, RouteError, Verb};

/// The media type RFC 9457 defines for these bodies.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// A problem's stable identity. URNs rather than URLs: there is no documentation site to point
/// at, and a `type` that 404s is worse than one that was never meant to be fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    NoSuchPath,
    MethodNotAllowed,
    UnsupportedMethod,
    Withheld,
    UnknownOperation,
    ArgumentMissing,
    ArgumentUnexpected,
    ArgumentInvalid,
    ArgumentDuplicated,
    MalformedRequest,
    UnsupportedMediaType,
    KnowledgeBaseError,
    KnowledgeBaseRefused,
    Interrupted,
}

impl Kind {
    pub fn urn(self) -> &'static str {
        match self {
            Kind::NoSuchPath => "urn:charon:no-such-path",
            Kind::MethodNotAllowed => "urn:charon:method-not-allowed",
            Kind::UnsupportedMethod => "urn:charon:unsupported-method",
            Kind::Withheld => "urn:charon:withheld",
            Kind::UnknownOperation => "urn:charon:unknown-operation",
            Kind::ArgumentMissing => "urn:charon:argument-missing",
            Kind::ArgumentUnexpected => "urn:charon:argument-unexpected",
            Kind::ArgumentInvalid => "urn:charon:argument-invalid",
            Kind::ArgumentDuplicated => "urn:charon:argument-duplicated",
            Kind::MalformedRequest => "urn:charon:malformed-request",
            Kind::UnsupportedMediaType => "urn:charon:unsupported-media-type",
            Kind::KnowledgeBaseError => "urn:charon:knowledge-base-error",
            Kind::KnowledgeBaseRefused => "urn:charon:knowledge-base-refused",
            Kind::Interrupted => "urn:charon:interrupted",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Kind::NoSuchPath => "No such predicate",
            Kind::MethodNotAllowed => "Method not allowed",
            Kind::UnsupportedMethod => "Unsupported method",
            Kind::Withheld => "Operation withheld",
            Kind::UnknownOperation => "Unknown operation",
            Kind::ArgumentMissing => "Missing argument",
            Kind::ArgumentUnexpected => "Unknown argument",
            Kind::ArgumentInvalid => "Invalid argument",
            Kind::ArgumentDuplicated => "Argument given twice",
            Kind::MalformedRequest => "Malformed request",
            Kind::UnsupportedMediaType => "Unsupported media type",
            Kind::KnowledgeBaseError => "Knowledge base error",
            Kind::KnowledgeBaseRefused => "The rules refused this request",
            Kind::Interrupted => "The query did not finish",
        }
    }

    pub fn status(self) -> StatusCode {
        match self {
            Kind::NoSuchPath | Kind::UnknownOperation => StatusCode::NOT_FOUND,
            Kind::MethodNotAllowed | Kind::UnsupportedMethod => StatusCode::METHOD_NOT_ALLOWED,
            Kind::Withheld => StatusCode::FORBIDDEN,
            // Well-formed, but the content is not something the rules can act on. 400 is for a
            // request that could not be parsed at all, which these were.
            Kind::ArgumentMissing
            | Kind::ArgumentUnexpected
            | Kind::ArgumentInvalid
            | Kind::KnowledgeBaseRefused => StatusCode::UNPROCESSABLE_ENTITY,
            Kind::ArgumentDuplicated | Kind::MalformedRequest => StatusCode::BAD_REQUEST,
            Kind::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Kind::KnowledgeBaseError | Kind::Interrupted => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// One problem, ready to be returned from a handler.
#[derive(Debug, Clone)]
pub struct Problem {
    kind: Kind,
    detail: String,
    extensions: Vec<(&'static str, Value)>,
    headers: Vec<(header::HeaderName, String)>,
}

impl Problem {
    pub fn new(kind: Kind, detail: impl Into<String>) -> Self {
        Problem {
            kind,
            detail: detail.into(),
            extensions: Vec::new(),
            headers: Vec::new(),
        }
    }

    /// Attaches a member beyond RFC 9457's own, e.g. which argument was at fault.
    pub fn with(mut self, name: &'static str, value: Value) -> Self {
        self.extensions.push((name, value));
        self
    }

    pub fn with_header(mut self, name: header::HeaderName, value: impl Into<String>) -> Self {
        self.headers.push((name, value.into()));
        self
    }

    pub fn status(&self) -> StatusCode {
        self.kind.status()
    }

    fn body(&self) -> Value {
        let mut body = json!({
            "type": self.kind.urn(),
            "title": self.kind.title(),
            "status": self.kind.status().as_u16(),
            "detail": self.detail,
        });
        for (name, value) in &self.extensions {
            body[*name] = value.clone();
        }
        body
    }
}

impl IntoResponse for Problem {
    fn into_response(self) -> Response {
        let mut response = (
            self.status(),
            [(header::CONTENT_TYPE, PROBLEM_JSON)],
            self.body().to_string(),
        )
            .into_response();
        for (name, value) in self.headers {
            if let Ok(value) = value.parse() {
                response.headers_mut().insert(name, value);
            }
        }
        response
    }
}

/// The problem a failed route resolution describes.
pub fn for_route(path: &str, error: RouteError) -> Problem {
    match error {
        RouteError::NoSuchPath => Problem::new(
            Kind::NoSuchPath,
            format!("no predicate is exposed at /{path}"),
        )
        .with("path", json!(format!("/{path}"))),
        RouteError::WrongVerb { allowed } => {
            let allow = allow_header(&allowed);
            Problem::new(
                Kind::MethodNotAllowed,
                format!("/{path} does not answer this method; try {allow}"),
            )
            .with("allowed", json!(allow))
            .with_header(header::ALLOW, allow)
        }
        RouteError::Withheld { name } => for_call(&CallError::Withheld { name }),
    }
}

/// The problem a failed call describes.
pub fn for_call(error: &CallError) -> Problem {
    let detail = error.to_string();
    match error {
        CallError::UnknownOperation { name } => {
            Problem::new(Kind::UnknownOperation, detail).with("operation", json!(name))
        }
        CallError::Withheld { name } => Problem::new(Kind::Withheld, detail)
            .with("operation", json!(name))
            // Says what to change, which is the whole reason this is a 403 and not a 404.
            .with("remedy", json!("restart charon with --persist")),
        CallError::Argument(ArgumentError::Missing { argument }) => {
            Problem::new(Kind::ArgumentMissing, detail).with("argument", json!(argument))
        }
        CallError::Argument(ArgumentError::Unexpected { arguments }) => {
            Problem::new(Kind::ArgumentUnexpected, detail).with("arguments", json!(arguments))
        }
        CallError::Argument(ArgumentError::Invalid { argument, .. }) => {
            Problem::new(Kind::ArgumentInvalid, detail).with("argument", json!(argument))
        }
        CallError::Prolog(cause) => {
            let kind = match cause.is_knowledge_base_fault() {
                true => Kind::KnowledgeBaseError,
                false => Kind::KnowledgeBaseRefused,
            };
            Problem::new(kind, detail).with("exception", cause.term.clone())
        }
        CallError::Interrupted => Problem::new(Kind::Interrupted, detail),
    }
}

/// The `Allow` header value for a set of verbs. A route answering GET answers HEAD too, and
/// `Allow` is meant to be exhaustive.
pub fn allow_header(allowed: &[Verb]) -> String {
    allowed
        .iter()
        .flat_map(|verb| match verb {
            Verb::Get => vec!["GET".to_string(), "HEAD".to_string()],
            other => vec![other.as_str().to_uppercase()],
        })
        .collect::<Vec<_>>()
        .join(", ")
}
