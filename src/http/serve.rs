//! Binding a socket and running a router until asked to stop.

use std::fmt;
use std::io;
use std::net::SocketAddr;

use axum::Router;
use tokio::net::TcpListener;

/// Serves `router` on `address` until the process is asked to shut down.
///
/// In-flight requests are allowed to finish: a `SIGINT` in the middle of a Prolog query that is
/// about to `assertz` should not take the answer — or the file write that follows it — with it.
pub async fn serve(router: Router, address: SocketAddr) -> Result<(), ServeError> {
    let listener = TcpListener::bind(address)
        .await
        .map_err(|cause| ServeError::Bind { address, cause })?;
    let bound = listener.local_addr().unwrap_or(address);
    if !bound.ip().is_loopback() {
        // Worth saying out loud: a knowledge base is often the only thing standing between a
        // reader and data that was never meant to leave the machine, and there is no auth here.
        tracing::warn!(
            address = %bound,
            "bound to a non-loopback address; anyone who can reach this port can query the \
             knowledge base"
        );
    }
    tracing::info!(address = %bound, "listening");

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_requested())
        .await
        .map_err(ServeError::Serve)
}

/// Resolves when the process is asked to stop, by `SIGINT` or (on Unix) `SIGTERM`.
async fn shutdown_requested() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            // Nothing to wait for, so wait forever and let the other arm win.
            Err(_) => std::future::pending().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }
    tracing::info!("shutting down");
}

/// Why the server stopped, other than being asked to.
#[derive(Debug)]
pub enum ServeError {
    Bind {
        address: SocketAddr,
        cause: io::Error,
    },
    Serve(io::Error),
}

impl fmt::Display for ServeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ServeError::Bind { address, cause } => {
                write!(f, "could not bind to {address}: {cause}")
            }
            ServeError::Serve(cause) => write!(f, "the server stopped: {cause}"),
        }
    }
}

impl std::error::Error for ServeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ServeError::Bind { cause, .. } | ServeError::Serve(cause) => Some(cause),
        }
    }
}
