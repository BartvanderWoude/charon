//! The command line: what Charon can be asked to do, and doing it.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Args, Parser, Subcommand};

use crate::service::{Config, Service};
use crate::{http, mcp};

/// Exposes a Prolog knowledge base as an HTTP/JSON API or as an MCP server.
#[derive(Debug, Parser, PartialEq)]
#[command(name = "charon", version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Log filter, in `tracing` syntax (`info`, `debug`, `charon=trace,warn`).
    ///
    /// `RUST_LOG` overrides this when set. Everything is written to stderr, so `charon mcp
    /// --stdio` keeps stdout clean for protocol traffic.
    #[arg(long, global = true, default_value = "info", value_name = "FILTER")]
    pub log: String,
}

#[derive(Debug, Subcommand, PartialEq)]
pub enum Command {
    /// Serve the knowledge base as an HTTP/JSON API, described by OpenAPI at `/openapi.json`.
    Http(HttpArgs),
    /// Serve the knowledge base as an MCP server, over HTTP by default.
    Mcp(McpArgs),
    /// Load the knowledge base, report what it exposes, and exit.
    Check(CheckArgs),
    /// Print the OpenAPI 3.1 document for the knowledge base and exit.
    Openapi(CheckArgs),
}

/// The knowledge base and how to treat it — shared by every subcommand.
#[derive(Debug, Args, PartialEq)]
pub struct Source {
    /// The `.pl` knowledge base to serve.
    #[arg(value_name = "KNOWLEDGE_BASE")]
    pub path: PathBuf,

    /// Mirror `assertz`/`retract` changes back into the knowledge base file.
    #[arg(long)]
    pub persist: bool,
}

/// Where to listen — shared by the two serving subcommands.
#[derive(Debug, Args, PartialEq)]
pub struct Listen {
    /// Port to listen on.
    #[arg(long, default_value_t = 3000)]
    pub port: u16,

    /// Address to bind. Defaults to loopback; pass `0.0.0.0` to accept connections from other
    /// machines, which exposes the knowledge base to your whole network.
    #[arg(long, default_value_t = IpAddr::V4(Ipv4Addr::LOCALHOST))]
    pub host: IpAddr,
}

impl Listen {
    pub fn address(&self) -> SocketAddr {
        SocketAddr::new(self.host, self.port)
    }
}

#[derive(Debug, Args, PartialEq)]
pub struct HttpArgs {
    #[command(flatten)]
    pub source: Source,
    #[command(flatten)]
    pub listen: Listen,
}

#[derive(Debug, Args, PartialEq)]
pub struct McpArgs {
    #[command(flatten)]
    pub source: Source,
    #[command(flatten)]
    pub listen: Listen,

    /// Speak MCP over stdin/stdout instead of HTTP, for a client that launches this binary
    /// itself. Nothing is bound, so `--host` and `--port` do not apply.
    #[arg(long, conflicts_with_all = ["port", "host"])]
    pub stdio: bool,
}

#[derive(Debug, Args, PartialEq)]
pub struct CheckArgs {
    #[command(flatten)]
    pub source: Source,
}

impl Source {
    fn config(&self) -> Config {
        Config {
            persist: self.persist,
        }
    }
}

/// Parses the command line and runs it.
pub fn main() -> ExitCode {
    let cli = Cli::parse();
    init_logging(&cli.log);

    match run(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!("{error}");
            ExitCode::FAILURE
        }
    }
}

/// Diagnostics go to stderr on every subcommand, not just the stdio one: `charon openapi kb.pl >
/// openapi.json` should produce a document, not a document with a log line in the middle of it.
fn init_logging(filter: &str) {
    use tracing_subscriber::EnvFilter;

    let filter = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(filter))
        .unwrap_or_else(|_| EnvFilter::new("info"));

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_target(false)
        .try_init();
}

fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Check(args) => {
            let service = open(&args.source)?;
            report(&service);
            println!(
                "{} operation(s) exposed from {}",
                service.catalog().operations().len(),
                service.path().display()
            );
            Ok(())
        }
        Command::Openapi(args) => {
            let service = open(&args.source)?;
            report(&service);
            println!("{}", service.catalog().openapi_json());
            Ok(())
        }
        Command::Http(args) => {
            let service = Arc::new(open(&args.source)?);
            report(&service);
            block_on(http::serve(
                http::routes(Arc::clone(&service)),
                args.listen.address(),
            ))?;
            Ok(())
        }
        Command::Mcp(args) => {
            let service = Arc::new(open(&args.source)?);
            report(&service);
            if args.stdio {
                block_on(mcp::serve_stdio(service))?;
            } else {
                block_on(http::serve(
                    mcp::routes(Arc::clone(&service)),
                    args.listen.address(),
                ))?;
            }
            Ok(())
        }
    }
}

fn open(source: &Source) -> Result<Service, crate::service::StartupError> {
    Service::open(&source.path, source.config())
}

/// Predicates a knowledge base documents but Charon cannot expose. Reported once, at startup,
/// because "the endpoint is simply not there" is otherwise a mystery with no error attached.
fn report(service: &Service) {
    let catalog = service.catalog();
    for (name, reason) in catalog.rejected() {
        tracing::warn!(predicate = %name, "documented but not exposed: {reason}");
    }
    tracing::info!(
        operations = catalog.operations().len(),
        persisting = service.is_persisting(),
        path = %service.path().display(),
        "knowledge base loaded"
    );
}

/// Runs `future` on a fresh multi-threaded runtime.
///
/// Built here rather than with `#[tokio::main]` so that the non-serving subcommands never start
/// a runtime at all.
fn block_on<E>(future: impl Future<Output = Result<(), E>>) -> Result<(), E> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("failed to start the async runtime")
        .block_on(future)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn parse(arguments: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("charon").chain(arguments.iter().copied()))
    }

    #[test]
    fn the_grammar_is_internally_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn an_interface_must_be_chosen() {
        // The old CLI served both at once and inferred the rest; now the choice is the first
        // thing said, and saying nothing is an error rather than a default.
        assert!(parse(&["kb.pl"]).is_err());
        assert!(parse(&[]).is_err());
    }

    #[test]
    fn each_interface_takes_the_knowledge_base_and_defaults_to_loopback() {
        let Command::Http(args) = parse(&["http", "kb.pl"]).unwrap().command else {
            panic!("expected the http subcommand");
        };
        assert_eq!(args.source.path, PathBuf::from("kb.pl"));
        assert!(!args.source.persist);
        // Not 0.0.0.0: a knowledge base should not become reachable from the network by
        // default just because it was started.
        assert_eq!(args.listen.address().to_string(), "127.0.0.1:3000");
    }

    #[test]
    fn mcp_speaks_http_unless_stdio_is_asked_for() {
        let Command::Mcp(args) = parse(&["mcp", "kb.pl"]).unwrap().command else {
            panic!("expected the mcp subcommand");
        };
        assert!(!args.stdio);
        assert_eq!(args.listen.port, 3000);

        let Command::Mcp(args) = parse(&["mcp", "kb.pl", "--stdio"]).unwrap().command else {
            panic!("expected the mcp subcommand");
        };
        assert!(args.stdio);
    }

    #[test]
    fn stdio_belongs_to_mcp_alone() {
        // Under `http` the flag does not exist at all, so it cannot be accepted and then
        // ignored; under `mcp` it contradicts binding a socket.
        assert!(parse(&["http", "kb.pl", "--stdio"]).is_err());
        assert!(parse(&["mcp", "kb.pl", "--stdio", "--port", "8080"]).is_err());
        assert!(parse(&["mcp", "kb.pl", "--stdio", "--host", "0.0.0.0"]).is_err());
    }

    #[test]
    fn a_port_value_is_never_mistaken_for_the_path() {
        // Both orderings must find the same file. Reading the value after `--port` as a
        // positional argument would silently try to load a knowledge base called "3101".
        for arguments in [
            ["http", "--port", "3101", "kb.pl"],
            ["http", "kb.pl", "--port", "3101"],
        ] {
            let Command::Http(args) = parse(&arguments).unwrap().command else {
                panic!("expected the http subcommand");
            };
            assert_eq!(args.source.path, PathBuf::from("kb.pl"));
            assert_eq!(args.listen.port, 3101);
        }
    }

    #[test]
    fn a_malformed_port_or_host_is_refused_rather_than_defaulted() {
        // The hand-rolled parser this replaced fell back to the default port, so a typo meant
        // listening somewhere other than where you asked.
        assert!(parse(&["http", "kb.pl", "--port", "nonsense"]).is_err());
        assert!(parse(&["http", "kb.pl", "--port"]).is_err());
        assert!(parse(&["http", "kb.pl", "--port", "70000"]).is_err());
        assert!(parse(&["http", "kb.pl", "--host", "not-an-address"]).is_err());
    }

    #[test]
    fn persistence_is_opt_in_on_every_subcommand() {
        for arguments in [
            vec!["http", "kb.pl", "--persist"],
            vec!["mcp", "kb.pl", "--persist"],
            vec!["check", "kb.pl", "--persist"],
        ] {
            let source = match parse(&arguments).unwrap().command {
                Command::Http(args) => args.source,
                Command::Mcp(args) => args.source,
                Command::Check(args) | Command::Openapi(args) => args.source,
            };
            assert!(source.persist, "{arguments:?}");
            assert_eq!(source.config(), Config { persist: true });
        }
    }

    #[test]
    fn the_non_serving_subcommands_take_no_socket() {
        assert!(parse(&["check", "kb.pl", "--port", "3000"]).is_err());
        assert!(parse(&["openapi", "kb.pl", "--host", "0.0.0.0"]).is_err());
        assert!(parse(&["check", "kb.pl"]).is_ok());
        assert!(parse(&["openapi", "kb.pl"]).is_ok());
    }

    #[test]
    fn the_log_filter_is_available_to_every_subcommand() {
        assert_eq!(parse(&["http", "kb.pl"]).unwrap().log, "info");
        assert_eq!(
            parse(&["mcp", "kb.pl", "--log", "debug"]).unwrap().log,
            "debug"
        );
        assert_eq!(
            parse(&["check", "kb.pl", "--log", "warn"]).unwrap().log,
            "warn"
        );
    }
}
