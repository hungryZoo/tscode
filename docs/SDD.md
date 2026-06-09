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
- cached Git status and dirty-parent-directory markers for explorer badges
- terminal layout state for normal height and maximized mode
- integrated terminal sessions and the active terminal index
- terminal state
- cached UI hit regions from the most recent draw
- syntax highlighter

### Explorer

The explorer stores a tree of `FsNode` values. Directories are loaded lazily when expanded. Filesystem metadata such as file size and read-only state is captured when entries are loaded. A flattened visible row list is produced after changes and during rendering, then filtered by app-level visibility state so generated folders can stay hidden by default without changing the underlying tree. Explorer reveal expands path ancestors and selects the requested file or folder row.

When the workspace is inside a Git repository, `App` shells out to `git status --porcelain=v1 -z --untracked-files=all` at startup, after explorer refreshes, and after editor saves. The parser maps porcelain records to absolute paths and derives dirty parent-directory markers so the renderer can show file-level badges such as `git:M` or `git:?` and folder-level `git:*` badges without mixing Git state into the filesystem tree nodes.

Explorer clipboard state stores copy/cut intent and the source path. Paste performs real filesystem copy or move operations, recursively copies directories, creates non-conflicting copy names, and updates open editor tab paths after moves.

### Editor

Each opened file tab stores:

- absolute path
- display name
- decoded lines
- vertical scroll offset
- horizontal scroll offset
- cursor line and column
- optional selection anchor
- dirty state
- trailing-newline state
- bounded undo and redo stacks

Editor buffers support insertion, deletion, newline, paste, cursor movement, word movement, selection, save, undo, redo, in-file search, go-to-line, and line commands. Selection is stored as an anchor plus the current cursor position and normalized when copying, cutting, deleting, rendering, replacing ranges, or deriving selected line ranges for block commands. Copy and cut update the internal editor clipboard and queue an OSC52 terminal clipboard export when the selection is small enough for terminal-safe transmission. Smart editing lives inside `EditorTab`: printable pair-open characters insert matching closing pairs, selected text can be wrapped by pair characters, existing closing pairs can be skipped over, paired Backspace deletes both sides, and newline insertion preserves or increases indentation depending on surrounding code. Long-line navigation tracks a horizontal scroll offset derived from the editor body width after the line-number gutter is removed; rendering crops styled spans after syntax/search/cursor/selection styling so horizontal scrolling does not discard visual metadata. Line commands include indent, outdent, duplicate, delete, move up/down, and file-type-aware line-comment toggling for either the current line or the selected line range, with each command captured as one undo snapshot. The first prerelease still does not attempt full VS Code parity such as multi-cursor editing or LSP rename.

### Quick Panel

The quick panel stores a mode, query text, result list, selected index, and scroll offset. Quick Open recursively scans workspace files while applying the same hidden/generated visibility policy as the explorer, then fuzzy matches path fragments. Workspace Search scans bounded-size text files under that same visibility policy, builds file/line preview results, and opens the selected result at its matching cursor location. Replace in Files uses prompt state rather than a result overlay: it collects a find string and replacement string, scans the same bounded workspace text-file set, writes replacements to disk, refreshes Git status, updates any clean open tab whose backing file changed, and skips dirty open tabs to avoid overwriting unsaved edits. Command Palette uses the same overlay model with `CommandAction` entries instead of file paths; activating a command dispatches through the app action layer.

### Terminal

Each integrated terminal session owns:

- a native PTY master/slave pair
- a spawned platform shell running in the workspace root
- a background reader thread
- a writer for user input
- a `vt100::Parser` screen with scrollback

The app stores a vector of terminal sessions, an active terminal index, and a monotonic id used for stable terminal tab titles. Keyboard input while terminal-focused is encoded as terminal byte sequences and written to the active PTY. PTY output is drained from every open terminal session so background terminals keep their screen state up to date.

Terminal management commands reset only the active session's `vt100::Parser` for clear-terminal, kill and replace only the active session for restart-terminal, create new independent PTY sessions for new-terminal, and kill/remove terminal sessions for close-terminal. Closing the last terminal restarts it instead of leaving the app without a shell.

Each terminal session also stores the working directory used to spawn its PTY. Normal new-terminal uses the workspace root, while New Terminal Here derives the selected explorer folder or selected file's parent folder and stores that path on the session. Restart-terminal reuses the active session's stored working directory so per-folder shell context survives a shell restart.

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

When terminal maximized mode is active, the editor column is replaced by the integrated terminal so command output can use the main workspace area while the title, explorer, and status bars remain visible. The terminal panel renders a one-line terminal tab strip above the active PTY screen when height allows. The tab strip records hit regions for terminal activation, terminal close, and new-terminal actions.

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

Vertical wheel events route to the hovered panel if known, otherwise to the focused panel. Horizontal wheel events over the editor pan the active tab's long-line viewport.

### Keyboard

Keyboard events map to panel-specific actions. Quick-panel input handles query editing, result movement, and activation before normal panel shortcuts. `F1` opens the command palette because many terminal sessions cannot reliably distinguish `Ctrl-P` from `Ctrl-Shift-P`. Explorer-focused input supports tree filtering and visibility toggles in addition to file operations. Editor-focused input supports save, search, replace, go-to-line, repeated search, undo, redo, selection, word movement, internal clipboard operations, terminal clipboard export, line commands, and saved-tab close shortcuts. Workspace-level shortcuts open quick search or replace prompts before panel-specific handling when terminal focus is not active. Terminal focus and maximize shortcuts are handled before PTY forwarding so `F6`, ``Ctrl-` ``, `F12`, and `Ctrl-J` can move in and out of the integrated terminal even while the shell is focused. Other terminal-focused input is forwarded to the PTY. The app-level exit shortcut is `Ctrl-Q` so `Ctrl-C` can be delivered to the shell when terminal focus is active. Dirty editor buffers trigger an explicit quit confirmation instead of exiting immediately.

### Editor Clipboard

The editor clipboard is an in-memory application clipboard used by `Ctrl-C`, `Ctrl-X`, and `Ctrl-V` in editor focus. It is deliberately separate from explorer copy/cut state and from terminal `Ctrl-C`, which remains a PTY signal when terminal focus is active. Range deletion and replacement are performed as single undoable edits.

For host/terminal clipboard integration, `App` stores one pending clipboard export string. The main event loop takes that value after rendering, encodes it as an OSC52 payload, and writes it directly to the terminal backend. This avoids GUI clipboard dependencies that would weaken SSH behavior or cross-platform release builds. Editor copy/cut and command-palette path-copy actions share this export path; explorer file copy/cut remains a filesystem operation clipboard.

In-file search state lives on `App::search_needle`. The editor renderer overlays visible search-match highlights on top of plain line text when a match is present, while keeping explicit editor selections higher priority. The status bar computes the active file's match count from the same search string.

Replace in file is modeled as a two-step prompt: find text, then replacement text. Single replace changes the current match when the cursor is on one or otherwise searches forward with wraparound. Replace-all updates every active-buffer line match in one `EditorTab` undo snapshot so a single undo restores the previous file contents.

Run Selection in Terminal is an editor-to-PTY bridge in the app action layer. The app derives the active selection, falling back to the current non-blank line, normalizes CRLF/CR line endings to LF, appends a final enter when needed, converts enters to carriage returns for the PTY, writes the bytes through `ShellPanel::send_text`, and focuses the integrated terminal so the submitted command and output are immediately visible.

## 6. Syntax Highlighting

`syntect` loads default syntax and theme sets once during startup. The renderer chooses syntax by token or file extension, highlights visible lines only, and converts style foreground colors into ratatui `Color::Rgb`.

If highlighting fails, rendering falls back to plain text.

## 7. Cross-Platform Shell Design

The integrated terminal uses:

- Unix: `$SHELL` or `/bin/sh`
- Windows: `%COMSPEC%` or `cmd.exe`

The shell runs inside a PTY with `TERM=xterm-256color`, receives resize notifications from the terminal panel, and inherits the workspace root as the current directory.

PTY output is parsed into `vt100` cells. `ShellPanel::styled_rows` groups adjacent cells with the same foreground color, background color, and text modifiers into lightweight terminal spans. The UI layer maps those spans into ratatui `Span` values so command output keeps ANSI color and style information without exposing ratatui rendering concerns to the PTY layer.

Keyboard events are converted to xterm-compatible byte sequences before being written to the PTY. The encoder handles control characters, modified navigation keys, function keys, Shift-Tab, null, and the parser's application-cursor mode so alternate-screen tools receive the arrow-key family they requested.

Paste events use normal byte writes unless the parsed terminal screen has bracketed paste mode enabled. In bracketed paste mode, pasted text is wrapped with the standard begin/end paste control sequences before being sent to the PTY.

Terminal mouse handling has two modes. If the child application has requested xterm mouse events, mouse down, release, drag, move, and wheel events over the terminal body are encoded using the parser's current xterm mouse mode/encoding and passed through to the PTY. Otherwise, a terminal click is interpreted as normal application UI input: the app inspects the clicked visible row and opens an existing `path`, `path:line`, or `path:line:column` reference in the editor when one is found.

## 8. Release and Packaging Design

The repository includes:

- `install.sh`
- `.github/workflows/release.yml`
- `dist/` helper scripts for packaging
- Cargo metadata suitable for `cargo-deb` and `cargo-generate-rpm`

The release workflow builds archive artifacts on macOS, Linux, and Windows runners. Linux package jobs generate `.deb` and `.rpm` packages. The workflow uploads all assets to a GitHub prerelease for tags matching `v*`.

## 9. Risks and Mitigations

- Full terminal emulation is complex. The prerelease uses `vt100` parsing and a real PTY, which supports normal shell interaction, xterm-style keyboard input, and common terminal mouse modes, while deeper terminal edge cases remain ongoing work.
- Cross-compiling every target from one machine may require external linkers. CI uses native runners and cross/zig where practical.
- Terminal mouse support varies by emulator. The app uses crossterm's standard mouse capture and also provides keyboard fallbacks.
