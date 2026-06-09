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
TSCODE_VERSION=v0.1.0-pre.6 curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | sh
```

## Controls

- Mouse hover: highlight explorer rows, tabs, and terminal input.
- Mouse click: focus panels, open files, toggle folders, select tabs, close tabs, and place the editor cursor.
- Mouse wheel: scroll the panel under the cursor.
- `Tab`: cycle focus until terminal focus; in terminal focus it is sent to the shell.
- Explorer: `n` new file, `N` new folder, `e` rename, `D` delete with confirmation, `c` copy, `x` cut, `p` paste, `y` duplicate, `o` reveal active file, `r` refresh.
- Workspace: `Ctrl-P` quick-open files by fuzzy path, `Ctrl-Shift-F` or `Ctrl-G` search text across workspace files.
- Editor: type to edit, paste text, `Enter` newline, `Backspace`/`Delete`, arrows, `Ctrl-S` save, `Ctrl-F` find in file, `F3` next match, `Shift-F3` previous match, `Ctrl-Z` undo, `Ctrl-Y` redo, `Ctrl-W` close saved tab, `Ctrl-Tab` next tab, `Shift-Tab` previous tab.
- Terminal: interactive shell input is sent to the PTY, including `Ctrl-C`, arrows, and tab completion.
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
