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

### R-111 Explorer Visibility Controls

The explorer shall support a visible-tree text filter, dot-prefixed entry visibility toggling, and generated-folder visibility toggling for common generated directories such as `target`, `dist`, `build`, and `node_modules`.

### R-112 Explorer Metadata

The explorer shall show available file size metadata and read-only markers for filesystem entries.

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

### R-217 Command Palette

`F1` or `Ctrl-shift-p` outside terminal focus shall open a command palette overlay. The command palette shall fuzzy-match available commands and execute the selected command with `Enter`.

### R-218 Line Editing Commands

The editor shall support indenting, outdenting, duplicating, deleting, moving, and toggling comments for the active line. `Tab`, `Shift-tab`, `Ctrl-d`, `Ctrl-/`, `Alt-up`, and `Alt-down` shall invoke the corresponding editor actions where the terminal can report those keys.

### R-219 Go To Line

The editor shall support jumping to one-based `line` or `line:column` input through `Ctrl-l` or the command palette.

### R-220 Save All

The command palette shall include a save-all command that writes every dirty editor tab to disk.

### R-221 Editor Selection

The editor shall support text selection with `Shift` plus arrow keys and with mouse drag inside the editor body. Selected text shall be visually highlighted.

### R-222 Editor Clipboard

When editor focus is active, `Ctrl-a` shall select the full buffer, `Ctrl-c` shall copy the selected text to an internal editor clipboard, `Ctrl-x` shall cut the selected text to that clipboard, and `Ctrl-v` shall paste that clipboard at the cursor or replace the active selection. `Ctrl-c` and `Ctrl-x` shall also queue an OSC52 terminal clipboard export for selections within the configured terminal-safe size limit.

### R-223 Selection Replacement

Typing, paste events, `Enter`, `Backspace`, or `Delete` with an active editor selection shall replace or remove the selected range as a single undoable edit.

### R-224 Editor Auto Pairs and Indentation

The editor shall auto-insert closing pairs for `()`, `[]`, `{}`, double quotes, single quotes, and backticks. Typing an existing closing pair at the cursor shall move over it instead of duplicating it. `Backspace` between an empty pair shall remove both sides. `Enter` shall preserve leading indentation and add one extra indent level after an opening `{`, `[`, or `(`, splitting an immediate closing pair onto its own base-indent line.

### R-225 Search Highlighting

When an active in-file search exists, the editor shall visually highlight visible matches and show the match count in the status bar for the active file.

### R-226 Replace in File

`Ctrl-h` or the command palette shall prompt for a search string and replacement string, then replace the current match when the cursor is on a match or the next match otherwise. The replacement shall be undoable as one edit.

### R-227 Replace All in File

The command palette shall include a replace-all-in-file action that prompts for a search string and replacement string, then replaces every active-file match as one undoable edit.

### R-228 Word Navigation

The editor shall support modified-arrow word movement and word selection for `Ctrl-Left`, `Ctrl-Right`, `Ctrl-Shift-Left`, and `Ctrl-Shift-Right` when the terminal reports those keys.

### R-229 Path Clipboard Commands

The command palette shall expose commands to copy the active editor file path and the selected explorer item path as both absolute and workspace-relative strings. These commands shall queue the same OSC52 terminal clipboard export used by editor copy and cut without modifying explorer copy/cut state.

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

Editor `Ctrl-c` shall copy selected editor text instead of sending a terminal signal because the terminal is not focused.

### R-308 Terminal Paste

Paste events while terminal focus is active shall write pasted bytes to the PTY shell.

### R-309 Terminal Clear

The command palette shall include a clear-terminal command that resets the rendered terminal viewport and scrollback while keeping the current shell session alive.

### R-310 Terminal Restart

The command palette shall include a restart-terminal command that terminates the current PTY child and creates a fresh shell session in the workspace root.

### R-311 Terminal ANSI Styles

The terminal renderer shall preserve visible ANSI foreground colors, background colors, bold, dim, italic, underline, and inverse styles parsed from PTY output.

### R-312 Terminal Modified Keys

When terminal focus is active, modified navigation keys, function keys, Shift-Tab, null, and application-cursor arrows shall be encoded as xterm-compatible sequences where crossterm reports enough information.

### R-313 Terminal Bracketed Paste

When the child terminal application enables bracketed paste mode, paste events shall be wrapped in bracketed paste control sequences before being written to the PTY.

### R-314 Terminal File References

Clicking a visible shell output token that resolves to an existing workspace file path, optionally followed by `:line` or `:line:column`, shall open that file in the editor and move the cursor to the referenced location.

### R-315 Terminal Mouse Pass-Through

When the child terminal application requests xterm mouse events, terminal mouse down, release, drag, move, and wheel events shall be forwarded to the PTY instead of being interpreted as source-reference clicks or scrollback movement. The forwarded encoding shall follow the requested xterm mouse mode where the parser exposes it.

### R-316 Terminal Layout Controls

The application shall support moving focus in and out of the terminal, maximizing/restoring the terminal panel, and increasing/decreasing the normal terminal panel height through shortcuts such as `F6`/`F12` or command palette actions.

### R-317 Multiple Terminal Sessions

The integrated terminal shall support multiple PTY shell sessions. The user shall be able to create a new terminal, switch the active terminal, close a terminal, and preserve each terminal's independent shell state while it remains open.

### R-318 Terminal Tab Mouse Controls

The terminal panel shall render terminal session tabs with mouse hover highlighting. Clicking a terminal tab shall activate that session, clicking a close target shall close that session, and clicking the new-terminal target shall create a new PTY session.

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

`Tab` shall cycle focus from the explorer, indent the active line in editor focus, and be forwarded to the PTY shell in terminal focus.

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

When `TSCODE_VERSION` is unset or set to `latest`, the installer shall choose the highest semantic release tag available from GitHub releases instead of trusting API response order.

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
- Use `Ctrl-f` and confirm visible search highlights and match count.
- Use `Ctrl-h` to replace one match, save, and confirm file contents on disk.
- Use the command palette replace-all action and confirm all active-file matches change as one undoable edit.
- Use `F1` to open the command palette and execute an editor command.
- Use `Ctrl-l` or the command palette to jump to a line.
- Select all text, cut it, paste it back, save, and confirm file contents on disk.
- Drag in the editor or use `Shift` with arrow keys and confirm selected text is highlighted.
- Type brackets/quotes and press `Enter` inside a brace pair, then confirm auto-pairing, skip-over, paired deletion, and auto-indent behavior.
- Use line editing commands, save, and confirm file contents on disk.
- Use `Ctrl-p` to quick-open a file by path fragment.
- Use workspace search to find text in a different file and jump to the matching line.
- Use the command palette to clear and restart the integrated terminal.
- Use the mouse wheel over explorer/editor/terminal and confirm scroll changes.
- Type `pwd` in the terminal panel and confirm the output points at the workspace.
- Print colored terminal output and confirm ANSI colors/styles are rendered.
- Print `src/app.rs:10:1`, click it in the terminal panel, and confirm the editor opens that file at line 10, column 1.
- Build release artifacts through the release workflow.
