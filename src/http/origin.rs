//! Which `Origin` headers a server with no authentication may answer.
//!
//! Both adapters that speak HTTP need the same answer — the MCP transport to decide whether to
//! accept a request at all, the REST adapter to decide whether to let a browser *read* the
//! reply — and two copies of a security predicate are two chances to fix only one of them.

/// Whether an `Origin` may reach a server that has no authentication.
///
/// Only loopback. A knowledge base served with `--host 0.0.0.0` is readable by anyone who can
/// reach the port anyway, but that is a decision its operator made explicitly; a browser being
/// steered into making the request is not.
pub fn is_local_origin(origin: &str) -> bool {
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
