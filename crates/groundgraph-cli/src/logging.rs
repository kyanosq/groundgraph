//! CLI-owned `tracing` subscriber init (issues.md #127 / #230).
//!
//! The CLI is the only binary that installs a subscriber. Engine/store
//! libraries only emit events (`tracing::info!` / `warn!` / …); if no
//! subscriber is installed — e.g. the engine is embedded in another host —
//! those events are simply dropped.
//!
//! Verbosity contract (default is `warn`, so the CLI is quiet on a healthy
//! run but surfaces warnings/errors without flags):
//!
//! | invocation      | effective level |
//! |-----------------|-----------------|
//! | (default)       | warn            |
//! | `-v`            | info            |
//! | `-vv`           | debug           |
//! | `-q`            | error           |
//! | `RUST_LOG=…`    | wins over flags |
//!
//! `RUST_LOG` follows the standard convention and overrides the `-v`/`-q`
//! flags so operators always have an escape hatch. All tracing output goes
//! to **stderr** — stdout is reserved for machine-readable reports and
//! `--format json`, which must stay free of diagnostic noise.

/// Map the `-v` count, `-q` flag, and an optional `RUST_LOG` value to a single
/// `tracing` filter directive string. Pure / branch-tested independently of
/// the global subscriber.
///
/// `quiet` wins over `verbose`; `RUST_LOG` wins over both.
pub fn log_directive(verbose: u8, quiet: bool, rust_log: Option<&str>) -> String {
    if let Some(directive) = rust_log {
        return directive.to_string();
    }
    if quiet {
        return "error".to_string();
    }
    let level = match verbose {
        0 => "warn",
        1 => "info",
        _ => "debug", // -vv and beyond cap at debug, never trace.
    };
    level.to_string()
}

/// Install the global `tracing` subscriber for the CLI process.
///
/// Reads `RUST_LOG` itself so callers pass only the parsed flags. Safe to call
/// once; a second call is a no-op (`try_init` returns `Err`, which we discard).
/// Output is steered to stderr so it never contaminates a `--format json`
/// report on stdout.
pub fn init_logging(verbose: u8, quiet: bool) {
    use tracing_subscriber::filter::EnvFilter;
    use tracing_subscriber::fmt;

    let directive = log_directive(verbose, quiet, std::env::var("RUST_LOG").ok().as_deref());
    let filter = EnvFilter::new(directive);
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}

#[cfg(test)]
mod tests {
    use super::log_directive;

    #[test]
    fn default_level_is_warn() {
        assert_eq!(log_directive(0, false, None), "warn");
    }

    #[test]
    fn one_v_bumps_to_info() {
        assert_eq!(log_directive(1, false, None), "info");
    }

    #[test]
    fn two_v_bumps_to_debug() {
        assert_eq!(log_directive(2, false, None), "debug");
    }

    #[test]
    fn three_v_caps_at_debug_not_trace() {
        // `-vvv` is still `debug`: trace-level engine internals are too noisy
        // for an indexer and not actionable.
        assert_eq!(log_directive(3, false, None), "debug");
        assert_eq!(log_directive(9, false, None), "debug");
    }

    #[test]
    fn quiet_demotes_to_error() {
        assert_eq!(log_directive(0, true, None), "error");
    }

    #[test]
    fn quiet_wins_over_verbose() {
        assert_eq!(log_directive(4, true, None), "error");
    }

    #[test]
    fn rust_log_overrides_flags() {
        assert_eq!(log_directive(0, false, Some("debug")), "debug");
    }

    #[test]
    fn rust_log_wins_even_over_quiet() {
        assert_eq!(log_directive(5, true, Some("off")), "off");
    }

    #[test]
    fn rust_log_passthrough_preserves_complex_directive() {
        // A scoped directive must survive verbatim into EnvFilter.
        assert_eq!(
            log_directive(0, false, Some("groundgraph=debug,warn")),
            "groundgraph=debug,warn"
        );
    }
}
