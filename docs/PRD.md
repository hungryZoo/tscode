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

- Full VS Code command parity and extension-host behavior.
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
- As a developer, I can run `tscode path/to/file.rs` and start in that file while the explorer and terminal use its parent folder.
- As a developer, I can run `tscode --help` or `tscode --version` from scripts or installers without requiring an interactive terminal.
- As a developer, I can click folders to expand or collapse them.
- As a developer, I can click files to open them in editor tabs.
- As a developer, I can click editor tabs to switch files.
- As a developer, I can scroll the tree, code view, and terminal output with the mouse wheel.
- As a developer, I can keep editing long source lines because the code view pans horizontally and keeps the cursor visible.
- As a developer, I can see hover highlights for clickable rows, tabs, and panel controls.
- As a developer, I can run shell commands from the bottom terminal panel and see real output.
- As a developer, I can type into a real shell session, use `Ctrl-c`, and keep session state between commands.
- As a developer, I can use full-screen terminal applications such as pagers and editors without tscode stealing their normal terminal keys.
- As a developer, I can see ANSI-colored command output such as compiler errors, `git status`, and `ls --color` without losing terminal styling.
- As a developer, I can click a `path:line:column` reference printed by the shell and jump to that source location in the editor.
- As a developer, I can drag across visible terminal output, see the selected cells highlighted, copy that text, and paste it back into the active shell with terminal-safe shortcuts.
- As a developer, I can search the integrated terminal's visible output and scrollback, see matches highlighted, and jump between matches without losing the PTY session.
- As a developer, I can edit text, save files, create files/folders, rename, delete, and refresh the explorer.
- As a developer, I can copy, cut, paste, and duplicate files or folders from the explorer.
- As a developer, I can right-click explorer rows and run common file actions from a mouse-selectable context menu instead of memorizing every shortcut.
- As a developer, I can reveal the active editor file in the explorer.
- As a developer, I can filter the explorer tree, hide or show dot-prefixed entries, hide or show generated folders, and see basic file metadata while browsing.
- As a developer, explorer filtering finds nested files inside collapsed folders and expands the matching path enough for me to click it.
- As a developer, I can see Git working tree status badges in the explorer when the workspace is inside a Git repository.
- As a developer, I can open a Source Control panel, inspect changed Git files and diff hunks, filter that list, and jump directly from a hunk to the changed source line.
- As a developer, I can undo and redo edits, paste text, repeat search matches, and close saved tabs without losing unsaved changes.
- As a developer, I can save the active editor buffer as a new relative or absolute path, have missing parent folders created, and keep editing the newly saved file.
- As a developer, I can see all matches for the active file search highlighted in the editor and replace either the next match or all matches.
- As a developer, I can use a command palette to discover and execute available editor, explorer, workspace, focus, and terminal actions.
- As a developer, I can right-click inside the editor and run common editing, navigation, formatting, path-copy, terminal-send, revert, and tab actions from a mouse-selectable context menu.
- As a developer, I can right-click the integrated terminal and run terminal copy/paste/search/session-management actions from a mouse-selectable context menu when the shell application is not owning mouse input.
- As a developer, I can select text with the keyboard or mouse, copy or cut it to both the editor clipboard and a terminal clipboard export where supported, then paste it back into the editor.
- As a developer, I can `Alt`+click inside the editor to place multiple cursors and type the same edit at every clicked location.
- As a developer, I can use `Ctrl-D` and `Ctrl-Shift-L` to select the next or all active-file occurrences of the current word or selection, then replace the selected occurrences together as one undoable edit.
- As a developer, I can copy active-file or selected-explorer absolute and workspace-relative paths for use in shell commands, bug reports, or external tools.
- As a developer, I can run terminal commands or external tools that create, delete, or rename files and see the explorer tree update automatically without losing expanded folders, filters, or current selection.
- As a developer, I can use common line editing actions on the current line or selected line range: indent, outdent, duplicate, delete, move, toggle comments, and go to line.
- As a developer, I can rely on editor conveniences such as auto-indent on newline, bracket/quote auto-pairs, skip-over of existing closing pairs, and paired deletion with Backspace.
- As a developer, I can move and select by word in the editor with modified arrow keys.
- As a developer, I can quickly open files by typing a fuzzy path fragment.
- As a developer, I can search text across the workspace, including unsaved edits in open buffers, see file/line previews, and jump to a matching location.
- As a developer, I can list functions, types, classes, interfaces, modules, and similar code symbols in the active file or across the workspace, then jump to the selected symbol.
- As a developer, I can trigger code suggestions at the editor cursor, choose from workspace symbols, identifiers, unsaved open-buffer words, and file-type keywords, and insert the selected suggestion as an undoable edit.
- As a developer, I can jump from the symbol under the editor cursor to a likely workspace definition without leaving the terminal.
- As a developer, I can list whole-word workspace references for the symbol under the editor cursor and jump to any occurrence.
- As a developer, I can move backward and forward through previous editor locations after definition, reference, search, line, or terminal-output jumps.
- As a developer, I can rename the symbol under the editor cursor across workspace files while open buffers receive undoable edits and longer identifiers with the same prefix are left intact.
- As a developer, I can replace text across real workspace files while clean open tabs stay synchronized and dirty open buffers are not overwritten.
- As a developer, I can run a workspace check for supported project types, see compiler diagnostics in a Problems panel, filter the collected diagnostics, and jump directly to the source file and line.
- As a developer, I can see collected compiler diagnostics marked directly in the editor gutter and status bar while editing the affected file.
- As a developer, I can run detected project tasks from `.vscode/tasks.json`, `package.json` scripts, Cargo, Make, Go, and Python projects in a new integrated PTY terminal.
- As a developer, I can send the current editor selection, or the current line when nothing is selected, directly into the integrated terminal shell.
- As a developer, I can clean up trailing spaces and tabs in the active file through the command palette, save the cleaned file, and undo the cleanup if needed.
- As a developer, I can format the active source file with the language formatter already installed on the machine and review the changed dirty buffer before saving.
- As a developer, I can revert the active editor tab back to the file currently on disk when I want to discard unsaved edits or reload external changes.
- As a developer, I can keep files open while terminal commands, Git operations, or external tools change those files on disk; clean tabs reload automatically, while dirty tabs show conflict status and prevent accidental overwrite saves.
- As a developer, I can clear the integrated terminal viewport or restart the PTY shell without restarting the whole application.
- As a developer, I can see which terminal session is active, where it is running, whether it is live, and whether scrollback or terminal child modes are active.
- As a developer, I can focus the integrated terminal quickly, maximize it when command output needs more space, and resize its normal panel height.
- As a developer, I can create multiple integrated terminal sessions, switch between them, close them, and keep each shell's state independent.
- As a developer, I can open a new integrated terminal in the selected explorer folder, or the selected file's parent folder, and keep that working directory when restarting the terminal.
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
- right click opens explorer, editor, or terminal context menus backed by the same commands as keyboard shortcuts and the command palette
- `Alt`+click in the editor toggles mouse-placed extra cursors
- mouse wheel scrolls the hovered/focused panel
- horizontal wheel gestures pan long editor lines

Keyboard fallback:

- `F1` or `Ctrl-Shift-P` opens the command palette; `F1` is the reliable fallback for terminals that cannot distinguish shifted control keys
- `Tab` cycles focus from the explorer, indents in editor focus, and is sent to the shell in terminal focus
- arrow keys navigate focused panels
- `Enter` opens files, edits newlines, or submits shell input depending on focus
- Terminal `Ctrl-Shift-C` copies the active terminal text selection; terminal `Ctrl-Shift-V` pastes the internal clipboard to the PTY shell
- Terminal `Ctrl-F` searches terminal scrollback; terminal `F3` and `Shift-F3` move to next and previous terminal search matches
- Terminal right-click opens a context menu for copy, paste, search, clear, restart, terminal session switching/creation/closing, maximize, resize, and focus actions when the child app is not using terminal mouse reporting
- Explorer `c`, `x`, `p`, `y`, and `o` perform copy, cut, paste, duplicate, and reveal-active-file actions
- Explorer right-click opens a context menu for open/toggle, create, copy/cut/paste/duplicate, rename, delete, path copy, terminal-here, refresh, collapse, and visibility actions
- Explorer `t` opens a new integrated terminal in the selected folder or selected file's parent folder
- Command palette path-copy commands copy active-file or selected-explorer absolute/relative paths through the terminal clipboard
- Explorer `/`, `.`, and `i` perform visible-tree filtering, dotfile visibility toggling, and generated-folder visibility toggling
- `Ctrl-P` opens the quick file picker
- `Ctrl-Shift-F` and `Ctrl-G` open workspace text search, including dirty open buffers from memory
- `Ctrl-Shift-H` opens replace-in-files; the command palette provides the same action as an SSH-friendly fallback
- `Ctrl-Shift-O`, `Ctrl-T`, `Ctrl-]`, and `Ctrl-R` provide document symbols, workspace symbols, go-to-definition, and find-references code navigation
- `Ctrl-Space` or the command palette opens code suggestions for the identifier at the editor cursor
- The command palette provides Run Workspace Check and Show Problems actions for project diagnostics
- The command palette provides a Source Control action for Git changed files and diff hunks
- `Ctrl-Shift-B` or the command palette opens Run Task for detected workspace tasks
- `F2` or the command palette renames the identifier under the editor cursor across visible workspace text files
- `Alt-Left`, `Alt-Right`, or the command palette move backward and forward through editor navigation history
- The command palette provides Save As for writing the active editor buffer to a new path and retargeting the tab
- `Ctrl-S`, `Ctrl-F`, `Ctrl-H`, `F3`, `Shift-F3`, `Ctrl-Z`, `Ctrl-Y`, and `Ctrl-W` provide editor save/search/replace/history/tab-close actions
- Editor right-click opens a context menu for save, copy/cut/paste/select-all, find/replace/go-to-line, definition/references/rename/suggest, format, comments, send-to-terminal, path-copy, revert, and close-tab actions
- `Ctrl-Left`, `Ctrl-Right`, `Ctrl-Shift-Left`, and `Ctrl-Shift-Right` provide word movement and word selection in the editor
- `Shift` with arrow keys and mouse drag select editor text
- `Enter` preserves indentation and adds one extra indent level after opening braces/brackets/parentheses
- bracket, quote, apostrophe, and backtick entry provides editor auto-pair insertion, selection wrapping, closing-pair skip-over, and paired Backspace deletion
- `Ctrl-A`, `Ctrl-C`, `Ctrl-X`, and `Ctrl-V` provide editor select-all, copy, cut, and paste when editor focus is active; copy and cut also export text through OSC52-compatible terminal clipboards
- `Ctrl-Enter` sends the editor selection or current line to the active integrated terminal shell and moves focus to the terminal
- The command palette provides trim-trailing-whitespace for the active editor buffer
- `Shift-Alt-F` or the command palette formats the active editor buffer with an installed language formatter
- The command palette provides revert-file for the active editor buffer
- `Ctrl-D` and `Ctrl-Shift-L` provide occurrence selection for the current word or selection
- `Ctrl-L`, `Ctrl-/`, `Ctrl-Shift-D`, `Alt-Up`, `Alt-Down`, `Tab`, and `Shift-Tab` provide editor go-to-line and selection-aware line editing actions
- `Esc` clears transient mode or exits when appropriate
- `q` exits outside terminal focus; `Ctrl-q` exits globally
- `Shift-PageUp` and `Shift-PageDown` scroll terminal scrollback when terminal focus is active
- `F6` or ``Ctrl-` `` moves focus in or out of the integrated terminal and `F12` or `Ctrl-J` toggles the maximized terminal layout from any panel
- `F7`, `F8`, and `F9` create a new terminal, switch to the next terminal, and close the active terminal

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
