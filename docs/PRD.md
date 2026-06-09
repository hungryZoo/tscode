# tscode Product Requirements Document

## 1. Product Summary

`tscode` is a single-binary, SSH-friendly terminal UI that gives developers a VS Code-like workspace in any terminal session.

The product starts with:

```sh
tscode [path]
```

If `path` is omitted, `tscode` opens the current working directory. The initial release focuses on browsing a real filesystem, opening and editing files in tabs, reading code with syntax highlighting and line numbers, and interacting with a real PTY-backed shell in the integrated terminal panel.

## 2. Goals

- Provide a responsive VS Code-inspired layout inside a terminal.
- Prefer mouse and wheel interactions while preserving keyboard fallbacks.
- Work as a cross-platform binary distributed through GitHub Releases.
- Be installable through a one-line `curl | sh` installer on Unix-like systems.
- Keep the first release lightweight while supporting essential editing, saving, file management, and shell workflows.

## 3. Non-Goals for the First Prerelease

- Advanced editor behavior such as multi-cursor editing and full VS Code command parity.
- Language server protocol integration.
- Debug adapter protocol integration.
- Extension marketplace compatibility.
- Perfect terminal emulation for every alternate-screen application and terminal mouse protocol edge case.

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
- As a developer, I can type into a real shell session, use `Ctrl-c`, and keep session state between commands.
- As a developer, I can see ANSI-colored command output such as compiler errors, `git status`, and `ls --color` without losing terminal styling.
- As a developer, I can click a `path:line:column` reference printed by the shell and jump to that source location in the editor.
- As a developer, I can edit text, save files, create files/folders, rename, delete, and refresh the explorer.
- As a developer, I can copy, cut, paste, and duplicate files or folders from the explorer.
- As a developer, I can reveal the active editor file in the explorer.
- As a developer, I can undo and redo edits, paste text, repeat search matches, and close saved tabs without losing unsaved changes.
- As a developer, I can use a command palette to discover and execute available editor, explorer, workspace, focus, and terminal actions.
- As a developer, I can select text with the keyboard or mouse, then copy, cut, and paste through an editor clipboard.
- As a developer, I can use common line editing actions: indent, outdent, duplicate line, delete line, move line, toggle line comment, and go to line.
- As a developer, I can quickly open files by typing a fuzzy path fragment.
- As a developer, I can search text across the workspace, see file/line previews, and jump to a matching location.
- As a developer, I can clear the integrated terminal viewport or restart the PTY shell without restarting the whole application.
- As a developer, I can rename or delete folders and have open tabs update or close consistently with the filesystem change.
- As a keyboard user, I can navigate, open files, switch focus, edit, scroll, and submit shell input without a mouse.

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

- `F1` or `Ctrl-Shift-P` opens the command palette; `F1` is the reliable fallback for terminals that cannot distinguish shifted control keys
- `Tab` cycles focus from the explorer, indents in editor focus, and is sent to the shell in terminal focus
- arrow keys navigate focused panels
- `Enter` opens files, edits newlines, or submits shell input depending on focus
- Explorer `c`, `x`, `p`, `y`, and `o` perform copy, cut, paste, duplicate, and reveal-active-file actions
- `Ctrl-P` opens the quick file picker
- `Ctrl-Shift-F` and `Ctrl-G` open workspace text search
- `Ctrl-S`, `Ctrl-F`, `F3`, `Shift-F3`, `Ctrl-Z`, `Ctrl-Y`, and `Ctrl-W` provide editor save/search/history/tab-close actions
- `Shift` with arrow keys and mouse drag select editor text
- `Ctrl-A`, `Ctrl-C`, `Ctrl-X`, and `Ctrl-V` provide editor select-all, copy, cut, and paste when editor focus is active
- `Ctrl-L`, `Ctrl-/`, `Ctrl-D`, `Alt-Up`, `Alt-Down`, `Tab`, and `Shift-Tab` provide editor go-to-line and line editing actions
- `Esc` clears transient mode or exits when appropriate
- `q` exits outside terminal focus; `Ctrl-q` exits globally
- `Shift-PageUp` and `Shift-PageDown` scroll terminal scrollback when terminal focus is active

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
- The integrated terminal runs a real PTY shell and forwards interactive input.
- Cross-platform packaging workflow exists.
- `install.sh` detects OS/architecture and installs the matching release binary.
- A GitHub prerelease contains release notes, supported platforms, `install.sh`, and build artifacts.
