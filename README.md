# tscode

`tscode` is an SSH-friendly VS Code-style workspace that runs entirely in the terminal.

```sh
tscode [path]
```

The prerelease includes a real filesystem explorer, editable tabbed code buffers with line numbers and syntax highlighting, mouse hover/click/wheel interactions, and a bottom integrated terminal backed by a real PTY shell.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | sh
```

The installer detects OS and CPU architecture, downloads the newest GitHub Release asset, and installs `tscode` into `~/.local/bin` unless `TSCODE_INSTALL_DIR` is set.

To install a specific tag:

```sh
TSCODE_VERSION=v0.1.0-pre.13 curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | sh
```

## Controls

- Mouse hover: highlight explorer rows, tabs, and terminal input.
- Mouse click: focus panels, open files, toggle folders, select tabs, close tabs, and place the editor cursor.
- Mouse drag in the editor: select text across lines.
- Mouse wheel: scroll the panel under the cursor.
- `F1` or `Ctrl-Shift-P`: command palette for files, editor actions, explorer actions, focus changes, and terminal management. `F1` is the reliable SSH fallback when a terminal cannot distinguish `Ctrl-P` from `Ctrl-Shift-P`.
- `Tab`: cycle focus from the explorer; indent the current editor line in editor focus; send tab completion to the shell in terminal focus.
- Explorer: `/` filter visible tree rows, `.` show/hide dot-prefixed entries, `i` show/hide generated folders such as `target`, `dist`, `build`, and `node_modules`, `n` new file, `N` new folder, `e` rename, `D` delete with confirmation, `c` copy, `x` cut, `p` paste, `y` duplicate, `o` reveal active file, `r` refresh. File rows show size metadata and read-only markers when available.
- Workspace: `Ctrl-P` quick-open files by fuzzy path, `Ctrl-Shift-F` or `Ctrl-G` search text across workspace files.
- Editor: type to edit, paste text, `Enter` newline, `Backspace`/`Delete`, arrows, `Shift`+arrows select text, `Ctrl-Left`/`Ctrl-Right` move by word and `Ctrl-Shift-Left`/`Ctrl-Shift-Right` extend selection by word, `Ctrl-A` select all, `Ctrl-C`/`Ctrl-X`/`Ctrl-V` copy/cut/paste through the internal editor clipboard, `Tab`/`Shift-Tab` indent or outdent, `Ctrl-S` save, `Ctrl-F` find in file with visible match highlighting, `Ctrl-H` replace next/current match, command palette replace-all, `Ctrl-L` go to line, `Ctrl-/` toggle line comment, `Ctrl-D` duplicate line, `Alt-Up`/`Alt-Down` move line, `F3` next match, `Shift-F3` previous match, `Ctrl-Z` undo, `Ctrl-Y` redo, `Ctrl-W` close saved tab, `Ctrl-Tab` next tab.
- Terminal: each terminal tab is a separate PTY shell session. Click terminal tabs to switch, click the tab `x` or middle-click to close, and click `+` or press `F7` to create another terminal. `F8` switches to the next terminal and `F9` closes the active terminal. Interactive shell input is sent to the active PTY, including `Ctrl-C`, arrows, modified navigation keys, tab completion, and bracketed paste. ANSI colors/styles are rendered in the panel. `F6` or ``Ctrl-` `` moves focus in or out of the terminal, `F12` or `Ctrl-J` toggles a maximized terminal layout outside terminal focus, and command palette actions can increase/decrease terminal height. `Shift-PageUp` and `Shift-PageDown` scroll terminal scrollback. Clicking a visible `path:line:column` shell output reference opens that file in the editor when the shell app has not requested terminal mouse events. The command palette can clear the active terminal viewport/scrollback or restart the active PTY shell.
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
