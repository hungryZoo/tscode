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

### R-107 Open Tab Synchronization

Renaming a file or folder shall update matching open tab paths. Deleting a file or folder shall close matching open tabs.

### R-108 Explorer Clipboard

The explorer shall support copying and cutting a selected file or folder, pasting it into the selected directory or selected file's parent directory, and recursively copying folder contents.

### R-109 Duplicate

The explorer shall duplicate the selected file or folder beside the original using a non-conflicting copy name.

### R-110 Reveal Active File

The explorer shall reveal the active editor file by expanding parent folders and selecting the file row.

## 4. Editor Requirements

### R-201 Tabs

The editor shall support multiple open file tabs.

### R-202 Tab Selection

Clicking a tab shall make it active.

### R-203 File Reading and Editing

The editor shall read UTF-8 text files and render their contents. Invalid UTF-8 shall be lossily decoded. The active editor buffer shall support text insertion, deletion, newline, cursor movement, and save.

### R-204 Line Numbers

The editor shall show line numbers for opened files.

### R-205 Syntax Highlighting

The editor shall apply syntax highlighting based on file extension or filename when a syntax definition is available.

### R-206 Scrolling

Mouse wheel and keyboard shortcuts shall vertically scroll the active file.

### R-207 Hover

Tabs shall visually highlight on mouse hover.

### R-208 Save

`Ctrl-s` in editor focus shall write the active buffer to its file path and clear dirty state.

### R-209 Search

`Ctrl-f` in editor focus shall prompt for text and move the cursor to the next match when found.

### R-210 Repeated Search

`F3` shall move to the next match for the active search text. `Shift-F3` shall move to the previous match.

### R-211 Undo and Redo

`Ctrl-z` and `Ctrl-y` in editor focus shall undo and redo text edits for the active buffer.

### R-212 Tab Close Safety

Clicking a tab close target, middle-clicking a tab, or pressing `Ctrl-w` shall close a saved tab. Dirty tabs shall not close until saved.

### R-213 Paste

Paste events shall insert pasted text into the active editor buffer or send pasted text to the PTY when terminal focus is active.

### R-214 Newline Preservation

Saving an edited file shall preserve an existing trailing newline unless the buffer content already encodes its own final newline.

### R-215 Quick Open

`Ctrl-p` outside terminal focus shall open a quick-open overlay that fuzzy matches workspace file paths. Pressing `Enter` on a selected result shall open that file.

### R-216 Workspace Text Search

`Ctrl-shift-f` or `Ctrl-g` outside terminal focus shall open a workspace search overlay. The overlay shall scan real workspace files, show file/line previews, and open the selected result at its matching line.

## 5. Integrated Terminal Requirements

### R-301 Command Input

The bottom panel shall contain an interactive platform shell running in a pseudo terminal.

### R-302 Command Execution

Typing while the terminal is focused shall send input bytes to the shell PTY.

### R-303 Output Capture

The terminal panel shall parse PTY output and render the resulting terminal screen.

### R-304 Working Directory

Commands shall execute with the workspace root as the working directory.

### R-305 Scrolling

Mouse wheel and keyboard shortcuts shall scroll terminal output.

### R-306 Hover

Terminal panel controls and focusable areas shall visually react to hover/focus.

### R-307 Interactive Signals

When the terminal is focused, `Ctrl-c` shall be forwarded to the PTY shell rather than exiting the application.

### R-308 Terminal Paste

Paste events while terminal focus is active shall write pasted bytes to the PTY shell.

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

`q` or `Esc` shall exit from non-terminal normal browsing mode. `Ctrl-q` shall exit globally.

Dirty editor buffers shall require an explicit `quit` confirmation before the application exits.

### R-503 Editor Scroll

`PageUp`, `PageDown`, `Up`, and `Down` shall scroll the focused editor.

### R-504 Terminal Editing

When terminal focus is active, printable characters and supported control/navigation keys shall be forwarded to the PTY shell.

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
- Copy, paste, duplicate, and cut/move explorer items and confirm filesystem results.
- Open a file, reveal it in the explorer, and confirm its row is selected.
- Edit text, use `Ctrl-z`/`Ctrl-y`, save with `Ctrl-s`, and confirm file contents on disk.
- Use `Ctrl-f` and `F3` to move through search matches.
- Use `Ctrl-p` to quick-open a file by path fragment.
- Use workspace search to find text in a different file and jump to the matching line.
- Use the mouse wheel over explorer/editor/terminal and confirm scroll changes.
- Type `pwd` in the terminal panel and confirm the output points at the workspace.
- Build release artifacts through the release workflow.
