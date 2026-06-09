# tscode

`tscode` is an SSH-friendly VS Code-style workspace that runs entirely in the terminal.

```sh
tscode [path]
tscode --help
tscode --version
```

The prerelease includes a real filesystem explorer, editable tabbed code buffers with line numbers, syntax highlighting, code folding, mouse hover/click/wheel interactions, and a bottom integrated terminal backed by a real PTY shell. Passing a file path opens its parent folder as the workspace and opens that file in the editor.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | sh
```

The installer detects OS and CPU architecture, downloads the newest GitHub Release asset, and installs `tscode` into `~/.local/bin` unless `TSCODE_INSTALL_DIR` is set.

To install a specific tag:

```sh
curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | TSCODE_VERSION=v0.1.0-pre.50 sh
```

## Controls

- Mouse hover: highlight explorer rows, tabs, and terminal input.
- Mouse click: focus panels, open files, toggle folders, select tabs, close tabs, and place the editor cursor. Right-click opens context menus for explorer, editor, and terminal workflows when the child terminal app is not owning mouse events. `Alt`+click in the editor toggles an additional cursor at the clicked text position.
- Mouse drag in the editor: select text across lines.
- Mouse drag in the terminal: select visible shell output when the child terminal app has not requested mouse events; release copies the selection through the internal clipboard and OSC52 where supported.
- Mouse wheel: scroll the panel under the cursor. Horizontal wheel gestures pan long editor lines.
- `F1` or `Ctrl-Shift-P`: command palette for files, editor actions, explorer actions, focus changes, and terminal management. `F1` is the reliable SSH fallback when a terminal cannot distinguish `Ctrl-P` from `Ctrl-Shift-P`.
- `Tab`: cycle focus from the explorer; indent the current editor line in editor focus; send tab completion to the shell in terminal focus.
- Explorer: `/` filters the real workspace tree and auto-expands collapsed parent folders for matching nested paths, `.` show/hide dot-prefixed entries, `i` show/hide `.gitignore`/`.ignore` ignored entries plus generated folders such as `target`, `dist`, `build`, and `node_modules`, `n` new file, `N` new folder, `e` rename, `D` delete with confirmation, `c` copy, `x` cut, `p` paste, `y` duplicate, `o` reveal active file, `t` open a new terminal in the selected folder or selected file's parent, `r` refresh. Right-clicking an explorer row opens a mouse-selectable context menu for open/toggle, new file/folder, copy/cut/paste/duplicate, rename, delete, path copy, New Terminal Here, refresh, collapse, and visibility toggles. The tree also auto-refreshes after external creates, deletes, renames, and metadata changes from the integrated terminal or other tools while preserving selection, expansion, filters, ignore visibility, and Git badges. Command palette actions can copy selected or active-file absolute/relative paths through the terminal clipboard. File rows show size metadata, read-only markers, and Git status badges such as `git:M`, `git:?`, and `git:*` when the workspace is inside a Git repository.
- Workspace: `Ctrl-P` quick-open files by fuzzy path, `Ctrl-Shift-F` or `Ctrl-G` search text across workspace files including unsaved open buffers, `Ctrl-T` search functions/types/classes/modules across workspace files, `F2` or the command palette renames the identifier under the cursor across workspace files using identifier boundaries, `Ctrl-Shift-H` or the command palette replace text across workspace files while skipping dirty open buffers, `Run Workspace Check` detects Cargo/Go/Python projects, runs the project checker, opens a Problems panel, marks affected editor lines with diagnostic gutter badges/status text, and jumps from a selected diagnostic to the source file and line, `Source Control` lists Git changed files plus diff hunks and jumps from a hunk to the changed source line, and `Run Task` or `Ctrl-Shift-B` detects `.vscode/tasks.json`, `package.json` scripts, Cargo, Make, Go, and Python project tasks. Workspace scans use the same hidden/ignored/generated visibility policy as the explorer, including `.gitignore`, `.ignore`, and global Git ignore rules when ignored entries are hidden.
- Editor: type to edit, paste text, `Enter` newline with auto-indent, `Backspace`/`Delete`, arrows, `Shift`+arrows select text, long-line horizontal scrolling with cursor tracking, auto-pair `()`, `[]`, `{}`, quotes/backticks with skip-over and pair deletion, `Ctrl-Left`/`Ctrl-Right` move by word and `Ctrl-Shift-Left`/`Ctrl-Shift-Right` extend selection by word, click a fold marker in the line-number gutter or press `Alt-[` to fold/unfold the code block at that line and `Alt-]` to unfold all, hover an identifier with the mouse to see definition/reference counts and the first matching workspace definition without moving the cursor, `Alt`+click toggles mouse-placed extra cursors, right-click opens a mouse-selectable editor context menu for save, copy/cut/paste/select all, find/replace/go-to-line, definition/references/rename/suggest, format, fold/unfold, toggle comment, run selection in terminal, path copy, revert, and close-tab actions, `Ctrl-D` adds the next occurrence of the current word/selection, `Ctrl-Shift-L` selects all active-file occurrences, typing or paste writes through every selected occurrence or cursor as one undoable edit, `Ctrl-A` select all, `Ctrl-C`/`Ctrl-X` copy/cut through the internal editor clipboard and OSC52 terminal clipboard where supported, `Ctrl-V` pastes the internal editor clipboard, `Tab`/`Shift-Tab` indent or outdent selected lines, `Ctrl-S` save, command palette Save As writes the active buffer to a new relative or absolute path and retargets the tab, clean tabs automatically reload when their backing file changes on disk, dirty tabs show disk-change markers and refuse overwrite saves until Reload/Revert or Save As is chosen, `Ctrl-F` find in file with visible match highlighting, `Ctrl-H` replace next/current match, `Ctrl-Space` or command palette Trigger Suggest opens code suggestions from workspace symbols, identifiers, dirty open buffers, and file-type keywords, then inserts the selected item as one undoable edit, `Ctrl-Shift-O` list functions/types/classes/modules in the active buffer and jump to one, `Ctrl-]` jumps from the symbol under the cursor to a workspace definition candidate, `Ctrl-R` lists whole-word workspace references for the symbol under the cursor, `F2` renames the symbol under the cursor across visible workspace files and open buffers, `Alt-Left`/`Alt-Right` move backward or forward through editor navigation history after symbol/search/line/terminal-reference jumps, `Shift-Alt-F` or the command palette formats the active buffer with an installed formatter such as `rustfmt`, `prettier`, `gofmt`, `black`, `shfmt`, or `clang-format`, command palette replace-all, trim-trailing-whitespace, reload-file-from-disk, and revert-file-from-disk, `Ctrl-L` go to line, `Ctrl-/` toggle line comments, `Ctrl-Shift-D` duplicate selected lines, `Alt-Up`/`Alt-Down` move selected lines, `Ctrl-Enter` sends the selection or current line to the integrated terminal, `F3` next match, `Shift-F3` previous match, `Ctrl-Z` undo, `Ctrl-Y` redo, `Ctrl-W` close saved tab, `Ctrl-Tab` next tab.
- Terminal: each terminal tab is a separate PTY shell session with its own working directory, and the terminal header shows the active session, cwd, live/exited state, scrollback offset, active search count, and active terminal modes such as bracketed paste, alternate screen, or child-requested mouse reporting. zsh and bash sessions emit cwd updates automatically, and shells or tools that emit OSC 7 update the session cwd after `cd`; restart uses that updated directory. Click terminal tabs to switch, click the tab `x` or middle-click to close, and click `+` or press `F7` to create another workspace-root terminal. Right-clicking the terminal body or terminal tabs opens a mouse-selectable terminal context menu for copy, paste, find, clear, restart, new/close/next/previous terminal, maximize, resize, and focus actions when the child app has not requested terminal mouse ownership. Press explorer `t` or run `New Terminal Here` from the command palette to create a terminal in the selected folder or selected file's parent; restart keeps that terminal's working directory. `F8` switches to the next terminal and `F9` closes the active terminal. Interactive shell input is sent to the active PTY, including `Ctrl-C`, arrows, modified navigation keys, function keys, application-cursor sequences, tab completion, and bracketed paste. ANSI colors/styles are rendered in the panel. `Run Task` or `Ctrl-Shift-B` opens a detected task picker and starts the selected task in a new PTY terminal named for that task. `Ctrl-Enter` or the command palette can run the editor selection/current line in the active PTY shell and then focus the terminal. `Ctrl-F` in terminal focus searches the active terminal scrollback with highlighted matches, and `F3`/`Shift-F3` jump through matches. `Ctrl-Shift-C` copies the active terminal selection, and `Ctrl-Shift-V` pastes the internal clipboard into the active PTY shell. `F6` or ``Ctrl-` `` moves focus in or out of the terminal from any panel, `F12` or `Ctrl-J` toggles a maximized terminal layout from any panel, and command palette actions can increase/decrease terminal height. When a child terminal application owns the screen through alternate-screen or mouse-reporting modes, app terminal-management shortcuts such as `F7`/`F8`/`F9`/`F12`, `Ctrl-F`, `F3`, `Ctrl-J`, and `Shift-PageUp/Down` are sent to the PTY instead of being intercepted, while `F6`/``Ctrl-` `` remain the escape hatch back to tscode panels. `Shift-PageUp` and `Shift-PageDown` scroll terminal scrollback in normal shell mode. Clicking a visible `path:line:column` shell output reference opens the file in the editor when the shell app has not requested terminal mouse events; dragging visible shell output highlights and copies that terminal text on release. When a child app requests mouse reporting, mouse down/up/drag/move/wheel events are forwarded through the PTY with xterm-compatible mouse encoding. The command palette can clear the active terminal viewport/scrollback or restart the active PTY shell.
- App exit: `Ctrl-Q`, or `q`/`Esc` outside terminal focus. Unsaved buffers require typing `quit` to confirm.

## Supported Release Targets

- `x86_64-apple-darwin`
- `aarch64-apple-darwin`
- `x86_64-unknown-linux-gnu`
- `aarch64-unknown-linux-gnu`
- `armv7-unknown-linux-gnueabihf`
- `x86_64-unknown-linux-musl`
- `aarch64-unknown-linux-musl`
- `x86_64-pc-windows-msvc`
- `aarch64-pc-windows-msvc`

Linux release jobs also produce `.deb` and `.rpm` packages for GNU targets.

## Development

```sh
cargo run -- .
```

This project uses Rust, ratatui, crossterm, syntect, portable-pty, and vt100.
