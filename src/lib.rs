//! Charon exposes a Prolog knowledge base as an API.
//!
//! A `.pl` file is loaded into an embedded [scryer-prolog] engine; every predicate documented
//! with a `%!` PlDoc comment becomes callable — over an HTTP/JSON API described by OpenAPI, or
//! over MCP. Both are renderings of one description of one operation set, so a predicate exposed
//! on either is exposed identically on the other.
//!
//! # Example
//!
//! ```no_run
//! use std::sync::Arc;
//! use charon::service::{Config, Service};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let service = Arc::new(Service::open("knowledge_base.pl", Config::default())?);
//! let catalog = service.catalog();
//!
//! let arguments = serde_json::json!({ "List": [1, 2, 3] });
//! let outcome = service.call_blocking(
//!     &catalog,
//!     "list_length",
//!     arguments.as_object().unwrap(),
//! )?;
//! assert_eq!(outcome.value, serde_json::json!(3));
//! # Ok(())
//! # }
//! ```
//!
//! [scryer-prolog]: https://github.com/mthom/scryer-prolog

pub mod cli;
pub mod http;
pub mod mcp;
pub mod prolog;
pub mod service;

pub use prolog::KnowledgeBase;
pub use service::{Catalog, Config, Service};
