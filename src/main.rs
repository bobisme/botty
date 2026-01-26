//! botty — PTY-based Agent Runtime

use botty::{default_socket_path, run_attach, AttachConfig, Cli, Client, Command, DumpFormat, Request, Response, Server, TmuxView, ViewError};
use clap::Parser;
use std::io::Write;
use tracing::error;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize logging
    let filter = if cli.verbose {
        EnvFilter::new("botty=debug")
    } else {
        EnvFilter::new("botty=warn")
    };
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

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
    if daemon {
        // Fork to background
        // For now, we don't actually daemonize - the caller handles that
        // TODO: proper daemonization
    }

    let mut server = Server::new(socket_path);
    server.run().await?;
    Ok(())
}

async fn run_doctor(
    socket_path: std::path::PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::os::unix::fs::FileTypeExt;

    let mut all_ok = true;

    // 1. Check socket path
    print!("Socket path: {} ", socket_path.display());
    let socket_dir = socket_path.parent().unwrap_or_else(|| std::path::Path::new("/tmp"));
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
            match tokio::net::UnixStream::connect(&socket_path).await {
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
    match botty::pty::spawn(&["true".to_string()], 24, 80) {
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
            env: vec![],
            env_clear: false,
        })
        .await
    {
        Ok(Response::Spawned { id, .. }) => {
            // Kill it
            match client.request(Request::Kill { id: Some(id.clone()), labels: vec![], signal: 9 }).await {
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

#[allow(clippy::too_many_lines)] // Command dispatch function, splitting would reduce clarity
async fn run_client(
    socket_path: std::path::PathBuf,
    command: Command,
) -> Result<(), Box<dyn std::error::Error>> {
    // Attach command needs direct socket access, handle it separately
    if let Command::Attach { id, readonly, detach_key } = command {
        return run_attach_command(socket_path, id, readonly, detach_key).await;
    }

    // Events command needs direct socket access (long-lived connection)
    if let Command::Events { filter, output } = command {
        return run_events_command(socket_path, filter, output).await;
    }

    // View command manages tmux session
    if let Command::View { mux, label } = command {
        return run_view_command(socket_path, mux, label).await;
    }

    let mut client = Client::new(socket_path);

    match command {
        Command::Spawn { rows, cols, name, label, env, env_clear, cmd } => {
            let request = Request::Spawn { cmd, rows, cols, name, labels: label, env, env_clear };
            let response = client.request(request).await?;

            match response {
                Response::Spawned { id, pid } => {
                    println!("{id}");
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

        Command::List { all, label, json } => {
            let response = client.request(Request::List { labels: label }).await?;

            match response {
                Response::Agents { agents } => {
                    // Filter to running only unless --all is specified
                    let agents: Vec<_> = if all {
                        agents
                    } else {
                        agents
                            .into_iter()
                            .filter(|a| matches!(a.state, botty::AgentState::Running))
                            .collect()
                    };

                    if json {
                        // JSON output for piping to jq
                        let json_agents: Vec<_> = agents
                            .iter()
                            .map(|a| {
                                serde_json::json!({
                                    "id": a.id,
                                    "pid": a.pid,
                                    "state": match a.state {
                                        botty::AgentState::Running => "running",
                                        botty::AgentState::Exited => "exited",
                                    },
                                    "command": a.command.join(" "),
                                    "labels": a.labels,
                                    "size": { "rows": a.size.0, "cols": a.size.1 },
                                    "exit_code": a.exit_code,
                                })
                            })
                            .collect();
                        println!("{}", serde_json::to_string(&json_agents)?);
                    } else if agents.is_empty() {
                        // Human-readable empty message
                        if all {
                            println!("(no agents)");
                        } else {
                            println!("(no agents currently active)");
                        }
                    } else {
                        // Default: TOON format (token-efficient for LLMs)
                        let json_data = serde_json::json!({
                            "agents": agents.iter().map(|a| {
                                let mut agent_json = serde_json::json!({
                                    "id": a.id,
                                    "pid": a.pid,
                                    "state": match a.state {
                                        botty::AgentState::Running => "running",
                                        botty::AgentState::Exited => "exited",
                                    },
                                    "command": a.command.join(" "),
                                });
                                // Only include labels if non-empty (keeps output compact)
                                if !a.labels.is_empty() {
                                    agent_json["labels"] = serde_json::json!(a.labels);
                                }
                                agent_json
                            }).collect::<Vec<_>>()
                        });
                        let toon = toon_format::encode(&json_data, &toon_format::EncodeOptions::default())
                            .unwrap_or_else(|_| format!("{json_data:?}"));
                        println!("{toon}");
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

        Command::Kill { id, label, term } => {
            // Must specify either id or label
            if id.is_none() && label.is_empty() {
                return Err("must specify either agent ID or --label".into());
            }
            let signal = if term { 15 } else { 9 }; // SIGTERM or SIGKILL (default)
            let request = Request::Kill { id, labels: label, signal };
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
            no_newline,
        } => {
            let request = Request::Send {
                id,
                data: text,
                newline: !no_newline,
            };
            let response = client.request(request).await?;

            match response {
                Response::Ok => {}
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::SendBytes { id, hex } => {
            let data = hex::decode(&hex).map_err(|e| format!("invalid hex: {e}"))?;
            let request = Request::SendBytes { id, data };
            let response = client.request(request).await?;

            match response {
                Response::Ok => {}
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        Command::Tail { id, lines, follow, raw, replay } => {
            // --replay implies --follow and --raw
            let follow = follow || replay;
            let raw = raw || replay;

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
                        Response::Output { data } => {
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
                            lines,
                            follow: false, // Server doesn't implement follow
                        })
                        .await?;

                    match response {
                        Response::Output { data } => {
                            // Only print new data
                            if data.len() > last_len {
                                let new_data = &data[last_len..];
                                let output = process_output(new_data, raw);
                                std::io::stdout().write_all(&output)?;
                                std::io::stdout().flush()?;
                                last_len = data.len();
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

                    tokio::time::sleep(poll_interval).await;
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
                    Response::Output { data } => {
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
                Response::Output { data } => {
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

        Command::Snapshot { id, raw } => {
            let request = Request::Snapshot {
                id,
                strip_colors: !raw,
            };
            let response = client.request(request).await?;

            match response {
                Response::Snapshot { content, .. } => {
                    println!("{content}");
                }
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    return Err("unexpected response".into());
                }
            }
        }

        // These commands are handled before this match
        Command::Attach { .. } | Command::Server { .. } | Command::Doctor | Command::Events { .. } | Command::View { .. } => {
            unreachable!("handled above")
        }

        Command::Resize { id, rows, cols } => {
            let response = client.request(Request::Resize { id, rows, cols }).await?;

            match response {
                Response::Ok => {
                    println!("Resized to {rows}x{cols}");
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
            id,
            contains,
            pattern,
            stable,
            timeout,
            print,
        } => {
            use regex::Regex;
            use std::time::{Duration, Instant};

            let timeout_duration = Duration::from_secs(timeout);
            let poll_interval = Duration::from_millis(50);
            let deadline = Instant::now() + timeout_duration;

            let mut last_snapshot = String::new();
            let mut stable_since = Instant::now();

            loop {
                if Instant::now() >= deadline {
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

                // Check conditions
                let condition_met = if let Some(ref needle) = contains {
                    snapshot.contains(needle)
                } else if let Some(ref pat) = pattern {
                    let re = Regex::new(pat).map_err(|e| format!("invalid regex: {e}"))?;
                    re.is_match(&snapshot)
                } else if let Some(stable_ms) = stable {
                    let stable_duration = Duration::from_millis(stable_ms);
                    if snapshot == last_snapshot {
                        stable_since.elapsed() >= stable_duration
                    } else {
                        stable_since = Instant::now();
                        false
                    }
                } else {
                    // No condition specified - just wait for any output change
                    !snapshot.is_empty() && snapshot != last_snapshot
                };

                if condition_met {
                    if print {
                        println!("{snapshot}");
                    }
                    break;
                }

                last_snapshot = snapshot;
                tokio::time::sleep(poll_interval).await;
            }
        }

        Command::Shutdown => {
            let response = client.request(Request::Shutdown).await?;

            match response {
                Response::Ok => {
                    println!("Server shutting down");
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
                env: vec![],
                env_clear: false,
            };
            let response = client.request(request).await?;

            let agent_id = match response {
                Response::Spawned { id, .. } => id,
                Response::Error { message } => return Err(message.into()),
                _ => return Err("unexpected response".into()),
            };

            // Give shell time to start
            tokio::time::sleep(Duration::from_millis(100)).await;

            // Send the command with a unique marker for detecting completion
            // The marker includes the exit code: __BOTTY_DONE_<pid>_<exitcode>__
            let marker_prefix = format!("__BOTTY_DONE_{}_", std::process::id());
            let full_cmd = format!("{cmd_str}; echo {marker_prefix}$?__\n");

            let send_response = client
                .request(Request::Send {
                    id: agent_id.clone(),
                    data: full_cmd,
                    newline: false, // Already has newline
                })
                .await?;

            if let Response::Error { message } = send_response {
                // Kill the agent before returning error
                let _ = client
                    .request(Request::Kill {
                        id: Some(agent_id),
                        labels: vec![],
                        signal: 9,
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
                            signal: 9,
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
                // Format: \n__BOTTY_DONE_<pid>_<exitcode>__
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
                                && code != 0 {
                                    // Kill agent, print output, then exit with the command's exit code
                                    let _ = client
                                        .request(Request::Kill {
                                            id: Some(agent_id.clone()),
                                            labels: vec![],
                                            signal: 9,
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

                tokio::time::sleep(poll_interval).await;
            }

            // Kill the agent
            let _ = client
                .request(Request::Kill {
                    id: Some(agent_id),
                    labels: vec![],
                    signal: 9,
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
    use botty::cli::parse_key_notation;
    use tokio::net::UnixStream;

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
                tokio::spawn(async move {
                    let mut server = Server::new(socket_path_clone);
                    let _ = server.run().await;
                });
                // Give server time to start
                tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
                UnixStream::connect(&socket_path).await?
            } else {
                return Err(e.into());
            }
        }
    };

    let config = AttachConfig {
        detach_prefix,
        readonly,
        ..Default::default()
    };

    match run_attach(&mut stream, &id, config).await {
        Ok(reason) => {
            use botty::protocol::AttachEndReason;
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
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

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

async fn run_view_command(
    socket_path: std::path::PathBuf,
    mux: String,
    labels: Vec<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    // Only tmux is supported for now
    if mux != "tmux" {
        return Err(ViewError::UnsupportedMux(mux).into());
    }

    // Check tmux is available
    TmuxView::check_tmux()?;

    // Get the path to our own binary
    let botty_path = std::env::current_exe()
        .map_or_else(|_| "botty".to_string(), |p| p.to_string_lossy().to_string());

    let mut view = TmuxView::new(botty_path);

    // Connect to server to get current agents
    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Get the list of current agents (optionally filtered by labels)
    let list_request = Request::List { labels: labels.clone() };
    let mut json = serde_json::to_string(&list_request)?;
    json.push('\n');
    writer.write_all(json.as_bytes()).await?;

    let mut line = String::new();
    reader.read_line(&mut line).await?;
    
    let current_agents: Vec<String> = match serde_json::from_str::<Response>(&line)? {
        Response::Agents { agents } => agents
            .into_iter()
            .filter(|a| a.state == botty::AgentState::Running)
            // If label filters are specified, they're already applied server-side
            .map(|a| a.id)
            .collect(),
        Response::Error { message } => return Err(message.into()),
        _ => return Err("unexpected response to list".into()),
    };

    // Kill any existing session and create fresh
    // (old sessions may have stale panes from killed agents)
    if view.session_exists() {
        view.kill_session()?;
    }
    view.create_session()?;

    // Create panes for existing agents
    for agent_id in &current_agents {
        view.add_pane(agent_id)?;
    }

    // If no agents, show a message
    if current_agents.is_empty() {
        eprintln!("No agents running. Waiting for agents to spawn...");
    }

    // Spawn a task to listen for events and manage panes
    let socket_path_clone = socket_path.clone();
    let existing_agents = current_agents.clone();
    let event_handle = tokio::spawn(async move {
        if let Err(e) = run_view_event_loop(socket_path_clone, existing_agents).await {
            tracing::warn!("Event loop error: {}", e);
        }
    });

    // Attach to tmux (this blocks until user detaches or session ends)
    let attach_result = view.attach();

    // Abort the event loop task
    event_handle.abort();

    // If attach failed, return the error
    attach_result?;

    Ok(())
}

/// Background task that listens for events and manages tmux panes.
async fn run_view_event_loop(
    socket_path: std::path::PathBuf,
    existing_agents: Vec<String>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use botty::protocol::Event;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::UnixStream;

    let stream = UnixStream::connect(&socket_path).await?;
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);

    // Get botty path
    let botty_path = std::env::current_exe()
        .map_or_else(|_| "botty".to_string(), |p| p.to_string_lossy().to_string());

    let mut view = TmuxView::new(botty_path);
    
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
                Response::Event(Event::AgentSpawned { id, .. }) => {
                    if let Err(e) = view.add_pane(&id) {
                        tracing::warn!("Failed to add pane for {}: {}", id, e);
                    }
                }
                Response::Event(Event::AgentExited { id, .. }) => {
                    if let Err(e) = view.remove_pane(&id) {
                        tracing::warn!("Failed to remove pane for {}: {}", id, e);
                    }
                    
                    // If no more panes, kill the session
                    if view.is_empty() {
                        view.kill_session()?;
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
