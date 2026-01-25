//! botty — PTY-based Agent Runtime

use botty::{default_socket_path, run_attach, AttachConfig, Cli, Client, Command, DumpFormat, Request, Response, Server};
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

async fn run_client(
    socket_path: std::path::PathBuf,
    command: Command,
) -> Result<(), Box<dyn std::error::Error>> {
    // Attach command needs direct socket access, handle it separately
    if let Command::Attach { id, readonly, detach_key } = command {
        return run_attach_command(socket_path, id, readonly, detach_key).await;
    }

    let mut client = Client::new(socket_path);

    match command {
        Command::Spawn { rows, cols, name, cmd } => {
            let request = Request::Spawn { cmd, rows, cols, name };
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

        Command::List { all, json } => {
            let response = client.request(Request::List).await?;

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
                                serde_json::json!({
                                    "id": a.id,
                                    "pid": a.pid,
                                    "state": match a.state {
                                        botty::AgentState::Running => "running",
                                        botty::AgentState::Exited => "exited",
                                    },
                                    "command": a.command.join(" "),
                                })
                            }).collect::<Vec<_>>()
                        });
                        let toon = toon_format::encode(&json_data, &toon_format::EncodeOptions::default())
                            .unwrap_or_else(|_| format!("{:?}", json_data));
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

        Command::Kill { id, term } => {
            let signal = if term { 15 } else { 9 }; // SIGTERM or SIGKILL (default)
            let request = Request::Kill { id, signal };
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

        Command::Tail { id, lines, follow } => {
            if follow {
                // Follow mode: continuously poll for new output
                use std::time::Duration;

                let mut last_len = 0usize;
                let poll_interval = Duration::from_millis(100);

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
                                std::io::stdout().write_all(new_data)?;
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
                        std::io::stdout().write_all(&data)?;
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

        Command::Attach { .. } => {
            unreachable!("handled above")
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

        Command::Server { .. } => {
            unreachable!("handled above")
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
            let marker = format!("__BOTTY_DONE_{}__", std::process::id());
            let full_cmd = format!("{cmd_str}; echo {marker}\n");

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
                        id: agent_id,
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
                            id: agent_id,
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
                let marker_line = format!("\n{marker}");
                if snapshot.contains(&marker_line) {
                    // Extract output between the command echo and the marker
                    if let Some(marker_pos) = snapshot.find(&marker_line) {
                        // Get everything before the marker line
                        let before_marker = &snapshot[..marker_pos];
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
                    }
                    break;
                }

                tokio::time::sleep(poll_interval).await;
            }

            // Kill the agent
            let _ = client
                .request(Request::Kill {
                    id: agent_id,
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
                    eprintln!("\r\nDetached from {}", id);
                }
                AttachEndReason::AgentExited { exit_code } => {
                    if let Some(code) = exit_code {
                        eprintln!("\r\nAgent {} exited with code {}", id, code);
                    } else {
                        eprintln!("\r\nAgent {} exited", id);
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
