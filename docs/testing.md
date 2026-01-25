# Testing botty

This document describes the testing methodology for botty, with a focus on usability testing from an AI agent's perspective.

## Test Categories

### 1. Unit Tests (`cargo test`)

Unit tests cover individual components:
- Protocol serialization/deserialization
- Transcript ring buffer operations
- Screen normalization
- Name generation

### 2. Integration Tests (`tests/integration.rs`)

Integration tests verify the server-client IPC:
- Server startup/shutdown
- Agent spawn, list, kill lifecycle
- Send commands and receive output
- Attach mode with detach

### 3. CLI Tests (`tests/cli.rs`)

End-to-end CLI tests using `assert_cmd`:
- Command-line argument parsing
- Full workflows from spawn to kill
- Error handling and output validation

### 4. Orchestration Tests (`tests/orchestration.rs`)

Multi-agent coordination scenarios:
- Spawning multiple concurrent agents
- Agent pipelines (output of one feeds another)
- TUI screen update handling
- Failure isolation between agents

### 5. Usability Tests (`scripts/orchestration-test.sh`)

Manual/scripted tests simulating real agent usage patterns.

## Running Tests

```bash
# All automated tests
cargo test

# With debug output
RUST_LOG=debug cargo test -- --nocapture

# Specific test
cargo test test_name

# Orchestration simulation
./scripts/orchestration-test.sh
```

## Usability Testing Methodology

### Purpose

Evaluate botty from the perspective of an orchestrating AI agent that needs to:
1. Spawn multiple worker agents (simulating coding agents)
2. Assign tasks to workers
3. Wait for task completion
4. Coordinate between workers
5. Collect results
6. Clean up resources

### Simulation Scenarios

#### Scenario 1: Multi-Agent Task Distribution

An orchestrator spawns specialized workers and coordinates their work:

```bash
# Spawn named workers
botty spawn --name frontend-worker -- bash
botty spawn --name backend-worker -- bash
botty spawn --name test-runner -- bash

# Assign tasks
botty send frontend-worker 'create_component Button'
botty send backend-worker 'create_api endpoint'

# Wait for completion
botty wait frontend-worker --contains "DONE" --timeout 30
botty wait backend-worker --contains "DONE" --timeout 30

# Coordinate verification
botty send test-runner 'verify_files'
botty wait test-runner --contains "PASS"

# Cleanup
botty kill -9 frontend-worker
botty kill -9 backend-worker  
botty kill -9 test-runner
```

#### Scenario 2: Quick One-Off Commands

Using `exec` for simple operations that don't need persistent agents:

```bash
# Get file contents
botty exec -- cat src/main.rs

# Run a build check
botty exec -- cargo check 2>&1

# Quick git status
botty exec -- git status --short
```

#### Scenario 3: Monitoring Long-Running Tasks

Using `tail -f` to watch agent output:

```bash
botty spawn --name builder -- bash
botty send builder 'cargo build 2>&1'
botty tail -f builder  # Watch build output in real-time
```

### Running the Simulation

The full orchestration simulation can be run with:

```bash
./scripts/orchestration-test.sh
```

This script:
1. Starts a clean server
2. Spawns 3 named worker agents
3. Assigns parallel tasks to workers
4. Waits for task completion
5. Coordinates cross-worker verification
6. Tests `exec` and `snapshot` commands
7. Cleans up all agents
8. Reports results

### Metrics Collected

- Time to spawn agents
- Time to execute commands
- Wait accuracy (does `wait` return when expected?)
- Output correctness (is `snapshot` accurate?)
- Cleanup reliability (do all agents terminate?)

## Adding New Tests

### Unit Test Pattern

```rust
#[test]
fn test_feature_name() {
    // Setup
    let mut manager = AgentManager::new();
    
    // Action
    let result = manager.do_something();
    
    // Assert
    assert!(result.is_ok());
}
```

### Integration Test Pattern

```rust
#[tokio::test]
async fn test_feature_integration() {
    let socket_path = unique_socket_path();
    let _cleanup = SocketCleanup(socket_path.clone());
    
    // Start server
    let server_handle = tokio::spawn(async move {
        let mut server = Server::new(socket_path);
        server.run().await
    });
    
    tokio::time::sleep(Duration::from_millis(100)).await;
    
    // Test client operations
    let mut client = Client::new(socket_path);
    // ... test code ...
    
    // Cleanup
    let _ = client.request(Request::Shutdown).await;
    server_handle.abort();
}
```

### CLI Test Pattern

```rust
#[test]
fn test_cli_feature() {
    let mut env = TestEnv::new();
    env.start_server();
    
    env.botty()
        .args(["command", "arg"])
        .assert()
        .success()
        .stdout(predicate::str::contains("expected"));
}
```

## Known Issues to Test For

1. **Bash ignores SIGTERM**: Always use `kill -9` for reliable termination
2. **Auto-start race**: After shutdown, wait before spawning new agents
3. **Tail shows escape codes**: Use `snapshot` for clean output
4. **List output parsing**: "No running agents" contains the word "running"
