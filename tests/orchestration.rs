//! Multi-agent orchestration test scenarios.
//!
//! These tests demonstrate and verify the primary use case: one agent (orchestrator)
//! spawning and coordinating multiple TUI agents.

use botty::testing::TestHarness;
use std::time::Duration;

/// Scenario: Orchestrator spawns multiple worker agents and coordinates their work.
///
/// This simulates a coding agent spawning multiple sub-agents to work on different
/// parts of a task, then collecting their results.
#[tokio::test]
async fn test_orchestrator_spawns_workers() {
    let harness = TestHarness::new().await;

    // Orchestrator spawns three workers
    let worker1 = harness
        .spawn(&["sh", "-c", "echo 'Worker 1 starting'; sleep 0.5; echo 'Worker 1 done: RESULT_A'"])
        .await
        .expect("spawn worker 1");

    let worker2 = harness
        .spawn(&["sh", "-c", "echo 'Worker 2 starting'; sleep 0.3; echo 'Worker 2 done: RESULT_B'"])
        .await
        .expect("spawn worker 2");

    let worker3 = harness
        .spawn(&["sh", "-c", "echo 'Worker 3 starting'; sleep 0.4; echo 'Worker 3 done: RESULT_C'"])
        .await
        .expect("spawn worker 3");

    // Wait for all workers to complete their tasks
    let timeout = Duration::from_secs(5);

    worker1
        .wait_for_content("RESULT_A", timeout)
        .await
        .expect("worker 1 should complete");

    worker2
        .wait_for_content("RESULT_B", timeout)
        .await
        .expect("worker 2 should complete");

    worker3
        .wait_for_content("RESULT_C", timeout)
        .await
        .expect("worker 3 should complete");

    // Verify all agents are tracked
    let agents = harness.list().await.expect("list agents");
    assert_eq!(agents.len(), 3, "should have 3 workers");

    // Cleanup
    worker1.kill().await.ok();
    worker2.kill().await.ok();
    worker3.kill().await.ok();
    harness.shutdown().await;
}

/// Scenario: Sequential agent pipeline - output of one feeds into another.
///
/// Simulates a pipeline where Agent A produces data, Agent B processes it.
#[tokio::test]
async fn test_agent_pipeline() {
    let harness = TestHarness::new().await;
    let timeout = Duration::from_secs(5);

    // Agent A: Producer - creates some data
    let producer = harness
        .spawn(&["bash"])
        .await
        .expect("spawn producer");

    // Wait for prompt
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Producer generates data
    producer.send("DATA_ITEM_1='hello'").await.expect("set data 1");
    producer.send("DATA_ITEM_2='world'").await.expect("set data 2");
    producer.send("echo \"OUTPUT: $DATA_ITEM_1 $DATA_ITEM_2\"").await.expect("echo");

    // Wait for producer output
    let _producer_output = producer
        .wait_for_content("OUTPUT: hello world", timeout)
        .await
        .expect("producer should output data");

    // Agent B: Consumer - uses the data
    let consumer = harness
        .spawn(&["bash"])
        .await
        .expect("spawn consumer");

    tokio::time::sleep(Duration::from_millis(200)).await;

    // Consumer receives the "data" and processes it
    consumer.send("RECEIVED='hello world'").await.expect("receive");
    consumer.send("echo \"PROCESSED: ${RECEIVED^^}\"").await.expect("process");

    // Wait for consumer to process
    let consumer_output = consumer
        .wait_for_content("PROCESSED: HELLO WORLD", timeout)
        .await
        .expect("consumer should process data");

    assert!(consumer_output.contains("HELLO WORLD"));

    producer.kill().await.ok();
    consumer.kill().await.ok();
    harness.shutdown().await;
}

/// Scenario: Interactive agent that responds to multiple commands.
///
/// Simulates an interactive TUI-style agent that maintains state across commands.
#[tokio::test]
async fn test_interactive_stateful_agent() {
    let harness = TestHarness::new().await;
    let timeout = Duration::from_secs(5);

    let agent = harness
        .spawn(&["bash"])
        .await
        .expect("spawn agent");

    // Wait for initial prompt
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Command 1: Set up state
    agent.send("COUNTER=0").await.expect("init counter");
    agent.send("echo 'Counter initialized'").await.expect("echo init");
    agent
        .wait_for_content("Counter initialized", timeout)
        .await
        .expect("init should complete");

    // Command 2: Increment state
    agent.send("COUNTER=$((COUNTER + 1))").await.expect("inc 1");
    agent.send("echo \"Count: $COUNTER\"").await.expect("echo 1");
    agent
        .wait_for_content("Count: 1", timeout)
        .await
        .expect("count should be 1");

    // Command 3: Increment again
    agent.send("COUNTER=$((COUNTER + 1))").await.expect("inc 2");
    agent.send("echo \"Count: $COUNTER\"").await.expect("echo 2");
    agent
        .wait_for_content("Count: 2", timeout)
        .await
        .expect("count should be 2");

    // Command 4: Final state check
    agent.send("echo \"Final count: $COUNTER\"").await.expect("final");
    agent
        .wait_for_content("Final count: 2", timeout)
        .await
        .expect("final count should be 2");

    agent.kill().await.ok();
    harness.shutdown().await;
}

/// Scenario: Agent failure handling - one agent crashes, others continue.
#[tokio::test]
async fn test_agent_failure_isolation() {
    let harness = TestHarness::new().await;
    let timeout = Duration::from_secs(5);

    // Spawn a stable agent
    let stable = harness
        .spawn(&["sh", "-c", "echo 'Stable agent running'; sleep 10"])
        .await
        .expect("spawn stable");

    // Spawn an agent that will exit with error
    let failing = harness
        .spawn(&["sh", "-c", "echo 'Failing agent starting'; sleep 0.3; exit 1"])
        .await
        .expect("spawn failing");

    // Wait for both to start
    stable
        .wait_for_content("Stable agent running", timeout)
        .await
        .expect("stable should start");

    failing
        .wait_for_content("Failing agent starting", timeout)
        .await
        .expect("failing should start");

    // Wait for failing agent to exit
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Stable agent should still be responsive
    let snapshot = stable.snapshot().await.expect("stable should still work");
    assert!(snapshot.contains("Stable agent running"));

    // List should show both (even exited ones stay in list)
    let agents = harness.list().await.expect("list");
    assert_eq!(agents.len(), 2);

    stable.kill().await.ok();
    harness.shutdown().await;
}

/// Scenario: Rapid spawn/kill cycle - stress test for resource management.
#[tokio::test]
async fn test_rapid_spawn_kill_cycle() {
    let harness = TestHarness::new().await;

    for i in 0..10 {
        let agent = harness
            .spawn(&["sh", "-c", &format!("echo 'Agent {i}'; sleep 10")])
            .await
            .expect(&format!("spawn agent {i}"));

        agent
            .wait_for_content(&format!("Agent {i}"), Duration::from_secs(2))
            .await
            .expect(&format!("agent {i} should output"));

        agent.kill().await.expect(&format!("kill agent {i}"));
    }

    // All agents should be killed, but may still be in list as exited
    harness.shutdown().await;
}

/// Scenario: Concurrent commands to multiple agents.
#[tokio::test]
async fn test_concurrent_agent_commands() {
    let harness = TestHarness::new().await;
    let timeout = Duration::from_secs(5);

    // Spawn three interactive agents
    let agents: Vec<_> = futures::future::join_all((0..3).map(|_| harness.spawn(&["bash"])))
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("spawn all agents");

    // Wait for shells to start
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Send commands to all agents concurrently
    let send_futures: Vec<_> = agents
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let agent = agent.clone();
            async move {
                agent
                    .send(&format!("echo 'Response from agent {i}'"))
                    .await
            }
        })
        .collect();

    futures::future::join_all(send_futures)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("send to all agents");

    // Wait for all responses concurrently
    let wait_futures: Vec<_> = agents
        .iter()
        .enumerate()
        .map(|(i, agent)| {
            let agent = agent.clone();
            async move {
                agent
                    .wait_for_content(&format!("Response from agent {i}"), timeout)
                    .await
            }
        })
        .collect();

    futures::future::join_all(wait_futures)
        .await
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .expect("all agents should respond");

    // Cleanup
    for agent in agents {
        agent.kill().await.ok();
    }
    harness.shutdown().await;
}

/// Scenario: Simulated TUI agent with screen updates.
///
/// Uses a simple script that updates the screen, simulating TUI behavior.
#[tokio::test]
async fn test_tui_screen_updates() {
    let harness = TestHarness::new().await;

    // Spawn a "TUI" that updates the screen
    let tui = harness
        .spawn(&[
            "sh",
            "-c",
            r#"
            echo "Loading..."
            sleep 0.3
            printf "\r          \r"  # Clear line
            echo "Ready!"
            sleep 10
            "#,
        ])
        .await
        .expect("spawn tui");

    // First we see loading
    tui.wait_for_content("Loading", Duration::from_secs(2))
        .await
        .expect("should see loading");

    // Then wait for ready state
    tui.wait_for_content("Ready", Duration::from_secs(2))
        .await
        .expect("should see ready");

    // Screen should be stable now
    let snapshot = tui
        .wait_for_stable(Duration::from_millis(200), Duration::from_secs(2))
        .await
        .expect("screen should stabilize");

    assert!(snapshot.contains("Ready"));

    tui.kill().await.ok();
    harness.shutdown().await;
}
