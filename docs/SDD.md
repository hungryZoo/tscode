# tscode Software Design Document

## 1. Architecture Overview

`tscode` is a Rust terminal application built on:

- `ratatui` for layout and rendering
- `crossterm` for terminal events, raw mode, alternate screen, and mouse capture
- `syntect` for syntax highlighting
- `portable-pty` for a real cross-platform shell-backed pseudo terminal
- `vt100` for terminal screen parsing and scrollback

The application is a single event loop that updates an in-memory model and redraws the full screen after meaningful input.

## 2. Module Layout

```text
src/
  app.rs        application state and actions
  fs_tree.rs    filesystem tree loading, flattening, and selection
  main.rs       terminal setup, event loop, panic restoration
  shell.rs      PTY shell session, terminal parser, input/output routing
  syntax.rs     syntect loading and line highlighting
  ui.rs         ratatui layout and widgets
```

## 3. State Model

### App

The top-level state owns:

- workspace root
- file explorer state
- opened tabs
- active tab index
- focused panel
- hover target
- quick panel state for quick open, workspace search, and command palette
- explorer visibility state for dotfile visibility, generated-folder visibility, and visible-tree filtering
- terminal layout state for normal height and maximized mode
- terminal state
- cached UI hit regions from the most recent draw
- syntax highlighter

### Explorer

The explorer stores a tree of `FsNode` values. Directories are loaded lazily when expanded. Filesystem metadata such as file size and read-only state is captured when entries are loaded. A flattened visible row list is produced after changes and during rendering, then filtered by app-level visibility state so generated folders can stay hidden by default without changing the underlying tree. Explorer reveal expands path ancestors and selects the requested file or folder row.

Explorer clipboard state stores copy/cut intent and the source path. Paste performs real filesystem copy or move operations, recursively copies directories, creates non-conflicting copy names, and updates open editor tab paths after moves.

### Editor

Each opened file tab stores:

- absolute path
- display name
- decoded lines
- vertical scroll offset
- cursor line and column
- optional selection anchor
- dirty state
- trailing-newline state
- bounded undo and redo stacks

Editor buffers support insertion, deletion, newline, paste, cursor movement, word movement, selection, save, undo, redo, in-file search, go-to-line, and active-line commands. Selection is stored as an anchor plus the current cursor position and normalized when copying, cutting, deleting, rendering, or replacing ranges. Line commands include indent, outdent, duplicate, delete, move up/down, and file-type-aware line-comment toggling. The first prerelease still does not attempt full VS Code parity such as multi-cursor editing or LSP rename.

### Quick Panel

The quick panel stores a mode, query text, result list, selected index, and scroll offset. Quick Open recursively scans workspace files while applying the same hidden/generated visibility policy as the explorer, then fuzzy matches path fragments. Workspace Search scans bounded-size text files under that same visibility policy, builds file/line preview results, and opens the selected result at its matching cursor location. Command Palette uses the same overlay model with `CommandAction` entries instead of file paths; activating a command dispatches through the app action layer.

### Terminal

The integrated terminal owns:

- a native PTY master/slave pair
- a spawned platform shell running in the workspace root
- a background reader thread
- a writer for user input
- a `vt100::Parser` screen with scrollback

Keyboard input while terminal-focused is encoded as terminal byte sequences and written to the PTY. PTY output is parsed asynchronously and rendered into the bottom panel.

Terminal management commands reset only the `vt100::Parser` for clear-terminal, or kill the current PTY child and replace the `ShellPanel` with a new workspace-root shell for restart-terminal.

## 4. Rendering Design

The UI uses a vertical root layout:

1. title bar
2. body
3. status bar

The body uses a horizontal split:

1. file explorer
2. editor column

The editor column normally uses a vertical split:

1. tab strip and code view
2. integrated terminal

Each render pass records clickable and hoverable rectangles into `HitRegions`.

When terminal maximized mode is active, the editor column is replaced by the integrated terminal so command output can use the main workspace area while the title, explorer, and status bars remain visible.

## 5. Input Design

### Mouse Move

Mouse movement updates `HoverTarget` by checking recorded hit regions in front-to-back order.

### Mouse Click

Mouse clicks use the current coordinate to:

- focus explorer/editor/terminal
- toggle a directory
- open a file
- select a tab
- close a tab through its tab-strip close target
- activate a quick-panel result when an overlay is visible

### Mouse Wheel

Wheel events route to the hovered panel if known, otherwise to the focused panel.

### Keyboard

Keyboard events map to panel-specific actions. Quick-panel input handles query editing, result movement, and activation before normal panel shortcuts. `F1` opens the command palette because many terminal sessions cannot reliably distinguish `Ctrl-P` from `Ctrl-Shift-P`. Explorer-focused input supports tree filtering and visibility toggles in addition to file operations. Editor-focused input supports save, search, go-to-line, repeated search, undo, redo, selection, word movement, internal clipboard operations, line commands, and saved-tab close shortcuts. Terminal-focused input is forwarded to the PTY. The app-level exit shortcut is `Ctrl-Q` so `Ctrl-C` can be delivered to the shell when terminal focus is active. Dirty editor buffers trigger an explicit quit confirmation instead of exiting immediately.

### Editor Clipboard

The editor clipboard is an in-memory application clipboard used by `Ctrl-C`, `Ctrl-X`, and `Ctrl-V` in editor focus. It is deliberately separate from explorer copy/cut state and from terminal `Ctrl-C`, which remains a PTY signal when terminal focus is active. Range deletion and replacement are performed as single undoable edits.

In-file search state lives on `App::search_needle`. The editor renderer overlays visible search-match highlights on top of plain line text when a match is present, while keeping explicit editor selections higher priority. The status bar computes the active file's match count from the same search string.

Replace in file is modeled as a two-step prompt: find text, then replacement text. Single replace changes the current match when the cursor is on one or otherwise searches forward with wraparound. Replace-all updates every active-buffer line match in one `EditorTab` undo snapshot so a single undo restores the previous file contents.

## 6. Syntax Highlighting

`syntect` loads default syntax and theme sets once during startup. The renderer chooses syntax by token or file extension, highlights visible lines only, and converts style foreground colors into ratatui `Color::Rgb`.

If highlighting fails, rendering falls back to plain text.

## 7. Cross-Platform Shell Design

The integrated terminal uses:

- Unix: `$SHELL` or `/bin/sh`
- Windows: `%COMSPEC%` or `cmd.exe`

The shell runs inside a PTY with `TERM=xterm-256color`, receives resize notifications from the terminal panel, and inherits the workspace root as the current directory.

PTY output is parsed into `vt100` cells. `ShellPanel::styled_rows` groups adjacent cells with the same foreground color, background color, and text modifiers into lightweight terminal spans. The UI layer maps those spans into ratatui `Span` values so command output keeps ANSI color and style information without exposing ratatui rendering concerns to the PTY layer.

Paste events use normal byte writes unless the parsed terminal screen has bracketed paste mode enabled. In bracketed paste mode, pasted text is wrapped with the standard begin/end paste control sequences before being sent to the PTY.

Terminal mouse handling has two modes. If the child application has requested xterm mouse events, clicks and wheel events are passed through to the PTY. Otherwise, a terminal click is interpreted as normal application UI input: the app inspects the clicked visible row and opens an existing `path`, `path:line`, or `path:line:column` reference in the editor when one is found.

## 8. Release and Packaging Design

The repository includes:

- `install.sh`
- `.github/workflows/release.yml`
- `dist/` helper scripts for packaging
- Cargo metadata suitable for `cargo-deb` and `cargo-generate-rpm`

The release workflow builds archive artifacts on macOS, Linux, and Windows runners. Linux package jobs generate `.deb` and `.rpm` packages. The workflow uploads all assets to a GitHub prerelease for tags matching `v*`.

## 9. Risks and Mitigations

- Full terminal emulation is complex. The prerelease uses `vt100` parsing and a real PTY, which supports normal shell interaction and many CLI programs, while deeper terminal mouse/application-mode edge cases remain ongoing work.
- Cross-compiling every target from one machine may require external linkers. CI uses native runners and cross/zig where practical.
- Terminal mouse support varies by emulator. The app uses crossterm's standard mouse capture and also provides keyboard fallbacks.
