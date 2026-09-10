//! Binding a socket and running a router until asked to stop.

use std::fmt;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use tokio::net::TcpListener;

use crate::service::Service;

/// Serves `router` on `address` until the process is asked to shut down.
///
/// In-flight requests are allowed to finish: a `SIGINT` in the middle of a Prolog query that is
/// about to `assertz` should not take the answer — or the file write that follows it — with it.
pub async fn serve(router: Router, address: SocketAddr) -> Result<(), ServeError> {
    serve_reloadable(router, address, None).await
}

/// [`serve`], with `SIGHUP` re-reading the knowledge base from disk.
///
/// A signal rather than an endpoint. Charon has no authentication, so an HTTP reload route would
/// let anyone who can reach the port force a re-read of the file — and a reload is the one
/// operation that can change what every subsequent request answers. A signal can only be sent by
/// someone who can already signal the process.
pub async fn serve_reloadable(
    router: Router,
    address: SocketAddr,
    service: Option<Arc<Service>>,
) -> Result<(), ServeError> {
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

    if let Some(service) = service {
        tokio::spawn(reload_on_hangup(service));
    }

    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown_requested())
        .await
        .map_err(ServeError::Serve)
}

/// Re-reads the knowledge base whenever the process is sent `SIGHUP`.
///
/// A failed reload is logged and otherwise ignored: [`Service::reload`] leaves the previous
/// catalog in place, so a half-saved file degrades to "the last good version keeps serving"
/// rather than to a dead server.
#[cfg(unix)]
async fn reload_on_hangup(service: Arc<Service>) {
    let Ok(mut hangup) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
    else {
        tracing::warn!("cannot listen for SIGHUP; reloading on signal is unavailable");
        return;
    };
    tracing::debug!("SIGHUP will reload the knowledge base");

    while hangup.recv().await.is_some() {
        tracing::info!(path = %service.path().display(), "reloading on SIGHUP");
        match service.reload() {
            Ok(()) => {
                let catalog = service.catalog();
                tracing::info!(
                    operations = catalog.available().count(),
                    withheld = catalog.withheld().count(),
                    "reloaded"
                );
            }
            // Worth an error rather than a warning: the operator asked for something and did not
            // get it, and is otherwise serving an older file than they think.
            Err(error) => tracing::error!(%error, "reload failed; the previous version is still \
                                                   being served"),
        }
    }
}

#[cfg(not(unix))]
async fn reload_on_hangup(_service: Arc<Service>) {}

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
