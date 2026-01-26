# Auto-Resize Fix Notes

## Issue: bd-1gi

Fix auto-resize feature for `botty view --auto-resize` - agents should resize to match their tmux pane dimensions dynamically.

## Problem Statement

When using `botty view --auto-resize`:
1. Agents weren't resizing to correct pane dimensions
2. No dynamic resizing when tmux panes changed size
3. TUI programs (htop, btop) displayed garbled output after resize

## Root Causes Found

### 1. Pane Detection via Title (FIXED)

**Problem**: Original code used tmux pane titles to identify which agent was in which pane. TUI programs overwrite pane titles (e.g., htop sets title to "htop", claude sets "✳ Claude Code").

**Solution**: Use tmux pane option `@agent_id` instead of pane title. This is immune to overwrites.

```rust
// In view.rs - add_pane_split() and add_window()
tmux set-option -p -t <pane> @agent_id <agent_id>

// In get_pane_sizes()
tmux list-panes -F '#{@agent_id}:#{pane_height}:#{pane_width}'
```

### 2. Missing Hooks for Session Switching (FIXED)

**Problem**: `client-attached` only fires on fresh tmux attach, not when switching sessions within tmux (e.g., Ctrl+B, s).

**Solution**: Added multiple hooks in `setup_resize_hook()`:
- `after-resize-pane` - fires when individual panes resize
- `client-attached` - fires on fresh attach
- `client-session-changed` - fires when switching TO this session
- `client-resized` - fires when terminal window resizes
- `window-layout-changed` (with `-w` flag) - fires on layout changes

### 3. Frozen Panes After Resize (FIXED)

**Problem**: The `tail --replay` command tracks `last_len` to only output new data. When transcript was cleared on resize, `data.len() < last_len`, so condition `data.len() > last_len` was NEVER true and nothing printed.

**Solution**: Handle the shrink case:
```rust
if data.len() < last_len {
    // Transcript shrank - just reset position
    last_len = data.len();
} else if data.len() > last_len {
    // Print new data
}
```

### 4. Glitchy Display on Resize (PARTIALLY FIXED)

**Problem**: When using `--replay`, the entire transcript (hundreds of KB) gets dumped, including historical output rendered at various sizes. This causes:
- Flashing/scrolling text
- Garbled display for TUI programs

**Current Solution**: Changed from `tail --replay` to `tail --follow --raw`:
```rust
// In view.rs
let tail_cmd = format!("{} tail --follow --raw {}", self.botty_path, agent_id);
```

This shows only NEW output. TUI programs draw themselves fresh via SIGWINCH.

**Tradeoff**: Panes start empty and fill in as programs output. No historical context shown.

## Files Modified (from main branch)

### src/cli.rs
- Added `--auto-resize` flag to `View` command
- Added `--clear` flag to `Resize` command  
- Added hidden `ResizePanes` command (called by tmux hooks)

### src/protocol.rs
- Added `clear_transcript: bool` field to `Request::Resize`

### src/server/mod.rs
- Handle `clear_transcript` in resize handler (calls `agent.transcript.clear()`)

### src/main.rs
- `setup_resize_hook()` - sets 5 tmux hooks
- `resize_agents_to_panes()` - resizes agents to match pane sizes
- `run_resize_panes_command()` - CLI handler for `resize-panes`
- Fixed `tail --replay` loop to handle `data.len() < last_len`

### src/view.rs
- `add_pane_split()` and `add_window()` set `@agent_id` pane option
- `get_pane_sizes()` uses `@agent_id` instead of pane title
- Changed tail command from `--replay` to `--follow --raw`

## Current State (2026-01-27)

### Working:
- Agents resize to match pane dimensions
- Dynamic resizing via hooks when panes change
- Panes update continuously (not frozen)
- `@agent_id` detection is reliable

### Partially Working:
- Display is cleaner but may still have issues on resize
- Using `--follow` instead of `--replay` means no historical context

### Not Yet Tested:
- Windows mode (only panes mode tested)
- Edge cases: agent exits during view, new agent spawns during view

## Alternative Approaches to Try

### 1. Snapshot + Follow
Show current screen state via snapshot, then follow new output:
```rust
let tail_cmd = format!(
    "{} snapshot --raw {} && {} tail --follow --raw {}",
    botty_path, agent_id, botty_path, agent_id
);
```
**Issue Found**: Snapshot at small size is garbled. Need to ensure agent is resized BEFORE snapshot.

### 2. Clear Transcript + Replay
Clear transcript on resize, then replay (which would be empty/minimal):
```rust
// In resize-panes:
clear_transcript: true,
// Then respawn pane to restart tail --replay
```
**Issue Found**: Respawning panes caused frozen display. The respawn itself works but something in the timing causes issues.

### 3. Events-Based Approach
Subscribe to agent events and handle resize events specially:
- On resize event, clear tmux pane scrollback
- Let TUI program redraw via SIGWINCH

### 4. Smarter Replay
Modify `--replay` to only replay recent output (last N seconds or last screen's worth):
```rust
// Instead of dumping entire transcript
// Only dump output since last resize, or last ~10KB
```

## Testing Commands

```bash
# Build
cargo build --release

# Kill existing session
tmux kill-session -t botty

# Manual setup (since botty view needs TTY)
tmux new-session -d -s botty -n agents
# ... add panes with tail commands
# ... set @agent_id on each pane
# ... set hooks

# Check pane sizes vs agent sizes
tmux list-panes -t botty:agents -F '#{pane_index}: @agent_id=#{@agent_id} size=#{pane_height}x#{pane_width}'
./target/release/botty list --json | jq -r '.[] | "\(.id): \(.size.rows)x\(.size.cols)"'

# Check if panes update
tmux capture-pane -t botty:agents.2 -p | grep Uptime
sleep 3
tmux capture-pane -t botty:agents.2 -p | grep Uptime

# Trigger resize manually
./target/release/botty resize-panes --mode panes

# Check hooks
tmux show-hooks -t botty
tmux show-hooks -w -t botty:agents
```

## Key Insight

The fundamental tension is between:
1. **Replay mode**: Shows historical context but includes output at wrong sizes
2. **Follow mode**: Clean display but no historical context
3. **Snapshot + follow**: Best of both but timing/sizing must be perfect

TUI programs (htop, btop, vim, etc.) handle resize via SIGWINCH and redraw themselves. The transcript-based approach (replay) fights against this because it includes pre-resize output.

For TUI programs, the cleanest approach may be:
1. Resize the agent PTY (sends SIGWINCH to program)
2. Wait briefly for program to redraw
3. Clear tmux pane scrollback
4. Continue following new output

This lets the program's own redraw logic handle the display rather than replaying historical terminal sequences.

## Update (2026-01-27 later)

### Solution Found: Respawn Panes on Resize

The core issue was that `tail --follow` output contains data rendered at old sizes. Even with `clear-history`, the tail process output stream still has old data.

**Working Solution**: Respawn the pane with a fresh tail command after resize:

```rust
// In resize-panes, after resizing the agent:
if let Some(pane_id) = pane_ids.get(agent_id) {
    let tail_cmd = format!("{} tail --follow --raw {}", botty_path, agent_id);
    let _ = std::process::Command::new("tmux")
        .args(["respawn-pane", "-t", pane_id, "-k", &tail_cmd])
        .status();
}
```

This:
1. Kills the old tail process
2. Starts a fresh tail that only sees new output
3. TUI program redraws via SIGWINCH at new size
4. Fresh tail captures the new output at correct size

**Testing**: btop now renders correctly after resize (shows CPU graphs, etc.)

## Update (2026-01-27 continued)

### Improvement: Explicit SIGWINCH for Reliability

Some TUI programs (especially btop) sometimes showed brief old-size rendering even with the respawn approach. The issue was timing - we were respawning panes before the TUI had fully redrawn.

**Solution**: Send explicit SIGWINCH signal directly to agent processes and increase delay:

```rust
// In resize-panes:
// 1. Resize all agents (sends implicit SIGWINCH via PTY)
// 2. Wait 50ms for PTY resize to propagate
// 3. Send explicit SIGWINCH to each agent process:
unsafe {
    libc::kill(pid as libc::pid_t, libc::SIGWINCH);
}
// 4. Wait 300ms for TUI programs to fully redraw
// 5. Respawn panes with fresh tail commands
```

This approach:
- Uses `libc::kill()` to send SIGWINCH (signal 28) directly to the agent process
- Provides double-signaling: implicit from PTY resize + explicit via kill()
- Gives 300ms (up from 200ms) for complex TUIs like btop to complete redraw
- The explicit signal ensures programs that might miss the PTY signal still get notified

**Result**: More reliable rendering across all TUI programs tested (claude, htop, btop, opencode)

## Update (2026-01-27 evening) - The Real Problem: tail vs attach

### Discovery: `tail` Cannot Display TUI Programs Correctly

After extensive testing, we found that `tail --follow --raw` and `tail --replay` fundamentally cannot display TUI programs correctly:

**Root Cause**: TUI programs (htop, btop) do incremental screen updates using:
- Cursor positioning (`\e[5;10H` = move to row 5, col 10)
- Scroll regions (`\e[14;18r` = set scroll region lines 14-18)
- Partial cell updates (only redraw changed cells)

These escape sequences assume the terminal already has the correct screen state from when the program started (alternate screen mode, initial full draw, etc.).

**Why `tail` fails**:
1. `tail --follow --raw` starts with `last_len = 0` and only outputs NEW bytes - misses initial screen setup
2. `tail --replay` dumps entire transcript - but transcript is incremental updates that assume prior state
3. `snapshot` renders current screen correctly but as plain text lines, not cursor-positioned output
4. Mixing snapshot + follow doesn't work - incremental updates assume different cursor positions

**Terminal mode issue (fixed but not sufficient)**:
- Added `disable_output_postprocessing()` to disable OPOST flag
- This fixes `\n` -> `\r\n` conversion but doesn't fix the fundamental state problem

### Solution: Use `attach --readonly` Instead of `tail`

`attach --readonly` works better because:
- It reads directly from the PTY in real-time
- Puts the terminal in raw mode
- Properly passes through all escape sequences

**Current issues with `attach --readonly`**:
1. **No initial screen** - attach starts by just forwarding new PTY output, doesn't replay or render initial state
2. **Occasional scroll glitches** - htop uses scroll regions; when cursor moves to bottom of a region, it scrolls

### Proposed Fix: Server-side Initial Screen Render

In `handle_attach()` (server/mod.rs), before starting the I/O bridge:
1. Render current screen state as ANSI escape sequences (clear screen + draw each line)
2. Send this to the client
3. Then start forwarding live PTY output

The screen can be rendered using `agent.screen.parser.screen()` which has methods like:
- `contents_formatted()` - returns content with ANSI formatting
- Or manually: clear screen (`\e[2J\e[H`) + output each row

### Files to Modify

- `src/server/mod.rs` - `handle_attach()` to send initial screen
- `src/server/screen.rs` - possibly add method to render full screen with cursor positioning
- `src/view.rs` - change tail commands to attach --readonly

### Testing Commands

```bash
# Current: attach --readonly shows live updates but no initial screen
tmux respawn-pane -t botty:agents.2 -k "/path/to/botty attach --readonly hurried-police"

# Verify sizes match
tmux list-panes -t botty:agents -F '#{pane_index}: #{pane_height}x#{pane_width} @agent_id=#{@agent_id}'
./target/release/botty list --json | jq -r '.[] | "\(.id): \(.size.rows)x\(.size.cols)"'

# Set @agent_id on recreated panes
tmux set-option -p -t botty:agents.2 @agent_id hurried-police
```

### Key Insight

The transcript is a **log of bytes sent to the PTY**, not a reconstructable screen state. TUI programs use complex terminal features (alternate screen, scroll regions, cursor save/restore) that only work when played back from the very beginning in the exact same terminal state.

The virtual terminal emulator (`vt100::Parser`) in the server correctly interprets all these sequences and maintains the true screen state. We need to leverage this for display, not replay raw bytes.

## Update (2026-01-27 evening continued) - SOLUTION FOUND

### Working Solution: `attach --readonly` with Initial Screen Render

**The fix has two parts:**

1. **Server sends initial screen render** (`src/server/mod.rs` in `handle_attach()`):
   - After sending `AttachStarted` JSON response
   - Call `agent.screen.render_full_screen()` to get the current screen as escape sequences
   - Send this to the client before starting the I/O bridge

2. **New `render_full_screen()` method** (`src/server/screen.rs`):
   - Outputs escape sequences to fully redraw the screen:
     - `\e[?1049h` - switch to alternate screen
     - `\e[2J\e[H` - clear screen and home cursor
     - For each row: position cursor, output cells with colors
     - Position cursor at correct location at end
   - This gives the client a complete screen state before incremental updates begin

3. **Client reads initial screen data** (`src/attach.rs`):
   - After parsing `AttachStarted` JSON, read additional data with 50ms timeout
   - Output this initial screen data to stdout
   - Then enter the normal I/O bridge loop

4. **View uses `attach --readonly`** (`src/view.rs`):
   - Changed from `tail --follow --raw` to `attach --readonly`
   - attach properly handles TUI programs because it:
     - Sends initial screen render
     - Streams live PTY output
     - Runs in raw terminal mode

**Result**: htop displays correctly with full UI (CPU graphs, etc.) immediately and updates work perfectly. Resize also works.

### Minor Issue: Cursor Jumping

The cursor visibly jumps around as htop redraws different parts of the screen. This is normal TUI behavior - htop uses cursor positioning to update specific cells. Could potentially be hidden with cursor hide/show sequences but not critical.

### Files Modified

- `src/server/mod.rs` - `handle_attach()` sends initial screen render
- `src/server/screen.rs` - Added `render_full_screen()` method  
- `src/attach.rs` - Client reads and outputs initial screen data
- `src/view.rs` - Changed to use `attach --readonly` instead of `tail`
