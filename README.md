# tscode

`tscode` is an SSH-friendly VS Code-style workspace that runs entirely in the terminal.

```sh
tscode [path]
```

The prerelease includes a real filesystem explorer, tabbed code viewer with line numbers and syntax highlighting, mouse hover/click/wheel interactions, and a bottom integrated terminal panel that executes shell commands in the workspace directory.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | sh
```

The installer detects OS and CPU architecture, downloads the newest GitHub Release asset, and installs `tscode` into `~/.local/bin` unless `TSCODE_INSTALL_DIR` is set.

To install a specific tag:

```sh
TSCODE_VERSION=v0.1.0-pre.2 curl -fsSL https://raw.githubusercontent.com/hungryZoo/tscode/main/install.sh | sh
```

## Controls

- Mouse hover: highlight explorer rows, tabs, and terminal input.
- Mouse click: focus panels, open files, toggle folders, select tabs.
- Mouse wheel: scroll the panel under the cursor.
- `Tab`: cycle focus.
- `Enter`: open/toggle explorer item or run terminal command.
- Arrow keys / PageUp / PageDown: navigate or scroll the focused panel.
- `q`, `Esc`, or `Ctrl-c`: exit.

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

This project uses Rust, ratatui, crossterm, and syntect.
