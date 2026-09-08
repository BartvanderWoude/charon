//! The binary itself: the subcommands, what they write where, and the stdio MCP transport.
//!
//! These are the only tests that go through `main`, so they are what proves the wiring — clap
//! grammar, logging destination, exit codes, transport selection — rather than the library the
//! other suites exercise directly.

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Output, Stdio};

use common::fixture;
use serde_json::{Value, json};

const CHARON: &str = env!("CARGO_BIN_EXE_charon");

fn run(arguments: &[&str]) -> Output {
    Command::new(CHARON)
        .args(arguments)
        .output()
        .expect("the binary should be runnable")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The long options a `--help` page lists, as opposed to ones merely mentioned in prose.
fn options(help: &str) -> Vec<String> {
    help.lines()
        .filter_map(|line| line.strip_prefix("      --"))
        .map(|line| format!("--{}", line.split_whitespace().next().unwrap_or_default()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Choosing an interface
// ---------------------------------------------------------------------------------------------

#[test]
fn an_interface_must_be_named() {
    // Serving both at once is gone: the first thing said is which interface this process is.
    let output = run(&[fixture("api_test.pl").to_str().unwrap()]);
    assert!(!output.status.success());
    let usage = stderr_of(&output);
    assert!(usage.contains("Usage: charon"), "{usage}");
    assert!(usage.contains("<COMMAND>"), "{usage}");
}

#[test]
fn help_lists_the_interfaces_and_their_options() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    let help = stdout_of(&output);
    for expected in ["http", "mcp", "check", "openapi"] {
        assert!(
            help.contains(expected),
            "`{expected}` missing from:\n{help}"
        );
    }

    // `--stdio` is an option of `mcp` and of nothing else, so an impossible combination cannot
    // be typed in the first place.
    assert!(options(&stdout_of(&run(&["mcp", "--help"]))).contains(&"--stdio".to_string()));
    assert!(!options(&stdout_of(&run(&["http", "--help"]))).contains(&"--stdio".to_string()));
}

// ---------------------------------------------------------------------------------------------
// The non-serving subcommands
// ---------------------------------------------------------------------------------------------

#[test]
fn check_succeeds_on_a_healthy_knowledge_base_and_reports_what_it_skipped() {
    let output = run(&["check", fixture("api_test.pl").to_str().unwrap()]);

    assert!(output.status.success(), "{}", stderr_of(&output));
    assert!(stdout_of(&output).contains("operation(s) exposed"));
    // `hidden(?X:int)` cannot be exposed, and saying so is the whole point of the diagnostic.
    assert!(
        stderr_of(&output).contains("hidden"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn check_fails_on_a_knowledge_base_whose_consult_failed() {
    let output = run(&["check", fixture("broken.pl").to_str().unwrap()]);

    assert!(!output.status.success());
    assert!(
        stderr_of(&output).contains("healthy/1"),
        "{}",
        stderr_of(&output)
    );
}

#[test]
fn a_missing_file_fails_with_a_message_naming_it() {
    let output = run(&["check", "/nonexistent/charon/kb.pl"]);

    assert!(!output.status.success());
    assert!(stderr_of(&output).contains("/nonexistent/charon/kb.pl"));
}

#[test]
fn openapi_writes_the_document_to_stdout_and_nothing_else() {
    let output = run(&["openapi", fixture("api_test.pl").to_str().unwrap()]);

    assert!(output.status.success(), "{}", stderr_of(&output));
    // Redirecting stdout to a file has to produce a document, not a document with a log line in
    // the middle of it — which is why every diagnostic goes to stderr on every subcommand.
    let document: Value =
        serde_json::from_str(&stdout_of(&output)).expect("stdout is the document alone");
    assert_eq!(document["openapi"], "3.1.0");
    assert!(document["paths"]["/list_length"]["get"].is_object());
}

#[test]
fn the_log_filter_can_silence_the_diagnostics() {
    let output = run(&[
        "check",
        fixture("api_test.pl").to_str().unwrap(),
        "--log",
        "error",
    ]);

    assert!(output.status.success());
    assert!(stderr_of(&output).is_empty(), "{}", stderr_of(&output));
    assert!(stdout_of(&output).contains("operation(s) exposed"));
}

// ---------------------------------------------------------------------------------------------
// MCP over stdio
// ---------------------------------------------------------------------------------------------

/// Feeds `requests` to `charon mcp --stdio` one per line, closes stdin, and collects the replies.
fn over_stdio(requests: &[Value]) -> Vec<Value> {
    let mut child = Command::new(CHARON)
        .args(["mcp", fixture("api_test.pl").to_str().unwrap(), "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary should be runnable");

    {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        for request in requests {
            writeln!(stdin, "{request}").expect("the server should accept a request");
        }
        // Dropping stdin is how a client says it is finished; the server should then exit.
    }

    let stdout = child.stdout.take().expect("stdout was piped");
    let replies: Vec<Value> = BufReader::new(stdout)
        .lines()
        .map(|line| line.expect("stdout is readable"))
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(&line).expect("every line is one JSON-RPC message"))
        .collect();

    let status = child.wait().expect("the server should exit");
    assert!(status.success(), "the server should exit cleanly");
    replies
}

#[test]
fn stdio_answers_one_json_rpc_message_per_line() {
    let replies = over_stdio(&[
        json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }),
        // A notification in the middle must not shift the ids that follow it.
        json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }),
        json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": { "name": "list_length", "arguments": { "List": [1, 2, 3] } },
        }),
    ]);

    assert_eq!(replies.len(), 2, "got {replies:?}");
    assert_eq!(replies[0]["id"], 1);
    assert_eq!(
        replies[0]["result"]["serverInfo"]["name"],
        "API Test Fixture"
    );
    assert_eq!(replies[1]["id"], 2);
    assert_eq!(
        replies[1]["result"]["structuredContent"]["solutions"],
        json!(3)
    );
}

#[test]
fn stdio_keeps_stdout_clean_when_a_line_is_not_json() {
    // The client's only symptom of a stray write to stdout is "the server does not work", so a
    // malformed line has to produce exactly one JSON-RPC error and nothing else.
    let mut child = Command::new(CHARON)
        .args(["mcp", fixture("api_test.pl").to_str().unwrap(), "--stdio"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the binary should be runnable");

    {
        let mut stdin = child.stdin.take().expect("stdin was piped");
        writeln!(stdin, "{{not json").unwrap();
        writeln!(stdin, "   ").unwrap();
        writeln!(
            stdin,
            "{}",
            json!({ "jsonrpc": "2.0", "id": 7, "method": "ping" })
        )
        .unwrap();
    }

    let output = child.wait_with_output().expect("the server should exit");
    let lines: Vec<&str> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();

    assert_eq!(lines.len(), 2, "got {lines:?}");
    let parse_error: Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(parse_error["error"]["code"], -32700);
    let pong: Value = serde_json::from_str(lines[1]).unwrap();
    assert_eq!(pong["id"], 7);
}
