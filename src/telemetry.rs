//! Logging initialization.
//!
//! Human-readable logs to stderr by default. Two environment variables adjust
//! that:
//!
//! - `VESSEL_LOG=<path>` also appends logs to a file, at `vessel=info` or
//!   better so server lifecycle events are captured without `--verbose`.
//! - `VESSEL_LOG_FORMAT=json` emits JSON spans and events to stderr instead of
//!   the human formatter, for callers that parse vessel's logs.
//!
//! `RUST_LOG` overrides the filter in every mode.

use tracing_subscriber::EnvFilter;

/// Opaque guard returned by [`init`].
///
/// Kept as a type (rather than returning `()`) so `main` holding it to exit
/// stays correct if a future backend needs flushing on shutdown.
pub struct TelemetryGuard {
    _priv: (),
}

/// Initialize logging.
///
/// When `verbose` is true the default filter is `vessel=debug`, otherwise
/// `vessel=warn`. `RUST_LOG` overrides both.
///
/// Returns a guard that should be held until the program exits.
#[must_use]
pub fn init(verbose: bool) -> TelemetryGuard {
    match std::env::var("VESSEL_LOG_FORMAT").as_deref() {
        Ok("json") => init_json(),
        _ => init_fmt(verbose),
    }
}

/// Default tracing: human-readable fmt to stderr (vessel's existing behavior).
///
/// If `VESSEL_LOG` is set to a file path, logs are also written there (appended).
/// The file log always uses `vessel=info` minimum so server lifecycle events
/// are captured even without `--verbose`.
fn init_fmt(verbose: bool) -> TelemetryGuard {
    let default = if verbose {
        "vessel=debug"
    } else {
        "vessel=warn"
    };
    let stderr_filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(default));

    if let Some(log_path) = std::env::var("VESSEL_LOG").ok().filter(|s| !s.is_empty()) {
        // Open log file (append mode, create if missing)
        match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            Ok(file) => {
                // When logging to a file, bump the filter to at least info so
                // server lifecycle events are always captured.
                let filter = if verbose {
                    EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| EnvFilter::new("vessel=debug"))
                } else {
                    EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| EnvFilter::new("vessel=info"))
                };

                let file = std::sync::Mutex::new(file);
                tracing_subscriber::fmt()
                    .with_env_filter(filter)
                    .with_ansi(false)
                    .with_writer(file)
                    .init();
            }
            Err(e) => {
                // Fall back to stderr-only, but warn about it
                eprintln!("warning: VESSEL_LOG={log_path}: {e}");
                tracing_subscriber::fmt()
                    .with_env_filter(stderr_filter)
                    .with_writer(std::io::stderr)
                    .init();
            }
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(stderr_filter)
            .with_writer(std::io::stderr)
            .init();
    }

    TelemetryGuard { _priv: () }
}

/// JSON spans and events to stderr, via tracing-subscriber's JSON formatter.
fn init_json() -> TelemetryGuard {
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(
            tracing_subscriber::fmt::layer()
                .json()
                .with_writer(std::io::stderr)
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE),
        )
        .init();

    TelemetryGuard { _priv: () }
}
