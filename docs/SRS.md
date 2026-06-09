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

When an explorer filter is applied, matching nested files or folders shall be discovered from the real workspace filesystem even if their parent folders are currently collapsed, and those parent folders shall be expanded enough to display the matches.

### R-112 Explorer Metadata

The explorer shall show available file size metadata and read-only markers for filesystem entries.

### R-113 Explorer Git Status

When the workspace is inside a Git repository, the explorer shall show compact Git status markers for changed files and dirty parent folders. Status markers shall refresh after explorer refreshes and editor saves.

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

Mouse wheel and keyboard shortcuts shall vertically scroll the active file. Long lines shall support horizontal scrolling, cursor tracking, and mouse coordinate mapping against the current horizontal offset.

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

### R-216A Replace in Files

`Ctrl-shift-h` or the command palette shall prompt for a search string and replacement string, then replace literal matches across real workspace text files. The operation shall skip generated folders according to the current explorer visibility policy, skip binary or oversized files, update clean open tabs with the saved replacement content, and skip dirty open tabs so unsaved work is not overwritten.

### R-217 Command Palette

`F1` or `Ctrl-shift-p` outside terminal focus shall open a command palette overlay. The command palette shall fuzzy-match available commands and execute the selected command with `Enter`.

### R-218 Line Editing Commands

The editor shall support indenting, outdenting, duplicating, deleting, moving, and toggling comments for the active line or selected line range. `Tab`, `Shift-tab`, `Ctrl-shift-d`, `Ctrl-/`, `Alt-up`, and `Alt-down` shall invoke the corresponding editor actions where the terminal can report those keys. Each selected-range line command shall be undoable as a single edit.

### R-219 Go To Line

The editor shall support jumping to one-based `line` or `line:column` input through `Ctrl-l` or the command palette.

### R-220 Save All

The command palette shall include a save-all command that writes every dirty editor tab to disk.

### R-221 Editor Selection

The editor shall support text selection with `Shift` plus arrow keys and with mouse drag inside the editor body. Selected text shall be visually highlighted. The editor shall also support multiple selected occurrence ranges in the active file.

### R-221A Mouse Multi-Cursor

`Alt`+click inside the editor body shall toggle an additional editor cursor at the clicked text position after applying line-number gutter, vertical scroll, and horizontal scroll offsets. Typing, paste, `Enter`, `Backspace`, and `Delete` shall operate on every active cursor as one undoable edit when no explicit selection ranges are active. A regular editor click shall clear extra cursors and move back to a single cursor.

### R-222 Editor Clipboard

When editor focus is active, `Ctrl-a` shall select the full buffer, `Ctrl-c` shall copy the selected text to an internal editor clipboard, `Ctrl-x` shall cut the selected text to that clipboard, and `Ctrl-v` shall paste that clipboard at the cursor or replace the active selection. `Ctrl-c` and `Ctrl-x` shall also queue an OSC52 terminal clipboard export for selections within the configured terminal-safe size limit.

### R-223 Selection Replacement

Typing, paste events, `Enter`, `Backspace`, or `Delete` with active editor selections shall replace or remove the selected range or selected occurrence ranges as a single undoable edit.

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

### R-230 Run Selection in Terminal

`Ctrl-enter` or the command palette shall send the active editor selection, or the current non-blank line when no selection exists, to the active PTY shell as submitted command text and then focus the integrated terminal. Blank editor selections and blank current lines shall not send PTY input.

### R-231 Trim Trailing Whitespace

The command palette shall include a trim-trailing-whitespace action for the active editor tab. The action shall remove trailing spaces and tabs from all editor lines, mark the buffer dirty only when a change occurs, preserve the file's trailing newline state, and be undoable as one edit.

### R-232 Revert File

The command palette shall include a revert-file action for the active editor tab. The action shall reload the active file's current contents from disk, discard unsaved editor-buffer changes, clear selection and undo/redo history for that tab, reset the dirty marker, preserve cursor visibility within the reloaded content, and refresh Git status markers.

### R-233 Document Symbols

`Ctrl-shift-o` or the command palette shall open a quick panel listing code symbols extracted from the active editor buffer. Selecting a symbol shall focus the editor and move the cursor to the symbol's file location. The extractor shall recognize common function, method, type, class, interface, module, namespace, and implementation declarations without requiring a language server.

### R-234 Workspace Symbols

`Ctrl-t` or the command palette shall open a quick panel that scans visible workspace text files for code symbols while applying the same hidden/generated-folder visibility policy as quick open and workspace search. Dirty open editor buffers shall be scanned from their in-memory text so unsaved symbols can be found. Selecting a workspace symbol shall open the owning file and move the editor cursor to that symbol.

### R-235 Go To Definition

`Ctrl-]` or the command palette shall use the active editor selection, when it is a valid identifier, or the identifier under the editor cursor to search visible workspace text files for matching code symbol definitions. If one definition is found, the editor shall open that file and move the cursor to the definition. If multiple definitions are found, a quick panel shall list the candidates. Dirty open editor buffers shall be scanned from their in-memory text.

### R-236 Find References

`Ctrl-r` or the command palette shall use the active editor selection, when it is a valid identifier, or the identifier under the editor cursor to open a quick panel listing whole-word workspace references. Selecting a reference shall open its file and move the cursor to the occurrence. Reference scanning shall apply the same visible workspace text-file policy as workspace search and shall use in-memory text for dirty open editor buffers.

### R-237 Format Document

`Shift-alt-f` or the command palette shall format the active editor buffer by piping its current text to a configured external formatter for the file type. Supported formatter integrations shall include `rustfmt` for Rust, `prettier` for JavaScript/TypeScript/JSON/CSS/HTML/Markdown/YAML, `gofmt` for Go, `black` for Python, `shfmt` for shell scripts, and `clang-format` for C-family files where those tools are installed. The command shall update the editor buffer as one undoable edit, mark the tab dirty when formatting changes text, preserve the on-disk file until save, and report a clear message when no formatter is configured.

### R-238 Occurrence Selection

`Ctrl-d` or the command palette shall add the next active-file occurrence of the current single-line selection, or the identifier under the editor cursor when no selection exists, to the editor selection set. `Ctrl-shift-l` or the command palette shall select all active-file occurrences. Identifier-based occurrence selection shall respect identifier boundaries. Copy, cut, typing, paste, `Enter`, `Backspace`, and `Delete` shall operate on all selected occurrence ranges as one undoable edit, and the status bar shall show the selected occurrence count. Continued typing after replacing selected occurrences shall preserve the resulting cursor set for additional simultaneous edits.

### R-239 Editor Navigation History

The app shall record the current editor file, line, and column before quick-panel result jumps, go-to-definition jumps, go-to-line jumps, and terminal `path:line:column` reference jumps when the destination differs from the current location. `Alt-left`, `Alt-right`, and command palette actions shall move backward and forward through this navigation history, reopening the target file if needed and restoring the recorded cursor position. Renaming or moving files and folders through the explorer shall remap matching navigation-history paths; deleting files or folders through the explorer shall remove matching history entries.

### R-240 Rename Symbol

`F2` or the command palette shall prompt for a replacement identifier for the active editor selection, when it is a valid identifier, or the identifier under the editor cursor. The command shall replace whole-identifier occurrences across visible workspace text files using the same hidden/generated-folder visibility policy as workspace search. Open editor buffers shall be updated in memory as undoable dirty edits without immediately writing their backing files. Matching closed files shall be written to disk, binary and oversized files shall be skipped, and longer identifiers that merely contain the old name shall not be modified.

### R-241 Workspace Problems

The command palette shall include Run Workspace Check and Show Problems actions. Run Workspace Check shall detect supported project roots, run the matching external checker in the workspace root, collect parseable file diagnostics from checker output, and open a Problems quick panel. Selecting a problem shall open the referenced file and move the editor cursor to the diagnostic line and column. The Problems panel shall be filterable with the same quick-panel query input and shall report when no supported checker is detected or no parseable diagnostics are found. If dirty editor buffers exist, the completion message shall indicate that unsaved buffers were not checked.

### R-242 Source Control

The command palette shall include a Source Control action. When the workspace is inside a Git repository, the action shall refresh Git status, open a quick panel listing changed files, include diff hunk entries parsed from `git diff --unified=0`, support filtering those entries with the quick-panel query, and open an existing changed file at the selected hunk line. Deleted or otherwise missing files shall remain visible as changed-file entries without crashing when selected.

### R-243 Run Task

`Ctrl-shift-b` or the command palette shall open a Run Task quick panel outside terminal focus. The panel shall detect shell tasks from `.vscode/tasks.json`, package manager scripts from `package.json`, Cargo tasks from `Cargo.toml`, Make targets from `Makefile`, Go tasks from `go.mod`, and Python tasks from `pyproject.toml` or `setup.py`. The panel shall support fuzzy filtering. Selecting a task shall create a new integrated PTY terminal session using the task working directory, send the task command as submitted shell input, focus the terminal, and keep other terminal sessions alive.

## 5. Integrated Terminal Requirements

### R-301 Command Input

The bottom panel shall contain an interactive platform shell running in a pseudo terminal.

### R-302 Command Execution

Typing while the terminal is focused shall send input bytes to the shell PTY.

### R-303 Output Capture

The terminal panel shall parse PTY output and render the resulting terminal screen.

The terminal panel shall show active-session status including terminal title, working directory, live/exited state, nonzero scrollback offset, and active child-requested modes such as alternate screen, bracketed paste, and mouse reporting.

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

The command palette shall include a restart-terminal command that terminates the current PTY child and creates a fresh shell session in that terminal session's stored working directory.

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

### R-316 Terminal Text Selection

When the child terminal application has not requested xterm mouse events, dragging across the terminal body shall select visible terminal cells and render a visual highlight over the selected range. Releasing a non-empty terminal selection shall copy the selected visible text to the internal clipboard and queue an OSC52 terminal clipboard export when it is within the configured terminal-safe size limit. Clicking without dragging shall continue to resolve visible terminal file references.

### R-317 Terminal Clipboard Shortcuts

When terminal focus is active, `Ctrl-Shift-C` shall copy the active terminal text selection without sending `Ctrl-C` to the PTY, and `Ctrl-Shift-V` shall paste the internal clipboard into the active PTY shell using bracketed paste when the child application has enabled it. Plain terminal `Ctrl-C` shall remain a PTY signal.

### R-318 Terminal Layout Controls

The application shall support moving focus in and out of the terminal, maximizing/restoring the terminal panel, and increasing/decreasing the normal terminal panel height through shortcuts such as `F6`/``Ctrl-` `` and `F12`/`Ctrl-J` or command palette actions. The terminal focus and maximize shortcuts shall work even when the terminal panel is currently focused.

### R-319 Multiple Terminal Sessions

The integrated terminal shall support multiple PTY shell sessions. The user shall be able to create a new terminal, switch the active terminal, close a terminal, and preserve each terminal's independent shell state while it remains open.

### R-320 Terminal Tab Mouse Controls

The terminal panel shall render terminal session tabs with mouse hover highlighting. Clicking a terminal tab shall activate that session, clicking a close target shall close that session, and clicking the new-terminal target shall create a new PTY session.

### R-321 New Terminal Here

Explorer `t` and the command palette shall create a new integrated PTY terminal whose current working directory is the selected explorer folder, or the selected file's parent folder. Restarting that terminal shall preserve its current working directory instead of resetting it to the workspace root.

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

`PageUp`, `PageDown`, `Up`, and `Down` shall scroll or move through the focused editor. Horizontal wheel gestures shall pan long editor lines.

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
- Run Workspace Check in a broken Cargo, Go, or Python project and confirm the Problems panel opens with clickable diagnostics.
- Modify a tracked Git file, open Source Control, and confirm changed-file and hunk rows appear and the hunk row opens the file at the changed line.
- Use `Ctrl-shift-b` or the command palette in a project with task metadata and confirm detected tasks appear, filtering works, and selecting one starts a new PTY terminal running that command.
- Use the command palette to clear and restart the integrated terminal.
- Use the mouse wheel over explorer/editor/terminal and confirm scroll changes.
- Type `pwd` in the terminal panel and confirm the output points at the workspace.
- Print colored terminal output and confirm ANSI colors/styles are rendered.
- Print `src/app.rs:10:1`, click it in the terminal panel, and confirm the editor opens that file at line 10, column 1.
- Build release artifacts through the release workflow.
