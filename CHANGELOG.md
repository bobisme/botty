# Changelog

## [0.18.2] — 2026-09-04

This supersedes 0.18.1. Its release workflow stopped before crates.io
publication and binary uploads.

### Fixed
- Restored clean checks under Rust 1.98 Clippy by making the private,
  non-awaiting server auto-start helper synchronous. Runtime behavior is
  unchanged.

## [0.18.1] — 2026-09-04

### Added
- `spawn --env-inherit` accepts validated trailing namespace wildcards such as
  quoted `RITE_*`. Broad suffix, embedded, unanchored, and trivial wildcard
  patterns fail instead of silently selecting unintended variables. Explicit
  `--env` values still take precedence over inherited values.

### Fixed
- `spawn --cwd DIR` now retains the canonical absolute working directory in
  agent metadata. `list --format json` reports `cwd` for both running and
  retained exited agents. The optional protocol field defaults cleanly when an
  older peer omits it, and records without `--cwd` continue to omit the field.

## [0.18.0] - 2026-07-31

Breaking release. The `otel`, `runtime-tokio` and `runtime-asupersync` features
are gone, and the `Send`/`SendBytes` IPC requests changed shape.

### Added
- `send --paste` (`-p`, alias `--bracketed`) wraps the payload in
  bracketed-paste markers (`ESC[200~` … `ESC[201~`), so a multi-line prompt
  reaches a full-screen TUI as one paste instead of submitting a truncated
  first line and turning each remaining line into its own turn. Pairs with
  `--enter` to paste and then submit as a single turn. Any `ESC[201~` inside
  the text is stripped rather than forwarded, since it would close the bracket
  early and deliver the remainder as live keystrokes.
- `send` accepts `-` as the text to read the payload from stdin, so a long
  prompt does not have to be quoted onto the command line.
- `send`, `send-bytes` and `send-keys` accept the `--label`, `--proc` and
  `--all` selectors that `kill` and `signal` already had, delivering the same
  input to every matching running agent. With a selector the agent ID is
  omitted and every positional is payload. Deliveries run concurrently, so
  per-agent submit delays overlap rather than stack.
- `send --submit-delay-ms` tunes the pause before the `--newline`/`--enter`
  key; `0` restores same-write speed for shells and other line-oriented
  programs.
- `VESSEL_LOG_FORMAT=json` emits JSON spans and events to stderr. This and
  `VESSEL_LOG` are now documented in the README; neither was before.

### Fixed
- `send --enter` and `--newline` silently failed to submit to TUI agents. The
  key rode in the same `write(2)` as the text, and full-screen TUIs classify
  input by arrival timing, so a trailing CR/LF inside one burst was inserted
  into the composer as pasted content instead of submitting it. The text is now
  flushed first, then the key goes out as its own write after a short pause.
- Large payloads were silently truncated. The PTY master is non-blocking, but
  `Send` and `SendBytes` treated a short `write(2)` as success, so any payload
  past the PTY input buffer lost its tail. Writes now loop over short counts
  and retry `EAGAIN` under a 5s cap.
- A send larger than the PTY buffer could stall until that cap. The write retry
  loop ran while holding the agent-manager lock, which the PTY reader also
  needs to drain output, so a child blocked writing its echo never read the
  rest of the payload. The descriptor is now duplicated and the manager lock
  released before writing, with a per-agent write lock preserving ordering.
- `vessel spawn` could hang forever. After `fork(2)` the child called
  `std::env` accessors and allocated while building its argv, none of which is
  async-signal-safe: the child inherits every lock in whatever state it was in,
  so one held by another thread at fork time is held forever. The server forks
  agents from a thread pool, so this was reachable in normal use. Everything
  the child needs is now built before the fork, and the child does only
  async-signal-safe calls before `exec`.

### Changed
- asupersync 0.2.9 → 0.3.10. 0.3.10 makes the borrowed `MutexGuard` `!Send`,
  which forbids holding one across an `.await` in a spawned task, so the
  runtime shim now hands back `OwnedMutexGuard`.
- **Protocol**: `Request::Send` and `Request::SendBytes` gained `labels`, `all`
  and `proc_filter`, and their `id` is now optional. `Send` also gained
  `paste` and `submit_delay_ms`. All new fields have serde defaults, so
  existing clients still deserialize. A selector-based request answers with the
  new `Response::SendResults`, carrying one outcome per agent so partial
  failure is visible; a request naming a single agent still answers `Ok`/
  `Error`.
- JSON logging moved from `OTEL_EXPORTER_OTLP_ENDPOINT=stderr` to
  `VESSEL_LOG_FORMAT=json`.

### Removed
- The `otel` feature and OTLP trace/log export, along with the automatic
  `TRACEPARENT` injection into spawned agent environments. It was never used,
  and it was the bulk of the dependency tree: 245 unique crates down to 177,
  dropping tokio, reqwest, hyper, tonic, mio, rand, url/idna and the ICU chain.
  `tracing` logging is unaffected.
- The `runtime-tokio` and `runtime-asupersync` features. The tokio backend had
  stopped compiling and nothing exercised it, since the two runtimes were
  mutually exclusive and only the default was ever built. asupersync is now a
  plain dependency and `Cargo.toml` has no `[features]` table.

## [0.17.5] - 2026-06-22

### Security
- Bound IPC request frames and `SendBytes` payloads to prevent server memory
  exhaustion (CWE-400). The server previously accumulated an unbounded line via
  `read_line` before parsing, and decoded `SendBytes.data` base64 into an
  unbounded `Vec<u8>`, so a same-user or spawned-agent client on the owner-only
  control socket could exhaust memory and deny the shared control plane. The
  server now caps each newline-delimited frame at 1 MiB (rejecting and closing
  the connection before parse/dispatch) and independently rejects oversized
  `SendBytes` payloads before allocating the decode buffer.

## [0.17.4] - 2026-06-19

### Fixed
- Build failure on macOS/BSD: cast `TIOCSCTTY` to the `c_ulong` type that
  `libc::ioctl` expects (it is `c_uint` there), fixing an `E0308` mismatched
  types error in `src/pty.rs`. Linux was unaffected.

### Added
- `send-keys` now accepts the `space` key name (sends a literal space byte,
  `0x20`). Previously there was no way to send a space, since a bare `" "`
  argument is trimmed away during key parsing.

## [0.17.3] - 2026-04-22

### Security
- Bind the Unix control socket under a restrictive umask (`0o177`) so the inode
  is created owner-only atomically. Closes a race window between `bind()` and
  the subsequent `set_permissions(0o600)` call during which a local user on a
  multi-user parent directory (notably the `/tmp/vessel-$UID.sock` fallback)
  could `connect()` and drive the server with unauthenticated `Spawn`
  requests. The existing `set_permissions` call is retained as a
  belt-and-suspenders safeguard.

## [0.17.2] - 2026-04-15

### Added
- `vessel wait --exited --any` to return as soon as any listed agent exits and print which agent IDs had exited when the wait completed

## [0.17.1] - 2026-03-26

### Changed
- Switch `asupersync` dependency from git rev to crates.io v0.2.9, enabling publication to crates.io
- Update `select!` macro patterns for `Select::new().await` returning `Result<Either<A,B>, SelectError>` (asupersync v0.2.9 API change)
- Handle new `broadcast::RecvError::PolledAfterCompletion` variant (asupersync v0.2.9)

## [0.17.0] - 2026-03-05

### Changed
- Rename crate from `botty-pty` to `vessel-pty`
- Default runtime switched to `asupersync`; tokio remains available via `runtime-tokio` feature

### Added
- `asupersync` runtime backend: feature-gated async runtime using asupersync for cancel-correct async I/O
- Runtime abstraction module (`src/runtime.rs`) re-exporting active runtime primitives
- `select!` macro compatible with both tokio and asupersync runtimes

## [0.16.1] - 2026-02-18

### Fixed
- `wait --exited` now supports multiple agent IDs
- Stable screen detection for `wait --stable`

## [0.16.0] - 2026-02-10

### Added
- `vessel tail` command for streaming agent output
- `vessel events` and `vessel subscribe` for event streaming
- PTY reader background task for real-time transcript and screen updates

## [0.13.2] - 2026-01-28

### Fixed
- Server shutdown respects running agents (SIGTERM/SIGINT ignored when agents are active)
- `view` pane identity uses agent ID instead of pane title
