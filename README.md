# vessel

`vessel` is a PTY-based runtime for spawning, controlling, and observing interactive terminal agents over a Unix socket.

It is designed for AI orchestrators, test harnesses, and automation systems that need real terminal semantics (not just stdout pipes).

![Summoning Pit](images/vessel-embed.jpg)

## What vessel is (and is not)

- **Is:** local control plane for interactive worker processes (`spawn`, `send`, `wait`, `snapshot`, `events`, `attach`, `view`).
- **Is:** good for multi-agent workflows, TUI testing, and reproducible terminal automation.
- **Is not:** container runtime, distributed scheduler, or durable job queue.

## Requirements

- Linux with Unix sockets + PTY support
- Rust 1.85+ (for building from source)
- `tmux` (optional, only for `vessel view`)

## Install

```bash
cargo install vessel-pty
```

## Quick start (2 minutes)

```bash
# 1) Spawn a worker shell
vessel spawn --name demo -- bash

# 2) Send a command (+ Enter)
vessel send demo "echo hello from vessel" -n

# 3) Wait for expected output, then inspect the virtual screen
vessel wait demo --contains "hello from vessel" --timeout 5
vessel snapshot demo

# 4) Clean up (SIGTERM by default; use --force for hard kill)
vessel kill demo --force

# 5) Stop server when done
vessel shutdown
```

## Mental model

```text
Agent  = PTY process + transcript ring + virtual screen
Server = owns all agent state, listens on Unix socket
Client = stateless CLI sending JSON requests
View   = tmux dashboard; panes run read-only attach streams
```

Key implications:

- `snapshot` reflects current terminal state (best for assertions).
- `tail`/`dump` reflect transcript bytes (useful for logs/streaming).
- State is in-memory in the server process (no persistence across server restart).

## Core command map

### Lifecycle

```bash
vessel spawn --name worker --label batch --timeout 60 -- bash
vessel list
vessel list --all --format json
vessel kill worker
vessel kill --label batch --force
vessel kill --all --force
vessel signal worker --signal USR1
```

### Input/output

```bash
vessel send worker "make test" -n
vessel send worker "run the tests" -e     # -e/--enter submits (CR)
vessel send worker "ls" -n --submit-delay-ms 0   # skip the pre-key pause
vessel send agent "$PROMPT" -p -e         # -p/--paste: multi-line prompt, then submit
vessel send agent - -p -e < prompt.md     # "-" reads the payload from stdin
vessel send-bytes worker 1b5b41           # up arrow
vessel send-keys worker ctrl-c enter
vessel send --label batch "status?" -e    # fan out to every agent with a label
vessel send-keys --label batch ctrl-c     # selectors work on the whole send family
vessel tail worker -f
vessel tail worker --raw
vessel snapshot worker
vessel snapshot worker --raw
vessel dump worker --format jsonl
```

`--newline`/`--enter` write the submit key in a separate PTY write, 50ms after
the text. Full-screen TUIs classify input by arrival timing: a CR/LF that lands
in the same burst as the text is treated as pasted content and gets inserted
into the composer instead of submitting it, so the prompt sits there looking
delivered. The pause puts the key outside that burst. Use
`--submit-delay-ms 0` to write it immediately (fine for shells and other
line-oriented programs), or raise it for a TUI that still swallows the key.

#### Multi-line prompts

Use `--paste` (`-p`) for any prompt that spans more than one line. It wraps the
text in bracketed-paste markers (`ESC[200~` … `ESC[201~`), which a TUI reads as
a single paste: the newlines become lines in the composer. Without it the first
newline submits a truncated prompt and every remaining line lands as its own
turn — three separate agent turns for one prompt, each billed, none of them
what was asked.

`--paste --enter` is the usual orchestrator pairing: paste the whole prompt,
then submit it as one turn. Pass `-` as the text to read the payload from
stdin, which avoids quoting a long prompt onto the command line:

```bash
vessel send agent - --paste --enter <<'PROMPT'
Refactor the parser to use the new token stream.
Keep the existing public API.
PROMPT
```

Any `ESC[201~` inside the text is dropped rather than forwarded — it would
otherwise close the bracket early and deliver the remainder as live keystrokes.

#### Sending to a group

`send`, `send-bytes`, and `send-keys` all take the same selectors `kill` and
`signal` use: `--label` (repeatable, agents must carry all of them), `--proc`
(command substring), and `--all`. They match running agents only.

**With a selector, omit the agent ID** — every positional is payload:

```bash
vessel send --label batch "run the tests" --paste --enter
vessel send-keys --label batch ctrl-c
vessel send --proc codex "status?" --enter
```

Passing both an ID and a selector is rejected rather than guessed at. Note that
`--proc` has no `-p` short form on these three commands, because `-p` is
`--paste` on `send`; `kill` and `signal` keep `-p` for `--proc`.

A fan-out reports one line per agent and exits non-zero if any of them missed
the input — a group send can partially fail, and staying silent would report
that as success:

```
$ vessel send --label batch "hi" --enter
tidy-otter    ok
brave-heron   error: write failed: Input/output error
```

Deliveries run concurrently, so the per-agent submit delays overlap instead of
stacking. A request naming a single agent ID is unchanged: silent on success,
non-zero on failure.

### Synchronization and assertions

```bash
vessel wait worker --contains "READY" --timeout 30
vessel wait worker --stable 200 --contains "$ "
vessel wait worker --exited
vessel assert worker --contains "PASS"
vessel assert worker --not-contains "ERROR"
```

### Streaming and observability

```bash
vessel events --output
vessel subscribe --id worker --prefix
vessel subscribe --label batch --format jsonl
vessel attach worker
vessel attach worker --readonly
vessel view
vessel view --mode windows
vessel view --label batch
```

### One-off command execution

```bash
vessel exec -- git status --short
vessel exec --timeout 120 -- cargo test
```

### Recording and replay scaffolding

```bash
vessel spawn --name rec --record -- bash
vessel send rec "echo hi" -n
vessel recording rec --format pretty
vessel gen-test rec > replay.sh
chmod +x replay.sh
```

## Orchestration patterns

Spawn dependencies:

```bash
# Wait for setup to exit before starting app
vessel spawn --name setup -- ./setup.sh
vessel spawn --name app --after setup -- ./run-app.sh

# Wait for output from another agent before spawning
vessel spawn --name db -- ./start-db.sh
vessel spawn --name api --wait-for db:READY -- ./start-api.sh
```

Recommended cleanup for automation:

```bash
vessel kill --label batch --force
```

## Output formats for automation

Many commands support `--format text|json|pretty`.

- `text`: compact, pipe-friendly
- `json`: structured envelope (`{"<key>": ..., "advice": [...]}`)
- `pretty`: human-oriented terminal output

Example:

```bash
vessel list --format json | jq '.agents[] | {id, state, labels}'
```

## Server behavior

- Server auto-starts for most regular commands.
- `events` and `subscribe` do **not** auto-start (they expect an existing server/session).
- Default socket path: `/run/user/$UID/vessel.sock` (fallback `/tmp/vessel-$UID.sock`).
- Override with `VESSEL_SOCKET` or `--socket`.

### Logging

Logs go to stderr, at `vessel=warn` by default and `vessel=debug` with
`--verbose`. `RUST_LOG` overrides the filter in every mode.

| Variable | Effect |
|---|---|
| `VESSEL_LOG=<path>` | Also append logs to a file, at `vessel=info` or better so server lifecycle events are captured without `--verbose`. |
| `VESSEL_LOG_FORMAT=json` | Emit JSON spans and events to stderr instead of the human formatter, for callers that parse vessel's logs. |

## Troubleshooting

```bash
vessel doctor
```

If you hit stale socket/session issues:

```bash
vessel shutdown
tmux kill-session -t vessel 2>/dev/null || true
```

Notes:

- `kill` sends SIGTERM by default; some interactive shells ignore it. Use `--force` for deterministic teardown.
- For TUI inspection, prefer `snapshot` or `attach --readonly` over plain `tail`.

## Development

```bash
just build
just test
```

Relevant docs:

- `AGENTS.md` - contributor + agent workflow
- `docs/testing.md` - testing approach and scenarios
