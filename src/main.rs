//! vessel — PTY-based Agent Runtime

use clap::Parser;
use serde_json::json;
use std::io::Write;
use tracing::error;
use vessel::{
    AttachConfig, Cli, Client, Command, DumpFormat, OutputFormat, RecordedCommand, Request,
    Response, Server, TmuxView, ViewError, default_socket_path, json_envelope, resolve_format,
    run_attach, text_record,
};

/// Parse a signal name or number into a Unix signal number.
///
/// Accepts: number (e.g., "15"), name with or without SIG prefix (e.g., "TERM",
/// "SIGTERM", "term"). Returns an error message if unrecognized.
fn parse_signal(s: &str) -> Result<i32, String> {
    // Try numeric first
    if let Ok(n) = s.parse::<i32>() {
        if n > 0 && n < 65 {
            return Ok(n);
        }
        return Err(format!("signal number out of range: {n}"));
    }

    // Normalize: uppercase, strip SIG prefix
    let name = s.to_ascii_uppercase();
    let name = name.strip_prefix("SIG").unwrap_or(&name);

    match name {
        "HUP" => Ok(1),
        "INT" => Ok(2),
        "QUIT" => Ok(3),
        "KILL" => Ok(9),
        "USR1" => Ok(10),
        "USR2" => Ok(12),
        "PIPE" => Ok(13),
        "ALRM" => Ok(14),
        "TERM" => Ok(15),
        "CONT" => Ok(18),
        "STOP" => Ok(19),
        "TSTP" => Ok(20),
        "TTIN" => Ok(21),
        "TTOU" => Ok(22),
        "WINCH" => Ok(28),
        _ => Err(format!("unknown signal: {s}")),
    }
}

/// Guard that restores terminal output settings on drop.
struct RawOutputGuard {
    original_termios: nix::sys::termios::Termios,
    fd: std::os::fd::OwnedFd,
}

impl Drop for RawOutputGuard {
    fn drop(&mut self) {
        use nix::sys::termios::{SetArg, tcsetattr};
        let _ = tcsetattr(&self.fd, SetArg::TCSAFLUSH, &self.original_termios);
    }
}

/// Disable output post-processing on stdout (OPOST flag).
/// This is required for TUI programs - without it, escape sequences like
/// cursor positioning get mangled (e.g., \n becomes \r\n).
/// Returns a guard that restores the original settings on drop.
fn disable_output_postprocessing() -> Option<RawOutputGuard> {
    use nix::sys::termios::{OutputFlags, SetArg, tcgetattr, tcsetattr};
    use std::os::fd::AsFd;

    let stdout = std::io::stdout();
    let stdout_fd = stdout.as_fd();

    // Check if stdout is a TTY
    if !nix::unistd::isatty(stdout_fd).unwrap_or(false) {
        return None;
    }

    // Get current settings
    let original_termios = tcgetattr(stdout_fd).ok()?;

    // Create modified settings with OPOST disabled
    let mut raw = original_termios.clone();
    raw.output_flags.remove(OutputFlags::OPOST);

    // Apply the new settings
    tcsetattr(stdout_fd, SetArg::TCSAFLUSH, &raw).ok()?;

    // Clone the fd for the guard
    let fd = stdout_fd.try_clone_to_owned().ok()?;

    Some(RawOutputGuard {
        original_termios,
        fd,
    })
}

/// Shell-escape a string for use in single quotes.
///
/// Wraps the string in single quotes and escapes any embedded single quotes
/// using the `'\''` idiom (end quote, escaped quote, start quote).
fn shell_escape(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Compute the delay in seconds between two timestamps (milliseconds).
///
/// Clamps the result to the range [0.1, 2.0] seconds. Delays below 0.1s
/// are bumped up to avoid races, and delays above 2.0s are capped since
/// longer gaps are typically idle time.
fn compute_delay(prev_ms: u64, curr_ms: u64) -> f64 {
    let delta_ms = curr_ms.saturating_sub(prev_ms);
    // Result is clamped to [0.1, 2.0]; precision loss on the millisecond delta is
    // irrelevant for a sub-second sleep delay.
    #[allow(clippy::cast_precision_loss)]
    let secs = delta_ms as f64 / 1000.0;
    secs.clamp(0.1, 2.0)
}

/// Format bytes into human-readable string (e.g., "142M", "1.2G").
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * KB;
    const GB: u64 = 1024 * MB;
    if bytes >= GB {
        // Human-readable display only; sub-mantissa precision is not meaningful here.
        #[allow(clippy::cast_precision_loss)]
        let gib = bytes as f64 / GB as f64;
        format!("{gib:.1}G")
    } else if bytes >= MB {
        format!("{}M", bytes / MB)
    } else if bytes >= KB {
        format!("{}K", bytes / KB)
    } else {
        format!("{bytes}B")
    }
}

/// Generate an executable bash test script from a sequence of recorded commands.
fn generate_test_script(agent_id: &str, commands: &[RecordedCommand]) -> String {
    use std::fmt::Write;
    use std::time::{SystemTime, UNIX_EPOCH};

    let now_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let now = format!("unix:{now_secs}");

    let mut script = String::new();
    writeln!(script, "#!/bin/bash").expect("writing to a String never fails");
    writeln!(script, "# Auto-generated test script from vessel recording")
        .expect("writing to a String never fails");
    writeln!(script, "# Agent: {agent_id}").expect("writing to a String never fails");
    writeln!(script, "# Generated: {now}").expect("writing to a String never fails");
    writeln!(script, "# Commands: {}", commands.len()).expect("writing to a String never fails");
    writeln!(script, "set -e").expect("writing to a String never fails");
    writeln!(script).expect("writing to a String never fails");
    writeln!(script, "# Spawn the agent").expect("writing to a String never fails");
    writeln!(
        script,
        "# TODO: Replace with the actual command that was used to spawn the agent"
    )
    .expect("writing to a String never fails");
    writeln!(
        script,
        "AGENT=$(vessel spawn --record -- echo 'replace with original command')"
    )
    .expect("writing to a String never fails");
    writeln!(script).expect("writing to a String never fails");
    writeln!(script, "# Cleanup on exit").expect("writing to a String never fails");
    writeln!(
        script,
        "cleanup() {{ vessel kill \"$AGENT\" 2>/dev/null || true; }}"
    )
    .expect("writing to a String never fails");
    writeln!(script, "trap cleanup EXIT").expect("writing to a String never fails");
    writeln!(script).expect("writing to a String never fails");
    writeln!(script, "# Wait for agent to be ready").expect("writing to a String never fails");
    writeln!(script, "sleep 0.5").expect("writing to a String never fails");

    for (i, cmd) in commands.iter().enumerate() {
        writeln!(script).expect("writing to a String never fails");

        // Compute delay from previous command
        if i > 0 {
            let delay = compute_delay(commands[i - 1].timestamp, cmd.timestamp);
            writeln!(script, "sleep {delay:.1}").expect("writing to a String never fails");
        }

        match cmd.command.as_str() {
            "send" => {
                // The payload may contain a trailing newline if --newline was used.
                // Detect that and use the -n flag accordingly.
                let (text, use_newline) = cmd
                    .payload
                    .strip_suffix('\n')
                    .map_or((cmd.payload.as_str(), false), |stripped| (stripped, true));

                let escaped = shell_escape(text);
                if use_newline {
                    writeln!(script, "# Command {}: send text (with newline)", i + 1)
                        .expect("writing to a String never fails");
                    writeln!(script, "vessel send -n \"$AGENT\" {escaped}")
                        .expect("writing to a String never fails");
                } else {
                    writeln!(script, "# Command {}: send text", i + 1)
                        .expect("writing to a String never fails");
                    writeln!(script, "vessel send \"$AGENT\" {escaped}")
                        .expect("writing to a String never fails");
                }
            }
            "send_bytes" => {
                writeln!(script, "# Command {}: send raw bytes", i + 1)
                    .expect("writing to a String never fails");
                writeln!(script, "vessel send-bytes \"$AGENT\" {}", cmd.payload)
                    .expect("writing to a String never fails");
            }
            "send_keys" => {
                let escaped = shell_escape(&cmd.payload);
                writeln!(script, "# Command {}: send key", i + 1)
                    .expect("writing to a String never fails");
                writeln!(script, "vessel send-keys \"$AGENT\" {escaped}")
                    .expect("writing to a String never fails");
            }
            other => {
                writeln!(
                    script,
                    "# Command {}: unknown command type '{other}' — skipped",
                    i + 1
                )
                .expect("writing to a String never fails");
            }
        }
    }

    writeln!(script).expect("writing to a String never fails");
    writeln!(script, "# Cleanup is handled by the EXIT trap")
        .expect("writing to a String never fails");
    writeln!(script, "echo 'Test passed!'").expect("writing to a String never fails");

    script
}

fn main() {
    let rt = asupersync::runtime::RuntimeBuilder::new()
        .build()
        .expect("failed to build asupersync runtime");
    let handle = rt.handle();
    vessel::runtime::task::set_runtime_handle(handle.clone());
    // Spawn main_inner as a task so it runs inside the scheduler with a Cx.
    // block_on alone doesn't set up CURRENT_CX, so Cx::current() would return None.
    let join = handle.spawn(main_inner());
    rt.block_on(join);
}

async fn main_inner() {
    let cli = Cli::parse();

    // Initialize telemetry (tracing + optional OTLP export).
    // The guard must be held until exit to flush pending spans.
    let _telemetry = vessel::telemetry::init(cli.verbose);

    let socket_path = cli.socket.unwrap_or_else(default_socket_path);

    let result = match cli.command {
        Command::Server { daemon } => run_server(socket_path, daemon).await,
        Command::Doctor => run_doctor(socket_path).await,
        cmd => run_client(socket_path, cmd).await,
    };

    if let Err(e) = result {
        error!("{}", e);
        std::process::exit(1);
    }
}

async fn run_server(
    socket_path: std::path::PathBuf,
    daemon: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // When running as --daemon, escape into our own systemd scope so that
    // killing the originating pane/terminal doesn't take us down.
    if daemon && !in_vessel_scope() && vessel::has_systemd_run() {
        tracing::info!("Re-execing into vessel-server.scope via systemd-run");
        let exe = std::env::current_exe()?;
        let status = std::process::Command::new("systemd-run")
            .args([
                "--user",
                "--scope",
                "--collect",
                "--unit=vessel-server",
                "--",
            ])
            .arg(&exe)
            .args(["--socket", socket_path.to_str().unwrap_or_default()])
            .arg("server")
            .arg("--daemon")
            .status()?;
        // If systemd-run succeeded, we're done — the child is the real server.
        if status.success() {
            return Ok(());
        }
        // Otherwise fall through and run in-process.
        tracing::warn!("systemd-run re-exec failed (status {status}), running in-process");
    }

    let mut server = Server::new(socket_path);
    server.run().await?;
    Ok(())
}

/// Check if we're already running inside a vessel-owned systemd scope.
fn in_vessel_scope() -> bool {
    std::fs::read_to_string("/proc/self/cgroup").is_ok_and(|cg| cg.contains("vessel-server.scope"))
}

// Linear diagnostic routine that runs a sequence of independent checks; splitting
// it would scatter the report without improving clarity.
#[allow(clippy::too_many_lines)]
async fn run_doctor(socket_path: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::FileTypeExt;

    let mut all_ok = true;

    // 1. Check socket path
    print!("Socket path: {} ", socket_path.display());
    let socket_dir = socket_path
        .parent()
        .unwrap_or_else(|| std::path::Path::new("/tmp"));
    if socket_dir.exists() {
        if socket_dir.metadata()?.permissions().readonly() {
            println!("[FAIL] directory not writable");
            all_ok = false;
        } else {
            println!("[OK]");
        }
    } else {
        println!("[FAIL] directory does not exist");
        all_ok = false;
    }

    // 2. Check for stale socket
    print!("Stale socket check: ");
    if socket_path.exists() {
        let metadata = std::fs::metadata(&socket_path)?;
        if metadata.file_type().is_socket() {
            // Try to connect to see if daemon is running
            match vessel::runtime::net::UnixStream::connect(&socket_path).await {
                Ok(_) => println!("[OK] daemon responding"),
                Err(_) => {
                    println!("[WARN] socket exists but daemon not responding (stale?)");
                }
            }
        } else {
            println!("[FAIL] path exists but is not a socket");
            all_ok = false;
        }
    } else {
        println!("[OK] no stale socket");
    }

    // 3. Check PTY allocation
    print!("PTY allocation: ");
    match vessel::pty::spawn(&["true".to_string()], 24, 80) {
        Ok(pty) => {
            // Wait for it to complete
            let _ = pty.wait();
            println!("[OK]");
        }
        Err(e) => {
            println!("[FAIL] {e}");
            all_ok = false;
        }
    }

    // 4. Check daemon connectivity (start if needed)
    print!("Daemon connection: ");
    let mut client = Client::new(socket_path.clone());
    match client.request(Request::Ping).await {
        Ok(Response::Pong) => println!("[OK]"),
        Ok(other) => {
            println!("[FAIL] unexpected response: {other:?}");
            all_ok = false;
        }
        Err(e) => {
            println!("[FAIL] {e}");
            all_ok = false;
        }
    }

    // 5. Test spawn/kill cycle
    print!("Spawn/kill cycle: ");
    match client
        .request(Request::Spawn {
            cmd: vec!["sleep".to_string(), "60".to_string()],
            rows: 24,
            cols: 80,
            name: Some("__doctor_test__".to_string()),
            labels: vec![],
            timeout: None,
            max_output: None,
            env: vec![],
            cwd: None,
            no_resize: false,
            record: false,
            memory_limit: None,
        })
        .await
    {
        Ok(Response::Spawned { id, .. }) => {
            // Kill it
            match client
                .request(Request::Kill {
                    id: Some(id.clone()),
                    labels: vec![],
                    all: false,
                    signal: 9,
                    proc_filter: None,
                })
                .await
            {
                Ok(Response::Ok) => println!("[OK]"),
                Ok(other) => {
                    println!("[FAIL] kill returned: {other:?}");
                    all_ok = false;
                }
                Err(e) => {
                    println!("[FAIL] kill failed: {e}");
                    all_ok = false;
                }
            }
        }
        Ok(other) => {
            println!("[FAIL] spawn returned: {other:?}");
            all_ok = false;
        }
        Err(e) => {
            println!("[FAIL] spawn failed: {e}");
            all_ok = false;
        }
    }

    // Summary
    println!();
    if all_ok {
        println!("All checks passed!");
        Ok(())
    } else {
        Err("Some checks failed".into())
    }
}

/// Print the per-agent outcomes of a fan-out send.
///
/// Text and pretty get one ID-first record per agent. This breaks the "stay
/// silent on success" rule the single-agent send follows, deliberately: a
/// fan-out can partially fail, and saying nothing would report that as total
/// success.
fn report_send_results(
    results: &[vessel::protocol::SendOutcome],
    fmt: OutputFormat,
) -> Result<(), Box<dyn std::error::Error>> {
    match fmt {
        OutputFormat::Json => {
            let envelope = json_envelope(
                "result",
                json!({
                    "delivered": results.iter().filter(|r| r.is_ok()).count(),
                    "failed": results.iter().filter(|r| !r.is_ok()).count(),
                    "results": results,
                }),
                results
                    .iter()
                    .filter(|r| r.is_ok())
                    .map(|r| format!("vessel snapshot {}", r.id))
                    .collect(),
            );
            println!("{}", serde_json::to_string(&envelope)?);
        }
        OutputFormat::Text | OutputFormat::Pretty => {
            for outcome in results {
                match &outcome.error {
                    None => println!("{}  ok", outcome.id),
                    Some(e) => println!("{}  error: {e}", outcome.id),
                }
            }
        }
    }
    Ok(())
}

/// One-line summary naming how many agents missed the input.
fn send_failure_summary(results: &[vessel::protocol::SendOutcome]) -> String {
    let failed = results.iter().filter(|r| !r.is_ok()).count();
    format!(
        "{failed} of {} agents did not receive the input",
        results.len()
    )
}

/// Handle the response to a `Send`/`SendBytes`, in either shape.
///
/// A request naming one agent answers `Ok`/`Error` as it always has; a
/// selector-based one answers with per-agent results, which are printed and
/// then turned into a non-zero exit if any agent missed the input.
fn handle_send_response(
    response: Response,
    fmt: OutputFormat,
    id: Option<&str>,
) -> Result<(), Box<dyn std::error::Error>> {
    match response {
        Response::Ok => {
            match fmt {
                OutputFormat::Json => {
                    let hint = id.map_or_else(Vec::new, |id| vec![format!("vessel snapshot {id}")]);
                    let envelope = json_envelope("result", json!({"status": "ok"}), hint);
                    println!("{}", serde_json::to_string(&envelope)?);
                }
                // Keep text/pretty silent for fire-and-forget commands
                OutputFormat::Text | OutputFormat::Pretty => {}
            }
            Ok(())
        }
        Response::SendResults { results } => {
            report_send_results(&results, fmt)?;
            if results.iter().any(|r| !r.is_ok()) {
                return Err(send_failure_summary(&results).into());
            }
            Ok(())
        }
        Response::Error { message } => Err(message.into()),
        _ => Err("unexpected response".into()),
    }
}

#[allow(clippy::too_many_lines)] // Command dispatch function, splitting would reduce clarity
async fn run_client(
    socket_path: std::path::PathBuf,
    command: Command,
) -> Result<(), Box<dyn std::error::Error>> {
    // Attach command needs direct socket access, handle it separately
    if let Command::Attach {
        id,
        readonly,
        detach_key,
    } = command
    {
        return run_attach_command(socket_path, id, readonly, detach_key).await;
    }

    // Events command needs direct socket access (long-lived connection)
    if let Command::Events { filter, output } = command {
        return run_events_command(socket_path, filter, output).await;
    }

    // Subscribe command streams output from agents
    if let Command::Subscribe {
        id,
        label,
        prefix,
        format,
    } = command
    {
        return run_subscribe_command(socket_path, id, label, prefix, format).await;
    }

    // View command manages tmux session
    if let Command::View {
        mux,
        mode,
        no_resize,
        label,
        new_session,
    } = command
    {
        let auto_resize = !no_resize; // auto-resize is now the default
        return run_view_command(socket_path, mux, mode, auto_resize, label, new_session).await;
    }

    // ResizePanes command (called from tmux hook)
    // Always exit with code 0 to avoid showing tmux errors to users
    if let Command::ResizePanes { mode } = command {
        if let Err(e) = run_resize_panes_command(socket_path, mode).await {
            // Log the error but don't propagate - we're in a tmux hook
            tracing::warn!("resize-panes failed (this is okay): {}", e);
        }
        return Ok(());
    }

    // Clone socket_path before moving it to client (needed for dependency waiting)
    let socket_path_ref = socket_path.clone();
    let mut client = Client::new(socket_path);

    match command {
        Command::Spawn {
            rows,
            cols,
            name,
            label,
            timeout,
            max_output,
            mut env,
            env_inherit,
            cwd,
            no_resize,
            record,
            memory_limit,
            after,
            wait_for,
            format,
            json,
            cmd,
        } => {
            // Wait for dependencies before spawning
            if !after.is_empty() || !wait_for.is_empty() {
                wait_for_dependencies(&socket_path_ref, &after, &wait_for).await?;
            }

            // --env-inherit: expand patterns against the client env, add to the
            // env list. Patterns are exact names (EDITOR) or trailing-wildcard
            // namespaces (RITE_*). A malformed pattern is a hard error, never a
            // silent skip, so a typo cannot vanish into an empty expansion.
            if !env_inherit.is_empty() {
                // vars_os(), not vars(): a single non-UTF-8 name or value anywhere
                // in the caller's env would make vars() panic and abort the spawn,
                // even for a variable the pattern never matches. Non-UTF-8 vars are
                // skipped — they cannot be carried in a KEY=VALUE string anyway.
                let client_env: Vec<(String, String)> = std::env::vars_os()
                    .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
                    .collect();
                // Keys already set via --env win: skip inherited matches for them.
                // A child shell resolves a duplicated key in an unspecified way, so
                // the explicit value must be the only one that reaches the child.
                let explicit_keys: std::collections::HashSet<&str> = env
                    .iter()
                    .filter_map(|kv| kv.split_once('=').map(|(k, _)| k))
                    .collect();
                let matched =
                    vessel::env_inherit::resolve(&env_inherit, &client_env, &explicit_keys)?;
                if !matched.is_empty() {
                    // Audit trail: log the matched names, never the values. A
                    // wildcard expansion is otherwise invisible, and it may pull
                    // secrets from the named namespace into an untrusted worker.
                    let names: Vec<&str> = matched.iter().map(|(n, _)| n.as_str()).collect();
                    eprintln!(
                        "vessel: --env-inherit matched {} var(s): {}",
                        names.len(),
                        names.join(", ")
                    );
                }
                for (name, value) in matched {
                    env.push(format!("{name}={value}"));
                }
            }

            let request = Request::Spawn {
                cmd,
                rows,
                cols,
                name,
                labels: label,
                timeout,
                max_output,
                env,
                cwd,
                no_resize,
                record,
                memory_limit,
            };
            let response = client.request(request).await?;

            match response {
                Response::Spawned { id, pid } => {
                    let fmt = resolve_format(if json {
                        Some("json")
                    } else {
                        format.as_deref()
                    });
                    match fmt {
                        OutputFormat::Text => {
                            // Text output: just the ID (for agents parsing this)
                            println!("{id}");
                        }
                        OutputFormat::Json => {
                            // JSON envelope with advice
                            let envelope = json_envelope(
                                "agent",
                                json!({"id": id, "pid": pid}),
                                vec![
                                    format!("vessel send {id} \"<text>\""),
                                    format!("vessel attach {id}"),
                                    format!("vessel kill {id}"),
                                ],
                            );
                            println!("{}", serde_json::to_string(&envelope)?);
                        }
                        OutputFormat::Pretty => {
                            // Human-friendly output with suggestions
                            println!("Spawned: {id} (pid {pid})");
                            println!("Next: vessel send {id} \"<text>\"");
                        }
                    }
                    tracing::debug!("Spawned agent {id} (pid {pid})");
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::List {
            all,
            label,
            format,
            json,
        } => {
            let response = client.request(Request::List { labels: label }).await?;

            match response {
                Response::Agents { agents } => {
                    // Filter to running only unless --all is specified
                    let agents: Vec<_> = if all {
                        agents
                    } else {
                        agents
                            .into_iter()
                            .filter(|a| matches!(a.state, vessel::AgentState::Running))
                            .collect()
                    };

                    // Determine output format (handle --json alias)
                    let format_flag = if json {
                        Some("json")
                    } else {
                        Some(format.as_str())
                    };
                    let output_format = resolve_format(format_flag);

                    // Build full JSON objects for JSON output
                    let build_full_json = |agents: &[vessel::AgentInfo]| -> Vec<serde_json::Value> {
                        agents
                            .iter()
                            .map(|a| {
                                let mut obj = serde_json::json!({
                                    "id": a.id,
                                    "pid": a.pid,
                                    "state": match a.state {
                                        vessel::AgentState::Running => "running",
                                        vessel::AgentState::Exited => "exited",
                                    },
                                    "command": a.command.join(" "),
                                    "labels": a.labels,
                                    "size": { "rows": a.size.0, "cols": a.size.1 },
                                    "exit_code": a.exit_code,
                                });
                                if let Some(cwd) = &a.cwd {
                                    obj["cwd"] = serde_json::json!(cwd);
                                }
                                if let Some(reason) = &a.exit_reason {
                                    obj["exit_reason"] = serde_json::json!(match reason {
                                        vessel::ExitReason::Normal => "normal",
                                        vessel::ExitReason::Timeout => "timeout",
                                        vessel::ExitReason::Killed => "killed",
                                    });
                                }
                                if let Some(limits) = &a.limits {
                                    obj["limits"] = serde_json::json!({
                                        "timeout": limits.timeout,
                                        "max_output": limits.max_output,
                                    });
                                }
                                if a.no_resize {
                                    obj["no_resize"] = serde_json::json!(true);
                                }
                                if let Some(rss) = a.rss_bytes {
                                    obj["rss_bytes"] = serde_json::json!(rss);
                                }
                                obj
                            })
                            .collect()
                    };

                    match output_format {
                        OutputFormat::Json => {
                            let agents_json = serde_json::to_value(build_full_json(&agents))?;
                            let advice = if agents.is_empty() {
                                vec!["vessel spawn -- <command>".to_string()]
                            } else {
                                vec![
                                    "vessel kill <id>".to_string(),
                                    "vessel send <id> \"<text>\"".to_string(),
                                    "vessel snapshot <id>".to_string(),
                                ]
                            };
                            let output = json_envelope("agents", agents_json, advice);
                            println!("{}", serde_json::to_string(&output)?);
                        }
                        OutputFormat::Text => {
                            // ID-first compact text output with two-space delimiters
                            for a in &agents {
                                let state = match a.state {
                                    vessel::AgentState::Running => "running",
                                    vessel::AgentState::Exited => "exited",
                                };
                                let cmd = a.command.join(" ");
                                let labels_str = if a.labels.is_empty() {
                                    String::new()
                                } else {
                                    a.labels.join(",")
                                };
                                let line = if labels_str.is_empty() {
                                    text_record(&[&a.id, state, &cmd])
                                } else {
                                    text_record(&[&a.id, state, &cmd, &labels_str])
                                };
                                println!("{line}");
                            }
                        }
                        OutputFormat::Pretty => {
                            // Human-friendly table with headers and aligned columns
                            if agents.is_empty() {
                                if all {
                                    println!("(no agents)");
                                } else {
                                    println!("(no agents currently active)");
                                }
                            } else {
                                // Check if any agent has RSS data
                                let has_rss = agents.iter().any(|a| a.rss_bytes.is_some());
                                if has_rss {
                                    println!(
                                        "{:<20} {:<8} {:<10} {:<8} COMMAND",
                                        "ID", "PID", "STATE", "RSS"
                                    );
                                } else {
                                    println!("{:<20} {:<8} {:<10} COMMAND", "ID", "PID", "STATE");
                                }
                                let mut total_rss: u64 = 0;
                                for a in &agents {
                                    let state = match a.state {
                                        vessel::AgentState::Running => "running",
                                        vessel::AgentState::Exited => "exited",
                                    };
                                    let cmd = a.command.join(" ");
                                    let labels = if a.labels.is_empty() {
                                        String::new()
                                    } else {
                                        format!(" [{}]", a.labels.join(","))
                                    };
                                    if has_rss {
                                        let rss_str = a.rss_bytes.map_or_else(
                                            || "-".to_string(),
                                            |bytes| {
                                                total_rss += bytes;
                                                format_bytes(bytes)
                                            },
                                        );
                                        println!(
                                            "{:<20} {:<8} {:<10} {:<8} {}{}",
                                            a.id, a.pid, state, rss_str, cmd, labels
                                        );
                                    } else {
                                        println!(
                                            "{:<20} {:<8} {:<10} {}{}",
                                            a.id, a.pid, state, cmd, labels
                                        );
                                    }
                                }
                                if has_rss && agents.len() > 1 {
                                    println!(
                                        "{:<20} {:<8} {:<10} {:<8}",
                                        "",
                                        "",
                                        "TOTAL",
                                        format_bytes(total_rss)
                                    );
                                }
                            }
                        }
                    }
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Kill {
            id,
            label,
            all,
            force,
            proc,
            format,
            json,
        } => {
            // Must specify either id, label, proc, or all
            if id.is_none() && label.is_empty() && !all && proc.is_none() {
                return Err("must specify agent ID, --label, --proc, or --all".into());
            }
            // Can't combine --all with specific id, labels, or proc
            if all && (id.is_some() || !label.is_empty() || proc.is_some()) {
                return Err("--all cannot be combined with agent ID, --label, or --proc".into());
            }
            let signal = if force { 9 } else { 15 }; // SIGKILL or SIGTERM (default)
            let request = Request::Kill {
                id: id.clone(),
                labels: label,
                all,
                signal,
                proc_filter: proc,
            };
            let response = client.request(request).await?;

            match response {
                Response::Ok => {
                    let fmt = resolve_format(if json {
                        Some("json")
                    } else {
                        format.as_deref()
                    });
                    match fmt {
                        OutputFormat::Text => {
                            // Keep backward-compatible output
                            println!("Signal sent");
                        }
                        OutputFormat::Json => {
                            // JSON envelope with advice
                            let data = id.map_or_else(
                                || json!({"status": "ok"}),
                                |agent_id| json!({"status": "ok", "id": agent_id}),
                            );
                            let envelope =
                                json_envelope("result", data, vec!["vessel list".to_string()]);
                            println!("{}", serde_json::to_string(&envelope)?);
                        }
                        OutputFormat::Pretty => {
                            // Human-friendly output
                            if let Some(agent_id) = id {
                                println!("Killed: {agent_id}");
                            } else {
                                println!("Signal sent");
                            }
                            println!("Next: vessel list");
                        }
                    }
                }
                Response::Error { message } => {
                    // Make kill idempotent: exit 0 when agent/agents not found
                    // This matches behavior of Unix tools like rm -f, pkill
                    if message.contains("agent not found")
                        || message.contains("no running agents to kill")
                        || message.contains("no agents match the specified labels")
                    {
                        // Silently succeed - agent is already gone or wasn't there
                        return Ok(());
                    }
                    // For other errors (permission denied, signal failures), still error
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Signal {
            id,
            signal,
            label,
            all,
            proc,
        } => {
            if id.is_none() && label.is_empty() && !all && proc.is_none() {
                return Err("must specify agent ID, --label, --proc, or --all".into());
            }
            if all && (id.is_some() || !label.is_empty() || proc.is_some()) {
                return Err("--all cannot be combined with agent ID, --label, or --proc".into());
            }
            let signal = parse_signal(&signal)?;
            let request = Request::Kill {
                id,
                labels: label,
                all,
                signal,
                proc_filter: proc,
            };
            let response = client.request(request).await?;

            match response {
                Response::Ok => {
                    println!("Signal sent");
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Send {
            id,
            text,
            label,
            all,
            proc,
            paste,
            newline,
            enter,
            submit_delay_ms,
            format,
            json,
        } => {
            let selector_used = all || !label.is_empty() || proc.is_some();
            let (id, text) = vessel::split_send_positionals(id, text, selector_used)?;
            if !selector_used && id.is_none() {
                return Err("must specify agent ID, --label, --proc, or --all".into());
            }

            // "-" reads the payload from stdin: prompts for coding agents are
            // routinely too long to be comfortable as argv.
            let data = match text.as_deref() {
                Some("-") => {
                    // Blocking read: this is a one-shot at command start, with
                    // nothing else driving the runtime yet, and it keeps the
                    // path identical across both runtime backends.
                    use std::io::Read;
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buf)
                        .map_err(|e| format!("failed to read stdin: {e}"))?;
                    buf
                }
                _ => text.unwrap_or_default(),
            };

            let request = Request::Send {
                id: id.clone(),
                labels: label,
                all,
                proc_filter: proc,
                data,
                newline,
                enter,
                submit_delay_ms,
                paste,
            };
            let response = client.request(request).await?;
            let fmt = resolve_format(if json {
                Some("json")
            } else {
                format.as_deref()
            });
            handle_send_response(response, fmt, id.as_deref())?;
        }

        Command::SendBytes {
            id,
            hex,
            label,
            all,
            proc,
            format,
            json,
        } => {
            let selector_used = all || !label.is_empty() || proc.is_some();
            let (id, hex) = vessel::split_send_positionals(id, hex, selector_used)?;
            if !selector_used && id.is_none() {
                return Err("must specify agent ID, --label, --proc, or --all".into());
            }
            let hex = hex.ok_or("missing hex payload")?;

            let data = hex::decode(&hex).map_err(|e| format!("invalid hex: {e}"))?;
            let request = Request::SendBytes {
                id: id.clone(),
                labels: label,
                all,
                proc_filter: proc,
                data,
            };
            let response = client.request(request).await?;
            let fmt = resolve_format(if json {
                Some("json")
            } else {
                format.as_deref()
            });
            handle_send_response(response, fmt, id.as_deref())?;
        }

        Command::SendKeys {
            id,
            keys,
            label,
            all,
            proc,
            format,
            json,
        } => {
            use vessel::parse_key_sequence;

            let selector_used = all || !label.is_empty() || proc.is_some();
            let (id, keys) = vessel::split_send_keys_positionals(id, keys, selector_used);
            if !selector_used && id.is_none() {
                return Err("must specify agent ID, --label, --proc, or --all".into());
            }
            if keys.is_empty() {
                return Err("no keys given".into());
            }

            let fmt = resolve_format(if json {
                Some("json")
            } else {
                format.as_deref()
            });

            // One request per key, so the keys arrive in order on every agent.
            // Only the last one reports, matching the previous behaviour of a
            // single summary line for the whole sequence.
            let last = keys.len() - 1;
            for (i, key) in keys.iter().enumerate() {
                let data = parse_key_sequence(key).ok_or_else(|| format!("unknown key: {key}"))?;
                let request = Request::SendBytes {
                    id: id.clone(),
                    labels: label.clone(),
                    all,
                    proc_filter: proc.clone(),
                    data,
                };
                let response = client.request(request).await?;

                if i == last {
                    handle_send_response(response, fmt, id.as_deref())?;
                } else {
                    // Fail fast so a bad key does not leave a half-sent
                    // sequence sitting in the composer.
                    match response {
                        Response::Ok => {}
                        Response::SendResults { results } => {
                            if results.iter().any(|r| !r.is_ok()) {
                                report_send_results(&results, fmt)?;
                                return Err(send_failure_summary(&results).into());
                            }
                        }
                        Response::Error { message } => return Err(message.into()),
                        _ => return Err("unexpected response".into()),
                    }
                }
            }
        }

        Command::Tail {
            id,
            lines,
            follow,
            raw,
            replay,
        } => {
            // --replay implies --follow and --raw
            let follow = follow || replay;
            let raw = raw || replay;

            // If raw mode and stdout is a TTY, disable output post-processing
            // This is critical for TUI programs - without this, escape sequences
            // like cursor positioning get mangled (e.g., \n becomes \r\n)
            let _raw_output_guard = if raw {
                disable_output_postprocessing()
            } else {
                None
            };

            // Helper to strip ANSI codes if not raw mode
            let process_output = |data: &[u8], raw: bool| -> Vec<u8> {
                if raw {
                    data.to_vec()
                } else {
                    strip_ansi_escapes::strip(data)
                }
            };

            if follow {
                // Follow mode: continuously poll for new output
                use std::time::Duration;

                let mut last_len = 0usize;
                let poll_interval = Duration::from_millis(100);

                // If replay mode, clear screen and replay entire transcript
                // This lets TUI programs rebuild their screen state correctly
                if replay {
                    // Clear screen and move cursor home
                    print!("\x1b[2J\x1b[H");
                    std::io::stdout().flush()?;

                    // Get and output the entire transcript so far
                    let response = client
                        .request(Request::Dump {
                            id: id.clone(),
                            since: None,
                            format: crate::DumpFormat::Text,
                        })
                        .await?;

                    match response {
                        Response::Output { data, .. } => {
                            std::io::stdout().write_all(&data)?;
                            std::io::stdout().flush()?;
                            last_len = data.len();
                        }
                        Response::Error { message } => {
                            return Err(message.into());
                        }
                        _ => {
                            return Err("unexpected response".into());
                        }
                    }
                }

                loop {
                    let response = client
                        .request(Request::Tail {
                            id: id.clone(),
                            lines: 0, // Need full transcript for offset tracking
                            follow: false,
                        })
                        .await?;

                    match response {
                        Response::Output { data, exited } => {
                            if data.len() < last_len {
                                // Transcript shrank (cleared or ring buffer wrapped)
                                // Just reset our position - TUI programs will redraw
                                // themselves via SIGWINCH from the resize
                                last_len = data.len();
                            } else if data.len() > last_len {
                                // Only print new data
                                let new_data = &data[last_len..];
                                let output = process_output(new_data, raw);
                                std::io::stdout().write_all(&output)?;
                                std::io::stdout().flush()?;
                                last_len = data.len();
                            }

                            if exited {
                                break;
                            }
                        }
                        Response::Error { message } => {
                            // Agent may have exited
                            if message.contains("not found") || message.contains("exited") {
                                break;
                            }
                            return Err(message.into());
                        }
                        _ => {
                            return Err("unexpected response".into());
                        }
                    }

                    vessel::runtime::time::sleep(poll_interval).await;
                }
            } else {
                // One-shot mode: just get current tail
                let request = Request::Tail {
                    id,
                    lines,
                    follow: false,
                };
                let response = client.request(request).await?;

                match response {
                    Response::Output { data, .. } => {
                        let output = process_output(&data, raw);
                        std::io::stdout().write_all(&output)?;
                        std::io::stdout().flush()?;
                    }
                    Response::Error { message } => {
                        return Err(message.into());
                    }
                    _ => {
                        return Err("unexpected response".into());
                    }
                }
            }
        }

        Command::Dump { id, since, format } => {
            let format = match format.as_str() {
                "jsonl" => DumpFormat::Jsonl,
                _ => DumpFormat::Text,
            };
            let request = Request::Dump { id, since, format };
            let response = client.request(request).await?;

            match response {
                Response::Output { data, .. } => {
                    std::io::stdout().write_all(&data)?;
                    std::io::stdout().flush()?;
                }
                Response::Transcript { entries } => {
                    for entry in entries {
                        let json = serde_json::json!({
                            "timestamp": entry.timestamp,
                            "data": base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &entry.data
                            ),
                        });
                        println!("{}", serde_json::to_string(&json)?);
                    }
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Snapshot { id, raw, diff } => {
            let request = Request::Snapshot {
                id,
                strip_colors: !raw,
            };
            let response = client.request(request).await?;

            match response {
                Response::Snapshot { content, .. } => {
                    if let Some(diff_file) = diff {
                        // Validate path to prevent path traversal
                        let diff_path = std::path::Path::new(&diff_file);

                        // Reject paths with .. components
                        if diff_path
                            .components()
                            .any(|c| matches!(c, std::path::Component::ParentDir))
                        {
                            return Err("path traversal not allowed (.. in path)".into());
                        }

                        // Read previous snapshot
                        let previous = std::fs::read_to_string(diff_path)
                            .map_err(|e| format!("failed to read {diff_file}: {e}"))?;

                        // Compare snapshots
                        if content == previous {
                            println!("No changes");
                            return Ok(());
                        }

                        // Show unified diff
                        let diff = similar::TextDiff::from_lines(&previous, &content);

                        for change in diff.iter_all_changes() {
                            let sign = match change.tag() {
                                similar::ChangeTag::Delete => "-",
                                similar::ChangeTag::Insert => "+",
                                similar::ChangeTag::Equal => " ",
                            };
                            print!("{sign}{change}");
                        }

                        std::process::exit(1);
                    } else {
                        println!("{content}");
                    }
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Recording { id, format, json } => {
            let request = Request::GetRecording { id: id.clone() };
            let response = client.request(request).await?;

            match response {
                Response::Recording { agent_id, commands } => {
                    let fmt = resolve_format(if json {
                        Some("json")
                    } else {
                        format.as_deref()
                    });
                    match fmt {
                        OutputFormat::Text => {
                            // Text output: one line per command (timestamp, command type, payload)
                            for cmd in &commands {
                                println!(
                                    "{}",
                                    text_record(&[
                                        &cmd.timestamp.to_string(),
                                        &cmd.command,
                                        &cmd.payload
                                    ])
                                );
                            }
                        }
                        OutputFormat::Json => {
                            // JSON envelope with advice
                            let envelope = json_envelope(
                                "recording",
                                json!({"agent_id": agent_id, "commands": commands}),
                                vec![format!("vessel gen-test {id}")],
                            );
                            println!("{}", serde_json::to_string(&envelope)?);
                        }
                        OutputFormat::Pretty => {
                            // Pretty-printed JSON (current behavior)
                            let json = serde_json::to_string_pretty(&commands)?;
                            println!("{json}");
                        }
                    }
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::GenTest { id } => {
            let request = Request::GetRecording { id: id.clone() };
            let response = client.request(request).await?;

            match response {
                Response::Recording { agent_id, commands } => {
                    let script = generate_test_script(&agent_id, &commands);
                    print!("{script}");
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Env { id, format, json } => {
            let fmt = resolve_format(if json {
                Some("json")
            } else {
                format.as_deref()
            });
            let request = Request::GetEnv { id: id.clone() };
            let response = client.request(request).await?;

            match response {
                Response::AgentEnv { id: agent_id, env } => match fmt {
                    OutputFormat::Json => {
                        let map: serde_json::Map<String, serde_json::Value> = env
                            .iter()
                            .map(|(k, v)| (k.clone(), serde_json::Value::String(v.clone())))
                            .collect();
                        let envelope = json_envelope(
                            "agent_env",
                            json!({
                                "id": agent_id,
                                "env": map,
                                "count": env.len(),
                            }),
                            vec![],
                        );
                        println!("{}", serde_json::to_string(&envelope)?);
                    }
                    _ => {
                        for (key, value) in &env {
                            println!("{key}={value}");
                        }
                    }
                },
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        // These commands are handled before this match
        Command::Attach { .. }
        | Command::Server { .. }
        | Command::Doctor
        | Command::Events { .. }
        | Command::Subscribe { .. }
        | Command::View { .. }
        | Command::ResizePanes { .. } => {
            unreachable!("handled above")
        }

        Command::Resize {
            id,
            rows,
            cols,
            clear,
        } => {
            let response = client
                .request(Request::Resize {
                    id,
                    rows,
                    cols,
                    clear_transcript: clear,
                })
                .await?;

            match response {
                Response::Ok => {
                    if clear {
                        println!("Resized to {rows}x{cols} and cleared transcript");
                    } else {
                        println!("Resized to {rows}x{cols}");
                    }
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Wait {
            id: ids,
            exited,
            any,
            contains,
            pattern,
            stable,
            timeout,
            print,
        } => {
            use regex::Regex;
            use std::time::{Duration, Instant};

            if ids.is_empty() {
                return Err("at least one agent ID is required".into());
            }

            let deadline = if timeout > 0 {
                Some(Instant::now() + Duration::from_secs(timeout))
            } else {
                None
            };

            // Screen-based conditions only work with a single agent
            let has_screen_conditions = contains.is_some() || pattern.is_some() || stable.is_some();
            if ids.len() > 1 && !exited {
                return Err("multiple agent IDs require --exited".into());
            }
            if any && !exited {
                return Err("--any requires --exited".into());
            }
            if ids.len() > 1 && (has_screen_conditions || print) {
                return Err(
                    "--contains, --pattern, --stable, and --print require a single agent ID".into(),
                );
            }

            if exited {
                // Event-based approach: wait for agent(s) to exit
                use std::collections::HashMap;
                use vessel::protocol::Event;
                use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
                use vessel::runtime::net::UnixStream;

                // First check current state - agents may have already exited
                let response = client.request(Request::List { labels: vec![] }).await?;
                let agents = match response {
                    Response::Agents { agents } => agents,
                    Response::Error { message } => return Err(message.into()),
                    _ => return Err("unexpected response".into()),
                };

                // Track exit codes and which agents still need to exit
                let mut exit_codes: HashMap<String, Option<i32>> = HashMap::new();
                let mut pending: std::collections::HashSet<String> =
                    std::collections::HashSet::new();

                for id in &ids {
                    let agent = agents.iter().find(|a| a.id == *id);
                    match agent {
                        Some(a) if a.state == vessel::AgentState::Exited => {
                            exit_codes.insert(id.clone(), a.exit_code);
                        }
                        Some(_) => {
                            pending.insert(id.clone());
                        }
                        None => return Err(format!("agent not found: {id}").into()),
                    }
                }

                if (!any || exit_codes.is_empty()) && !pending.is_empty() {
                    // Subscribe to events and wait for remaining agents
                    let stream = UnixStream::connect(&socket_path_ref).await?;
                    let (reader, mut writer) = stream.into_split();
                    let mut reader = BufReader::new(reader);

                    let events_request = Request::Events {
                        filter: pending.iter().cloned().collect(),
                        include_output: false,
                    };
                    let mut json = serde_json::to_string(&events_request)?;
                    json.push('\n');
                    writer.write_all(json.as_bytes()).await?;

                    let mut line = String::new();
                    while !pending.is_empty() {
                        if let Some(dl) = deadline
                            && Instant::now() >= dl
                        {
                            eprintln!("error: timeout waiting for agent(s) to exit");
                            std::process::exit(1);
                        }

                        let read_fut = reader.read_line(&mut line);
                        let result = if let Some(dl) = deadline {
                            let remaining = dl - Instant::now();
                            vessel::runtime::time::timeout(remaining, read_fut)
                                .await
                                .unwrap_or_else(|_| {
                                    eprintln!("error: timeout waiting for agent(s) to exit");
                                    std::process::exit(1);
                                })
                        } else {
                            read_fut.await
                        };
                        match result {
                            Ok(0) => {
                                return Err("server closed connection while waiting".into());
                            }
                            Ok(_) => {
                                let response: Response = serde_json::from_str(&line)?;
                                match response {
                                    Response::Event(Event::AgentExited { ref id, exit_code })
                                        if pending.contains(id) =>
                                    {
                                        exit_codes.insert(id.clone(), exit_code);
                                        pending.remove(id);
                                        if any {
                                            break;
                                        }
                                    }
                                    Response::Error { message } => return Err(message.into()),
                                    _ => {} // Other events, keep waiting
                                }
                            }
                            Err(e) => return Err(format!("read error: {e}").into()),
                        }
                        line.clear();
                    }
                }

                if any && !pending.is_empty() {
                    let response = client.request(Request::List { labels: vec![] }).await?;
                    let agents = match response {
                        Response::Agents { agents } => agents,
                        Response::Error { message } => return Err(message.into()),
                        _ => return Err("unexpected response".into()),
                    };

                    for id in &ids {
                        if !pending.contains(id) {
                            continue;
                        }

                        let agent = agents.iter().find(|a| a.id == *id);
                        match agent {
                            Some(a) if a.state == vessel::AgentState::Exited => {
                                exit_codes.insert(id.clone(), a.exit_code);
                                pending.remove(id);
                            }
                            Some(_) => {}
                            None => return Err(format!("agent not found: {id}").into()),
                        }
                    }
                }

                // For single-agent: check screen conditions and print
                if ids.len() == 1 {
                    let id = &ids[0];
                    if has_screen_conditions || print {
                        let response = client
                            .request(Request::Snapshot {
                                id: id.clone(),
                                strip_colors: true,
                            })
                            .await?;

                        let snapshot = match response {
                            Response::Snapshot { content, .. } => content,
                            Response::Error { message } => return Err(message.into()),
                            _ => return Err("unexpected response".into()),
                        };

                        if let Some(ref needle) = contains
                            && !snapshot.contains(needle)
                        {
                            eprintln!("error: output does not contain: {needle:?}");
                            std::process::exit(1);
                        }

                        if let Some(ref pat) = pattern {
                            if pat.len() > 1000 {
                                return Err("regex pattern too long (max 1000 chars)".into());
                            }
                            let re = Regex::new(pat).map_err(|e| format!("invalid regex: {e}"))?;
                            if !re.is_match(&snapshot) {
                                eprintln!("error: output does not match pattern: {pat:?}");
                                std::process::exit(1);
                            }
                        }

                        if print {
                            println!("{snapshot}");
                        }
                    }
                }

                if any && ids.len() > 1 {
                    for id in ids.iter().filter(|id| exit_codes.contains_key(*id)) {
                        println!("{id}");
                    }
                }

                // Propagate worst exit code
                let worst_code = exit_codes
                    .values()
                    .filter_map(|c| *c)
                    .filter(|c| *c != 0)
                    .max()
                    .unwrap_or(0);
                if worst_code != 0 {
                    std::process::exit(worst_code);
                }
            } else {
                // Original snapshot-polling approach (single agent only)
                let id = &ids[0];
                let poll_interval = Duration::from_millis(50);

                let mut last_snapshot = String::new();
                let mut stable_since = Instant::now();

                loop {
                    if let Some(dl) = deadline
                        && Instant::now() >= dl
                    {
                        return Err("timeout waiting for condition".into());
                    }

                    let response = client
                        .request(Request::Snapshot {
                            id: id.clone(),
                            strip_colors: true,
                        })
                        .await?;

                    let snapshot = match response {
                        Response::Snapshot { content, .. } => content,
                        Response::Error { message } => return Err(message.into()),
                        _ => return Err("unexpected response".into()),
                    };

                    // Check conditions - all specified conditions must be met (AND logic)
                    let mut all_conditions_met = true;
                    let mut any_condition_specified = false;

                    // Check contains condition
                    if let Some(ref needle) = contains {
                        any_condition_specified = true;
                        if !snapshot.contains(needle) {
                            all_conditions_met = false;
                        }
                    }

                    // Check pattern condition
                    if let Some(ref pat) = pattern {
                        any_condition_specified = true;
                        // Limit pattern length to mitigate ReDoS
                        if pat.len() > 1000 {
                            return Err("regex pattern too long (max 1000 chars)".into());
                        }
                        let re = Regex::new(pat).map_err(|e| format!("invalid regex: {e}"))?;
                        if !re.is_match(&snapshot) {
                            all_conditions_met = false;
                        }
                    }

                    // Check stable condition (always track stability)
                    let is_stable = if let Some(stable_ms) = stable {
                        any_condition_specified = true;
                        let stable_duration = Duration::from_millis(stable_ms);
                        if snapshot == last_snapshot {
                            stable_since.elapsed() >= stable_duration
                        } else {
                            stable_since = Instant::now();
                            false
                        }
                    } else {
                        // Update stability tracking even if not checking for it
                        if snapshot != last_snapshot {
                            stable_since = Instant::now();
                        }
                        true // Not checking stability, so treat as satisfied
                    };

                    if !is_stable {
                        all_conditions_met = false;
                    }

                    // If no conditions specified, wait for any output change
                    if !any_condition_specified {
                        all_conditions_met = !snapshot.is_empty() && snapshot != last_snapshot;
                    }

                    if all_conditions_met {
                        if print {
                            println!("{snapshot}");
                        }
                        break;
                    }

                    last_snapshot = snapshot;
                    vessel::runtime::time::sleep(poll_interval).await;
                }
            }
        }

        Command::Assert {
            id,
            contains,
            not_contains,
            pattern,
            timeout,
        } => {
            use regex::Regex;
            use std::time::{Duration, Instant};

            let timeout_duration = Duration::from_secs(timeout);
            let poll_interval = Duration::from_millis(50);
            let deadline = if timeout > 0 {
                Some(Instant::now() + timeout_duration)
            } else {
                None
            };

            let response = client
                .request(Request::Snapshot {
                    id: id.clone(),
                    strip_colors: true,
                })
                .await?;

            let mut snapshot = match response {
                Response::Snapshot { content, .. } => content,
                Response::Error { message } => return Err(message.into()),
                _ => return Err("unexpected response".into()),
            };

            // If timeout specified, poll until conditions met or timeout
            if let Some(deadline_time) = deadline {
                loop {
                    // Check all conditions
                    let mut all_passed = true;
                    // Reassigned across several sequential checks below, so this
                    // cannot collapse into a single `let = if/else`.
                    #[allow(clippy::useless_let_if_seq)]
                    let mut failure_reason = String::new();

                    // Check contains
                    if let Some(ref needle) = contains
                        && !snapshot.contains(needle)
                    {
                        all_passed = false;
                        failure_reason = format!("expected output to contain: {needle:?}");
                    }

                    // Check not_contains
                    if all_passed
                        && let Some(ref needle) = not_contains
                        && snapshot.contains(needle)
                    {
                        all_passed = false;
                        failure_reason = format!("expected output NOT to contain: {needle:?}");
                    }

                    // Check pattern
                    if all_passed && let Some(ref pat) = pattern {
                        // Limit pattern length to mitigate ReDoS
                        if pat.len() > 1000 {
                            return Err("regex pattern too long (max 1000 chars)".into());
                        }
                        let re = Regex::new(pat).map_err(|e| format!("invalid regex: {e}"))?;
                        if !re.is_match(&snapshot) {
                            all_passed = false;
                            failure_reason = format!("expected output to match pattern: {pat:?}");
                        }
                    }

                    if all_passed {
                        return Ok(());
                    }

                    if Instant::now() >= deadline_time {
                        eprintln!("Assertion failed: {failure_reason}");
                        eprintln!("\nActual output:");
                        eprintln!("{snapshot}");
                        std::process::exit(1);
                    }

                    vessel::runtime::time::sleep(poll_interval).await;

                    // Get new snapshot
                    let response = client
                        .request(Request::Snapshot {
                            id: id.clone(),
                            strip_colors: true,
                        })
                        .await?;

                    snapshot = match response {
                        Response::Snapshot { content, .. } => content,
                        Response::Error { message } => return Err(message.into()),
                        _ => return Err("unexpected response".into()),
                    };
                }
            } else {
                // No timeout - check immediately
                let mut all_passed = true;
                // Reassigned across several sequential checks below, so this
                // cannot collapse into a single `let = if/else`.
                #[allow(clippy::useless_let_if_seq)]
                let mut failure_reason = String::new();

                // Check contains
                if let Some(ref needle) = contains
                    && !snapshot.contains(needle)
                {
                    all_passed = false;
                    failure_reason = format!("expected output to contain: {needle:?}");
                }

                // Check not_contains
                if all_passed
                    && let Some(ref needle) = not_contains
                    && snapshot.contains(needle)
                {
                    all_passed = false;
                    failure_reason = format!("expected output NOT to contain: {needle:?}");
                }

                // Check pattern
                if all_passed && let Some(ref pat) = pattern {
                    // Limit pattern length to mitigate ReDoS
                    if pat.len() > 1000 {
                        return Err("regex pattern too long (max 1000 chars)".into());
                    }
                    let re = Regex::new(pat).map_err(|e| format!("invalid regex: {e}"))?;
                    if !re.is_match(&snapshot) {
                        all_passed = false;
                        failure_reason = format!("expected output to match pattern: {pat:?}");
                    }
                }

                if !all_passed {
                    eprintln!("Assertion failed: {failure_reason}");
                    eprintln!("\nActual output:");
                    eprintln!("{snapshot}");
                    std::process::exit(1);
                }
            }
        }

        Command::Shutdown => {
            let response = client.request(Request::Shutdown).await?;

            match response {
                Response::Ok => {
                    println!("Server shutting down");

                    // Kill tmux session (hardcoded to "vessel" for now - see bd-1tr for unique names)
                    let _ = std::process::Command::new("tmux")
                        .args(["kill-session", "-t", "vessel"])
                        .status();
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Exec {
            rows,
            cols,
            timeout,
            shell,
            cmd,
        } => {
            use std::time::{Duration, Instant};

            // Build the command string
            let cmd_str = cmd.join(" ");

            // Spawn a shell
            let request = Request::Spawn {
                cmd: vec![shell.clone()],
                rows,
                cols,
                name: None,
                labels: vec![],
                timeout: None,
                max_output: None,
                env: vec![],
                cwd: None,
                no_resize: false,
                record: false,
                memory_limit: None,
            };
            let response = client.request(request).await?;

            let agent_id = match response {
                Response::Spawned { id, .. } => id,
                Response::Error { message } => return Err(message.into()),
                _ => return Err("unexpected response".into()),
            };

            // Give shell time to start
            vessel::runtime::time::sleep(Duration::from_millis(100)).await;

            // Send the command with a unique marker for detecting completion
            // The marker includes the exit code: __VESSEL_DONE_<pid>_<exitcode>__
            let marker_prefix = format!("__VESSEL_DONE_{}_", std::process::id());
            let full_cmd = format!("{cmd_str}; echo {marker_prefix}$?__\n");

            let send_response = client
                .request(Request::Send {
                    id: Some(agent_id.clone()),
                    labels: Vec::new(),
                    all: false,
                    proc_filter: None,
                    data: full_cmd,
                    newline: false, // Already has newline
                    enter: false,
                    submit_delay_ms: None,
                    paste: false,
                })
                .await?;

            if let Response::Error { message } = send_response {
                // Kill the agent before returning error
                let _ = client
                    .request(Request::Kill {
                        id: Some(agent_id),
                        labels: vec![],
                        all: false,
                        signal: 9,
                        proc_filter: None,
                    })
                    .await;
                return Err(message.into());
            }

            // Wait for the marker to appear
            let timeout_duration = Duration::from_secs(timeout);
            let poll_interval = Duration::from_millis(50);
            let deadline = Instant::now() + timeout_duration;

            let mut output = String::new();
            loop {
                if Instant::now() >= deadline {
                    // Kill the agent and return timeout error
                    let _ = client
                        .request(Request::Kill {
                            id: Some(agent_id),
                            labels: vec![],
                            all: false,
                            signal: 9,
                            proc_filter: None,
                        })
                        .await;
                    return Err("timeout waiting for command completion".into());
                }

                let response = client
                    .request(Request::Snapshot {
                        id: agent_id.clone(),
                        strip_colors: true,
                    })
                    .await?;

                let snapshot = match response {
                    Response::Snapshot { content, .. } => content,
                    Response::Error { message } => {
                        // Agent may have exited
                        return Err(message.into());
                    }
                    _ => return Err("unexpected response".into()),
                };

                // Look for marker at the start of a line (not in command echo)
                // Format: \n__VESSEL_DONE_<pid>_<exitcode>__
                let marker_pattern = format!("\n{marker_prefix}");
                if let Some(marker_start) = snapshot.find(&marker_pattern) {
                    // Extract output between the command echo and the marker
                    let before_marker = &snapshot[..marker_start];
                    let lines: Vec<&str> = before_marker.lines().collect();

                    // Skip the first line (command echo), take the rest as output
                    if lines.len() > 1 {
                        let output_lines: Vec<&str> = lines
                            .iter()
                            .skip(1) // Skip command echo
                            .copied()
                            .collect();
                        output = output_lines.join("\n");
                    }

                    // Extract exit code from marker
                    let after_marker = &snapshot[marker_start + 1..]; // Skip the \n
                    if let Some(exit_code_start) = after_marker.find(&marker_prefix) {
                        let code_start = exit_code_start + marker_prefix.len();
                        if let Some(code_end) = after_marker[code_start..].find("__") {
                            let code_str = &after_marker[code_start..code_start + code_end];
                            if let Ok(code) = code_str.parse::<i32>()
                                && code != 0
                            {
                                // Kill agent, print output, then exit with the command's exit code
                                let _ = client
                                    .request(Request::Kill {
                                        id: Some(agent_id.clone()),
                                        labels: vec![],
                                        all: false,
                                        signal: 9,
                                        proc_filter: None,
                                    })
                                    .await;
                                if !output.is_empty() {
                                    println!("{output}");
                                }
                                std::process::exit(code);
                            }
                        }
                    }
                    break;
                }

                vessel::runtime::time::sleep(poll_interval).await;
            }

            // Kill the agent
            let _ = client
                .request(Request::Kill {
                    id: Some(agent_id),
                    labels: vec![],
                    all: false,
                    signal: 9,
                    proc_filter: None,
                })
                .await;

            // Print the output
            if !output.is_empty() {
                println!("{output}");
            }
        }
    }

    Ok(())
}

async fn run_attach_command(
    socket_path: std::path::PathBuf,
    id: String,
    readonly: bool,
    detach_key: String,
) -> Result<(), Box<dyn std::error::Error>> {
    use vessel::cli::parse_key_notation;
    use vessel::runtime::net::UnixStream;

    // Parse detach key
    let detach_prefix = parse_key_notation(&detach_key)
        .ok_or_else(|| format!("invalid detach key notation: {detach_key}"))?;

    // Connect to the server
    let mut stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(e) => {
            // Try to start server if not running
            if e.kind() == std::io::ErrorKind::ConnectionRefused
                || e.kind() == std::io::ErrorKind::NotFound
            {
                // Start server in background
                let socket_path_clone = socket_path.clone();
                vessel::runtime::task::spawn(async move {
                    let mut server = Server::new(socket_path_clone);
                    let _ = server.run().await;
                });
                // Give server time to start
                vessel::runtime::time::sleep(vessel::runtime::time::Duration::from_millis(100))
                    .await;
                UnixStream::connect(&socket_path).await?
            } else {
                return Err(e.into());
            }
        }
    };

    let mut config = AttachConfig::new(id.clone());
    config.detach_prefix = detach_prefix;
    config.readonly = readonly;

    match run_attach(&mut stream, &id, config).await {
        Ok(reason) => {
            use vessel::protocol::AttachEndReason;
            match reason {
                AttachEndReason::Detached => {
                    eprintln!("\r\nDetached from {id}");
                }
                AttachEndReason::AgentExited { exit_code } => {
                    if let Some(code) = exit_code {
                        eprintln!("\r\nAgent {id} exited with code {code}");
                    } else {
                        eprintln!("\r\nAgent {id} exited");
                    }
                }
                AttachEndReason::Error { message } => {
                    return Err(message.into());
                }
            }
        }
        Err(e) => {
            return Err(e.into());
        }
    }

    Ok(())
}

async fn run_events_command(
    socket_path: std::path::PathBuf,
    filter: Vec<String>,
    include_output: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use vessel::runtime::net::UnixStream;

    // Connect to the server (don't auto-start - events are useless with no agents)
    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Send events request
    let request = Request::Events {
        filter,
        include_output,
    };
    let mut json = serde_json::to_string(&request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    // Stream events to stdout
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // Server disconnected
            break;
        }

        // Parse and re-emit just the event (strip Response wrapper)
        if let Ok(response) = serde_json::from_str::<Response>(&line) {
            match response {
                Response::Event(event) => {
                    // Output the event as JSON (newline-delimited)
                    let event_json = serde_json::to_string(&event)?;
                    println!("{event_json}");
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    // Ignore other responses
                }
            }
        }
    }

    Ok(())
}

// Sequential connect/subscribe/event-loop flow read top-to-bottom; extracting
// pieces would only obscure the streaming protocol handling.
#[allow(clippy::too_many_lines)]
async fn run_subscribe_command(
    socket_path: std::path::PathBuf,
    ids: Vec<String>,
    labels: Vec<String>,
    prefix: bool,
    format: String,
) -> Result<(), Box<dyn std::error::Error>> {
    use vessel::protocol::Event;
    use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use vessel::runtime::net::UnixStream;

    // Must specify at least one filter
    if ids.is_empty() && labels.is_empty() {
        return Err("must specify at least one --id or --label to subscribe to".into());
    }

    // Connect to server (don't auto-start - subscriptions are useless with no agents)
    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // If we have labels, first get the list of matching agent IDs
    // Then subscribe to events for those specific IDs
    let mut filter_ids = ids.clone();

    if !labels.is_empty() {
        // Get current agents matching the labels
        let list_request = Request::List {
            labels: labels.clone(),
        };
        let mut json = serde_json::to_string(&list_request)?;
        json.push('\n');
        writer.write_all(json.as_bytes()).await?;

        let mut line = String::new();
        reader.read_line(&mut line).await?;

        match serde_json::from_str::<Response>(&line)? {
            Response::Agents { agents } => {
                for agent in agents {
                    if !filter_ids.contains(&agent.id) {
                        filter_ids.push(agent.id);
                    }
                }
            }
            Response::Error { message } => return Err(message.into()),
            _ => return Err("unexpected response to list".into()),
        }
        line.clear();
    }

    // Subscribe to events (include output, filter to our agents)
    let request = Request::Events {
        filter: filter_ids.clone(),
        include_output: true,
    };
    let mut json = serde_json::to_string(&request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    // Process events
    let mut line = String::new();
    let jsonl_format = format == "jsonl";

    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // Server disconnected
            break;
        }

        if let Ok(response) = serde_json::from_str::<Response>(&line) {
            match response {
                Response::Event(Event::AgentOutput { id, data }) => {
                    if jsonl_format {
                        // JSONL format: emit JSON object per output chunk
                        let json_out = serde_json::json!({
                            "agent": id,
                            "data": base64::Engine::encode(
                                &base64::engine::general_purpose::STANDARD,
                                &data
                            ),
                        });
                        println!("{}", serde_json::to_string(&json_out)?);
                    } else if prefix {
                        // Prefixed raw output: [agent-id] data
                        // Split by newlines to prefix each line
                        let text = String::from_utf8_lossy(&data);
                        for chunk in text.split_inclusive('\n') {
                            print!("[{id}] {chunk}");
                        }
                        std::io::Write::flush(&mut std::io::stdout())?;
                    } else {
                        // Raw output
                        std::io::Write::write_all(&mut std::io::stdout(), &data)?;
                        std::io::Write::flush(&mut std::io::stdout())?;
                    }
                }
                Response::Event(Event::AgentSpawned {
                    id,
                    labels: agent_labels,
                    ..
                }) => {
                    // If we're filtering by labels and a new agent matches, add it to our filter
                    if !labels.is_empty()
                        && labels.iter().all(|l| agent_labels.contains(l))
                        && !filter_ids.contains(&id)
                    {
                        filter_ids.push(id.clone());
                        // Note: We can't dynamically update the filter on existing connection
                        // The new agent will be picked up if we reconnect
                        eprintln!("[subscribe] new agent matches labels: {id}");
                    }
                }
                Response::Event(Event::AgentExited { id, exit_code }) => {
                    if jsonl_format {
                        let json_out = serde_json::json!({
                            "agent": id,
                            "event": "exited",
                            "exit_code": exit_code,
                        });
                        println!("{}", serde_json::to_string(&json_out)?);
                    } else if prefix {
                        if let Some(code) = exit_code {
                            eprintln!("[{id}] exited with code {code}");
                        } else {
                            eprintln!("[{id}] exited");
                        }
                    }
                    // Remove from filter
                    filter_ids.retain(|i| i != &id);

                    // If no more agents to watch, exit
                    if filter_ids.is_empty() {
                        break;
                    }
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {}
            }
        }
    }

    Ok(())
}

// Sets up the tmux view step by step (session, panes, hooks, attach); the
// ordered side effects are clearest kept inline.
#[allow(clippy::too_many_lines)]
async fn run_view_command(
    socket_path: std::path::PathBuf,
    mux: String,
    mode: String,
    auto_resize: bool,
    labels: Vec<String>,
    new_session: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    use vessel::ViewMode;
    use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use vessel::runtime::net::UnixStream;

    // Only tmux is supported for now
    if mux != "tmux" {
        return Err(ViewError::UnsupportedMux(mux).into());
    }

    // Parse view mode
    let view_mode = ViewMode::parse(&mode)?;

    // Check tmux is available
    TmuxView::check_tmux()?;

    // Get the path to our own binary
    let vessel_path = std::env::current_exe().map_or_else(
        |_| "vessel".to_string(),
        |p| p.to_string_lossy().to_string(),
    );

    let mut view = TmuxView::with_mode(vessel_path.clone(), view_mode);

    // Connect to server, auto-starting if necessary
    let stream = match UnixStream::connect(&socket_path).await {
        Ok(s) => s,
        Err(e) => {
            // Only auto-start for expected "not running" errors
            use std::io::ErrorKind;
            match e.kind() {
                ErrorKind::NotFound | ErrorKind::ConnectionRefused => {
                    // Server not running, start it
                    tracing::info!("Starting server...");
                    std::process::Command::new(&vessel_path)
                        .arg("server")
                        .arg("--daemon")
                        .spawn()?;

                    // Wait for server to be ready (exponential backoff: 50ms → 500ms cap)
                    let mut connected = None;
                    let mut delay_ms = 50u64;
                    for _ in 0..20 {
                        vessel::runtime::time::sleep(std::time::Duration::from_millis(delay_ms))
                            .await;
                        if let Ok(s) = UnixStream::connect(&socket_path).await {
                            connected = Some(s);
                            break;
                        }
                        delay_ms = (delay_ms * 2).min(500);
                    }
                    connected.ok_or_else(|| -> Box<dyn std::error::Error> {
                        "server did not start in time".into()
                    })?
                }
                _ => {
                    // Real error (permission denied, etc.) - don't mask it
                    return Err(e.into());
                }
            }
        }
    };
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Get the list of current agents (optionally filtered by labels)
    let list_request = Request::List {
        labels: labels.clone(),
    };
    let mut json = serde_json::to_string(&list_request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let current_agents: Vec<vessel::AgentInfo> = match serde_json::from_str::<Response>(&line)? {
        Response::Agents { agents } => agents
            .into_iter()
            .filter(|a| a.state == vessel::AgentState::Running)
            .collect(),
        Response::Error { message } => return Err(message.into()),
        _ => return Err("unexpected response to list".into()),
    };
    let current_agent_ids: Vec<String> = current_agents.iter().map(|a| a.id.clone()).collect();

    if view.session_exists() && !new_session {
        // Reattach: session already exists, just reconcile panes
        tracing::info!("Reattaching to existing vessel session");

        // Ensure remain-on-exit is set (may be missing if session was created by older version)
        view.ensure_remain_on_exit();

        // Re-register pane-died hook (may be missing if session was created by older version)
        setup_pane_died_hook(&view)?;

        let existing_panes = view.discover_existing_panes()?;

        // Add panes for agents that are running but don't have a pane yet
        // (spawned while we were detached)
        let running_ids: std::collections::HashSet<&str> =
            current_agents.iter().map(|a| a.id.as_str()).collect();
        for agent in &current_agents {
            if !existing_panes.contains(&agent.id) {
                view.add_pane(&agent.id)?;
            }
            // Always update metadata (command/labels may have been set after initial spawn)
            view.set_pane_metadata(&agent.id, &agent.command.join(" "), &agent.labels);
        }

        // Respawn dead panes whose agents are still running
        // (e.g., attach process died due to server restart while detached)
        if let Ok(dead_panes) = view.find_dead_panes() {
            for (pane_id, agent_id) in &dead_panes {
                if running_ids.contains(agent_id.as_str()) {
                    tracing::info!(
                        "Respawning dead pane {} for running agent {}",
                        pane_id,
                        agent_id
                    );
                    if let Err(e) = view.respawn_pane(pane_id, agent_id) {
                        tracing::warn!("Failed to respawn pane for {}: {}", agent_id, e);
                    }
                }
            }
        }
    } else {
        // Fresh session — kill stale session if --new-session was passed
        if view.session_exists() {
            view.kill_session()?;
        }
        view.create_session()?;

        // Set up tmux hook for dynamic resizing when panes change
        if auto_resize {
            setup_resize_hook(&view, &mode)?;
        }

        // Set up pane-died hook to clean up dead panes
        setup_pane_died_hook(&view)?;

        // Create panes for existing agents
        for agent in &current_agents {
            view.add_pane(&agent.id)?;
            view.set_pane_metadata(&agent.id, &agent.command.join(" "), &agent.labels);
        }

        // Resize agents to match their pane sizes
        if auto_resize && !current_agents.is_empty() {
            vessel::runtime::time::sleep(std::time::Duration::from_millis(300)).await;

            if let Err(e) = resize_agents_to_panes(&socket_path, &view).await {
                tracing::warn!("Failed to resize agents: {}", e);
            }
        }

        // If no agents, show the waiting placeholder
        if current_agents.is_empty() {
            view.show_waiting_placeholder()?;
        }
    }

    // Bind Ctrl+P command palette before attaching (runs for both fresh and reattach)
    setup_command_palette(&view)?;

    // Spawn a task to listen for events and manage panes
    let socket_path_clone = socket_path.clone();
    let existing_agents = current_agent_ids.clone();
    let event_handle = vessel::runtime::task::spawn(async move {
        if let Err(e) = run_view_event_loop(socket_path_clone, existing_agents, view_mode).await {
            tracing::warn!("Event loop error: {}", e);
        }
    });

    // Attach to tmux (this blocks until user detaches or session ends)
    // Run in spawn_blocking so we don't block the async runtime
    let attach_result = vessel::runtime::task::spawn_blocking(move || view.attach()).await?;

    // Unbind Ctrl+P so it doesn't leak into other tmux sessions
    let _ = std::process::Command::new("tmux")
        .args(["unbind-key", "-T", "root", "C-p"])
        .status();

    // Abort the event loop task
    event_handle.abort();

    // If attach failed, return the error
    attach_result?;

    // After detach, check if there are any running agents
    // If not, clean up the server and tmux session
    let mut client = Client::new(socket_path.clone());
    let response = client.request(Request::List { labels: vec![] }).await?;

    if let Response::Agents { agents } = response {
        let running_count = agents
            .iter()
            .filter(|a| matches!(a.state, vessel::AgentState::Running))
            .count();

        if running_count == 0 {
            tracing::info!(
                "No agents running after detach - shutting down server and cleaning up tmux session"
            );

            // Request server shutdown
            let _ = client.request(Request::Shutdown).await;

            // Kill tmux session (hardcoded to "vessel" for now - see bd-1tr for unique names)
            let _ = std::process::Command::new("tmux")
                .args(["kill-session", "-t", "vessel"])
                .status();
        } else {
            tracing::debug!(
                "Agents still running after detach - leaving server and session active"
            );
        }
    }

    Ok(())
}

/// Background task that listens for events and manages tmux panes.
async fn run_view_event_loop(
    socket_path: std::path::PathBuf,
    existing_agents: Vec<String>,
    mode: vessel::ViewMode,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use vessel::protocol::Event;
    use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use vessel::runtime::net::UnixStream;

    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Get vessel path
    let vessel_path = std::env::current_exe().map_or_else(
        |_| "vessel".to_string(),
        |p| p.to_string_lossy().to_string(),
    );

    let mut view = TmuxView::with_mode(vessel_path, mode);

    // Initialize with existing agents so we track them properly
    for agent_id in existing_agents {
        view.mark_pane_exists(&agent_id);
    }

    // Subscribe to events (no output, just lifecycle)
    let request = Request::Events {
        filter: vec![],
        include_output: false,
    };
    let mut json = serde_json::to_string(&request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    // Process events
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            // Server disconnected
            break;
        }

        if let Ok(response) = serde_json::from_str::<Response>(&line) {
            match response {
                Response::Event(Event::AgentSpawned {
                    id,
                    command,
                    labels,
                    ..
                }) => {
                    let was_empty = view.is_empty();
                    if let Err(e) = view.add_pane(&id) {
                        if !view.session_exists() {
                            tracing::info!("tmux session gone, exiting event loop");
                            break;
                        }
                        tracing::warn!("Failed to add pane for {}: {}", id, e);
                    }
                    view.set_pane_metadata(&id, &command.join(" "), &labels);
                    // When transitioning from placeholder to first real pane,
                    // retile so it fills the window properly
                    if was_empty && let Err(e) = view.retile() {
                        if !view.session_exists() {
                            tracing::info!("tmux session gone, exiting event loop");
                            break;
                        }
                        tracing::warn!("Failed to retile after placeholder transition: {}", e);
                    }
                }
                Response::Event(Event::AgentExited { id, .. }) => {
                    // Check if this is the last pane BEFORE removing
                    // If so, show placeholder instead of killing the pane
                    // (killing the last pane would destroy the session)
                    if view.pane_count() == 1 {
                        view.clear_pane_tracking();
                        if let Err(e) = view.show_waiting_placeholder() {
                            if !view.session_exists() {
                                tracing::info!("tmux session gone, exiting event loop");
                                break;
                            }
                            tracing::warn!("Failed to show placeholder: {}", e);
                        }
                    } else if let Err(e) = view.remove_pane(&id) {
                        if !view.session_exists() {
                            tracing::info!("tmux session gone, exiting event loop");
                            break;
                        }
                        tracing::warn!("Failed to remove pane for {}: {}", id, e);
                    }
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Wait for spawn dependencies before proceeding.
///
/// - `after`: Wait for these agents to exit
/// - `wait_for`: Wait for pattern match in agent output. Format: "agent-id" or "agent-id:regex"
// Polls two dependency lists with shared deadline/timeout bookkeeping; keeping
// the loop inline preserves the readable wait sequence.
#[allow(clippy::too_many_lines)]
async fn wait_for_dependencies(
    socket_path: &std::path::Path,
    after: &[String],
    wait_for: &[String],
) -> Result<(), Box<dyn std::error::Error>> {
    use regex::Regex;
    use std::collections::{HashMap, HashSet};
    use vessel::protocol::Event;
    use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use vessel::runtime::net::UnixStream;

    // Parse wait_for specs into (agent_id, optional_pattern)
    let mut pattern_waits: HashMap<String, Option<Regex>> = HashMap::new();
    for spec in wait_for {
        if let Some((agent_id, pattern)) = spec.split_once(':') {
            let regex =
                Regex::new(pattern).map_err(|e| format!("invalid pattern '{pattern}': {e}"))?;
            pattern_waits.insert(agent_id.to_string(), Some(regex));
        } else {
            // No pattern - wait for any output
            pattern_waits.insert(spec.clone(), None);
        }
    }

    // Track what we're still waiting for
    let mut waiting_for_exit: HashSet<String> = after.iter().cloned().collect();
    let mut waiting_for_pattern: HashMap<String, Option<Regex>> = pattern_waits;

    // If nothing to wait for, return immediately
    if waiting_for_exit.is_empty() && waiting_for_pattern.is_empty() {
        return Ok(());
    }

    // First, check current state - some agents may have already exited
    let stream = UnixStream::connect(socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // List current agents
    let list_request = Request::List { labels: vec![] };
    let mut json = serde_json::to_string(&list_request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let agents: Vec<vessel::AgentInfo> = match serde_json::from_str::<Response>(&line)? {
        Response::Agents { agents } => agents,
        Response::Error { message } => return Err(message.into()),
        _ => return Err("unexpected response to list".into()),
    };

    // Check for already-exited agents in --after list
    for agent in &agents {
        if agent.state == vessel::AgentState::Exited && waiting_for_exit.contains(&agent.id) {
            tracing::debug!("Agent {} already exited", agent.id);
            waiting_for_exit.remove(&agent.id);
        }
    }

    // Validate that all referenced agents exist
    let agent_ids: HashSet<_> = agents.iter().map(|a| a.id.as_str()).collect();
    for id in &waiting_for_exit {
        if !agent_ids.contains(id.as_str()) {
            return Err(format!("--after: agent '{id}' not found").into());
        }
    }
    for id in waiting_for_pattern.keys() {
        if !agent_ids.contains(id.as_str()) {
            return Err(format!("--wait-for: agent '{id}' not found").into());
        }
    }

    // If all conditions already satisfied, we're done
    if waiting_for_exit.is_empty() && waiting_for_pattern.is_empty() {
        return Ok(());
    }

    // Subscribe to events to wait for remaining conditions
    drop(reader);
    drop(writer);

    let stream = UnixStream::connect(socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Subscribe to events with output (needed for pattern matching)
    let events_request = Request::Events {
        filter: vec![], // All agents
        include_output: !waiting_for_pattern.is_empty(),
    };
    let mut json = serde_json::to_string(&events_request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    // Wait for conditions
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).await? == 0 {
            return Err("server closed connection while waiting for dependencies".into());
        }

        let response: Response = serde_json::from_str(&line)?;

        match response {
            Response::Event(Event::AgentExited { id, .. }) => {
                if waiting_for_exit.remove(&id) {
                    tracing::debug!("Dependency satisfied: {} exited", id);
                }
            }
            Response::Event(Event::AgentOutput { id, data }) => {
                if let Some(pattern_opt) = waiting_for_pattern.get(&id) {
                    let output = String::from_utf8_lossy(&data);
                    // None means any output matches.
                    let matched = pattern_opt
                        .as_ref()
                        .is_none_or(|regex| regex.is_match(&output));
                    if matched {
                        tracing::debug!("Dependency satisfied: {} matched pattern", id);
                        waiting_for_pattern.remove(&id);
                    }
                }
            }
            Response::Error { message } => {
                return Err(format!("error while waiting: {message}").into());
            }
            _ => {}
        }

        // Check if all conditions satisfied
        if waiting_for_exit.is_empty() && waiting_for_pattern.is_empty() {
            tracing::debug!("All dependencies satisfied");
            return Ok(());
        }
    }
}

/// Set up tmux hooks to resize agents when panes change.
// Returns `Result` to match the fallible setup-hook family and keep `?` at call
// sites; the body is currently infallible but the signature is part of the API.
#[allow(clippy::unnecessary_wraps)]
fn setup_resize_hook(view: &TmuxView, mode: &str) -> Result<(), ViewError> {
    use std::process::Command;

    let vessel_path = view.vessel_path();
    let session_name = "vessel";
    let session_window = format!("{session_name}:agents");

    // Hook command: call vessel resize-panes when any pane is resized
    // The hook runs asynchronously (-b) so it won't block tmux
    let hook_cmd = format!("{vessel_path} resize-panes --mode={mode}");
    let run_shell = format!("run-shell -b '{hook_cmd}'");

    // Session-level hook: after-resize-pane (fires when individual panes are resized)
    let _ = Command::new("tmux")
        .args([
            "set-hook",
            "-t",
            session_name,
            "after-resize-pane",
            &run_shell,
        ])
        .status();

    // Session-level hook: client-attached (fires when a client attaches to the session)
    let _ = Command::new("tmux")
        .args([
            "set-hook",
            "-t",
            session_name,
            "client-attached",
            &run_shell,
        ])
        .status();

    // Session-level hook: client-session-changed (fires when switching to this session)
    let _ = Command::new("tmux")
        .args([
            "set-hook",
            "-t",
            session_name,
            "client-session-changed",
            &run_shell,
        ])
        .status();

    // Session-level hook: client-resized (fires when the terminal window is resized)
    let _ = Command::new("tmux")
        .args(["set-hook", "-t", session_name, "client-resized", &run_shell])
        .status();

    // Window-level hook: window-layout-changed (fires when layout changes, e.g., after split/close)
    // Note: requires -w flag for window-level hooks
    let _ = Command::new("tmux")
        .args([
            "set-hook",
            "-w",
            "-t",
            &session_window,
            "window-layout-changed",
            &run_shell,
        ])
        .status();

    Ok(())
}

/// Set up tmux hook to clean up dead panes when agents exit.
///
/// Enables `remain-on-exit` so tmux fires the `pane-died` hook instead of
/// immediately destroying panes. The hook then either kills the dead pane
/// (if other panes exist) or respawns it as a "waiting for agents" placeholder
/// (if it's the last pane, to keep the session alive).
///
/// Scoped to the vessel session — does not affect other tmux sessions.
// Infallible today, but kept fallible to match the setup-hook family API.
#[allow(clippy::unnecessary_wraps)]
fn setup_pane_died_hook(view: &TmuxView) -> Result<(), ViewError> {
    use std::process::Command;

    let session_name = "vessel";
    let vessel_path = view.vessel_path();

    // Enable remain-on-exit so pane-died hook fires (instead of pane being
    // destroyed immediately, which would skip the hook entirely)
    let _ = Command::new("tmux")
        .args(["set-option", "-t", session_name, "remain-on-exit", "on"])
        .status();

    // pane-died hook: try to respawn the attach process if the agent is still running.
    //
    // When a pane's `vessel attach --readonly` process dies (e.g., server restart,
    // connection hiccup), the pane goes stale while the agent keeps running.
    // This hook auto-reconnects by respawning the attach command.
    //
    // Flow:
    // 1. If @agent_id is set on the pane, try to respawn with attach --readonly.
    //    If the agent has exited, attach will fail and pane dies again — the view
    //    event loop processes AgentExited and removes the pane. A short sleep
    //    prevents tight respawn loops in that case.
    // 2. If @agent_id is empty (placeholder pane) and it's the last pane,
    //    respawn as the waiting placeholder.
    // 3. Otherwise, do nothing — let the view event loop handle cleanup.
    // Use run-shell so tmux expands format variables (#{@agent_id}, #{window_panes},
    // #{pane_id}) before passing to the shell. This avoids nested if-shell quoting.
    #[allow(clippy::literal_string_with_formatting_args)]
    let hook_cmd = format!(
        "run-shell 'AID=\"#{{@agent_id}}\"; \
         if [ -n \"$AID\" ]; then \
           tmux respawn-pane -k -t \"#{{pane_id}}\" \"{vessel_path} attach --readonly \\\"$AID\\\" || sleep 2\"; \
         elif [ \"#{{window_panes}}\" = \"1\" ]; then \
           tmux respawn-pane -k -t \"#{{pane_id}}\" \"printf \\\"\\033[2J\\033[H\\033[90mWaiting for agents...\\033[0m\\\"; sleep 3600\"; \
         fi'"
    );

    let _ = Command::new("tmux")
        .args(["set-hook", "-t", session_name, "pane-died", &hook_cmd])
        .status();

    Ok(())
}

/// Register vessel commands as tmux command aliases and bind Ctrl+P.
///
/// Creates aliases like `vessel-menu`, `vessel-list`, `vessel-snapshot`, etc.
/// that can be invoked from the tmux command prompt (prefix+:) in any session.
/// Also binds Ctrl+P to `vessel-menu`, scoped to the vessel session via if-shell.
// Infallible today, but kept fallible to match the setup-hook family API.
#[allow(clippy::unnecessary_wraps)]
// The "#{...}" tokens are tmux format-string syntax passed to the tmux binary,
// not Rust formatting args.
#[allow(clippy::literal_string_with_formatting_args)]
fn setup_command_palette(view: &TmuxView) -> Result<(), ViewError> {
    use std::process::Command;

    let vessel_path = view.vessel_path();
    let session_name = "vessel";

    // Register tmux command aliases (server-level, available from any session)
    let list_alias = format!(
        "vessel-list=display-popup -h 75% -w 80% -E '{vessel_path} list --format text | less -R'"
    );
    let snapshot_alias = format!(
        "vessel-snapshot=display-popup -h 75% -w 80% -E '{vessel_path} snapshot --raw #{{@agent_id}} | less -R'"
    );
    let shutdown_alias =
        format!("vessel-shutdown=display-popup -E '{vessel_path} shutdown && tmux detach-client'");

    let aliases: &[(&str, &str)] = &[
        (
            "command-alias[100]",
            "vessel-menu=display-menu -T '#[align=centre]vessel' \
            'List Agents' l vessel-list \
            'Snapshot Pane' s vessel-snapshot \
            '' '' '' \
            'Refresh Layout' r vessel-refresh \
            '' '' '' \
            'Detach' d detach-client \
            'Shutdown' S vessel-shutdown",
        ),
        ("command-alias[101]", &list_alias),
        ("command-alias[102]", &snapshot_alias),
        ("command-alias[103]", &shutdown_alias),
        ("command-alias[104]", "vessel-refresh=select-layout tiled"),
    ];

    for (key, value) in aliases {
        let _ = Command::new("tmux")
            .args(["set-option", "-s", key, value])
            .status();
    }

    // Bind Ctrl+P scoped to vessel session: shows menu in vessel, passes through elsewhere
    let _ = Command::new("tmux")
        .args([
            "bind-key",
            "-T",
            "root",
            "C-p",
            "if-shell",
            "-F",
            "#{==:#{session_name},vessel}",
            "vessel-menu",
            "send-keys C-p",
        ])
        .status();

    // Show a brief status message
    let _ = Command::new("tmux")
        .args([
            "display-message",
            "-t",
            session_name,
            "-d",
            "3000",
            "vessel view — Ctrl+P for menu, or prefix+: then vessel-<tab>",
        ])
        .status();

    Ok(())
}

/// Resize all agents to match their tmux pane sizes.
async fn resize_agents_to_panes(
    socket_path: &std::path::Path,
    view: &TmuxView,
) -> Result<(), Box<dyn std::error::Error>> {
    use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use vessel::runtime::net::UnixStream;

    let pane_sizes = view.get_pane_sizes()?;

    if pane_sizes.is_empty() {
        return Ok(());
    }

    let stream = UnixStream::connect(socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Query agent list to find no_resize agents
    let list_request = Request::List { labels: vec![] };
    let mut json = serde_json::to_string(&list_request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    let no_resize_ids: std::collections::HashSet<String> =
        match serde_json::from_str::<Response>(&line)? {
            Response::Agents { agents } => agents
                .into_iter()
                .filter(|a| a.no_resize)
                .map(|a| a.id)
                .collect(),
            _ => std::collections::HashSet::new(),
        };

    for (agent_id, (rows, cols)) in pane_sizes {
        if no_resize_ids.contains(&agent_id) {
            tracing::debug!("Skipping resize for {} (no_resize)", agent_id);
            continue;
        }
        let request = Request::Resize {
            id: agent_id.clone(),
            rows,
            cols,
            clear_transcript: true, // Clear to avoid displaying old-size output
        };

        let mut json = serde_json::to_string(&request)?;
        json.push('\n');
        writer.write_all(json.as_bytes()).await?;

        let mut line = String::new();
        reader.read_line(&mut line).await?;

        match serde_json::from_str::<Response>(&line)? {
            Response::Ok => {
                tracing::debug!(
                    "Resized {} to {}x{} (cleared transcript)",
                    agent_id,
                    rows,
                    cols
                );
            }
            Response::Error { message } => {
                tracing::warn!("Failed to resize {}: {}", agent_id, message);
            }
            _ => {}
        }
    }

    Ok(())
}

/// Handle resize-panes command (called from tmux hook).
async fn run_resize_panes_command(
    socket_path: std::path::PathBuf,
    mode: String,
) -> Result<(), Box<dyn std::error::Error>> {
    use vessel::ViewMode;
    use vessel::runtime::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use vessel::runtime::net::UnixStream;

    let view_mode = ViewMode::parse(&mode)?;

    // Get path to our binary
    let vessel_path = std::env::current_exe().map_or_else(
        |_| "vessel".to_string(),
        |p| p.to_string_lossy().to_string(),
    );

    // Create a view instance to query pane sizes
    let mut view = TmuxView::with_mode(vessel_path.clone(), view_mode);

    // First, get the list of running agents to populate active_panes
    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    let list_request = Request::List { labels: vec![] };
    let mut json = serde_json::to_string(&list_request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;

    // Collect agent IDs and their PIDs for SIGWINCH (skip no_resize agents)
    let agents: Vec<(String, u32)> = match serde_json::from_str::<Response>(&line)? {
        Response::Agents { agents } => agents
            .into_iter()
            .filter(|a| a.state == vessel::AgentState::Running && !a.no_resize)
            .map(|a| (a.id, a.pid))
            .collect(),
        Response::Error { message } => return Err(message.into()),
        _ => return Err("unexpected response to list".into()),
    };

    // Mark agents as having panes
    for (agent_id, _) in &agents {
        view.mark_pane_exists(agent_id);
    }

    // Get pane sizes and resize agents to match
    let pane_sizes = view.get_pane_sizes()?;

    // Build a map of agent_id -> pid for SIGWINCH
    let agent_pids: std::collections::HashMap<String, u32> = agents.iter().cloned().collect();

    for (agent_id, (rows, cols)) in &pane_sizes {
        // Don't clear transcript - let the running tail continue and programs
        // will redraw themselves when they receive SIGWINCH from the resize
        let request = Request::Resize {
            id: agent_id.clone(),
            rows: *rows,
            cols: *cols,
            clear_transcript: false,
        };

        let mut json = serde_json::to_string(&request)?;
        json.push('\n');
        writer.write_all(json.as_bytes()).await?;

        let mut line = String::new();
        reader.read_line(&mut line).await?;

        if matches!(serde_json::from_str::<Response>(&line), Ok(Response::Ok)) {
            tracing::debug!("Resized {} to {}x{}", agent_id, rows, cols);
        }
    }

    // Give a brief moment for the PTY resize to propagate
    vessel::runtime::time::sleep(std::time::Duration::from_millis(50)).await;

    // Send explicit SIGWINCH to each agent process to ensure they redraw
    // Some TUI programs (like btop) need this extra signal to reliably redraw
    for (agent_id, (_, _)) in &pane_sizes {
        if let Some(&pid) = agent_pids.get(agent_id) {
            // Send SIGWINCH (28) to the process. Process IDs always fit in a
            // positive i32 on supported platforms, so this cast never wraps.
            #[allow(clippy::cast_possible_wrap)]
            let signed_pid = pid as i32;
            let _ = vessel::sys::kill(signed_pid, libc::SIGWINCH);
            tracing::debug!("Sent SIGWINCH to {} (pid {})", agent_id, pid);
        }
    }

    // With attach --readonly, we don't need to respawn panes.
    // The attach is already streaming live PTY output, so when the TUI
    // program redraws after SIGWINCH, the attach passes it through directly.
    // Respawning would kill the attach and start a new one, which would
    // replay the (now stale) initial screen render.

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_escape_simple() {
        assert_eq!(shell_escape("hello"), "'hello'");
    }

    #[test]
    fn test_shell_escape_with_single_quotes() {
        assert_eq!(shell_escape("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_shell_escape_empty() {
        assert_eq!(shell_escape(""), "''");
    }

    #[test]
    fn test_shell_escape_special_chars() {
        assert_eq!(shell_escape("hello world $VAR"), "'hello world $VAR'");
    }

    #[test]
    fn test_compute_delay_normal() {
        // 500ms gap -> 0.5s
        let delay = compute_delay(1000, 1500);
        assert!((delay - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_capped_at_max() {
        // 10s gap -> capped at 2.0s
        let delay = compute_delay(1000, 11_000);
        assert!((delay - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_minimum() {
        // 10ms gap -> bumped to 0.1s
        let delay = compute_delay(1000, 1010);
        assert!((delay - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_zero_diff() {
        // Same timestamp -> minimum 0.1s
        let delay = compute_delay(1000, 1000);
        assert!((delay - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compute_delay_underflow() {
        // curr < prev (shouldn't happen, but handle gracefully)
        let delay = compute_delay(2000, 1000);
        assert!((delay - 0.1).abs() < f64::EPSILON);
    }

    #[test]
    fn test_generate_test_script_empty() {
        let script = generate_test_script("test-agent", &[]);
        assert!(script.contains("#!/bin/bash"));
        assert!(script.contains("# Agent: test-agent"));
        assert!(script.contains("# Commands: 0"));
        assert!(script.contains("set -e"));
        assert!(script.contains("trap cleanup EXIT"));
        assert!(script.contains("Test passed!"));
    }

    #[test]
    fn test_generate_test_script_send_with_newline() {
        let commands = vec![RecordedCommand {
            timestamp: 1000,
            command: "send".into(),
            payload: "hello\n".into(),
        }];
        let script = generate_test_script("agent-1", &commands);
        assert!(script.contains("vessel send -n \"$AGENT\" 'hello'"));
        assert!(script.contains("# Command 1: send text (with newline)"));
    }

    #[test]
    fn test_generate_test_script_send_without_newline() {
        let commands = vec![RecordedCommand {
            timestamp: 1000,
            command: "send".into(),
            payload: "hello".into(),
        }];
        let script = generate_test_script("agent-1", &commands);
        assert!(script.contains("vessel send \"$AGENT\" 'hello'"));
        assert!(script.contains("# Command 1: send text"));
        assert!(!script.contains("(with newline)"));
    }

    #[test]
    fn test_generate_test_script_send_bytes() {
        let commands = vec![RecordedCommand {
            timestamp: 1000,
            command: "send_bytes".into(),
            payload: "1b5b41".into(),
        }];
        let script = generate_test_script("agent-1", &commands);
        assert!(script.contains("vessel send-bytes \"$AGENT\" 1b5b41"));
    }

    #[test]
    fn test_generate_test_script_send_keys() {
        let commands = vec![RecordedCommand {
            timestamp: 1000,
            command: "send_keys".into(),
            payload: "enter".into(),
        }];
        let script = generate_test_script("agent-1", &commands);
        assert!(script.contains("vessel send-keys \"$AGENT\" 'enter'"));
    }

    #[test]
    fn test_generate_test_script_timing() {
        let commands = vec![
            RecordedCommand {
                timestamp: 1000,
                command: "send".into(),
                payload: "first\n".into(),
            },
            RecordedCommand {
                timestamp: 1500,
                command: "send".into(),
                payload: "second\n".into(),
            },
        ];
        let script = generate_test_script("agent-1", &commands);
        // Second command should have a 0.5s delay
        assert!(script.contains("sleep 0.5"));
    }

    #[test]
    fn test_generate_test_script_timing_capped() {
        let commands = vec![
            RecordedCommand {
                timestamp: 1000,
                command: "send".into(),
                payload: "first\n".into(),
            },
            RecordedCommand {
                timestamp: 60_000,
                command: "send".into(),
                payload: "second\n".into(),
            },
        ];
        let script = generate_test_script("agent-1", &commands);
        // Large gap should be capped at 2.0s
        assert!(script.contains("sleep 2.0"));
    }

    #[test]
    fn test_generate_test_script_unknown_command() {
        let commands = vec![RecordedCommand {
            timestamp: 1000,
            command: "unknown_type".into(),
            payload: "data".into(),
        }];
        let script = generate_test_script("agent-1", &commands);
        assert!(script.contains("unknown command type 'unknown_type'"));
        assert!(script.contains("skipped"));
    }

    #[test]
    fn test_generate_test_script_shell_escape_in_payload() {
        let commands = vec![RecordedCommand {
            timestamp: 1000,
            command: "send".into(),
            payload: "echo 'hello world'\n".into(),
        }];
        let script = generate_test_script("agent-1", &commands);
        // Single quotes in payload should be escaped
        assert!(script.contains("'echo '\\''hello world'\\'''"));
    }
}
