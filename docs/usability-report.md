# botty Usability Report

**Date:** 2026-01-25  
**Tester:** Claude (AI Agent)  
**Test Type:** Agent-perspective usability evaluation  
**Version:** Post-UX improvements (commits 5b6b3b1 through 41400ee)

## Executive Summary

botty is highly usable for AI agent orchestration. The core workflow of spawn -> send -> wait -> snapshot -> kill works smoothly. Recent UX improvements (`wait`, `exec`, `--name`, `tail -f`) address the most common pain points. Several minor issues remain that could be improved.

**Overall Rating: 8/10** - Production-ready for agent orchestration with minor rough edges.

## Test Methodology

Simulated a realistic multi-agent coding scenario:
1. Spawned 3 named worker agents (frontend-worker, backend-worker, test-runner)
2. Assigned parallel file-creation tasks
3. Used `wait` to detect task completion
4. Coordinated cross-worker verification
5. Used `exec` for quick one-off operations
6. Tested `snapshot` for state inspection
7. Cleaned up all agents

Full simulation: `./scripts/orchestration-test.sh`

## What Works Well

### 1. Custom Agent Names (`--name`)
```bash
botty spawn --name frontend-worker -- bash
```
- **Benefit:** Can refer to agents by role instead of random IDs
- **Benefit:** Makes logs and coordination much clearer
- **Benefit:** Easy to remember, no need to capture spawn output

### 2. Wait Command
```bash
botty wait frontend-worker --contains "DONE" --timeout 30
```
- **Benefit:** Eliminates manual polling loops
- **Benefit:** Multiple condition types (content, pattern, stable)
- **Benefit:** Configurable timeout prevents hangs
- **Works as expected:** Returns immediately when condition met

### 3. Exec Command
```bash
botty exec -- cat /tmp/file.txt
```
- **Benefit:** Perfect for quick, stateless operations
- **Benefit:** No agent management overhead
- **Benefit:** Clean output (just the command result)
- **Benefit:** Auto-cleanup of temporary agent

### 4. List Running Only (Default)
```bash
botty list        # Only running agents
botty list --all  # Include exited
```
- **Benefit:** Cleaner output in normal operation
- **Benefit:** Can still see history with `--all`

### 5. Tail Follow Mode
```bash
botty tail -f worker
```
- **Benefit:** Watch long-running tasks in real-time
- **Benefit:** Exits when agent exits

### 6. Snapshot for Clean Output
```bash
botty snapshot worker
```
- **Benefit:** ANSI codes stripped by default
- **Benefit:** Shows actual screen state, not raw bytes

## Issues Found

### Issue 1: Tail Shows Raw ANSI Escape Codes

**Severity:** Minor  
**Impact:** Confusing output when tailing interactive shells

```bash
$ botty tail backend-worker
]0;bob@8o8-arch:~/src/botty[?2004h[bob@8o8-arch botty]$ echo hi
```

**Expected:** Clean text like `snapshot` provides  
**Recommendation:** Add `--raw` flag to `tail`, make clean output default

### Issue 2: Kill with SIGTERM Often Ignored

**Severity:** Medium  
**Impact:** Need to remember to use `-9` for reliable cleanup

```bash
$ botty kill worker      # Often doesn't work for bash
$ botty kill -9 worker   # Works reliably
```

**Root Cause:** Interactive bash ignores SIGTERM  
**Recommendation:** Consider making SIGKILL the default, or add prominent documentation

### Issue 3: "No running agents" Contains "running"

**Severity:** Minor  
**Impact:** Naive grep-based checks fail

```bash
$ botty list | grep running  # Matches even when no agents!
```

**Recommendation:** Change message to "No agents running" or add machine-readable output format

### Issue 4: Server Auto-Start Race Condition

**Severity:** Minor  
**Impact:** Scripts may need explicit server start after shutdown

```bash
$ botty shutdown
$ botty spawn -- bash  # May hang briefly during auto-start
```

**Recommendation:** Document the race, or add `botty ping` that waits for server ready

### Issue 5: No Machine-Readable Output Format

**Severity:** Medium  
**Impact:** Harder to parse output reliably in scripts/agents

**Current:**
```
ID                        PID      STATE COMMAND
frontend-worker       3680201    running bash
```

**Recommendation:** Add `--json` flag for structured output:
```bash
botty list --json
# [{"id": "frontend-worker", "pid": 3680201, "state": "running", ...}]
```

### Issue 6: No Exit Code from Exec

**Severity:** Minor  
**Impact:** Can't detect if command failed

```bash
$ botty exec -- false
$ echo $?  # Always 0
```

**Recommendation:** Propagate command exit code through exec

## Feature Requests (From Agent Perspective)

### 1. Batch Operations
```bash
botty kill --all  # Kill all agents
botty send --all "shutdown"  # Send to all
```

### 2. Agent Groups/Tags
```bash
botty spawn --tag workers -- bash
botty kill --tag workers  # Kill all with tag
```

### 3. Wait for Multiple Agents
```bash
botty wait --any worker1 worker2 --contains "DONE"
botty wait --all worker1 worker2 --contains "DONE"
```

### 4. Health Check / Status
```bash
botty status worker  # Is it running? CPU usage? Memory?
```

### 5. Environment Variables
```bash
botty spawn --env API_KEY=xxx -- bash
```

## Recommended Workflow for Agents

Based on testing, here's the recommended workflow:

```bash
# 1. Start with named agents for clarity
botty spawn --name worker-1 -- bash
botty spawn --name worker-2 -- bash

# 2. Send commands with clear completion markers
botty send worker-1 'do_work && echo "__DONE__"'

# 3. Wait for completion with timeout
botty wait worker-1 --contains "__DONE__" --timeout 60

# 4. Use snapshot (not tail) for clean output inspection
OUTPUT=$(botty snapshot worker-1)

# 5. Use exec for quick one-off operations
RESULT=$(botty exec -- git status --short)

# 6. Always use -9 for reliable cleanup
botty kill -9 worker-1
botty kill -9 worker-2
```

## Comparison: Before vs After UX Improvements

| Task | Before | After |
|------|--------|-------|
| Wait for output | Manual polling loop | `botty wait --contains X` |
| Quick command | spawn + send + wait + kill | `botty exec -- cmd` |
| Agent naming | Capture random ID | `--name my-agent` |
| See running only | Parse full list | `botty list` (default) |
| Stream output | Not implemented | `botty tail -f` |

## Conclusion

botty is an effective tool for multi-agent orchestration. The recent UX improvements address the main pain points I encountered in earlier testing. The remaining issues are minor and have workarounds.

**For AI agents using botty:**
- Use `--name` for all agents
- Use `wait` instead of polling
- Use `exec` for stateless operations
- Use `snapshot` instead of `tail` for clean output
- Always `kill -9` for reliable cleanup

**Priority improvements for next iteration:**
1. Add `--json` output flag for machine parsing
2. Make `tail` strip ANSI codes by default
3. Document SIGTERM vs SIGKILL behavior prominently
