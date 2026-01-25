# botty — PTY-based Agent Runtime

## Overview

**botty** is a tmux-style, user-scoped PTY server for running and coordinating interactive agents (e.g. `codex-cli`) as terminal programs.

It is designed for:

- agent orchestration via real terminals (PTYs)
- deterministic-ish automation and snapshot testing of TUIs
- optional human observability via tmux/zellij or a built-in viewer

botty is **not** a terminal multiplexer replacement. It is an agent runtime that _exposes_ terminal state.

---

## Core Model

- botty runs an **implicit server** (tmux-style):
  - auto-started on first command
  - user-scoped
  - exits when no agents remain

- The server owns:
  - PTYs
  - transcripts
  - virtual screen models

- CLI commands are lightweight clients communicating over a Unix socket

```
client (botty CLI)
      │
      ▼
botty server (PTY + state owner)
      │
      ▼
agent processes (codex-cli, shells, TUIs)
```

---

## Goals

- Spawn and manage many interactive agent processes
- Programmatic control (send input, read output)
- Maintain **virtual screen state** for snapshot testing
- Allow humans to observe or attach interactively
- Clean separation between control plane and viewer

---

## Non-Goals

- General-purpose terminal multiplexing
- tmux feature parity
- Config language, keybinding DSLs, plugin ecosystems
- Perfect terminal emulation
- System-level daemon or persistence across reboots (v0)

---

## Core Features

### 1. Agent + PTY Lifecycle

- `spawn` — start a command in a new PTY
- `list` — list agents and status
- `kill` / `stop` / `hard-kill`
- capture exit code and termination reason

Each agent has:

- unique `agent_id`
- PID
- PTY master FD
- lifecycle state

---

### 2. IO Primitives

- `send <id> <text>` — UTF-8 input (optionally auto-append newline)
- `send-bytes <id>` — raw byte input (escapes, arrows, etc.)
- backpressure-safe output handling

These are the foundational automation hooks.

---

### 3. Transcript Buffer

Per-agent ring buffer of raw output bytes + timestamps.

Commands:

- `tail <id> [-n] [--follow]`
- `dump <id> [--since t] [--format text|jsonl]`

This replaces tmux `capture-pane` at the source of truth.

---

### 4. Virtual Screen Model (Primary)

botty maintains a VT/ANSI-interpreted screen model per agent:

- fixed rows × cols
- cursor position
- alt-screen awareness
- cell attributes (optional)

Commands:

- `snapshot <id>` → normalized screen text
- optional structured snapshot (cells, cursor)

Normalization knobs:

- strip colors by default
- regex-based filters for volatile content (timestamps, PIDs)

Snapshots are intended for **TUI snapshot testing**.

---

### 5. Attach / Interactive Takeover

- `attach <id>` bridges user TTY ↔ agent PTY
- modes:
  - read-only
  - interactive (full control)

- explicit detach key sequence

This is the escape hatch for debugging and human-in-the-loop control.

---

## Viewer Integration

botty is the control plane; viewers are replaceable skins.

### `botty view`

```bash
botty view                 # defaults to tmux (v0: tmux only)
botty view --mux=tmux
```

### tmux Mode (v0)

**Session**: Named `botty`, created or reused.

**Layout**: Tiled (tmux's `tiled` layout), one pane per agent.

**Pane content**: Each pane runs:

```bash
botty tail --replay <id>
```

This shows the current screen state immediately, then streams updates.
Panes are titled with the agent ID.

**Lifecycle**:
- On startup: create panes for all existing agents
- On `agent_spawned` event: add new pane
- On `agent_exited` event: close pane
- When all agents exit: close the tmux session

**Interaction**: Panes are read-only viewers. To interact:

```bash
botty attach <id>
```

tmux never owns the PTY — it is a viewer only.

### Future: zellij, built-in TUI

Deferred to post-v0. The `--mux` flag exists for forward compatibility.

---

## Suggested CLI Surface (v0)

```bash
botty spawn -- codex-cli
botty list
botty send 3 "help\n"
botty tail 3 -f
botty snapshot 3 > snaps/3.txt
botty view --mux=tmux
botty attach 3
botty kill 3
```

---

## Architecture Decisions (Locked In)

- PTY-direct, not tmux-as-control-plane
- Virtual screen is primary, transcript is secondary
- Implicit tmux-style server (not a system daemon)
- tmux/zellij as optional viewers

---

## Open Questions (Future)

- Built-in TUI vs external viewers only
- Persistence / crash recovery
- Recording + replay with timing
- Remote or multi-host runners

## Resolved Decisions

- **Structured events**: Implemented via `botty events` command. Streams JSON events for agent lifecycle (spawned, output, exited). Enables reactive orchestration.

---

## Summary

botty treats terminals as **inspectable, controllable state machines** for agents.

> tmux is for humans. botty is for agents — with humans allowed to watch.
