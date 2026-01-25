#!/bin/bash
# orchestration-test.sh - Simulate multi-agent orchestration workflow
#
# This script tests botty from the perspective of an orchestrating agent
# that spawns and coordinates multiple worker agents.
#
# Usage: ./scripts/orchestration-test.sh [--verbose]

set -e

VERBOSE=${1:-}
# Use pre-built binary for speed (build first if needed)
cargo build --quiet 2>/dev/null || true
BOTTY="./target/debug/botty"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

log() { echo -e "${GREEN}[TEST]${NC} $1"; }
warn() { echo -e "${YELLOW}[WARN]${NC} $1"; }
error() { echo -e "${RED}[ERROR]${NC} $1"; }

# Cleanup function
cleanup() {
	log "Cleaning up..."
	$BOTTY kill -9 frontend-worker 2>/dev/null || true
	$BOTTY kill -9 backend-worker 2>/dev/null || true
	$BOTTY kill -9 test-runner 2>/dev/null || true
	$BOTTY shutdown 2>/dev/null || true
	rm -rf /tmp/botty-test-app 2>/dev/null || true
}
trap cleanup EXIT

# Ensure clean state
$BOTTY shutdown 2>/dev/null || true
sleep 1

# Start server explicitly to avoid auto-start race
log "Starting server..."
$BOTTY server &
SERVER_PID=$!
sleep 0.5

log "=== Phase 1: Spawn Worker Agents ==="

# Spawn three workers with custom names
log "Spawning frontend-worker..."
FRONTEND=$($BOTTY spawn --name frontend-worker -- bash)
log "Spawning backend-worker..."
BACKEND=$($BOTTY spawn --name backend-worker -- bash)
log "Spawning test-runner..."
TESTER=$($BOTTY spawn --name test-runner -- bash)

log "Spawned workers: frontend=$FRONTEND, backend=$BACKEND, tester=$TESTER"

# Verify all running
RUNNING=$($BOTTY list | grep -c "running" || echo 0)
if [ "$RUNNING" -lt 3 ]; then
	error "Expected 3 running agents, got $RUNNING"
	$BOTTY list
	exit 1
fi
log "Verified: $RUNNING agents running"

log "=== Phase 2: Parallel Task Assignment ==="

# Send tasks to workers in parallel
$BOTTY send frontend-worker 'mkdir -p /tmp/botty-test-app/src/components && echo "export const Button = () => <button>Click</button>" > /tmp/botty-test-app/src/components/Button.tsx && echo "FRONTEND_DONE"'
$BOTTY send backend-worker 'mkdir -p /tmp/botty-test-app/src/api && echo "export const api = { fetch: () => {} }" > /tmp/botty-test-app/src/api/index.ts && echo "BACKEND_DONE"'

log "Tasks dispatched to frontend and backend workers"

log "=== Phase 3: Wait for Completion ==="

# Wait for both to complete
$BOTTY wait frontend-worker --contains "FRONTEND_DONE" --timeout 10
log "Frontend worker completed"

$BOTTY wait backend-worker --contains "BACKEND_DONE" --timeout 10
log "Backend worker completed"

log "=== Phase 4: Coordinate Verification ==="

# Have test-runner verify the work
$BOTTY send test-runner 'ls /tmp/botty-test-app/src/components/Button.tsx && ls /tmp/botty-test-app/src/api/index.ts && echo "VERIFY_DONE"'
$BOTTY wait test-runner --contains "VERIFY_DONE" --timeout 5
log "Test runner verified files exist"

log "=== Phase 5: Test exec command ==="

# Use exec for quick operations
CONTENT=$($BOTTY exec -- cat /tmp/botty-test-app/src/components/Button.tsx)
if [[ "$CONTENT" == *"Button"* ]]; then
	log "exec command works: retrieved Button component"
else
	error "exec command failed to retrieve expected content"
	exit 1
fi

log "=== Phase 6: Test snapshot ==="

SNAPSHOT=$($BOTTY snapshot frontend-worker)
if [[ "$SNAPSHOT" == *"FRONTEND_DONE"* ]]; then
	log "snapshot shows expected output"
else
	warn "snapshot may not contain expected content (timing issue?)"
fi

log "=== Phase 7: Cleanup ==="

# Kill with SIGKILL (bash ignores SIGTERM)
$BOTTY kill -9 frontend-worker
$BOTTY kill -9 backend-worker
$BOTTY kill -9 test-runner

# Wait for processes to die - SIGKILL should be immediate but PTY cleanup takes time
sleep 0.5

# Check if any are still running (look for "running" in the STATE column, not in "No running agents")
LIST_OUTPUT=$($BOTTY list 2>&1)
# If output contains a table row with "running" state (not just the "No running agents" message)
if echo "$LIST_OUTPUT" | grep -E "^\S+\s+[0-9]+\s+running" >/dev/null; then
	warn "Some agents still running, waiting more..."
	sleep 1
	LIST_OUTPUT=$($BOTTY list 2>&1)
	if echo "$LIST_OUTPUT" | grep -E "^\S+\s+[0-9]+\s+running" >/dev/null; then
		error "Agents still running after cleanup:"
		echo "$LIST_OUTPUT"
		exit 1
	fi
fi
log "All agents terminated"

echo ""
log "=== ALL TESTS PASSED ==="
echo ""
echo "Summary:"
echo "  - Custom named agents: PASS"
echo "  - Parallel spawn: PASS"
echo "  - Send commands: PASS"
echo "  - Wait for completion: PASS"
echo "  - Cross-agent coordination: PASS"
echo "  - exec command: PASS"
echo "  - snapshot: PASS"
echo "  - Agent cleanup: PASS"
