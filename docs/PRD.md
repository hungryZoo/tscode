# tscode Product Requirements Document

## 1. Product Summary

`tscode` is a single-binary, SSH-friendly terminal UI that gives developers a VS Code-like workspace in any terminal session.

The product starts with:

```sh
tscode [path]
```

If `path` is omitted, `tscode` opens the current working directory. The initial release focuses on browsing a real filesystem, opening files into tabs, reading code with syntax highlighting and line numbers, and running shell commands in an integrated terminal panel.

## 2. Goals

- Provide a responsive VS Code-inspired layout inside a terminal.
- Prefer mouse and wheel interactions while preserving keyboard fallbacks.
- Work as a cross-platform binary distributed through GitHub Releases.
- Be installable through a one-line `curl | sh` installer on Unix-like systems.
- Keep the first release intentionally read-oriented and shell-capable rather than a full text editor.

## 3. Non-Goals for the First Prerelease

- Full text editing and persistence.
- Language server protocol integration.
- Debug adapter protocol integration.
- Extension marketplace compatibility.
- Embedded pseudo-terminal emulation with fully interactive alternate-screen apps.

## 4. Target Users

- Developers connected to remote machines through SSH.
- Operators who need to inspect and run commands in repository directories.
- Engineers who prefer mouse-capable terminal workflows.

## 5. Core User Stories

- As a developer, I can run `tscode ~/project` and immediately see the directory tree.
- As a developer, I can click folders to expand or collapse them.
- As a developer, I can click files to open them in editor tabs.
- As a developer, I can click editor tabs to switch files.
- As a developer, I can scroll the tree, code view, and terminal output with the mouse wheel.
- As a developer, I can see hover highlights for clickable rows, tabs, and panel controls.
- As a developer, I can run shell commands from the bottom terminal panel and see real output.
- As a keyboard user, I can navigate, open files, switch focus, scroll, and submit commands without a mouse.

## 6. Required Layout

The first viewport follows the supplied VS Code TUI reference:

- top title/status bar
- left file explorer
- center editor area with tab strip
- bottom integrated terminal
- bottom status bar

The layout must degrade gracefully for small terminal sizes by preserving core regions and avoiding panics.

## 7. Input Model

Mouse input is first-class:

- hover updates visual state
- left click changes focus or activates items
- mouse wheel scrolls the hovered/focused panel

Keyboard fallback:

- `Tab` cycles focus
- arrow keys navigate focused panels
- `Enter` opens files or submits terminal commands
- `Esc` clears transient mode or exits when appropriate
- `q` / `Ctrl-c` exits

## 8. Release Requirements

Prerelease artifacts must target:

- macOS x86_64 and aarch64
- Linux x86_64 and aarch64
- Linux armv7 and aarch64 for Raspberry Pi
- Windows x86_64 and ARM64
- `.deb`, `.rpm`, and tar/zip archives where applicable

GitHub Actions should build and upload release artifacts automatically for tagged prereleases.

## 9. Definition of Done

- `docs/PRD.md`, `docs/SRS.md`, and `docs/SDD.md` exist.
- `cargo build` succeeds on macOS.
- Running `tscode` on macOS shows explorer, editor, terminal, title/status bars, and hover/click/wheel interactions.
- The integrated terminal runs real shell commands and captures output.
- Cross-platform packaging workflow exists.
- `install.sh` detects OS/architecture and installs the matching release binary.
- A GitHub prerelease contains release notes, supported platforms, `install.sh`, and build artifacts.
