//! botty — PTY-based Agent Runtime

use botty::{default_socket_path, Cli, Client, Command, DumpFormat, Request, Response, Server};
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
    let mut client = Client::new(socket_path);

    match command {
        Command::Spawn { rows, cols, cmd } => {
            let request = Request::Spawn { cmd, rows, cols };
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

        Command::List => {
            let response = client.request(Request::List).await?;

            match response {
                Response::Agents { agents } => {
                    if agents.is_empty() {
                        println!("No agents running");
                    } else {
                        println!("{:<20} {:>8} {:>10} {}", "ID", "PID", "STATE", "COMMAND");
                        for agent in agents {
                            let state = match agent.state {
                                botty::AgentState::Running => "running",
                                botty::AgentState::Exited => "exited",
                            };
                            let cmd = agent.command.join(" ");
                            println!("{:<20} {:>8} {:>10} {}", agent.id, agent.pid, state, cmd);
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

        Command::Kill { id, force } => {
            let signal = if force { 9 } else { 15 }; // SIGKILL or SIGTERM
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
            let request = Request::Tail { id, lines, follow };
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

        Command::Attach { id, readonly } => {
            let request = Request::Attach { id, readonly };
            let response = client.request(request).await?;

            match response {
                Response::Error { message } => {
                    return Err(message.into());
                }
                _ => {
                    // Attach mode not yet implemented
                    return Err("attach mode not yet implemented".into());
                }
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
    }

    Ok(())
}
