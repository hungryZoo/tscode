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
- quick panel state for quick open and workspace search
- terminal state
- cached UI hit regions from the most recent draw
- syntax highlighter

### Explorer

The explorer stores a tree of `FsNode` values. Directories are loaded lazily when expanded. A flattened visible row list is produced after changes and during rendering.

### Editor

Each opened file tab stores:

- absolute path
- display name
- decoded lines
- vertical scroll offset
- cursor line and column
- dirty state
- trailing-newline state
- bounded undo and redo stacks

Editor buffers support insertion, deletion, newline, paste, cursor movement, save, undo, redo, and in-file search. The first prerelease still does not attempt full VS Code parity such as multi-cursor editing or LSP rename.

### Quick Panel

The quick panel stores a mode, query text, result list, selected index, and scroll offset. Quick Open recursively scans workspace files while skipping common generated directories, then fuzzy matches path fragments. Workspace Search scans bounded-size text files, builds file/line preview results, and opens the selected result at its matching cursor location.

### Terminal

The integrated terminal owns:

- a native PTY master/slave pair
- a spawned platform shell running in the workspace root
- a background reader thread
- a writer for user input
- a `vt100::Parser` screen with scrollback

Keyboard input while terminal-focused is encoded as terminal byte sequences and written to the PTY. PTY output is parsed asynchronously and rendered into the bottom panel.

## 4. Rendering Design

The UI uses a vertical root layout:

1. title bar
2. body
3. status bar

The body uses a horizontal split:

1. file explorer
2. editor column

The editor column uses a vertical split:

1. tab strip and code view
2. integrated terminal

Each render pass records clickable and hoverable rectangles into `HitRegions`.

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

Keyboard events map to panel-specific actions. Quick-panel input handles query editing, result movement, and activation before normal panel shortcuts. Editor-focused input supports save, search, repeated search, undo, redo, and saved-tab close shortcuts. Terminal-focused input is forwarded to the PTY. The app-level exit shortcut is `Ctrl-Q` so `Ctrl-C` can be delivered to the shell when terminal focus is active. Dirty editor buffers trigger an explicit quit confirmation instead of exiting immediately.

## 6. Syntax Highlighting

`syntect` loads default syntax and theme sets once during startup. The renderer chooses syntax by token or file extension, highlights visible lines only, and converts style foreground colors into ratatui `Color::Rgb`.

If highlighting fails, rendering falls back to plain text.

## 7. Cross-Platform Shell Design

The integrated terminal uses:

- Unix: `$SHELL` or `/bin/sh`
- Windows: `%COMSPEC%` or `cmd.exe`

The shell runs inside a PTY with `TERM=xterm-256color`, receives resize notifications from the terminal panel, and inherits the workspace root as the current directory.

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
