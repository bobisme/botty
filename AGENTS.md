<!-- botbus-agent-instructions-v1 -->

## BotBus Agent Coordination

This project uses [BotBus](https://github.com/anomalyco/botbus) for multi-agent coordination. Before starting work, check for other agents and active file claims.

### Quick Start

```bash
# Register yourself (once per session)
botbus register --name YourAgentName --description "Brief description"

# Check what's happening
botbus status              # Overview of project state
botbus history             # Recent messages
botbus agents              # Who's registered

# Communicate
botbus send general "Starting work on X"
botbus send general "Done with X, ready for review"
botbus send @OtherAgent "Question about Y"

# Coordinate file access
botbus claim "src/api/**" -m "Working on API routes"
botbus check-claim src/api/routes.rs   # Before editing
botbus release --all                    # When done
```

### Best Practices

1. **Announce your intent** before starting significant work
2. **Claim files** you plan to edit to avoid conflicts
3. **Check claims** before editing files outside your claimed area
4. **Send updates** on blockers, questions, or completed work
5. **Release claims** when done - don't hoard files

### Channel Conventions

- `#general` - Default channel for project-wide updates
- `#backend`, `#frontend`, etc. - Create topic channels as needed
- `@AgentName` - Direct messages for specific coordination

### Message Conventions

Keep messages concise and actionable:

- "Starting work on issue #123: Add foo feature"
- "Blocked: need database credentials to proceed"
- "Question: should auth middleware go in src/api or src/auth?"
- "Done: implemented bar, tests passing"

### When to Post to #botty

**Reserve #botty for releases only**, not individual feature completions:

- ✅ **DO post**: Release announcements with version numbers and changelog summaries
- ❌ **DON'T post**: Individual feature implementations, bug fixes, or work-in-progress updates
- ❌ **DON'T post**: Bead closures or task completions

Example release message:
```bash
botbus send botty "Released v0.4.0 - Added named key sequences, idempotent kill, and combined wait conditions. See release notes for details."
```

For individual work updates, use beads (`br close`), git commits, or crit reviews instead.

<!-- end-botbus-agent-instructions -->
<!-- maw-agent-instructions-v1 -->

## Multi-Agent Workflow with MAW

This project uses MAW for coordinating multiple agents via jj workspaces.
Each agent gets an isolated working copy - you can edit files without blocking other agents.

### Workspace Naming

**Your workspace name will be assigned by the coordinator** (human or orchestrating agent).
If you need to create your own, use:

- Lowercase alphanumeric with hyphens: `agent-1`, `feature-auth`, `bugfix-123`
- Check existing workspaces first: `maw ws list`

### Quick Reference

| Task                 | Command                 |
| -------------------- | ----------------------- |
| Create workspace     | `maw ws create <name>`  |
| List workspaces      | `maw ws list`           |
| Check status         | `maw ws status`         |
| Sync stale workspace | `maw ws sync`           |
| Merge all work       | `maw ws merge --all`    |
| Destroy workspace    | `maw ws destroy <name>` |

### Starting Work

```bash
# Check what workspaces exist
maw ws list

# Create your workspace (if not already assigned)
maw ws create <assigned-name>
cd .workspaces/<assigned-name>

# Start working - jj tracks changes automatically
jj describe -m "wip: implementing feature X"
```

### During Work

```bash
# See your changes
jj diff
jj status

# Save your work (describe current commit)
jj describe -m "feat: add feature X"

# Or commit and start fresh
jj commit -m "feat: add feature X"

# See what other agents are doing
maw ws status
```

### Handling Stale Workspace

If you see "working copy is stale", the main repo changed while you were working:

```bash
maw ws sync
```

### Finishing Work

When done, notify the coordinator. They will merge from the main workspace:

```bash
# Coordinator runs from main workspace:
maw ws merge --all --destroy
```

### Resolving Conflicts

jj records conflicts in commits rather than blocking. If you see conflicts:

```bash
jj status  # shows conflicted files
# Edit the files to resolve (remove conflict markers)
jj describe -m "resolve: merge conflicts"
```

<!-- end-maw-agent-instructions -->

### Using bv as an AI sidecar

bv is a fast terminal UI for Beads projects (.beads/issues.jsonl). It renders lists/details and precomputes dependency metrics (PageRank, critical path, cycles, etc.) so you instantly see blockers and execution order. Source of truth here is `.beads/issues.jsonl` (exported from `beads.db`); legacy `.beads/beads.jsonl` is deprecated and must not be used. For agents, it’s a graph sidecar: instead of parsing JSONL or risking hallucinated traversal, call the robot flags to get deterministic, dependency-aware outputs.

- bv --robot-help — shows all AI-facing commands.
- bv --robot-insights — JSON graph metrics (PageRank, betweenness, HITS, critical path, cycles) with top-N summaries for quick triage.
- bv --robot-plan — JSON execution plan: parallel tracks, items per track, and unblocks lists showing what each item frees up.
- bv --robot-priority — JSON priority recommendations with reasoning and confidence.
- bv --robot-recipes — list recipes (default, actionable, blocked, etc.); apply via bv --recipe <name> to pre-filter/sort before other flags.
- bv --robot-diff --diff-since <commit|date> — JSON diff of issue changes, new/closed items, and cycles introduced/resolved.

Use these commands instead of hand-rolling graph logic; bv already computes the hard parts so agents can act safely and quickly.

### ast-grep vs ripgrep (quick guidance)

**Use `ast-grep` when structure matters.** It parses code and matches AST nodes, so results ignore comments/strings, understand syntax, and can **safely rewrite** code.

- Refactors/codemods: rename APIs, change import forms, rewrite call sites or variable kinds.
- Policy checks: enforce patterns across a repo (`scan` with rules + `test`).
- Editor/automation: LSP mode; `--json` output for tooling.

**Use `ripgrep` when text is enough.** It’s the fastest way to grep literals/regex across files.

- Recon: find strings, TODOs, log lines, config values, or non‑code assets.
- Pre-filter: narrow candidate files before a precise pass.

**Rule of thumb**

- Need correctness over speed, or you’ll **apply changes** → start with `ast-grep`.
- Need raw speed or you’re just **hunting text** → start with `rg`.
- Often combine: `rg` to shortlist files, then `ast-grep` to match/modify with precision.

**Snippets**

Find structured code (ignores comments/strings):

```bash
ast-grep run -l TypeScript -p 'import $X from "$P"'
```

Codemod (only real `var` declarations become `let`):

```bash
ast-grep run -l JavaScript -p 'var $A = $B' -r 'let $A = $B' -U
```

Quick textual hunt:

```bash
rg -n 'console\.log\(' -t js
```

Combine speed + precision:

```bash
rg -l -t ts 'useQuery\(' | xargs ast-grep run -l TypeScript -p 'useQuery($A)' -r 'useSuspenseQuery($A)' -U
```

**Mental model**

- Unit of match: `ast-grep` = node; `rg` = line.
- False positives: `ast-grep` low; `rg` depends on your regex.
- Rewrites: `ast-grep` first-class; `rg` requires ad‑hoc sed/awk and risks collateral edits.

## Testing Strategy for botty

botty is a PTY-based daemon with Unix socket IPC. Testing requires care around process lifecycle and socket cleanup.

### Test Categories

1. **Unit tests** (`#[cfg(test)]` in modules)
   - Protocol serialization/deserialization roundtrips
   - Transcript ring buffer operations
   - Screen normalization logic
   - Name generation uniqueness

2. **Integration tests** (`tests/` directory)
   - Server startup/shutdown lifecycle
   - Full request/response cycles over Unix socket
   - Agent spawn → send → snapshot → kill flows
   - **Socket cleanup**: Each test should use a unique socket path (e.g., `/tmp/botty-test-{uuid}.sock`)

3. **End-to-end CLI tests**
   - Run actual `cargo run -- spawn`, `cargo run -- list`, etc.
   - Use `assert_cmd` crate for ergonomic CLI testing
   - Verify exit codes and stdout/stderr

### Running Tests

```bash
# Unit + integration tests
cargo test

# With logging for debugging
RUST_LOG=debug cargo test -- --nocapture

# Single test
cargo test test_name

# Integration tests only
cargo test --test '*'
```

### Manual Testing Checklist

For attach mode and interactive features that are hard to automate:

```bash
# 1. Basic spawn and interaction
botty spawn -- bash
botty list
botty send <id> "echo hello"
botty tail <id>
botty snapshot <id>
botty kill <id>

# 2. Attach mode
botty spawn -- bash
botty attach <id>
# Type commands, verify they work
# Press Ctrl+A then 'd' to detach
# Verify you're back at your shell

# 3. TUI program
botty spawn -- htop
botty snapshot <id>  # Should show htop UI
botty attach <id>    # Should be interactive
```

### Test Fixtures

For deterministic snapshot testing, use simple programs with predictable output:

```bash
# Spawn a program that prints known output
botty spawn -- sh -c 'echo "line1"; echo "line2"; sleep 999'
botty snapshot <id>  # Compare against expected

# Test screen handling with cursor movement
botty spawn -- sh -c 'printf "ABC\rX"; sleep 999'
botty snapshot <id>  # Should show "XBC"
```

<!-- br-agent-instructions-v1 -->

---

## Beads Workflow Integration

This project uses [beads_rust](https://github.com/Dicklesworthstone/beads_rust) (`br`/`bd`) for issue tracking. Issues are stored in `.beads/` and tracked in git.

### Essential Commands

```bash
# View ready issues (unblocked, not deferred)
br ready              # or: bd ready

# List and search
br list --status=open # All open issues
br show <id>          # Full issue details with dependencies
br search "keyword"   # Full-text search

# Create and update
br create --title="..." --description="..." --type=task --priority=2
br update <id> --status=in_progress
br close <id> --reason="Completed"
br close <id1> <id2>  # Close multiple issues at once

# Sync with git
br sync --flush-only  # Export DB to JSONL
br sync --status      # Check sync status
```

### Workflow Pattern

1. **Start**: Run `br ready` to find actionable work
2. **Claim**: Use `br update <id> --status=in_progress`
3. **Work**: Implement the task
4. **Complete**: Use `br close <id>`
5. **Sync**: Always run `br sync --flush-only` at session end

### Key Concepts

- **Dependencies**: Issues can block other issues. `br ready` shows only unblocked work.
- **Priority**: P0=critical, P1=high, P2=medium, P3=low, P4=backlog (use numbers 0-4, not words)
- **Types**: task, bug, feature, epic, chore, docs, question
- **Blocking**: `br dep add <issue> <depends-on>` to add dependencies

### Session Protocol

**Before ending any session, run this checklist:**

```bash
git status              # Check what changed
git add <files>         # Stage code changes
br sync --flush-only    # Export beads changes to JSONL
git commit -m "..."     # Commit everything
git push                # Push to remote
```

### Best Practices

- Check `br ready` at session start to find available work
- Update status as you work (in_progress → closed)
- Create new issues with `br create` when you discover tasks
- Use descriptive titles and set appropriate priority/type
- Always sync before ending session

### Beads Conventions

**Acceptance criteria for large tasks**: When creating P0-P2 features or any task estimated to take more than a few hours, include acceptance criteria in the description. Use a checklist format:

```bash
br create --title="Add user authentication" --priority=1 --type=feature --description="
Implement JWT-based authentication for the API.

Acceptance criteria:
- [ ] POST /auth/login returns JWT token
- [ ] Protected routes return 401 without valid token
- [ ] Token expiry is configurable via env var
- [ ] Unit tests cover happy path and error cases
- [ ] Documentation updated in API.md
"
```

**Size guidance**:
- **P0-P1**: Always include acceptance criteria
- **P2**: Include if task is non-trivial (>2 hours estimated)
- **P3-P4**: Optional, but helpful for complex features

**Updating acceptance criteria**: As you work, check off completed items by editing the bead:

```bash
br update <id> --description="...updated with [x] for completed items..."
```

<!-- end-br-agent-instructions -->

### Commit Conventions

Use [semantic commits](https://www.conventionalcommits.org/):

```
<type>(<scope>): <description>

[optional body]

[optional footer]
Co-Authored-By: Claude <noreply@anthropic.com>
```

**Types**: `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`

**Examples**:

```bash
git commit -m "docs(jj): add workspace documentation for parallel agents

Co-Authored-By: Claude <noreply@anthropic.com>"

git commit -m "fix(tui): correct mouse click handling in popups

Co-Authored-By: Claude <noreply@anthropic.com>"
```

Always include the `Co-Authored-By` trailer when Claude contributed to the work.

<!-- crit-agent-instructions -->

## Crit: Agent-Centric Code Review

This project uses [crit](https://github.com/anomalyco/botcrit) for distributed code reviews optimized for AI agents.

### Quick Start

```bash
# Initialize crit in the repository (once)
crit init

# Create a review for current change
crit reviews create --title "Add feature X"

# List open reviews
crit reviews list

# Check reviews needing your attention
crit reviews list --needs-review --author $BOTBUS_AGENT

# Show review details
crit reviews show <review_id>
```

### Adding Comments (Recommended)

The simplest way to comment on code - auto-creates threads:

```bash
# Add a comment on a specific line (creates thread automatically)
crit comment <review_id> --file src/main.rs --line 42 "Consider using Option here"

# Add another comment on same line (reuses existing thread)
crit comment <review_id> --file src/main.rs --line 42 "Good point, will fix"

# Comment on a line range
crit comment <review_id> --file src/main.rs --line 10-20 "This block needs refactoring"
```

### Managing Threads

```bash
# List threads on a review
crit threads list <review_id>

# Show thread with context
crit threads show <thread_id>

# Resolve a thread
crit threads resolve <thread_id> --reason "Fixed in latest commit"
```

### Voting on Reviews

```bash
# Approve a review (LGTM)
crit lgtm <review_id> -m "Looks good!"

# Block a review (request changes)
crit block <review_id> -r "Need more test coverage"
```

### Viewing Full Reviews

```bash
# Show full review with all threads and comments
crit review <review_id>

# Show with more context lines
crit review <review_id> --context 5

# List threads with first comment preview
crit threads list <review_id> -v
```

### Approving and Merging

```bash
# Approve a review (changes status to approved)
crit reviews approve <review_id>

# Mark as merged (after jj squash/merge)
# Note: Will fail if there are blocking votes
crit reviews merge <review_id>

# Self-approve and merge in one step (solo workflows)
crit reviews merge <review_id> --self-approve
```

### Agent Best Practices

1. **Set your identity** via environment:
   ```bash
   export BOTBUS_AGENT=my-agent-name
   ```

2. **Check for pending reviews** at session start:
   ```bash
   crit reviews list --needs-review --author $BOTBUS_AGENT
   ```

3. **Check status** to see unresolved threads:
   ```bash
   crit status <review_id> --unresolved-only
   ```

4. **Run doctor** to verify setup:
   ```bash
   crit doctor
   ```

### Output Formats

- Default output is TOON (token-optimized, human-readable)
- Use `--json` flag for machine-parseable JSON output

### Key Concepts

- **Reviews** are anchored to jj Change IDs (survive rebases)
- **Threads** group comments on specific file locations
- **crit comment** is the simple way to leave feedback (auto-creates threads)
- Works across jj workspaces (shared .crit/ in main repo)

<!-- end-crit-agent-instructions -->

## Daily Development Workflow

### Starting a Work Session

1. **Check for new work** and triage if needed:
   ```bash
   br ready                    # See what's actionable
   botbus history botty        # Check for messages from other agents
   git pull origin main        # Get latest changes
   ```

2. **Triage new issues** (if any were filed):
   - Read the actual code to assess feasibility
   - Check for existing infrastructure you can leverage
   - Estimate complexity and update priority if needed
   - Add implementation notes to the bead description
   ```bash
   br show <issue-id>
   br update <issue-id> --priority=2 --description="Updated with implementation notes"
   ```

3. **Pick work** based on priority and scope:
   - Prefer P2 over P3
   - Consider batching related features for a release
   - Bugs before features (users are affected now)

### Feature Development Loop

For each feature, follow this cycle:

1. **Start the work**:
   ```bash
   br update <issue-id> --status=in_progress
   ```

2. **Implement the feature**:
   - Read existing code to understand patterns
   - Make minimal, focused changes
   - Avoid over-engineering or premature abstraction
   - Follow existing conventions (file structure, naming, error handling)

3. **Test thoroughly**:
   ```bash
   cargo test                  # Unit + integration tests
   cargo test <test-name>      # Specific test
   cargo build --release       # Verify it builds
   ```
   - Add unit tests for new functions
   - Add integration/CLI tests for new commands
   - Do manual testing for UX features
   - Verify all tests pass before committing

4. **Commit with semantic message**:
   ```bash
   git add <files>
   git commit -m "feat(scope): description

   Co-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>"
   ```

5. **Close the bead**:
   ```bash
   br close <issue-id> --reason="Implemented in commit <sha>. [brief summary]"
   ```

6. **Push to main** (for small, safe changes):
   ```bash
   git push origin main
   ```

### Batch Releases

Instead of releasing after each feature, batch multiple features into a release:

1. Work on 2-4 related features
2. Test everything together
3. Bump version, tag, and release as one unit
4. **Only then** announce on #botty

This creates coherent releases with clear themes (e.g., "testing improvements").

### Bug Investigation Workflow

1. **Understand the symptom**: Read the bug report carefully
2. **Find the code**: Use `grep`, `rg`, or `ast-grep` to locate relevant code
3. **Reproduce locally**: Try to trigger the bug yourself
4. **Identify root cause**: Read the code, trace the execution path
5. **Design minimal fix**: Target the root cause, avoid over-engineering
6. **Test the fix**: Verify it solves the problem without breaking anything
7. **Consider edge cases**: What else might be affected?

### End of Session Checklist

Before ending a work session:

```bash
git status                   # Check for uncommitted work
br sync --flush-only         # Export beads to JSONL
git add .beads/issues.jsonl  # Stage bead changes
git commit -m "chore(beads): update issue tracking"
git push origin main         # Push everything
```

## Release Workflow

This section covers the full release cycle: creating a feature branch, implementing changes, getting review, and releasing.

### 1. Start a Feature Branch

```bash
# Create a new commit for your work
jj new -m "wip: description of change"

# Create a bookmark for the feature
jj bookmark create feature-name

# Work on your changes...
jj describe -m "feat(scope): description of change"
```

### 2. Request Code Review

After completing your changes and ensuring tests pass:

```bash
# Verify build and tests
cargo build --release && cargo test

# Create a review
crit reviews create --title "feat(scope): description of change"
# Note the review ID (e.g., cr-xxxx)
```

**Spawn specialist reviewers** using the code-review skill (`~/.claude/skills/code-review/SKILL.md`):

- **Security reviewer** (always): Looks for injection, auth issues, resource exhaustion, etc.
- **Architecture reviewer** (for structural changes): Evaluates design, abstractions, maintainability

The skill has ready-to-use prompts for spawning these subagents.

### 3. Address Review Feedback

Monitor botbus for reviewer completion:

```bash
botbus history general
```

For each thread raised:

```bash
# View threads
crit threads list <review_id>
crit threads show <thread_id>

# Respond (set your agent identity first)
export BOTBUS_AGENT=<your-agent>
crit comments add <thread_id> "Response explaining fix or rationale"

# After addressing, resolve with reason
crit threads resolve <thread_id> --reason "Fixed: description"
crit threads resolve <thread_id> --reason "Won't fix: rationale"
crit threads resolve <thread_id> --reason "Deferred: created bead bd-xxx"
```

### 4. Get Approval

Reviewers vote with:

```bash
crit lgtm <review_id> -m "Reason"    # Approve
crit block <review_id> -r "Reason"   # Block
```

### 5. Merge and Release

Once approved (LGTM votes, no blocking votes, all threads resolved):

```bash
# Approve and merge the review
crit reviews approve <review_id>
crit reviews merge <review_id>

# Bump version in Cargo.toml (edit manually or with sed)
# e.g., 0.2.0 → 0.3.0

# Update commit message
jj describe -m "chore: bump version to X.Y.Z

Co-Authored-By: Claude <noreply@anthropic.com>"

# Move main bookmark forward and push
jj bookmark set main -r @
jj git push --bookmark main

# Tag the release and push tag
jj tag set vX.Y.Z -r main
git push origin vX.Y.Z

# Install locally
just install

# Verify
botty --version

# Announce on botbus
export BOTBUS_AGENT=<your-agent>
botbus send botty "Released vX.Y.Z - [summary of changes]"
```

### Quick Reference

| Stage | Key Commands |
|-------|--------------|
| Start feature | `jj new -m "wip: ..."` then `jj bookmark create name` |
| Create review | `crit reviews create --title "..."` |
| View threads | `crit threads list <review_id>` |
| Respond | `crit comments add <thread_id> "..."` |
| Resolve | `crit threads resolve <thread_id> --reason "..."` |
| Approve/merge | `crit reviews approve <id> && crit reviews merge <id>` |
| Release | bump version → `jj bookmark set main` → push → tag → `just install` |