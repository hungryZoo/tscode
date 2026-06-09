# tscode Software Requirements Specification

## 1. Scope

This document defines verifiable software requirements for the `tscode` prerelease.

## 2. Runtime Requirements

### R-001 Startup

The application shall start from `tscode [path]`.

### R-002 Workspace Path

If a path is provided, the application shall use that path as the workspace root. If omitted, it shall use the current working directory.

### R-003 Terminal Safety

The application shall enter raw mode, use an alternate screen, enable mouse capture, and restore the terminal on normal exit or panic.

## 3. File Explorer Requirements

### R-101 Real Filesystem

The explorer shall read actual filesystem entries from the workspace root.

### R-102 Sorting

The explorer shall show directories before files and sort each group case-insensitively.

### R-103 Expand and Collapse

Clicking or pressing `Enter` on a directory shall toggle expanded state.

### R-104 File Open

Clicking or pressing `Enter` on a file shall open it in the editor.

### R-105 Scrolling

Mouse wheel and keyboard navigation shall scroll the explorer when content exceeds viewport height.

### R-106 Hover

The explorer shall visually highlight the row under the mouse cursor.

## 4. Editor Requirements

### R-201 Tabs

The editor shall support multiple open file tabs.

### R-202 Tab Selection

Clicking a tab shall make it active.

### R-203 File Reading

The editor shall read UTF-8 text files and render their contents. Invalid UTF-8 shall be lossily decoded.

### R-204 Line Numbers

The editor shall show line numbers for opened files.

### R-205 Syntax Highlighting

The editor shall apply syntax highlighting based on file extension or filename when a syntax definition is available.

### R-206 Scrolling

Mouse wheel and keyboard shortcuts shall vertically scroll the active file.

### R-207 Hover

Tabs shall visually highlight on mouse hover.

## 5. Integrated Terminal Requirements

### R-301 Command Input

The bottom panel shall contain a shell command input line.

### R-302 Command Execution

Submitting a command shall execute it through the user's platform shell.

### R-303 Output Capture

The terminal panel shall capture stdout and stderr and append them to the visible output buffer.

### R-304 Working Directory

Commands shall execute with the workspace root as the working directory.

### R-305 Scrolling

Mouse wheel and keyboard shortcuts shall scroll terminal output.

### R-306 Hover

Terminal panel controls and focusable areas shall visually react to hover/focus.

## 6. Mouse Requirements

### R-401 Mouse Capture

The application shall enable mouse capture while running.

### R-402 Click Focus

Clicking explorer, editor, tab strip, or terminal shall move focus to that panel.

### R-403 Wheel Routing

Mouse wheel events shall apply to the panel under the cursor when possible.

### R-404 Hover Routing

Mouse move events shall update hover target state and cause a redraw.

## 7. Keyboard Requirements

### R-501 Focus Cycle

`Tab` shall cycle through explorer, editor, and terminal focus.

### R-502 Exit

`q`, `Esc`, or `Ctrl-c` shall exit from normal browsing mode.

### R-503 Editor Scroll

`PageUp`, `PageDown`, `Up`, and `Down` shall scroll the focused editor.

### R-504 Terminal Editing

When terminal input is focused, printable characters shall append to the command input, `Backspace` shall delete, and `Enter` shall execute.

## 8. Packaging Requirements

### R-601 Release Archives

Each binary target shall be packaged as an archive with `tscode` or `tscode.exe`, README, and license metadata when available.

### R-602 Linux Packages

The release workflow shall produce `.deb` and `.rpm` packages for supported Linux targets where tooling permits.

### R-603 Installer

`install.sh` shall detect OS and CPU architecture, download the matching GitHub release artifact, and install it into a writable path.

### R-604 CI Release

The GitHub Actions workflow shall build and upload release artifacts when a version tag is pushed.

## 9. Acceptance Tests

- Start `cargo run -- .` on macOS and confirm the TUI renders.
- Click a directory and confirm it expands or collapses.
- Click a file and confirm a tab opens.
- Hover rows/tabs and confirm highlight changes.
- Use the mouse wheel over explorer/editor/terminal and confirm scroll changes.
- Type `pwd` in the terminal panel and confirm the output points at the workspace.
- Build release artifacts through the release workflow.
