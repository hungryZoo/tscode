# tscode Software Design Document

## 1. Architecture Overview

`tscode` is a Rust terminal application built on:

- `ratatui` for layout and rendering
- `crossterm` for terminal events, raw mode, alternate screen, and mouse capture
- `syntect` for syntax highlighting
- standard process APIs for command execution

The application is a single event loop that updates an in-memory model and redraws the full screen after meaningful input.

## 2. Module Layout

```text
src/
  app.rs        application state and actions
  fs_tree.rs    filesystem tree loading, flattening, and selection
  main.rs       terminal setup, event loop, panic restoration
  shell.rs      shell command execution and output buffer
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
- optional syntax extension/name

### Terminal

The integrated terminal stores:

- command input
- output lines
- output scroll offset
- workspace root

Commands run synchronously for the first prerelease. The UI appends a prompt, then stdout/stderr lines, then a status line for failed exit codes.

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

### Mouse Wheel

Wheel events route to the hovered panel if known, otherwise to the focused panel.

### Keyboard

Keyboard events map to panel-specific actions. Terminal input receives printable characters when focused.

## 6. Syntax Highlighting

`syntect` loads default syntax and theme sets once during startup. The renderer chooses syntax by token or file extension, highlights visible lines only, and converts style foreground colors into ratatui `Color::Rgb`.

If highlighting fails, rendering falls back to plain text.

## 7. Cross-Platform Shell Design

Command execution uses:

- Unix: `$SHELL -lc <command>` or `/bin/sh -lc <command>`
- Windows: `cmd /C <command>`

Commands inherit the workspace root as the current directory.

## 8. Release and Packaging Design

The repository includes:

- `install.sh`
- `.github/workflows/release.yml`
- `dist/` helper scripts for packaging
- Cargo metadata suitable for `cargo-deb` and `cargo-generate-rpm`

The release workflow builds archive artifacts on macOS, Linux, and Windows runners. Linux package jobs generate `.deb` and `.rpm` packages. The workflow uploads all assets to a GitHub prerelease for tags matching `v*`.

## 9. Risks and Mitigations

- Fully interactive terminal emulation is complex. The prerelease uses command execution and captured output while preserving the panel UX.
- Cross-compiling every target from one machine may require external linkers. CI uses native runners and cross/zig where practical.
- Terminal mouse support varies by emulator. The app uses crossterm's standard mouse capture and also provides keyboard fallbacks.
