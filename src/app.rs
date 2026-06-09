use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::Result;
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;

use crate::{
    fs_tree::{FsTree, VisibleNode},
    shell::ShellPanel,
    syntax::SyntaxHighlighter,
};

const MAX_QUICK_ITEMS: usize = 200;
const MAX_FILE_SCAN_BYTES: u64 = 1_000_000;
const MAX_OSC52_CLIPBOARD_BYTES: usize = 512 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Explorer,
    Editor,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HoverTarget {
    #[default]
    None,
    Explorer,
    ExplorerRow(usize),
    Editor,
    Tab(usize),
    TabClose(usize),
    TerminalTab(usize),
    TerminalTabClose(usize),
    TerminalNew,
    QuickRow(usize),
    Terminal,
    TerminalInput,
}

#[derive(Debug, Clone, Default)]
pub struct HitRegions {
    pub explorer_area: Option<Rect>,
    pub editor_area: Option<Rect>,
    pub editor_body: Option<Rect>,
    pub terminal_area: Option<Rect>,
    pub terminal_body: Option<Rect>,
    pub terminal_input: Option<Rect>,
    pub explorer_rows: Vec<(Rect, usize)>,
    pub tabs: Vec<(Rect, usize)>,
    pub tab_closes: Vec<(Rect, usize)>,
    pub terminal_tabs: Vec<(Rect, usize)>,
    pub terminal_tab_closes: Vec<(Rect, usize)>,
    pub terminal_new: Option<Rect>,
    pub quick_rows: Vec<(Rect, usize)>,
    pub last_mouse_x: u16,
    pub last_mouse_y: u16,
}

impl HitRegions {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn target_at(&self, x: u16, y: u16) -> HoverTarget {
        for (rect, index) in &self.quick_rows {
            if contains(*rect, x, y) {
                return HoverTarget::QuickRow(*index);
            }
        }

        if self.terminal_new.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::TerminalNew;
        }

        for (rect, index) in &self.terminal_tab_closes {
            if contains(*rect, x, y) {
                return HoverTarget::TerminalTabClose(*index);
            }
        }

        for (rect, index) in &self.terminal_tabs {
            if contains(*rect, x, y) {
                return HoverTarget::TerminalTab(*index);
            }
        }

        for (rect, index) in &self.tab_closes {
            if contains(*rect, x, y) {
                return HoverTarget::TabClose(*index);
            }
        }

        for (rect, index) in &self.tabs {
            if contains(*rect, x, y) {
                return HoverTarget::Tab(*index);
            }
        }

        for (rect, index) in &self.explorer_rows {
            if contains(*rect, x, y) {
                return HoverTarget::ExplorerRow(*index);
            }
        }

        if self.terminal_input.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::TerminalInput;
        }

        if self.explorer_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Explorer;
        }

        if self.editor_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Editor;
        }

        if self.terminal_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Terminal;
        }

        HoverTarget::None
    }
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

#[derive(Debug, Clone)]
struct EditorSnapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    trailing_newline: bool,
}

#[derive(Debug, Clone)]
pub struct EditorTab {
    pub path: PathBuf,
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub selection_anchor: Option<(usize, usize)>,
    pub dirty: bool,
    trailing_newline: bool,
    undo_stack: Vec<EditorSnapshot>,
    redo_stack: Vec<EditorSnapshot>,
}

impl EditorTab {
    fn open(path: PathBuf) -> Result<Self> {
        let bytes = std::fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes);
        let trailing_newline = text.ends_with('\n');
        let mut lines = text.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
        if lines.is_empty() {
            lines.push(String::new());
        }

        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("[file]")
            .to_owned();

        Ok(Self {
            path,
            title,
            lines,
            scroll: 0,
            cursor_line: 0,
            cursor_col: 0,
            selection_anchor: None,
            dirty: false,
            trailing_newline,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    fn save(&mut self) -> Result<()> {
        let mut text = self.lines.join("\n");
        if self.trailing_newline && !text.ends_with('\n') {
            text.push('\n');
        }
        fs::write(&self.path, text)?;
        self.dirty = false;
        Ok(())
    }

    fn insert_char(&mut self, c: char) {
        if self.selection_range().is_none() && self.char_at_cursor() == Some(c) && is_pair_close(c)
        {
            self.cursor_col += 1;
            return;
        }

        if let Some(close) = auto_pair_close(c) {
            if self.selection_range().is_some() {
                self.wrap_selection_with(c, close);
                return;
            }

            self.push_undo();
            self.insert_char_raw(c);
            self.insert_char_raw(close);
            self.cursor_col = self.cursor_col.saturating_sub(1);
            self.dirty = true;
            return;
        }

        if self.selection_range().is_some() {
            self.replace_selection_with(&c.to_string());
            return;
        }

        self.push_undo();
        self.insert_char_raw(c);
        self.dirty = true;
    }

    fn insert_text(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }

        if self.selection_range().is_some() {
            self.replace_selection_with(text);
            return;
        }

        self.push_undo();
        self.insert_text_raw(text);
        self.dirty = true;
    }

    fn insert_text_raw(&mut self, text: &str) {
        for c in text.chars() {
            match c {
                '\r' => {}
                '\n' => self.newline_raw(),
                c => self.insert_char_raw(c),
            }
        }
    }

    fn insert_char_raw(&mut self, c: char) {
        let cursor_col = self.cursor_col;
        let line = self.current_line_mut();
        let byte = byte_index_for_char(line, cursor_col);
        line.insert(byte, c);
        self.cursor_col += 1;
    }

    fn newline(&mut self) {
        if self.selection_range().is_some() {
            self.replace_selection_with("\n");
            return;
        }

        self.push_undo();
        self.newline_auto_indent_raw();
        self.dirty = true;
    }

    fn newline_raw(&mut self) {
        let cursor_col = self.cursor_col;
        let line = self.current_line_mut();
        let byte = byte_index_for_char(line, cursor_col);
        let rest = line.split_off(byte);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
    }

    fn newline_auto_indent_raw(&mut self) {
        let cursor_col = self.cursor_col;
        let current = self.lines[self.cursor_line].clone();
        let before = take_chars_owned(&current, cursor_col);
        let after = skip_chars_owned(&current, cursor_col);
        let base_indent = leading_whitespace(&current);
        let indent_unit = indent_unit_for(&base_indent);
        let open = before.trim_end().chars().last();
        let should_indent = open.is_some_and(is_auto_indent_open);
        let inner_indent = if should_indent {
            format!("{base_indent}{indent_unit}")
        } else {
            base_indent.clone()
        };
        let should_split_pair = open
            .and_then(auto_pair_close)
            .is_some_and(|close| after.trim_start().starts_with(close));

        self.lines[self.cursor_line] = before;
        self.cursor_line += 1;
        self.cursor_col = inner_indent.chars().count();
        if should_split_pair {
            self.lines.insert(self.cursor_line, inner_indent);
            self.lines
                .insert(self.cursor_line + 1, format!("{base_indent}{after}"));
        } else {
            self.lines
                .insert(self.cursor_line, format!("{inner_indent}{after}"));
        }
    }

    fn backspace(&mut self) {
        if self.delete_selection().is_some() {
            return;
        }

        if self.cursor_col == 0 && self.cursor_line == 0 {
            return;
        }

        self.push_undo();
        if self.cursor_col > 0
            && self.cursor_line < self.lines.len()
            && let Some(previous) = self.char_before_cursor()
            && auto_pair_close(previous) == self.char_at_cursor()
        {
            let line = self.cursor_line;
            let start = self.cursor_col - 1;
            let end = self.cursor_col + 1;
            self.delete_range_raw((line, start), (line, end));
            self.dirty = true;
            return;
        }

        if self.cursor_col > 0 {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            let end = byte_index_for_char(line, cursor_col);
            let start = byte_index_for_char(line, cursor_col - 1);
            line.replace_range(start..end, "");
            self.cursor_col -= 1;
        } else if self.cursor_line > 0 {
            let removed = self.lines.remove(self.cursor_line);
            self.cursor_line -= 1;
            self.cursor_col = self.lines[self.cursor_line].chars().count();
            self.lines[self.cursor_line].push_str(&removed);
        }
        self.dirty = true;
    }

    fn delete(&mut self) {
        if self.delete_selection().is_some() {
            return;
        }

        let line_len = self.lines[self.cursor_line].chars().count();
        if self.cursor_col >= line_len && self.cursor_line + 1 >= self.lines.len() {
            return;
        }

        self.push_undo();
        if self.cursor_col < line_len {
            let cursor_col = self.cursor_col;
            let line = self.current_line_mut();
            let start = byte_index_for_char(line, cursor_col);
            let end = byte_index_for_char(line, cursor_col + 1);
            line.replace_range(start..end, "");
        } else if self.cursor_line + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor_line + 1);
            self.lines[self.cursor_line].push_str(&next);
        }
        self.dirty = true;
    }

    fn indent_line(&mut self) {
        self.push_undo();
        self.lines[self.cursor_line].insert_str(0, "    ");
        self.cursor_col = self.cursor_col.saturating_add(4);
        self.dirty = true;
    }

    fn outdent_line(&mut self) -> bool {
        let remove_count = leading_indent_width(&self.lines[self.cursor_line]);
        if remove_count == 0 {
            return false;
        }

        self.push_undo();
        let end = byte_index_for_char(&self.lines[self.cursor_line], remove_count);
        self.lines[self.cursor_line].replace_range(0..end, "");
        self.cursor_col = self.cursor_col.saturating_sub(remove_count);
        self.dirty = true;
        true
    }

    fn duplicate_line(&mut self) {
        self.push_undo();
        let duplicate = self.lines[self.cursor_line].clone();
        self.cursor_line += 1;
        self.lines.insert(self.cursor_line, duplicate);
        self.clamp_cursor_col();
        self.dirty = true;
    }

    fn delete_line(&mut self) {
        self.push_undo();
        if self.lines.len() == 1 {
            self.lines[0].clear();
            self.cursor_col = 0;
        } else {
            self.lines.remove(self.cursor_line);
            self.cursor_line = self.cursor_line.min(self.lines.len().saturating_sub(1));
            self.clamp_cursor_col();
        }
        self.dirty = true;
    }

    fn move_line_up(&mut self) -> bool {
        if self.cursor_line == 0 {
            return false;
        }

        self.push_undo();
        self.lines.swap(self.cursor_line, self.cursor_line - 1);
        self.cursor_line -= 1;
        self.dirty = true;
        true
    }

    fn move_line_down(&mut self) -> bool {
        if self.cursor_line + 1 >= self.lines.len() {
            return false;
        }

        self.push_undo();
        self.lines.swap(self.cursor_line, self.cursor_line + 1);
        self.cursor_line += 1;
        self.dirty = true;
        true
    }

    fn toggle_line_comment(&mut self) -> bool {
        let Some(token) = comment_token_for_path(&self.path) else {
            return false;
        };

        self.push_undo();
        let line = &mut self.lines[self.cursor_line];
        let indent_chars = line.chars().take_while(|c| c.is_whitespace()).count();
        let indent_byte = byte_index_for_char(line, indent_chars);
        let body = &line[indent_byte..];
        let token_with_space = format!("{token} ");

        if body.starts_with(&token_with_space) {
            let remove_end = indent_byte + token_with_space.len();
            line.replace_range(indent_byte..remove_end, "");
            self.cursor_col = self
                .cursor_col
                .saturating_sub(token_with_space.chars().count());
        } else if body.starts_with(token) {
            let remove_end = indent_byte + token.len();
            line.replace_range(indent_byte..remove_end, "");
            self.cursor_col = self.cursor_col.saturating_sub(token.chars().count());
        } else {
            line.insert_str(indent_byte, &token_with_space);
            if self.cursor_col >= indent_chars {
                self.cursor_col = self
                    .cursor_col
                    .saturating_add(token_with_space.chars().count());
            }
        }

        self.dirty = true;
        true
    }

    fn move_cursor_with_selection(&mut self, line_delta: isize, col_delta: isize, selecting: bool) {
        let previous = self.cursor_position();
        if selecting && self.selection_anchor.is_none() {
            self.selection_anchor = Some(previous);
        } else if !selecting {
            self.selection_anchor = None;
        }

        self.cursor_line =
            add_signed(self.cursor_line, line_delta).min(self.lines.len().saturating_sub(1));
        self.cursor_col = add_signed(self.cursor_col, col_delta);
        self.clamp_cursor_col();
        self.clear_collapsed_selection();
    }

    fn move_word(&mut self, forward: bool, selecting: bool) {
        let previous = self.cursor_position();
        if selecting && self.selection_anchor.is_none() {
            self.selection_anchor = Some(previous);
        } else if !selecting {
            self.selection_anchor = None;
        }

        let (line, col) = if forward {
            next_word_position(&self.lines, self.cursor_line, self.cursor_col)
        } else {
            previous_word_position(&self.lines, self.cursor_line, self.cursor_col)
        };
        self.set_cursor_raw(line, col);
        self.clear_collapsed_selection();
    }

    fn set_cursor(&mut self, line: usize, col: usize) {
        self.selection_anchor = None;
        self.set_cursor_raw(line, col);
    }

    fn set_cursor_selecting(&mut self, line: usize, col: usize) {
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor_position());
        }
        self.set_cursor_raw(line, col);
        self.clear_collapsed_selection();
    }

    fn set_cursor_raw(&mut self, line: usize, col: usize) {
        self.cursor_line = line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = col;
        self.clamp_cursor_col();
    }

    fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    fn current_line_mut(&mut self) -> &mut String {
        &mut self.lines[self.cursor_line]
    }

    fn char_before_cursor(&self) -> Option<char> {
        if self.cursor_col == 0 {
            return None;
        }
        self.lines[self.cursor_line]
            .chars()
            .nth(self.cursor_col - 1)
    }

    fn char_at_cursor(&self) -> Option<char> {
        self.lines[self.cursor_line].chars().nth(self.cursor_col)
    }

    fn clamp_cursor_col(&mut self) {
        let line_len = self.lines[self.cursor_line].chars().count();
        self.cursor_col = self.cursor_col.min(line_len);
    }

    pub fn selection_range(&self) -> Option<((usize, usize), (usize, usize))> {
        let anchor = self.selection_anchor?;
        let cursor = self.cursor_position();
        if anchor == cursor {
            None
        } else if anchor < cursor {
            Some((anchor, cursor))
        } else {
            Some((cursor, anchor))
        }
    }

    fn clear_selection(&mut self) {
        self.selection_anchor = None;
    }

    fn clear_collapsed_selection(&mut self) {
        if self.selection_anchor == Some(self.cursor_position()) {
            self.selection_anchor = None;
        }
    }

    fn select_all(&mut self) {
        self.selection_anchor = Some((0, 0));
        self.cursor_line = self.lines.len().saturating_sub(1);
        self.cursor_col = self.lines[self.cursor_line].chars().count();
    }

    pub fn selected_text(&self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        Some(self.text_in_range(start, end))
    }

    fn delete_selection(&mut self) -> Option<String> {
        let (start, end) = self.selection_range()?;
        let deleted = self.text_in_range(start, end);
        self.push_undo();
        self.delete_range_raw(start, end);
        self.dirty = true;
        Some(deleted)
    }

    fn replace_selection_with(&mut self, text: &str) {
        let Some((start, end)) = self.selection_range() else {
            return;
        };
        self.push_undo();
        self.delete_range_raw(start, end);
        self.insert_text_raw(text);
        self.dirty = true;
    }

    fn wrap_selection_with(&mut self, open: char, close: char) {
        let Some((start, end)) = self.selection_range() else {
            return;
        };
        let selected = self.text_in_range(start, end);
        self.push_undo();
        self.delete_range_raw(start, end);
        self.insert_char_raw(open);
        self.insert_text_raw(&selected);
        self.insert_char_raw(close);
        let open_width = open.len_utf8();
        self.selection_anchor = Some((start.0, start.1 + open_width));
        self.cursor_line = end.0;
        self.cursor_col = end.1 + open_width;
        self.dirty = true;
    }

    fn replace_match_at(
        &mut self,
        line_index: usize,
        col: usize,
        needle: &str,
        replacement: &str,
    ) -> bool {
        if needle.is_empty() || line_index >= self.lines.len() {
            return false;
        }

        let line = &self.lines[line_index];
        let start_byte = byte_index_for_char(line, col);
        if !line[start_byte..].starts_with(needle) {
            return false;
        }

        let end_col = col + needle.chars().count();
        self.push_undo();
        self.set_cursor_raw(line_index, col);
        self.delete_range_raw((line_index, col), (line_index, end_col));
        self.insert_text_raw(replacement);
        self.dirty = true;
        true
    }

    fn replace_all_matches(&mut self, needle: &str, replacement: &str) -> usize {
        if needle.is_empty() {
            return 0;
        }

        let count = self
            .lines
            .iter()
            .map(|line| line.matches(needle).count())
            .sum::<usize>();
        if count == 0 {
            return 0;
        }

        self.push_undo();
        for line in &mut self.lines {
            *line = line.replace(needle, replacement);
        }
        self.clear_selection();
        self.clamp_cursor_col();
        self.dirty = true;
        count
    }

    fn text_in_range(&self, start: (usize, usize), end: (usize, usize)) -> String {
        let (start_line, start_col) = start;
        let (end_line, end_col) = end;
        if start_line == end_line {
            return slice_chars(&self.lines[start_line], start_col, end_col);
        }

        let mut parts = Vec::new();
        parts.push(skip_chars_owned(&self.lines[start_line], start_col));
        for line_index in (start_line + 1)..end_line {
            parts.push(self.lines[line_index].clone());
        }
        parts.push(take_chars_owned(&self.lines[end_line], end_col));
        parts.join("\n")
    }

    fn delete_range_raw(&mut self, start: (usize, usize), end: (usize, usize)) {
        let (start_line, start_col) = start;
        let (end_line, end_col) = end;
        if start_line == end_line {
            let start_byte = byte_index_for_char(&self.lines[start_line], start_col);
            let end_byte = byte_index_for_char(&self.lines[start_line], end_col);
            self.lines[start_line].replace_range(start_byte..end_byte, "");
        } else {
            let prefix = take_chars_owned(&self.lines[start_line], start_col);
            let suffix = skip_chars_owned(&self.lines[end_line], end_col);
            self.lines[start_line] = format!("{prefix}{suffix}");
            self.lines.drain((start_line + 1)..=end_line);
        }

        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.cursor_line = start_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = start_col;
        self.clamp_cursor_col();
        self.clear_selection();
    }

    fn undo(&mut self) -> bool {
        let Some(snapshot) = self.undo_stack.pop() else {
            return false;
        };
        self.redo_stack.push(self.snapshot());
        self.restore_snapshot(snapshot);
        self.dirty = true;
        true
    }

    fn redo(&mut self) -> bool {
        let Some(snapshot) = self.redo_stack.pop() else {
            return false;
        };
        self.undo_stack.push(self.snapshot());
        self.restore_snapshot(snapshot);
        self.dirty = true;
        true
    }

    fn snapshot(&self) -> EditorSnapshot {
        EditorSnapshot {
            lines: self.lines.clone(),
            cursor_line: self.cursor_line,
            cursor_col: self.cursor_col,
            trailing_newline: self.trailing_newline,
        }
    }

    fn restore_snapshot(&mut self, snapshot: EditorSnapshot) {
        self.lines = if snapshot.lines.is_empty() {
            vec![String::new()]
        } else {
            snapshot.lines
        };
        self.trailing_newline = snapshot.trailing_newline;
        self.cursor_line = snapshot.cursor_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = snapshot.cursor_col;
        self.clamp_cursor_col();
        self.clear_selection();
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > 200 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    NewFile,
    NewDir,
    Rename(PathBuf),
    Delete(PathBuf),
    ExplorerFilter,
    Search,
    ReplaceFind { all: bool },
    ReplaceWith { needle: String, all: bool },
    GotoLine,
    QuitDirty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub input: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickPanelKind {
    OpenFile,
    WorkspaceSearch,
    CommandPalette,
}

#[derive(Debug, Clone)]
pub struct QuickItem {
    pub label: String,
    pub detail: String,
    pub path: PathBuf,
    pub line: Option<usize>,
    pub col: Option<usize>,
    pub preview: Option<String>,
    pub command: Option<CommandAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    QuickOpen,
    WorkspaceSearch,
    SaveFile,
    SaveAll,
    CloseActiveTab,
    CloseSavedTabs,
    NewFile,
    NewFolder,
    RenameSelected,
    DeleteSelected,
    RefreshExplorer,
    CollapseExplorer,
    RevealActiveFile,
    CopyActiveFilePath,
    CopyActiveFileRelativePath,
    CopySelectedExplorerPath,
    CopySelectedExplorerRelativePath,
    FilterExplorer,
    ClearExplorerFilter,
    ToggleHiddenFiles,
    ToggleIgnoredFiles,
    FindInFile,
    ReplaceInFile,
    ReplaceAllInFile,
    GotoLine,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    ToggleLineComment,
    IndentLine,
    OutdentLine,
    SelectAll,
    CopySelection,
    CutSelection,
    PasteClipboard,
    FocusExplorer,
    FocusEditor,
    FocusTerminal,
    ClearTerminal,
    RestartTerminal,
    NewTerminal,
    CloseTerminal,
    NextTerminal,
    PreviousTerminal,
    ToggleTerminalFocus,
    ToggleTerminalMaximized,
    IncreaseTerminalHeight,
    DecreaseTerminalHeight,
}

#[derive(Debug, Clone)]
pub struct QuickPanel {
    pub kind: QuickPanelKind,
    pub query: String,
    pub items: Vec<QuickItem>,
    pub selected: usize,
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardAction {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
pub struct ExplorerClipboard {
    pub action: ClipboardAction,
    pub path: PathBuf,
}

pub struct TerminalSession {
    pub id: usize,
    pub title: String,
    pub shell: ShellPanel,
    pub exited: bool,
}

impl TerminalSession {
    fn new(id: usize, workspace: PathBuf) -> Result<Self> {
        Ok(Self {
            id,
            title: format!("term {id}"),
            shell: ShellPanel::new(workspace)?,
            exited: false,
        })
    }
}

pub struct App {
    pub root: PathBuf,
    pub explorer: FsTree,
    pub tabs: Vec<EditorTab>,
    pub active_tab: Option<usize>,
    pub focus: FocusPanel,
    pub hover: HoverTarget,
    pub hit_regions: HitRegions,
    pub terminals: Vec<TerminalSession>,
    pub active_terminal: usize,
    next_terminal_id: usize,
    pub syntax: SyntaxHighlighter,
    pub should_quit: bool,
    pub explorer_height: usize,
    pub editor_height: usize,
    pub terminal_height: usize,
    pub terminal_rows: u16,
    pub terminal_maximized: bool,
    pub last_error: Option<String>,
    pub prompt: Option<PromptState>,
    pub message: Option<String>,
    pub search_needle: Option<String>,
    pub explorer_filter: Option<String>,
    pub show_hidden: bool,
    pub show_ignored: bool,
    pub quick_panel: Option<QuickPanel>,
    pub quick_panel_height: usize,
    pub explorer_clipboard: Option<ExplorerClipboard>,
    pub editor_clipboard: Option<String>,
    pending_clipboard_export: Option<String>,
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let root = root.canonicalize().unwrap_or(root);
        let explorer = FsTree::new(root.clone())?;
        let terminal = TerminalSession::new(1, root.clone())?;
        Ok(Self {
            root: root.clone(),
            explorer,
            tabs: Vec::new(),
            active_tab: None,
            focus: FocusPanel::Explorer,
            hover: HoverTarget::None,
            hit_regions: HitRegions::default(),
            terminals: vec![terminal],
            active_terminal: 0,
            next_terminal_id: 2,
            syntax: SyntaxHighlighter::new(),
            should_quit: false,
            explorer_height: 0,
            editor_height: 0,
            terminal_height: 0,
            terminal_rows: 10,
            terminal_maximized: false,
            last_error: None,
            prompt: None,
            message: Some("F1/Ctrl-Shift-P commands | Ctrl-P files | Editor: Ctrl-A/C/X/V selection | Terminal: Ctrl-Q quit".to_owned()),
            search_needle: None,
            explorer_filter: None,
            show_hidden: true,
            show_ignored: false,
            quick_panel: None,
            quick_panel_height: 0,
            explorer_clipboard: None,
            editor_clipboard: None,
            pending_clipboard_export: None,
        })
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        filtered_visible_nodes(
            self.explorer.visible_nodes(),
            &self.root,
            self.show_hidden,
            self.show_ignored,
            self.explorer_filter.as_deref(),
        )
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        if self.quick_panel.is_some() {
            return self.handle_quick_panel_key(key);
        }

        if self.prompt.is_some() {
            return self.handle_prompt_key(key);
        }

        if !matches!(self.focus, FocusPanel::Terminal) && key.code == KeyCode::F(1) {
            self.open_quick_panel(QuickPanelKind::CommandPalette)?;
            return Ok(());
        }
        if !matches!(self.focus, FocusPanel::Terminal) && key.code == KeyCode::F(6) {
            self.toggle_terminal_focus();
            return Ok(());
        }
        if !matches!(self.focus, FocusPanel::Terminal) && key.code == KeyCode::F(12) {
            self.toggle_terminal_maximized();
            return Ok(());
        }
        if key.code == KeyCode::F(7) {
            self.new_terminal()?;
            return Ok(());
        }
        if key.code == KeyCode::F(8) {
            self.next_terminal();
            return Ok(());
        }
        if key.code == KeyCode::F(9) {
            self.close_active_terminal()?;
            return Ok(());
        }

        if !matches!(self.focus, FocusPanel::Terminal)
            && key.modifiers.contains(KeyModifiers::CONTROL)
        {
            match key.code {
                KeyCode::Char('`') => {
                    self.toggle_terminal_focus();
                    return Ok(());
                }
                KeyCode::Char('j') => {
                    self.toggle_terminal_maximized();
                    return Ok(());
                }
                KeyCode::Char('P') => {
                    self.open_quick_panel(QuickPanelKind::CommandPalette)?;
                    return Ok(());
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.open_quick_panel(QuickPanelKind::CommandPalette)?;
                    return Ok(());
                }
                KeyCode::Char('p') => {
                    self.open_quick_panel(QuickPanelKind::OpenFile)?;
                    return Ok(());
                }
                KeyCode::Char('F') | KeyCode::Char('g') => {
                    self.open_quick_panel(QuickPanelKind::WorkspaceSearch)?;
                    return Ok(());
                }
                KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.open_quick_panel(QuickPanelKind::WorkspaceSearch)?;
                    return Ok(());
                }
                _ => {}
            }
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            self.request_quit();
            return Ok(());
        }

        match key.code {
            KeyCode::Tab if key.modifiers.contains(KeyModifiers::CONTROL) => self.next_tab(),
            KeyCode::BackTab if self.focus == FocusPanel::Editor => {
                self.handle_editor_key(key)?;
            }
            KeyCode::BackTab if !matches!(self.focus, FocusPanel::Terminal) => self.previous_tab(),
            KeyCode::Tab if self.focus == FocusPanel::Editor => {
                self.handle_editor_key(key)?;
            }
            KeyCode::Tab if !matches!(self.focus, FocusPanel::Terminal) => self.cycle_focus(),
            KeyCode::Esc if !matches!(self.focus, FocusPanel::Terminal) => {
                if self.explorer_filter.is_some() {
                    self.clear_explorer_filter();
                } else {
                    self.request_quit();
                }
            }
            KeyCode::Char('q') if self.focus != FocusPanel::Terminal => self.request_quit(),
            _ => match self.focus {
                FocusPanel::Explorer => self.handle_explorer_key(key)?,
                FocusPanel::Editor => self.handle_editor_key(key)?,
                FocusPanel::Terminal => self.handle_terminal_key(key)?,
            },
        }

        Ok(())
    }

    pub fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<()> {
        self.hit_regions.last_mouse_x = mouse.column;
        self.hit_regions.last_mouse_y = mouse.row;
        let target = self.hit_regions.target_at(mouse.column, mouse.row);
        self.hover = target.clone();

        match mouse.kind {
            MouseEventKind::Moved => {}
            MouseEventKind::Down(MouseButton::Left) => self.activate_target(target)?,
            MouseEventKind::Drag(MouseButton::Left) => {
                if target == HoverTarget::Editor {
                    self.focus = FocusPanel::Editor;
                    self.set_editor_cursor_from_mouse(true);
                }
            }
            MouseEventKind::Down(MouseButton::Middle) => {
                if let HoverTarget::Tab(index) | HoverTarget::TabClose(index) = target {
                    self.close_tab(index);
                } else if let HoverTarget::TerminalTab(index) | HoverTarget::TerminalTabClose(index) =
                    target
                    && let Err(error) = self.close_terminal(index)
                {
                    self.last_error = Some(error.to_string());
                }
            }
            MouseEventKind::ScrollUp => self.handle_scroll(target, -3, true)?,
            MouseEventKind::ScrollDown => self.handle_scroll(target, 3, false)?,
            MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight => {}
            MouseEventKind::Drag(_) | MouseEventKind::Up(_) => {}
            MouseEventKind::Down(_) => {}
        }

        Ok(())
    }

    fn activate_target(&mut self, target: HoverTarget) -> Result<()> {
        match target {
            HoverTarget::Explorer | HoverTarget::ExplorerRow(_) => {
                self.focus = FocusPanel::Explorer;
            }
            HoverTarget::Tab(_) | HoverTarget::TabClose(_) | HoverTarget::Editor => {
                self.focus = FocusPanel::Editor;
            }
            HoverTarget::TerminalTab(_)
            | HoverTarget::TerminalTabClose(_)
            | HoverTarget::TerminalNew => {
                self.focus = FocusPanel::Terminal;
            }
            HoverTarget::QuickRow(_) => {}
            HoverTarget::Terminal | HoverTarget::TerminalInput => {
                self.focus = FocusPanel::Terminal;
            }
            HoverTarget::None => {}
        }

        match target {
            HoverTarget::ExplorerRow(index) => {
                self.explorer.selected = index;
                self.open_or_toggle_selected()?;
            }
            HoverTarget::Tab(index) if index < self.tabs.len() => {
                self.active_tab = Some(index);
            }
            HoverTarget::TabClose(index) => self.close_tab(index),
            HoverTarget::TerminalTab(index) => self.select_terminal(index),
            HoverTarget::TerminalTabClose(index) => {
                if let Err(error) = self.close_terminal(index) {
                    self.last_error = Some(error.to_string());
                }
            }
            HoverTarget::TerminalNew => {
                if let Err(error) = self.new_terminal() {
                    self.last_error = Some(error.to_string());
                }
            }
            HoverTarget::QuickRow(index) => self.activate_quick_row(index),
            HoverTarget::Editor => {
                self.set_editor_cursor_from_mouse(false);
            }
            HoverTarget::Terminal | HoverTarget::TerminalInput => {
                self.send_terminal_mouse_click();
            }
            _ => {}
        }

        Ok(())
    }

    fn handle_explorer_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => self.move_explorer_selection(-1),
            KeyCode::Down => self.move_explorer_selection(1),
            KeyCode::PageUp => self.move_explorer_selection(-(self.explorer_height as isize)),
            KeyCode::PageDown => self.move_explorer_selection(self.explorer_height as isize),
            KeyCode::Enter | KeyCode::Right => self.open_or_toggle_selected()?,
            KeyCode::Left => self.collapse_selected(),
            KeyCode::Char('r') => self.refresh_explorer()?,
            KeyCode::Char('/') => self.start_explorer_filter_prompt(),
            KeyCode::Char('.') => self.toggle_hidden_files(),
            KeyCode::Char('i') => self.toggle_ignored_files(),
            KeyCode::Char('n') => self.start_prompt(PromptKind::NewFile, ""),
            KeyCode::Char('N') => self.start_prompt(PromptKind::NewDir, ""),
            KeyCode::Char('e') => self.prompt_rename(),
            KeyCode::Char('D') => self.prompt_delete(),
            KeyCode::Char('c') => self.copy_selected_path(),
            KeyCode::Char('x') => self.cut_selected_path(),
            KeyCode::Char('p') => self.paste_into_selected()?,
            KeyCode::Char('y') => self.duplicate_selected()?,
            KeyCode::Char('o') => self.reveal_active_file()?,
            _ => {}
        }

        Ok(())
    }

    fn handle_editor_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::SHIFT) && key.code == KeyCode::F(3) {
            self.find_next(false);
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::ALT) {
            match key.code {
                KeyCode::Up => self.move_active_line_up(),
                KeyCode::Down => self.move_active_line_down(),
                _ => {}
            }
            return Ok(());
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Left => {
                    self.move_editor_word(false, key.modifiers.contains(KeyModifiers::SHIFT))
                }
                KeyCode::Right => {
                    self.move_editor_word(true, key.modifiers.contains(KeyModifiers::SHIFT))
                }
                KeyCode::Char('a') | KeyCode::Char('A') => self.select_all_active_tab(),
                KeyCode::Char('c') | KeyCode::Char('C') => self.copy_editor_selection(),
                KeyCode::Char('x') | KeyCode::Char('X') => self.cut_editor_selection(),
                KeyCode::Char('v') | KeyCode::Char('V') => self.paste_editor_clipboard(),
                KeyCode::Char('s') => self.save_active_tab(),
                KeyCode::Char('f') => {
                    let initial = self.search_needle.clone().unwrap_or_default();
                    self.start_prompt(PromptKind::Search, &initial);
                }
                KeyCode::Char('h') => self.start_replace_prompt(false),
                KeyCode::Char('l') => self.start_prompt(PromptKind::GotoLine, ""),
                KeyCode::Char('/') => self.toggle_active_line_comment(),
                KeyCode::Char('d') => self.duplicate_active_line(),
                KeyCode::Char('w') => self.close_active_tab(),
                KeyCode::Char('z') => self.undo_active_tab(),
                KeyCode::Char('y') => self.redo_active_tab(),
                _ => {}
            }
            return Ok(());
        }

        let selecting = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Up => self.move_editor_cursor_with_selection(-1, 0, selecting),
            KeyCode::Down => self.move_editor_cursor_with_selection(1, 0, selecting),
            KeyCode::Left => self.move_editor_cursor_with_selection(0, -1, selecting),
            KeyCode::Right => self.move_editor_cursor_with_selection(0, 1, selecting),
            KeyCode::PageUp => self.scroll_editor(-(self.editor_height as isize)),
            KeyCode::PageDown => self.scroll_editor(self.editor_height as isize),
            KeyCode::Home => self.set_editor_cursor_col(0, selecting),
            KeyCode::End => self.set_editor_cursor_end(selecting),
            KeyCode::F(3) => self.find_next(true),
            KeyCode::Tab => self.indent_active_line(),
            KeyCode::BackTab => self.outdent_active_line(),
            KeyCode::Enter => self.edit_newline(),
            KeyCode::Backspace => self.edit_backspace(),
            KeyCode::Delete => self.edit_delete(),
            KeyCode::Char(c) => self.edit_insert(c),
            _ => {}
        }

        Ok(())
    }

    fn handle_terminal_key(&mut self, key: KeyEvent) -> Result<()> {
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            match key.code {
                KeyCode::PageUp => {
                    self.scroll_terminal(-(self.terminal_height.max(1) as isize));
                    return Ok(());
                }
                KeyCode::PageDown => {
                    self.scroll_terminal(self.terminal_height.max(1) as isize);
                    return Ok(());
                }
                _ => {}
            }
        }

        self.active_terminal_mut().shell.send_key(key)
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => self.finish_prompt()?,
            KeyCode::Backspace => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.pop();
                }
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(prompt) = &mut self.prompt {
                    prompt.input.push(c);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_quick_panel_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.quick_panel = None,
            KeyCode::Enter => self.activate_selected_quick_item(),
            KeyCode::Up => self.move_quick_selection(-1),
            KeyCode::Down => self.move_quick_selection(1),
            KeyCode::PageUp => self.move_quick_selection(-(self.quick_panel_height as isize)),
            KeyCode::PageDown => self.move_quick_selection(self.quick_panel_height as isize),
            KeyCode::Home => self.set_quick_selection(0),
            KeyCode::End => {
                if let Some(panel) = &self.quick_panel {
                    self.set_quick_selection(panel.items.len().saturating_sub(1));
                }
            }
            KeyCode::Backspace => {
                if let Some(panel) = &mut self.quick_panel {
                    panel.query.pop();
                }
                self.refresh_quick_panel()?;
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(panel) = &mut self.quick_panel {
                    panel.query.push(c);
                }
                self.refresh_quick_panel()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn cycle_focus(&mut self) {
        self.focus = match self.focus {
            FocusPanel::Explorer => FocusPanel::Editor,
            FocusPanel::Editor => FocusPanel::Terminal,
            FocusPanel::Terminal => FocusPanel::Explorer,
        };
    }

    pub fn handle_paste(&mut self, text: String) -> Result<()> {
        if let Some(panel) = &mut self.quick_panel {
            panel.query.push_str(&text.replace(['\r', '\n'], " "));
            return self.refresh_quick_panel();
        }

        if let Some(prompt) = &mut self.prompt {
            prompt.input.push_str(&text.replace(['\r', '\n'], " "));
            return Ok(());
        }

        match self.focus {
            FocusPanel::Editor => {
                if let Some(tab) = self.active_tab_mut() {
                    tab.insert_text(&text);
                    self.ensure_editor_cursor_visible();
                }
            }
            FocusPanel::Terminal => self.active_terminal_mut().shell.send_paste(&text)?,
            FocusPanel::Explorer => {}
        }
        Ok(())
    }

    pub fn take_clipboard_export(&mut self) -> Option<String> {
        self.pending_clipboard_export.take()
    }

    fn queue_clipboard_export(&mut self, text: &str) -> bool {
        if text.len() > MAX_OSC52_CLIPBOARD_BYTES {
            self.pending_clipboard_export = None;
            return false;
        }

        self.pending_clipboard_export = Some(text.to_owned());
        true
    }

    fn move_explorer_selection(&mut self, delta: isize) {
        let len = self.visible_nodes().len();
        if len == 0 {
            return;
        }

        let next = self.explorer.selected as isize + delta;
        self.explorer.selected = next.clamp(0, len.saturating_sub(1) as isize) as usize;
        self.ensure_explorer_selection_visible();
    }

    fn ensure_explorer_selection_visible(&mut self) {
        let height = self.explorer_height.max(1);
        if self.explorer.selected < self.explorer.scroll {
            self.explorer.scroll = self.explorer.selected;
        } else if self.explorer.selected >= self.explorer.scroll + height {
            self.explorer.scroll = self.explorer.selected.saturating_sub(height - 1);
        }
    }

    fn open_or_toggle_selected(&mut self) -> Result<()> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return Ok(());
        };

        if node.is_dir {
            if let Err(error) = self.explorer.toggle(&node.path) {
                self.last_error = Some(error.to_string());
            }
        } else {
            self.open_file(&node.path);
        }

        self.ensure_explorer_selection_visible();
        Ok(())
    }

    fn collapse_selected(&mut self) {
        if let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() {
            self.explorer.collapse(&node.path);
        }
    }

    fn open_file(&mut self, path: &Path) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active_tab = Some(index);
            self.focus = FocusPanel::Editor;
            return;
        }

        match EditorTab::open(path.to_path_buf()) {
            Ok(tab) => {
                self.tabs.push(tab);
                self.active_tab = Some(self.tabs.len() - 1);
                self.focus = FocusPanel::Editor;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    pub fn drain_terminal(&mut self) -> bool {
        let mut changed = false;
        let mut exited = Vec::new();
        for terminal in &mut self.terminals {
            changed |= terminal.shell.drain();
            if !terminal.exited && terminal.shell.child_exited() {
                terminal.exited = true;
                exited.push(terminal.title.clone());
            }
        }
        if !exited.is_empty() {
            self.message = if self.terminals.len() == 1 {
                Some("terminal shell exited".to_owned())
            } else {
                Some(format!("terminal exited: {}", exited.join(", ")))
            };
            changed = true;
        }
        self.active_terminal = self
            .active_terminal
            .min(self.terminals.len().saturating_sub(1));
        changed
    }

    pub fn active_tab(&self) -> Option<&EditorTab> {
        self.active_tab.and_then(|index| self.tabs.get(index))
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut EditorTab> {
        self.active_tab.and_then(|index| self.tabs.get_mut(index))
    }

    pub fn active_terminal(&self) -> &TerminalSession {
        &self.terminals[self.active_terminal]
    }

    pub fn active_terminal_mut(&mut self) -> &mut TerminalSession {
        &mut self.terminals[self.active_terminal]
    }

    fn handle_scroll(&mut self, target: HoverTarget, amount: isize, up: bool) -> Result<()> {
        if matches!(target, HoverTarget::Terminal | HoverTarget::TerminalInput)
            && self.send_terminal_mouse_wheel(up)?
        {
            return Ok(());
        }

        self.scroll_target(target, amount);
        Ok(())
    }

    fn scroll_target(&mut self, target: HoverTarget, amount: isize) {
        match target {
            HoverTarget::Explorer | HoverTarget::ExplorerRow(_) => self.scroll_explorer(amount),
            HoverTarget::Editor | HoverTarget::Tab(_) | HoverTarget::TabClose(_) => {
                self.scroll_editor(amount)
            }
            HoverTarget::QuickRow(_) => self.scroll_quick_panel(amount),
            HoverTarget::Terminal
            | HoverTarget::TerminalInput
            | HoverTarget::TerminalTab(_)
            | HoverTarget::TerminalTabClose(_)
            | HoverTarget::TerminalNew => self.scroll_terminal(amount),
            HoverTarget::None => match self.focus {
                FocusPanel::Explorer => self.scroll_explorer(amount),
                FocusPanel::Editor => self.scroll_editor(amount),
                FocusPanel::Terminal => self.scroll_terminal(amount),
            },
        }
    }

    fn scroll_explorer(&mut self, amount: isize) {
        let len = self.visible_nodes().len();
        let max_scroll = len.saturating_sub(self.explorer_height.max(1));
        self.explorer.scroll = add_signed(self.explorer.scroll, amount).min(max_scroll);
    }

    fn scroll_editor(&mut self, amount: isize) {
        let height = self.editor_height.max(1);
        if let Some(tab) = self.active_tab_mut() {
            let max_scroll = tab.lines.len().saturating_sub(height);
            tab.scroll = add_signed(tab.scroll, amount).min(max_scroll);
        }
    }

    fn scroll_terminal(&mut self, amount: isize) {
        self.active_terminal_mut().shell.scroll(amount);
    }

    fn scroll_quick_panel(&mut self, amount: isize) {
        let height = self.quick_panel_height.max(1);
        if let Some(panel) = &mut self.quick_panel {
            let max_scroll = panel.items.len().saturating_sub(height);
            panel.scroll = add_signed(panel.scroll, amount).min(max_scroll);
            panel.selected = panel.selected.clamp(
                panel.scroll,
                panel
                    .scroll
                    .saturating_add(height.saturating_sub(1))
                    .min(panel.items.len().saturating_sub(1)),
            );
        }
    }
}

fn add_signed(value: usize, amount: isize) -> usize {
    if amount.is_negative() {
        value.saturating_sub(amount.unsigned_abs())
    } else {
        value.saturating_add(amount as usize)
    }
}

fn focus_label(focus: FocusPanel) -> &'static str {
    match focus {
        FocusPanel::Explorer => "explorer",
        FocusPanel::Editor => "editor",
        FocusPanel::Terminal => "terminal",
    }
}

fn filtered_visible_nodes(
    nodes: Vec<VisibleNode>,
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
    filter: Option<&str>,
) -> Vec<VisibleNode> {
    let base_visible = nodes
        .into_iter()
        .filter(|node| node_passes_explorer_visibility(node, root, show_hidden, show_ignored))
        .collect::<Vec<_>>();

    let Some(filter) = filter.map(str::trim).filter(|filter| !filter.is_empty()) else {
        return base_visible;
    };
    let filter = filter.to_lowercase();
    let matched_paths = base_visible
        .iter()
        .filter(|node| explorer_filter_matches(node, root, &filter))
        .map(|node| node.path.clone())
        .collect::<Vec<_>>();

    if matched_paths.is_empty() {
        return base_visible
            .into_iter()
            .filter(|node| node.path == root)
            .collect();
    }

    base_visible
        .into_iter()
        .filter(|node| {
            node.path == root
                || matched_paths.iter().any(|matched_path| {
                    matched_path == &node.path || matched_path.starts_with(&node.path)
                })
        })
        .collect()
}

fn node_passes_explorer_visibility(
    node: &VisibleNode,
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
) -> bool {
    if node.path == root {
        return true;
    }
    if !show_hidden && path_has_hidden_component(root, &node.path) {
        return false;
    }
    if !show_ignored && path_has_generated_component(root, &node.path) {
        return false;
    }
    true
}

fn explorer_filter_matches(node: &VisibleNode, root: &Path, filter: &str) -> bool {
    node.name.to_lowercase().contains(filter)
        || relative_path(root, &node.path)
            .to_lowercase()
            .contains(filter)
}

#[derive(Debug, Clone, Copy)]
struct CommandSpec {
    label: &'static str,
    detail: &'static str,
    shortcut: &'static str,
    action: CommandAction,
}

fn command_catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            label: "Quick Open",
            detail: "Open a workspace file by fuzzy path",
            shortcut: "Ctrl-P",
            action: CommandAction::QuickOpen,
        },
        CommandSpec {
            label: "Search Workspace",
            detail: "Search text across workspace files",
            shortcut: "Ctrl-Shift-F",
            action: CommandAction::WorkspaceSearch,
        },
        CommandSpec {
            label: "Save File",
            detail: "Write the active editor tab to disk",
            shortcut: "Ctrl-S",
            action: CommandAction::SaveFile,
        },
        CommandSpec {
            label: "Save All",
            detail: "Write all dirty editor tabs to disk",
            shortcut: "",
            action: CommandAction::SaveAll,
        },
        CommandSpec {
            label: "Close Active Tab",
            detail: "Close the active tab when it has no unsaved edits",
            shortcut: "Ctrl-W",
            action: CommandAction::CloseActiveTab,
        },
        CommandSpec {
            label: "Close Saved Tabs",
            detail: "Close every clean tab and keep dirty tabs open",
            shortcut: "",
            action: CommandAction::CloseSavedTabs,
        },
        CommandSpec {
            label: "New File",
            detail: "Create a file under the selected explorer location",
            shortcut: "n",
            action: CommandAction::NewFile,
        },
        CommandSpec {
            label: "New Folder",
            detail: "Create a folder under the selected explorer location",
            shortcut: "N",
            action: CommandAction::NewFolder,
        },
        CommandSpec {
            label: "Rename Selected Explorer Item",
            detail: "Rename the selected file or folder",
            shortcut: "e",
            action: CommandAction::RenameSelected,
        },
        CommandSpec {
            label: "Delete Selected Explorer Item",
            detail: "Delete the selected file or folder after confirmation",
            shortcut: "D",
            action: CommandAction::DeleteSelected,
        },
        CommandSpec {
            label: "Refresh Explorer",
            detail: "Reload the workspace file tree from disk",
            shortcut: "r",
            action: CommandAction::RefreshExplorer,
        },
        CommandSpec {
            label: "Collapse Explorer Folders",
            detail: "Collapse all expanded explorer folders",
            shortcut: "",
            action: CommandAction::CollapseExplorer,
        },
        CommandSpec {
            label: "Reveal Active File in Explorer",
            detail: "Select the active editor file in the explorer tree",
            shortcut: "o",
            action: CommandAction::RevealActiveFile,
        },
        CommandSpec {
            label: "Copy Active File Path",
            detail: "Copy the active editor file absolute path to the terminal clipboard",
            shortcut: "",
            action: CommandAction::CopyActiveFilePath,
        },
        CommandSpec {
            label: "Copy Active File Relative Path",
            detail: "Copy the active editor file path relative to the workspace",
            shortcut: "",
            action: CommandAction::CopyActiveFileRelativePath,
        },
        CommandSpec {
            label: "Copy Selected Explorer Path",
            detail: "Copy the selected explorer item absolute path to the terminal clipboard",
            shortcut: "",
            action: CommandAction::CopySelectedExplorerPath,
        },
        CommandSpec {
            label: "Copy Selected Explorer Relative Path",
            detail: "Copy the selected explorer item path relative to the workspace",
            shortcut: "",
            action: CommandAction::CopySelectedExplorerRelativePath,
        },
        CommandSpec {
            label: "Filter Explorer",
            detail: "Filter the visible explorer tree by path text",
            shortcut: "/",
            action: CommandAction::FilterExplorer,
        },
        CommandSpec {
            label: "Clear Explorer Filter",
            detail: "Remove the active explorer tree filter",
            shortcut: "Esc",
            action: CommandAction::ClearExplorerFilter,
        },
        CommandSpec {
            label: "Toggle Hidden Files",
            detail: "Show or hide dot-prefixed files and folders in the explorer",
            shortcut: ".",
            action: CommandAction::ToggleHiddenFiles,
        },
        CommandSpec {
            label: "Toggle Generated Folders",
            detail: "Show or hide generated folders such as target, dist, node_modules, and build",
            shortcut: "i",
            action: CommandAction::ToggleIgnoredFiles,
        },
        CommandSpec {
            label: "Find in File",
            detail: "Search inside the active editor tab",
            shortcut: "Ctrl-F",
            action: CommandAction::FindInFile,
        },
        CommandSpec {
            label: "Replace in File",
            detail: "Replace the next/current match inside the active editor tab",
            shortcut: "Ctrl-H",
            action: CommandAction::ReplaceInFile,
        },
        CommandSpec {
            label: "Replace All in File",
            detail: "Replace every match inside the active editor tab",
            shortcut: "",
            action: CommandAction::ReplaceAllInFile,
        },
        CommandSpec {
            label: "Go to Line",
            detail: "Jump to a line or line:column in the active editor tab",
            shortcut: "Ctrl-L",
            action: CommandAction::GotoLine,
        },
        CommandSpec {
            label: "Duplicate Line",
            detail: "Duplicate the current editor line",
            shortcut: "Ctrl-D",
            action: CommandAction::DuplicateLine,
        },
        CommandSpec {
            label: "Delete Line",
            detail: "Delete the current editor line",
            shortcut: "",
            action: CommandAction::DeleteLine,
        },
        CommandSpec {
            label: "Move Line Up",
            detail: "Move the current editor line upward",
            shortcut: "Alt-Up",
            action: CommandAction::MoveLineUp,
        },
        CommandSpec {
            label: "Move Line Down",
            detail: "Move the current editor line downward",
            shortcut: "Alt-Down",
            action: CommandAction::MoveLineDown,
        },
        CommandSpec {
            label: "Toggle Line Comment",
            detail: "Comment or uncomment the current editor line",
            shortcut: "Ctrl-/",
            action: CommandAction::ToggleLineComment,
        },
        CommandSpec {
            label: "Indent Line",
            detail: "Indent the current editor line",
            shortcut: "Tab",
            action: CommandAction::IndentLine,
        },
        CommandSpec {
            label: "Outdent Line",
            detail: "Outdent the current editor line",
            shortcut: "Shift-Tab",
            action: CommandAction::OutdentLine,
        },
        CommandSpec {
            label: "Select All",
            detail: "Select the entire active editor buffer",
            shortcut: "Ctrl-A",
            action: CommandAction::SelectAll,
        },
        CommandSpec {
            label: "Copy Selection",
            detail: "Copy the current editor selection to the internal and terminal clipboard",
            shortcut: "Ctrl-C",
            action: CommandAction::CopySelection,
        },
        CommandSpec {
            label: "Cut Selection",
            detail: "Cut the current editor selection to the internal and terminal clipboard",
            shortcut: "Ctrl-X",
            action: CommandAction::CutSelection,
        },
        CommandSpec {
            label: "Paste Clipboard",
            detail: "Paste the internal editor clipboard at the cursor",
            shortcut: "Ctrl-V",
            action: CommandAction::PasteClipboard,
        },
        CommandSpec {
            label: "Focus Explorer",
            detail: "Move focus to the file explorer",
            shortcut: "",
            action: CommandAction::FocusExplorer,
        },
        CommandSpec {
            label: "Focus Editor",
            detail: "Move focus to the editor",
            shortcut: "",
            action: CommandAction::FocusEditor,
        },
        CommandSpec {
            label: "Focus Terminal",
            detail: "Move focus to the integrated terminal",
            shortcut: "",
            action: CommandAction::FocusTerminal,
        },
        CommandSpec {
            label: "Clear Terminal",
            detail: "Clear the terminal viewport and scrollback",
            shortcut: "",
            action: CommandAction::ClearTerminal,
        },
        CommandSpec {
            label: "Restart Terminal",
            detail: "Kill the current shell and start a fresh PTY shell",
            shortcut: "",
            action: CommandAction::RestartTerminal,
        },
        CommandSpec {
            label: "New Terminal",
            detail: "Create a new integrated PTY terminal session",
            shortcut: "F7",
            action: CommandAction::NewTerminal,
        },
        CommandSpec {
            label: "Close Terminal",
            detail: "Close the active integrated terminal session",
            shortcut: "F9",
            action: CommandAction::CloseTerminal,
        },
        CommandSpec {
            label: "Next Terminal",
            detail: "Switch to the next integrated terminal session",
            shortcut: "F8",
            action: CommandAction::NextTerminal,
        },
        CommandSpec {
            label: "Previous Terminal",
            detail: "Switch to the previous integrated terminal session",
            shortcut: "",
            action: CommandAction::PreviousTerminal,
        },
        CommandSpec {
            label: "Toggle Terminal Focus",
            detail: "Move focus in or out of the integrated terminal",
            shortcut: "F6 / Ctrl-`",
            action: CommandAction::ToggleTerminalFocus,
        },
        CommandSpec {
            label: "Toggle Terminal Maximize",
            detail: "Expand the integrated terminal to fill the main workspace area",
            shortcut: "F12 / Ctrl-J",
            action: CommandAction::ToggleTerminalMaximized,
        },
        CommandSpec {
            label: "Increase Terminal Height",
            detail: "Give the integrated terminal more rows",
            shortcut: "",
            action: CommandAction::IncreaseTerminalHeight,
        },
        CommandSpec {
            label: "Decrease Terminal Height",
            detail: "Give the editor more rows by shrinking the terminal",
            shortcut: "",
            action: CommandAction::DecreaseTerminalHeight,
        },
    ]
}

impl App {
    fn start_prompt(&mut self, kind: PromptKind, initial: &str) {
        self.prompt = Some(PromptState {
            kind,
            input: initial.to_owned(),
        });
    }

    fn start_replace_prompt(&mut self, all: bool) {
        let initial = self.search_needle.clone().unwrap_or_default();
        self.start_prompt(PromptKind::ReplaceFind { all }, &initial);
    }

    fn open_quick_panel(&mut self, kind: QuickPanelKind) -> Result<()> {
        self.quick_panel = Some(QuickPanel {
            kind,
            query: String::new(),
            items: Vec::new(),
            selected: 0,
            scroll: 0,
        });
        self.refresh_quick_panel()
    }

    fn refresh_quick_panel(&mut self) -> Result<()> {
        let Some(panel) = &self.quick_panel else {
            return Ok(());
        };
        let kind = panel.kind.clone();
        let query = panel.query.clone();
        let items = match kind {
            QuickPanelKind::OpenFile => self.quick_open_items(&query)?,
            QuickPanelKind::WorkspaceSearch => self.workspace_search_items(&query)?,
            QuickPanelKind::CommandPalette => self.command_palette_items(&query),
        };

        if let Some(panel) = &mut self.quick_panel {
            panel.items = items;
            panel.selected = panel.selected.min(panel.items.len().saturating_sub(1));
            panel.scroll = panel.scroll.min(panel.items.len().saturating_sub(1));
            self.ensure_quick_selection_visible();
        }
        Ok(())
    }

    fn quick_open_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let mut scored = Vec::new();
        for path in collect_workspace_files(&self.root, self.show_hidden, self.show_ignored)? {
            let relative = relative_path(&self.root, &path);
            if let Some(score) = fuzzy_score(&relative, query) {
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("[file]")
                    .to_owned();
                scored.push((
                    score,
                    relative.len(),
                    QuickItem {
                        label,
                        detail: relative,
                        path,
                        line: None,
                        col: None,
                        preview: None,
                        command: None,
                    },
                ));
            }
        }

        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        Ok(scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, _, item)| item)
            .collect())
    }

    fn workspace_search_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let needle = query.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }

        let needle_lower = needle.to_lowercase();
        let mut items = Vec::new();
        for path in collect_workspace_files(&self.root, self.show_hidden, self.show_ignored)? {
            if items.len() >= MAX_QUICK_ITEMS {
                break;
            }
            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if metadata.len() > MAX_FILE_SCAN_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }

            let text = String::from_utf8_lossy(&bytes);
            for (line_index, line) in text.lines().enumerate() {
                let line_lower = line.to_lowercase();
                let Some(byte_col) = line_lower.find(&needle_lower) else {
                    continue;
                };
                let relative = relative_path(&self.root, &path);
                let col = line[..byte_col].chars().count();
                items.push(QuickItem {
                    label: format!("{}:{}", relative, line_index + 1),
                    detail: relative,
                    path: path.clone(),
                    line: Some(line_index),
                    col: Some(col),
                    preview: Some(line.trim().to_owned()),
                    command: None,
                });
                if items.len() >= MAX_QUICK_ITEMS {
                    break;
                }
            }
        }

        Ok(items)
    }

    fn command_palette_items(&self, query: &str) -> Vec<QuickItem> {
        let mut scored = command_catalog()
            .into_iter()
            .filter_map(|command| {
                let haystack = format!("{} {}", command.label, command.detail);
                fuzzy_score(&haystack, query).map(|score| (score, command))
            })
            .collect::<Vec<_>>();

        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.label.cmp(b.1.label)));
        scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, command)| QuickItem {
                label: command.label.to_owned(),
                detail: command.detail.to_owned(),
                path: self.root.clone(),
                line: None,
                col: None,
                preview: (!command.shortcut.is_empty()).then(|| command.shortcut.to_owned()),
                command: Some(command.action),
            })
            .collect()
    }

    fn prompt_rename(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        self.start_prompt(PromptKind::Rename(node.path), &node.name);
    }

    fn prompt_delete(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        self.start_prompt(PromptKind::Delete(node.path), "");
    }

    fn copy_selected_path(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        self.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Copy,
            path: node.path.clone(),
        });
        self.message = Some(format!("copied {}", node.path.display()));
    }

    fn cut_selected_path(&mut self) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return;
        };
        if node.path == self.root {
            self.message = Some("refusing to cut workspace root".to_owned());
            return;
        }
        self.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Cut,
            path: node.path.clone(),
        });
        self.message = Some(format!("cut {}", node.path.display()));
    }

    fn paste_into_selected(&mut self) -> Result<()> {
        let Some(clipboard) = self.explorer_clipboard.clone() else {
            self.message = Some("clipboard empty".to_owned());
            return Ok(());
        };
        let source = clipboard.path;
        if !source.exists() {
            self.message = Some("clipboard source no longer exists".to_owned());
            self.explorer_clipboard = None;
            return Ok(());
        }

        let target_dir = self.selected_base_dir();
        let Some(name) = source.file_name() else {
            return Ok(());
        };
        let destination = unique_copy_path(&target_dir.join(name));
        if source.is_dir() && target_dir.starts_with(&source) {
            self.message = Some("cannot paste a folder into itself".to_owned());
            return Ok(());
        }

        let success_message;
        match clipboard.action {
            ClipboardAction::Copy => {
                copy_path_recursive(&source, &destination)?;
                success_message = format!("copied to {}", destination.display());
            }
            ClipboardAction::Cut => {
                fs::rename(&source, &destination)?;
                self.update_open_tabs_for_move(&source, &destination);
                self.explorer_clipboard = None;
                success_message = format!("moved to {}", destination.display());
            }
        }

        self.refresh_explorer()?;
        self.reveal_path(&destination)?;
        self.message = Some(success_message);
        Ok(())
    }

    fn duplicate_selected(&mut self) -> Result<()> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return Ok(());
        };
        if node.path == self.root {
            self.message = Some("refusing to duplicate workspace root".to_owned());
            return Ok(());
        }
        let Some(parent) = node.path.parent() else {
            return Ok(());
        };
        let destination = unique_copy_path(
            &parent.join(
                node.path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .map(copy_name)
                    .unwrap_or_else(|| "copy".to_owned()),
            ),
        );
        copy_path_recursive(&node.path, &destination)?;
        self.refresh_explorer()?;
        self.reveal_path(&destination)?;
        self.message = Some(format!("duplicated to {}", destination.display()));
        Ok(())
    }

    fn reveal_active_file(&mut self) -> Result<()> {
        let Some(path) = self.active_tab().map(|tab| tab.path.clone()) else {
            self.message = Some("no active file to reveal".to_owned());
            return Ok(());
        };
        self.reveal_path(&path)?;
        self.focus = FocusPanel::Explorer;
        self.message = Some(format!("revealed {}", path.display()));
        Ok(())
    }

    fn copy_active_file_path_to_clipboard(&mut self, relative: bool) {
        let Some(path) = self.active_tab().map(|tab| tab.path.clone()) else {
            self.message = Some("no active file path to copy".to_owned());
            return;
        };
        self.copy_path_to_clipboard(&path, relative, "active file");
    }

    fn copy_selected_explorer_path_to_clipboard(&mut self, relative: bool) {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            self.message = Some("no explorer path to copy".to_owned());
            return;
        };
        self.copy_path_to_clipboard(&node.path, relative, "explorer item");
    }

    fn copy_path_to_clipboard(&mut self, path: &Path, relative: bool, label: &str) {
        let text = if relative {
            relative_path(&self.root, path)
        } else {
            path.to_string_lossy().into_owned()
        };
        let display_kind = if relative { "relative path" } else { "path" };
        if self.queue_clipboard_export(&text) {
            self.message = Some(format!("copied {label} {display_kind}: {text}"));
        } else {
            self.message = Some(format!(
                "{label} {display_kind} too large for terminal clipboard"
            ));
        }
    }

    fn finish_prompt(&mut self) -> Result<()> {
        let Some(prompt) = self.prompt.take() else {
            return Ok(());
        };
        match prompt.kind {
            PromptKind::NewFile => self.create_file_from_prompt(prompt.input)?,
            PromptKind::NewDir => self.create_dir_from_prompt(prompt.input)?,
            PromptKind::Rename(path) => self.rename_from_prompt(path, prompt.input)?,
            PromptKind::Delete(path) => {
                if prompt.input == "yes" {
                    self.delete_path(path)?;
                } else {
                    self.message = Some("delete cancelled".to_owned());
                }
            }
            PromptKind::ExplorerFilter => self.set_explorer_filter(prompt.input),
            PromptKind::Search => self.search_active(prompt.input),
            PromptKind::ReplaceFind { all } => self.replace_find_from_prompt(prompt.input, all),
            PromptKind::ReplaceWith { needle, all } => {
                if all {
                    self.replace_all_active_matches(needle, prompt.input);
                } else {
                    self.replace_next_active_match(needle, prompt.input);
                }
            }
            PromptKind::GotoLine => self.goto_line_from_prompt(prompt.input),
            PromptKind::QuitDirty => {
                if prompt.input == "quit" {
                    self.should_quit = true;
                } else {
                    self.message = Some("quit cancelled".to_owned());
                }
            }
        }
        Ok(())
    }

    fn selected_base_dir(&self) -> PathBuf {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return self.root.clone();
        };
        if node.is_dir {
            node.path
        } else {
            node.path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| self.root.clone())
        }
    }

    fn start_explorer_filter_prompt(&mut self) {
        let initial = self.explorer_filter.clone().unwrap_or_default();
        self.start_prompt(PromptKind::ExplorerFilter, &initial);
    }

    fn set_explorer_filter(&mut self, input: String) {
        let filter = input.trim();
        let selected_path = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.explorer_filter = (!filter.is_empty()).then(|| filter.to_owned());
        self.restore_explorer_selection(selected_path);
        self.message = match &self.explorer_filter {
            Some(filter) => Some(format!("explorer filter: {filter}")),
            None => Some("explorer filter cleared".to_owned()),
        };
    }

    fn clear_explorer_filter(&mut self) {
        self.explorer_filter = None;
        self.restore_explorer_selection(None);
        self.message = Some("explorer filter cleared".to_owned());
    }

    fn toggle_hidden_files(&mut self) {
        let selected_path = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.show_hidden = !self.show_hidden;
        self.restore_explorer_selection(selected_path);
        self.message = Some(format!(
            "{} hidden files",
            if self.show_hidden {
                "showing"
            } else {
                "hiding"
            }
        ));
    }

    fn toggle_ignored_files(&mut self) {
        let selected_path = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.show_ignored = !self.show_ignored;
        self.restore_explorer_selection(selected_path);
        self.message = Some(format!(
            "{} generated/ignored folders",
            if self.show_ignored {
                "showing"
            } else {
                "hiding"
            }
        ));
    }

    fn restore_explorer_selection(&mut self, selected_path: Option<PathBuf>) {
        let nodes = self.visible_nodes();
        if nodes.is_empty() {
            self.explorer.selected = 0;
            self.explorer.scroll = 0;
            return;
        }

        if let Some(path) = selected_path
            && let Some(index) = nodes.iter().position(|node| node.path == path)
        {
            self.explorer.selected = index;
            self.ensure_explorer_selection_visible();
            return;
        }

        self.explorer.selected = self.explorer.selected.min(nodes.len().saturating_sub(1));
        self.ensure_explorer_selection_visible();
    }

    fn create_file_from_prompt(&mut self, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let path = self.selected_base_dir().join(name);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(&path, "")?;
        }
        self.refresh_explorer()?;
        self.open_file(&path);
        self.message = Some(format!("created {}", path.display()));
        Ok(())
    }

    fn create_dir_from_prompt(&mut self, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let path = self.selected_base_dir().join(name);
        fs::create_dir_all(&path)?;
        self.refresh_explorer()?;
        self.message = Some(format!("created {}", path.display()));
        Ok(())
    }

    fn rename_from_prompt(&mut self, path: PathBuf, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let new_path = path
            .parent()
            .map(|parent| parent.join(name))
            .unwrap_or_else(|| self.root.join(name));
        fs::rename(&path, &new_path)?;
        self.update_open_tabs_for_move(&path, &new_path);
        self.refresh_explorer()?;
        self.reveal_path(&new_path)?;
        self.message = Some(format!("renamed to {}", new_path.display()));
        Ok(())
    }

    fn delete_path(&mut self, path: PathBuf) -> Result<()> {
        if path == self.root {
            self.message = Some("refusing to delete workspace root".to_owned());
            return Ok(());
        }
        if path.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
        self.tabs.retain(|tab| !tab.path.starts_with(&path));
        if self
            .explorer_clipboard
            .as_ref()
            .is_some_and(|clipboard| clipboard.path.starts_with(&path))
        {
            self.explorer_clipboard = None;
        }
        if self.tabs.is_empty() {
            self.active_tab = None;
        } else {
            self.active_tab = Some(self.active_tab.unwrap_or(0).min(self.tabs.len() - 1));
        }
        self.refresh_explorer()?;
        self.message = Some(format!("deleted {}", path.display()));
        Ok(())
    }

    fn update_open_tabs_for_move(&mut self, old_path: &Path, new_path: &Path) {
        for tab in &mut self.tabs {
            if let Ok(relative) = tab.path.strip_prefix(old_path) {
                tab.path = new_path.join(relative);
                tab.title = tab
                    .path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .unwrap_or("[file]")
                    .to_owned();
            }
        }
    }

    fn reveal_path(&mut self, path: &Path) -> Result<()> {
        self.explorer.reveal(path)?;
        self.ensure_explorer_selection_visible();
        Ok(())
    }

    fn refresh_explorer(&mut self) -> Result<()> {
        let selected = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.explorer.refresh()?;
        if let Some(path) = selected
            && let Some(index) = self
                .visible_nodes()
                .iter()
                .position(|node| node.path == path)
        {
            self.explorer.selected = index;
        }
        self.ensure_explorer_selection_visible();
        self.message = Some("explorer refreshed".to_owned());
        Ok(())
    }

    fn collapse_explorer(&mut self) {
        self.explorer.collapse_all();
        self.explorer.selected = 0;
        self.explorer.scroll = 0;
        self.message = Some("explorer collapsed".to_owned());
    }

    fn save_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            match tab.save() {
                Ok(()) => self.message = Some(format!("saved {}", tab.path.display())),
                Err(error) => self.last_error = Some(error.to_string()),
            }
        }
    }

    fn save_all_tabs(&mut self) {
        let mut saved = 0usize;
        for tab in &mut self.tabs {
            if !tab.dirty {
                continue;
            }
            match tab.save() {
                Ok(()) => saved += 1,
                Err(error) => {
                    self.last_error = Some(error.to_string());
                    return;
                }
            }
        }
        self.message = Some(format!("saved {saved} dirty tab(s)"));
    }

    fn search_active(&mut self, needle: String) {
        let needle = needle.trim().to_owned();
        if needle.is_empty() {
            return;
        }
        self.search_needle = Some(needle);
        self.find_next(true);
    }

    fn replace_find_from_prompt(&mut self, needle: String, all: bool) {
        let needle = needle.trim().to_owned();
        if needle.is_empty() {
            self.message = Some("replace requires a search string".to_owned());
            return;
        }

        self.search_needle = Some(needle.clone());
        self.start_prompt(PromptKind::ReplaceWith { needle, all }, "");
    }

    fn replace_next_active_match(&mut self, needle: String, replacement: String) {
        if needle.is_empty() {
            self.message = Some("replace requires a search string".to_owned());
            return;
        }

        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let found = if match_at_cursor(tab, &needle) {
            Some((tab.cursor_line, tab.cursor_col))
        } else {
            find_forward_including(tab, &needle)
        };

        if let Some((line, col)) = found {
            if tab.replace_match_at(line, col, &needle, &replacement) {
                self.search_needle = Some(needle.clone());
                self.ensure_editor_cursor_visible();
                self.message = Some(format!("replaced next '{needle}'"));
            }
        } else {
            self.message = Some(format!("not found: {needle}"));
        }
    }

    fn replace_all_active_matches(&mut self, needle: String, replacement: String) {
        if needle.is_empty() {
            self.message = Some("replace all requires a search string".to_owned());
            return;
        }

        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let count = tab.replace_all_matches(&needle, &replacement);
        self.search_needle = Some(needle.clone());
        if count == 0 {
            self.message = Some(format!("not found: {needle}"));
        } else {
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("replaced {count} match(es) for '{needle}'"));
        }
    }

    pub fn active_search_match_count(&self) -> Option<usize> {
        let needle = self.search_needle.as_ref()?.trim();
        if needle.is_empty() {
            return None;
        }
        self.active_tab()
            .map(|tab| count_tab_matches(tab, needle))
            .filter(|count| *count > 0)
    }

    fn find_next(&mut self, forward: bool) {
        let Some(needle) = self.search_needle.clone() else {
            self.message = Some("no active search".to_owned());
            return;
        };
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let found = if forward {
            find_forward(tab, &needle)
        } else {
            find_backward(tab, &needle)
        };
        if let Some((line, col)) = found {
            tab.set_cursor(line, col);
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("found '{needle}'"));
        } else {
            self.message = Some(format!("not found: {needle}"));
        }
    }

    fn edit_insert(&mut self, c: char) {
        if let Some(tab) = self.active_tab_mut() {
            tab.insert_char(c);
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_newline(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.newline();
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_backspace(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.backspace();
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_delete(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.delete();
            self.ensure_editor_cursor_visible();
        }
    }

    fn indent_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.indent_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("indented line".to_owned());
        }
    }

    fn outdent_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.outdent_line() {
                self.ensure_editor_cursor_visible();
                self.message = Some("outdented line".to_owned());
            } else {
                self.message = Some("line is not indented".to_owned());
            }
        }
    }

    fn duplicate_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.duplicate_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("duplicated line".to_owned());
        }
    }

    fn delete_active_line(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.delete_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("deleted line".to_owned());
        }
    }

    fn move_active_line_up(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.move_line_up() {
                self.ensure_editor_cursor_visible();
                self.message = Some("moved line up".to_owned());
            } else {
                self.message = Some("line already at top".to_owned());
            }
        }
    }

    fn move_active_line_down(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.move_line_down() {
                self.ensure_editor_cursor_visible();
                self.message = Some("moved line down".to_owned());
            } else {
                self.message = Some("line already at bottom".to_owned());
            }
        }
    }

    fn toggle_active_line_comment(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.toggle_line_comment() {
                self.ensure_editor_cursor_visible();
                self.message = Some("toggled line comment".to_owned());
            } else {
                self.message = Some("no line comment token for file type".to_owned());
            }
        }
    }

    fn select_all_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.select_all();
            self.ensure_editor_cursor_visible();
            self.message = Some("selected all".to_owned());
        }
    }

    fn copy_editor_selection(&mut self) {
        let Some(text) = self.active_tab().and_then(EditorTab::selected_text) else {
            self.message = Some("no editor selection to copy".to_owned());
            return;
        };
        let count = text.chars().count();
        self.editor_clipboard = Some(text.clone());
        if self.queue_clipboard_export(&text) {
            self.message = Some(format!("copied {count} char(s) to clipboard"));
        } else {
            self.message = Some(format!(
                "copied {count} char(s) internally; selection too large for terminal clipboard"
            ));
        }
    }

    fn cut_editor_selection(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let Some(text) = tab.delete_selection() else {
            self.message = Some("no editor selection to cut".to_owned());
            return;
        };
        let count = text.chars().count();
        self.editor_clipboard = Some(text.clone());
        self.ensure_editor_cursor_visible();
        if self.queue_clipboard_export(&text) {
            self.message = Some(format!("cut {count} char(s) to clipboard"));
        } else {
            self.message = Some(format!(
                "cut {count} char(s) internally; selection too large for terminal clipboard"
            ));
        }
    }

    fn paste_editor_clipboard(&mut self) {
        let Some(text) = self.editor_clipboard.clone() else {
            self.message = Some("editor clipboard empty".to_owned());
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            tab.insert_text(&text);
            self.ensure_editor_cursor_visible();
            self.message = Some("pasted editor clipboard".to_owned());
        }
    }

    fn move_editor_cursor_with_selection(
        &mut self,
        line_delta: isize,
        col_delta: isize,
        selecting: bool,
    ) {
        if let Some(tab) = self.active_tab_mut() {
            tab.move_cursor_with_selection(line_delta, col_delta, selecting);
            self.ensure_editor_cursor_visible();
        }
    }

    fn set_editor_cursor_col(&mut self, col: usize, selecting: bool) {
        if let Some(tab) = self.active_tab_mut() {
            if selecting {
                tab.set_cursor_selecting(tab.cursor_line, col);
            } else {
                tab.set_cursor(tab.cursor_line, col);
            }
            self.ensure_editor_cursor_visible();
        }
    }

    fn set_editor_cursor_end(&mut self, selecting: bool) {
        if let Some(tab) = self.active_tab_mut() {
            let col = tab.lines[tab.cursor_line].chars().count();
            if selecting {
                tab.set_cursor_selecting(tab.cursor_line, col);
            } else {
                tab.set_cursor(tab.cursor_line, col);
            }
            self.ensure_editor_cursor_visible();
        }
    }

    fn move_editor_word(&mut self, forward: bool, selecting: bool) {
        if let Some(tab) = self.active_tab_mut() {
            tab.move_word(forward, selecting);
            self.ensure_editor_cursor_visible();
        }
    }

    fn ensure_editor_cursor_visible(&mut self) {
        let height = self.editor_height.max(1);
        if let Some(tab) = self.active_tab_mut() {
            if tab.cursor_line < tab.scroll {
                tab.scroll = tab.cursor_line;
            } else if tab.cursor_line >= tab.scroll + height {
                tab.scroll = tab.cursor_line.saturating_sub(height - 1);
            }
        }
    }

    fn set_editor_cursor_from_mouse(&mut self, selecting: bool) {
        let Some(body) = self.hit_regions.editor_body else {
            return;
        };
        let HoverTarget::Editor = self.hover else {
            return;
        };
        let mouse_x = self.hit_regions.last_mouse_x;
        let mouse_y = self.hit_regions.last_mouse_y;
        if let Some(tab) = self.active_tab_mut() {
            let line_number_width = tab.lines.len().max(1).to_string().len().max(3) + 2;
            let x = body.x;
            let y = body.y;
            let col = mouse_x
                .saturating_sub(x)
                .saturating_sub(line_number_width as u16) as usize;
            let line = tab.scroll + mouse_y.saturating_sub(y) as usize;
            if selecting {
                tab.set_cursor_selecting(line, col);
            } else {
                tab.set_cursor(line, col);
            }
            self.ensure_editor_cursor_visible();
        }
    }

    fn move_quick_selection(&mut self, delta: isize) {
        let Some(panel) = &mut self.quick_panel else {
            return;
        };
        if panel.items.is_empty() {
            return;
        }
        panel.selected = add_signed(panel.selected, delta).min(panel.items.len() - 1);
        self.ensure_quick_selection_visible();
    }

    fn set_quick_selection(&mut self, index: usize) {
        let Some(panel) = &mut self.quick_panel else {
            return;
        };
        if panel.items.is_empty() {
            return;
        }
        panel.selected = index.min(panel.items.len() - 1);
        self.ensure_quick_selection_visible();
    }

    fn ensure_quick_selection_visible(&mut self) {
        let height = self.quick_panel_height.max(1);
        let Some(panel) = &mut self.quick_panel else {
            return;
        };
        if panel.selected < panel.scroll {
            panel.scroll = panel.selected;
        } else if panel.selected >= panel.scroll + height {
            panel.scroll = panel.selected.saturating_sub(height - 1);
        }
    }

    fn activate_quick_row(&mut self, index: usize) {
        if let Some(panel) = &mut self.quick_panel {
            panel.selected = index.min(panel.items.len().saturating_sub(1));
        }
        self.activate_selected_quick_item();
    }

    fn activate_selected_quick_item(&mut self) {
        let Some(item) = self
            .quick_panel
            .as_ref()
            .and_then(|panel| panel.items.get(panel.selected))
            .cloned()
        else {
            return;
        };

        if let Some(command) = item.command {
            self.quick_panel = None;
            if let Err(error) = self.run_command(command) {
                self.last_error = Some(error.to_string());
            }
            return;
        }

        let search_query = self.quick_panel.as_ref().and_then(|panel| {
            (panel.kind == QuickPanelKind::WorkspaceSearch).then(|| panel.query.clone())
        });
        self.quick_panel = None;
        self.open_file(&item.path);
        if let Some(line) = item.line
            && let Some(tab) = self.active_tab_mut()
        {
            tab.set_cursor(line, item.col.unwrap_or(0));
            self.ensure_editor_cursor_visible();
        }
        if let Some(query) = search_query {
            self.search_needle = Some(query);
        }
        self.message = Some(format!("opened {}", item.path.display()));
    }

    fn run_command(&mut self, command: CommandAction) -> Result<()> {
        match command {
            CommandAction::QuickOpen => self.open_quick_panel(QuickPanelKind::OpenFile)?,
            CommandAction::WorkspaceSearch => {
                self.open_quick_panel(QuickPanelKind::WorkspaceSearch)?
            }
            CommandAction::SaveFile => self.save_active_tab(),
            CommandAction::SaveAll => self.save_all_tabs(),
            CommandAction::CloseActiveTab => self.close_active_tab(),
            CommandAction::CloseSavedTabs => self.close_saved_tabs(),
            CommandAction::NewFile => self.start_prompt(PromptKind::NewFile, ""),
            CommandAction::NewFolder => self.start_prompt(PromptKind::NewDir, ""),
            CommandAction::RenameSelected => self.prompt_rename(),
            CommandAction::DeleteSelected => self.prompt_delete(),
            CommandAction::RefreshExplorer => self.refresh_explorer()?,
            CommandAction::CollapseExplorer => self.collapse_explorer(),
            CommandAction::RevealActiveFile => self.reveal_active_file()?,
            CommandAction::CopyActiveFilePath => self.copy_active_file_path_to_clipboard(false),
            CommandAction::CopyActiveFileRelativePath => {
                self.copy_active_file_path_to_clipboard(true)
            }
            CommandAction::CopySelectedExplorerPath => {
                self.copy_selected_explorer_path_to_clipboard(false)
            }
            CommandAction::CopySelectedExplorerRelativePath => {
                self.copy_selected_explorer_path_to_clipboard(true)
            }
            CommandAction::FilterExplorer => self.start_explorer_filter_prompt(),
            CommandAction::ClearExplorerFilter => self.clear_explorer_filter(),
            CommandAction::ToggleHiddenFiles => self.toggle_hidden_files(),
            CommandAction::ToggleIgnoredFiles => self.toggle_ignored_files(),
            CommandAction::FindInFile => {
                let initial = self.search_needle.clone().unwrap_or_default();
                self.start_prompt(PromptKind::Search, &initial);
            }
            CommandAction::ReplaceInFile => self.start_replace_prompt(false),
            CommandAction::ReplaceAllInFile => self.start_replace_prompt(true),
            CommandAction::GotoLine => self.start_prompt(PromptKind::GotoLine, ""),
            CommandAction::DuplicateLine => self.duplicate_active_line(),
            CommandAction::DeleteLine => self.delete_active_line(),
            CommandAction::MoveLineUp => self.move_active_line_up(),
            CommandAction::MoveLineDown => self.move_active_line_down(),
            CommandAction::ToggleLineComment => self.toggle_active_line_comment(),
            CommandAction::IndentLine => self.indent_active_line(),
            CommandAction::OutdentLine => self.outdent_active_line(),
            CommandAction::SelectAll => self.select_all_active_tab(),
            CommandAction::CopySelection => self.copy_editor_selection(),
            CommandAction::CutSelection => self.cut_editor_selection(),
            CommandAction::PasteClipboard => self.paste_editor_clipboard(),
            CommandAction::FocusExplorer => self.focus = FocusPanel::Explorer,
            CommandAction::FocusEditor => self.focus = FocusPanel::Editor,
            CommandAction::FocusTerminal => self.focus = FocusPanel::Terminal,
            CommandAction::ClearTerminal => {
                self.active_terminal_mut().shell.clear();
                self.message = Some("terminal cleared".to_owned());
            }
            CommandAction::RestartTerminal => self.restart_terminal()?,
            CommandAction::NewTerminal => self.new_terminal()?,
            CommandAction::CloseTerminal => self.close_active_terminal()?,
            CommandAction::NextTerminal => self.next_terminal(),
            CommandAction::PreviousTerminal => self.previous_terminal(),
            CommandAction::ToggleTerminalFocus => self.toggle_terminal_focus(),
            CommandAction::ToggleTerminalMaximized => self.toggle_terminal_maximized(),
            CommandAction::IncreaseTerminalHeight => self.resize_terminal_panel(2),
            CommandAction::DecreaseTerminalHeight => self.resize_terminal_panel(-2),
        }
        Ok(())
    }

    fn send_terminal_mouse_click(&mut self) {
        let Some(body) = self.hit_regions.terminal_body else {
            return;
        };
        let row = self.hit_regions.last_mouse_y.saturating_sub(body.y);
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x);
        match self.active_terminal_mut().shell.send_mouse_click(row, col) {
            Ok(true) => {}
            Ok(false) => {
                let _ = self.open_terminal_reference(row, col);
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn send_terminal_mouse_wheel(&mut self, up: bool) -> Result<bool> {
        let Some(body) = self.hit_regions.terminal_body else {
            return Ok(false);
        };
        let row = self.hit_regions.last_mouse_y.saturating_sub(body.y);
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x);
        self.active_terminal_mut()
            .shell
            .send_mouse_wheel(row, col, up)
    }

    fn open_terminal_reference(&mut self, row: u16, col: u16) -> bool {
        let Some(line) = self.active_terminal().shell.row_text(row) else {
            return false;
        };
        let Some(reference) = terminal_file_reference_at(&line, col as usize, &self.root) else {
            return false;
        };

        self.open_file(&reference.path);
        if let Some(line) = reference.line
            && let Some(tab) = self.active_tab_mut()
        {
            tab.set_cursor(line, reference.col.unwrap_or(0));
            self.ensure_editor_cursor_visible();
        }
        self.message = Some(format!("opened {}", reference.path.display()));
        true
    }

    fn next_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let next = self
            .active_tab
            .map_or(0, |index| (index + 1) % self.tabs.len());
        self.active_tab = Some(next);
        self.focus = FocusPanel::Editor;
    }

    fn previous_tab(&mut self) {
        if self.tabs.is_empty() {
            return;
        }
        let previous = self
            .active_tab
            .map_or(0, |index| (index + self.tabs.len() - 1) % self.tabs.len());
        self.active_tab = Some(previous);
        self.focus = FocusPanel::Editor;
    }

    fn close_active_tab(&mut self) {
        if let Some(index) = self.active_tab {
            self.close_tab(index);
        }
    }

    fn close_saved_tabs(&mut self) {
        let before = self.tabs.len();
        self.tabs.retain(|tab| tab.dirty);
        let closed = before.saturating_sub(self.tabs.len());
        self.active_tab = if self.tabs.is_empty() { None } else { Some(0) };
        self.message = if self.tabs.is_empty() {
            Some(format!("closed {closed} saved tab(s)"))
        } else {
            Some(format!(
                "closed {closed} saved tab(s); {} dirty tab(s) remain",
                self.tabs.len()
            ))
        };
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs[index].dirty {
            self.message = Some(format!("save {} before closing", self.tabs[index].title));
            return;
        }

        let title = self.tabs[index].title.clone();
        self.tabs.remove(index);
        self.active_tab = if self.tabs.is_empty() {
            None
        } else {
            Some(index.saturating_sub(1).min(self.tabs.len() - 1))
        };
        self.message = Some(format!("closed {title}"));
    }

    fn undo_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.undo() {
                self.ensure_editor_cursor_visible();
                self.message = Some("undo".to_owned());
            } else {
                self.message = Some("nothing to undo".to_owned());
            }
        }
    }

    fn redo_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            if tab.redo() {
                self.ensure_editor_cursor_visible();
                self.message = Some("redo".to_owned());
            } else {
                self.message = Some("nothing to redo".to_owned());
            }
        }
    }

    fn request_quit(&mut self) {
        if self.tabs.iter().any(|tab| tab.dirty) {
            self.start_prompt(PromptKind::QuitDirty, "");
        } else {
            self.should_quit = true;
        }
    }

    fn goto_line_from_prompt(&mut self, input: String) {
        let Some((line, col)) = parse_line_col(&input) else {
            self.message = Some("enter a line number, optionally line:column".to_owned());
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            tab.set_cursor(line, col);
            self.ensure_editor_cursor_visible();
            self.focus = FocusPanel::Editor;
            self.message = Some(format!("jumped to line {}", line + 1));
        }
    }

    fn restart_terminal(&mut self) -> Result<()> {
        let title = self.active_terminal().title.clone();
        let id = self.active_terminal().id;
        let _ = self.active_terminal_mut().shell.kill();
        self.terminals[self.active_terminal] = TerminalSession {
            id,
            title: title.clone(),
            shell: ShellPanel::new(self.root.clone())?,
            exited: false,
        };
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal restarted: {title}"));
        Ok(())
    }

    fn new_terminal(&mut self) -> Result<()> {
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        let terminal = TerminalSession::new(id, self.root.clone())?;
        let title = terminal.title.clone();
        self.terminals.push(terminal);
        self.active_terminal = self.terminals.len() - 1;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("new terminal: {title}"));
        Ok(())
    }

    fn select_terminal(&mut self, index: usize) {
        if index >= self.terminals.len() {
            return;
        }
        self.active_terminal = index;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal: {}", self.active_terminal().title));
    }

    fn close_active_terminal(&mut self) -> Result<()> {
        self.close_terminal(self.active_terminal)
    }

    fn close_terminal(&mut self, index: usize) -> Result<()> {
        if index >= self.terminals.len() {
            return Ok(());
        }
        if self.terminals.len() == 1 {
            self.restart_terminal()?;
            self.message = Some("last terminal restarted instead of closed".to_owned());
            return Ok(());
        }

        let mut terminal = self.terminals.remove(index);
        let title = terminal.title.clone();
        let _ = terminal.shell.kill();
        self.active_terminal = if self.active_terminal > index {
            self.active_terminal - 1
        } else {
            self.active_terminal
                .min(self.terminals.len().saturating_sub(1))
        };
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("closed terminal: {title}"));
        Ok(())
    }

    fn next_terminal(&mut self) {
        if self.terminals.is_empty() {
            return;
        }
        self.active_terminal = (self.active_terminal + 1) % self.terminals.len();
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal: {}", self.active_terminal().title));
    }

    fn previous_terminal(&mut self) {
        if self.terminals.is_empty() {
            return;
        }
        self.active_terminal =
            (self.active_terminal + self.terminals.len() - 1) % self.terminals.len();
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal: {}", self.active_terminal().title));
    }

    #[cfg(test)]
    fn kill_all_terminals(&mut self) {
        for terminal in &mut self.terminals {
            let _ = terminal.shell.kill();
        }
    }

    fn toggle_terminal_focus(&mut self) {
        self.focus = if self.focus == FocusPanel::Terminal {
            if self.active_tab.is_some() {
                FocusPanel::Editor
            } else {
                FocusPanel::Explorer
            }
        } else {
            FocusPanel::Terminal
        };
        self.message = Some(format!("focus: {}", focus_label(self.focus)));
    }

    fn toggle_terminal_maximized(&mut self) {
        self.terminal_maximized = !self.terminal_maximized;
        self.focus = FocusPanel::Terminal;
        self.message = Some(if self.terminal_maximized {
            "terminal maximized".to_owned()
        } else {
            "terminal restored".to_owned()
        });
    }

    fn resize_terminal_panel(&mut self, delta: isize) {
        self.terminal_maximized = false;
        self.terminal_rows = add_signed(self.terminal_rows as usize, delta)
            .clamp(4, 24)
            .try_into()
            .unwrap_or(10);
        self.message = Some(format!("terminal height: {} rows", self.terminal_rows));
    }
}

fn byte_index_for_char(s: &str, char_index: usize) -> usize {
    s.char_indices()
        .nth(char_index)
        .map(|(index, _)| index)
        .unwrap_or(s.len())
}

fn take_chars_owned(s: &str, count: usize) -> String {
    s.chars().take(count).collect()
}

fn skip_chars_owned(s: &str, count: usize) -> String {
    s.chars().skip(count).collect()
}

fn slice_chars(s: &str, start: usize, end: usize) -> String {
    s.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn find_forward(tab: &EditorTab, needle: &str) -> Option<(usize, usize)> {
    let start_line = tab.cursor_line;
    let start_col = tab.cursor_col.saturating_add(1);
    for line_index in start_line..tab.lines.len() {
        let col = if line_index == start_line {
            start_col
        } else {
            0
        };
        if let Some(found) = line_find_from(&tab.lines[line_index], needle, col) {
            return Some((line_index, found));
        }
    }

    for line_index in 0..=start_line.min(tab.lines.len().saturating_sub(1)) {
        if let Some(found) = line_find_from(&tab.lines[line_index], needle, 0) {
            return Some((line_index, found));
        }
    }

    None
}

fn find_forward_including(tab: &EditorTab, needle: &str) -> Option<(usize, usize)> {
    let start_line = tab.cursor_line;
    let start_col = tab.cursor_col;
    for line_index in start_line..tab.lines.len() {
        let col = if line_index == start_line {
            start_col
        } else {
            0
        };
        if let Some(found) = line_find_from(&tab.lines[line_index], needle, col) {
            return Some((line_index, found));
        }
    }

    for line_index in 0..=start_line.min(tab.lines.len().saturating_sub(1)) {
        if let Some(found) = line_find_from(&tab.lines[line_index], needle, 0) {
            return Some((line_index, found));
        }
    }

    None
}

fn find_backward(tab: &EditorTab, needle: &str) -> Option<(usize, usize)> {
    let start_line = tab.cursor_line;
    for line_index in (0..=start_line).rev() {
        let col = if line_index == start_line {
            tab.cursor_col
        } else {
            tab.lines[line_index].chars().count()
        };
        if let Some(found) = line_rfind_before(&tab.lines[line_index], needle, col) {
            return Some((line_index, found));
        }
    }

    for line_index in ((start_line + 1)..tab.lines.len()).rev() {
        let col = tab.lines[line_index].chars().count();
        if let Some(found) = line_rfind_before(&tab.lines[line_index], needle, col) {
            return Some((line_index, found));
        }
    }

    None
}

fn match_at_cursor(tab: &EditorTab, needle: &str) -> bool {
    if needle.is_empty() {
        return false;
    }
    let Some(line) = tab.lines.get(tab.cursor_line) else {
        return false;
    };
    let byte = byte_index_for_char(line, tab.cursor_col);
    line[byte..].starts_with(needle)
}

fn count_tab_matches(tab: &EditorTab, needle: &str) -> usize {
    if needle.is_empty() {
        return 0;
    }
    tab.lines
        .iter()
        .map(|line| line.matches(needle).count())
        .sum()
}

fn line_find_from(line: &str, needle: &str, char_col: usize) -> Option<usize> {
    let byte = byte_index_for_char(line, char_col);
    line[byte..]
        .find(needle)
        .map(|found| line[..byte + found].chars().count())
}

fn line_rfind_before(line: &str, needle: &str, char_col: usize) -> Option<usize> {
    let byte = byte_index_for_char(line, char_col);
    line[..byte]
        .rfind(needle)
        .map(|found| line[..found].chars().count())
}

fn next_word_position(lines: &[String], mut line: usize, mut col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    line = line.min(lines.len().saturating_sub(1));

    loop {
        let chars = lines[line].chars().collect::<Vec<_>>();
        col = col.min(chars.len());
        if col >= chars.len() {
            if line + 1 >= lines.len() {
                return (line, chars.len());
            }
            line += 1;
            col = 0;
            continue;
        }

        while col < chars.len() && chars[col].is_whitespace() {
            col += 1;
        }
        while col < chars.len() && !chars[col].is_whitespace() {
            col += 1;
        }
        return (line, col);
    }
}

fn previous_word_position(lines: &[String], mut line: usize, mut col: usize) -> (usize, usize) {
    if lines.is_empty() {
        return (0, 0);
    }
    line = line.min(lines.len().saturating_sub(1));

    loop {
        let chars = lines[line].chars().collect::<Vec<_>>();
        col = col.min(chars.len());
        if col == 0 {
            if line == 0 {
                return (0, 0);
            }
            line -= 1;
            col = lines[line].chars().count();
            continue;
        }

        let mut skipped_whitespace = false;
        while col > 0 && chars[col - 1].is_whitespace() {
            col -= 1;
            skipped_whitespace = true;
        }
        if col == 0 && skipped_whitespace {
            continue;
        }
        while col > 0 && !chars[col - 1].is_whitespace() {
            col -= 1;
        }
        return (line, col);
    }
}

fn collect_workspace_files(
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    collect_workspace_files_into(root, &mut files, show_hidden, show_ignored)?;
    files.sort();
    Ok(files)
}

fn collect_workspace_files_into(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        entries.push(entry);
    }
    entries.sort_by_key(|a| a.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let hidden = name.to_str().is_some_and(is_hidden_file_name);
        if file_type.is_dir() {
            if (!show_hidden && hidden) || (!show_ignored && should_skip_dir(&path)) {
                continue;
            }
            let _ = collect_workspace_files_into(&path, files, show_hidden, show_ignored);
        } else if file_type.is_file() && (show_hidden || !hidden) {
            files.push(path);
        }
    }
    Ok(())
}

fn should_skip_dir(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    matches!(
        name,
        ".git"
            | ".hg"
            | ".svn"
            | "target"
            | "node_modules"
            | "dist"
            | "build"
            | ".next"
            | ".nuxt"
            | ".cache"
            | "__pycache__"
    )
}

fn path_has_hidden_component(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|component| {
            component
                .as_os_str()
                .to_str()
                .is_some_and(is_hidden_file_name)
        })
}

fn path_has_generated_component(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .unwrap_or(path)
        .components()
        .any(|component| {
            component.as_os_str().to_str().is_some_and(|name| {
                matches!(
                    name,
                    ".git"
                        | ".hg"
                        | ".svn"
                        | "target"
                        | "node_modules"
                        | "dist"
                        | "build"
                        | ".next"
                        | ".nuxt"
                        | ".cache"
                        | "__pycache__"
                )
            })
        })
}

fn is_hidden_file_name(name: &str) -> bool {
    name.starts_with('.') && name != "." && name != ".."
}

fn relative_path(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

fn fuzzy_score(candidate: &str, query: &str) -> Option<usize> {
    let query = query.trim().to_lowercase();
    if query.is_empty() {
        return Some(candidate.len());
    }

    let candidate_lower = candidate.to_lowercase();
    let mut last_match = 0usize;
    let mut score = 0usize;
    for query_char in query.chars() {
        let haystack = &candidate_lower[last_match..];
        let found = haystack.find(query_char)?;
        score = score.saturating_add(found);
        last_match = last_match.saturating_add(found + query_char.len_utf8());
    }

    let starts_bonus = match candidate_lower.find(&query) {
        Some(0) => 0,
        Some(_) => 10,
        None => 25,
    };
    Some(score.saturating_add(starts_bonus))
}

fn copy_path_recursive(source: &Path, destination: &Path) -> Result<()> {
    if source.is_dir() {
        fs::create_dir_all(destination)?;
        let mut entries = Vec::new();
        for entry in fs::read_dir(source)? {
            entries.push(entry?);
        }
        entries.sort_by_key(|entry| entry.file_name());

        for entry in entries {
            let child_source = entry.path();
            let child_destination = destination.join(entry.file_name());
            copy_path_recursive(&child_source, &child_destination)?;
        }
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn unique_copy_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }

    let parent = path.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("copy");
    let extension = path.extension().and_then(|extension| extension.to_str());

    for index in 1.. {
        let suffix = if index == 1 {
            " copy".to_owned()
        } else {
            format!(" copy {index}")
        };
        let file_name = match extension {
            Some(extension) => format!("{stem}{suffix}.{extension}"),
            None => format!("{stem}{suffix}"),
        };
        let candidate = parent.join(file_name);
        if !candidate.exists() {
            return candidate;
        }
    }

    unreachable!()
}

fn copy_name(name: &str) -> String {
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    match path.extension().and_then(|extension| extension.to_str()) {
        Some(extension) => format!("{stem} copy.{extension}"),
        None => format!("{stem} copy"),
    }
}

fn leading_indent_width(line: &str) -> usize {
    if line.starts_with('\t') {
        return 1;
    }
    line.chars().take_while(|c| *c == ' ').take(4).count()
}

fn leading_whitespace(line: &str) -> String {
    line.chars().take_while(|c| c.is_whitespace()).collect()
}

fn indent_unit_for(base_indent: &str) -> &'static str {
    if base_indent.ends_with('\t') {
        "\t"
    } else {
        "    "
    }
}

fn auto_pair_close(open: char) -> Option<char> {
    match open {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '"' => Some('"'),
        '\'' => Some('\''),
        '`' => Some('`'),
        _ => None,
    }
}

fn is_auto_indent_open(c: char) -> bool {
    matches!(c, '(' | '[' | '{')
}

fn is_pair_close(c: char) -> bool {
    matches!(c, ')' | ']' | '}' | '"' | '\'' | '`')
}

fn comment_token_for_path(path: &Path) -> Option<&'static str> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some(
            "rs" | "js" | "jsx" | "ts" | "tsx" | "go" | "java" | "kt" | "kts" | "c" | "h" | "cc"
            | "cpp" | "hpp" | "cs" | "swift" | "scala" | "php" | "dart",
        ) => Some("//"),
        Some(
            "py" | "rb" | "sh" | "bash" | "zsh" | "fish" | "toml" | "yml" | "yaml" | "ini" | "conf"
            | "dockerfile" | "makefile",
        ) => Some("#"),
        Some("sql") => Some("--"),
        _ => {
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_ascii_lowercase);
            match name.as_deref() {
                Some("dockerfile" | "makefile") => Some("#"),
                _ => None,
            }
        }
    }
}

fn parse_line_col(input: &str) -> Option<(usize, usize)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }

    let mut parts = trimmed.split([':', ',']);
    let line = parts.next()?.trim().parse::<usize>().ok()?;
    if line == 0 {
        return None;
    }
    let col = parts
        .next()
        .and_then(|part| part.trim().parse::<usize>().ok())
        .unwrap_or(1);
    Some((line - 1, col.saturating_sub(1)))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileReference {
    path: PathBuf,
    line: Option<usize>,
    col: Option<usize>,
}

fn terminal_file_reference_at(line: &str, char_col: usize, root: &Path) -> Option<FileReference> {
    let chars = line.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return None;
    }

    let col = char_col.min(chars.len().saturating_sub(1));
    let mut start = col;
    while start > 0 && !is_terminal_reference_delimiter(chars[start - 1]) {
        start -= 1;
    }
    let mut end = col;
    while end < chars.len() && !is_terminal_reference_delimiter(chars[end]) {
        end += 1;
    }
    if start >= end {
        return None;
    }

    let token = chars[start..end].iter().collect::<String>();
    parse_terminal_reference_token(&token, root)
}

fn parse_terminal_reference_token(token: &str, root: &Path) -> Option<FileReference> {
    let token = clean_terminal_reference_token(token)?;
    parse_trailing_terminal_reference(&token, root)
        .or_else(|| parse_embedded_terminal_reference(&token, root))
        .or_else(|| file_reference_from_parts(&token, None, None, root))
}

fn clean_terminal_reference_token(token: &str) -> Option<String> {
    let token = token
        .trim_matches(|c: char| {
            matches!(
                c,
                '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>'
            )
        })
        .trim_end_matches(['.', ',', ';', ':']);
    let token = token.strip_prefix("file://").unwrap_or(token);
    (!token.is_empty()).then(|| token.to_owned())
}

fn parse_trailing_terminal_reference(token: &str, root: &Path) -> Option<FileReference> {
    let mut path_part = token;
    let mut numbers = Vec::new();

    for _ in 0..2 {
        let Some((before, number)) = path_part.rsplit_once(':') else {
            break;
        };
        if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            break;
        }
        numbers.push(number.parse::<usize>().ok()?);
        path_part = before;
    }

    let line = numbers.last().copied().map(|line| line.saturating_sub(1));
    let col = (numbers.len() == 2).then(|| numbers[0].saturating_sub(1));
    file_reference_from_parts(path_part, line, col, root)
}

fn parse_embedded_terminal_reference(token: &str, root: &Path) -> Option<FileReference> {
    for (colon_index, _) in token.match_indices(':') {
        let after_colon = &token[colon_index + 1..];
        let line_digits = leading_ascii_digits(after_colon);
        if line_digits.is_empty() {
            continue;
        }

        let line = line_digits.parse::<usize>().ok()?.saturating_sub(1);
        let mut col = None;
        let after_line = &after_colon[line_digits.len()..];
        if let Some(after_col) = after_line.strip_prefix(':') {
            let col_digits = leading_ascii_digits(after_col);
            if !col_digits.is_empty() {
                col = Some(col_digits.parse::<usize>().ok()?.saturating_sub(1));
            }
        }

        let path_part = &token[..colon_index];
        if let Some(reference) = file_reference_from_parts(path_part, Some(line), col, root) {
            return Some(reference);
        }
    }

    None
}

fn leading_ascii_digits(input: &str) -> &str {
    let end = input
        .char_indices()
        .take_while(|(_, c)| c.is_ascii_digit())
        .map(|(index, c)| index + c.len_utf8())
        .last()
        .unwrap_or(0);
    &input[..end]
}

fn file_reference_from_parts(
    path_part: &str,
    line: Option<usize>,
    col: Option<usize>,
    root: &Path,
) -> Option<FileReference> {
    let path_part = path_part.trim();
    if path_part.is_empty() {
        return None;
    }

    let raw = Path::new(path_part);
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        root.join(raw)
    };
    let path = candidate.canonicalize().unwrap_or(candidate);
    path.is_file().then_some(FileReference { path, line, col })
}

fn is_terminal_reference_delimiter(c: char) -> bool {
    c.is_whitespace()
        || matches!(
            c,
            '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}' | '<' | '>' | '|'
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tscode-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn editor_undo_redo_and_save_preserves_trailing_newline() {
        let path = temp_file("undo-redo.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.insert_char('X');
        assert_eq!(tab.lines[0], "Xalpha");

        assert!(tab.undo());
        assert_eq!(tab.lines[0], "alpha");

        assert!(tab.redo());
        assert_eq!(tab.lines[0], "Xalpha");

        tab.save().unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "Xalpha\n");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_line_commands_modify_real_buffer() {
        let path = temp_file("line-commands.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "fn main() {\nprintln!(\"hi\");\n}\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(1, 0);
        tab.indent_line();
        assert_eq!(tab.lines[1], "    println!(\"hi\");");

        assert!(tab.outdent_line());
        assert_eq!(tab.lines[1], "println!(\"hi\");");

        assert!(tab.toggle_line_comment());
        assert_eq!(tab.lines[1], "// println!(\"hi\");");
        assert!(tab.toggle_line_comment());
        assert_eq!(tab.lines[1], "println!(\"hi\");");

        tab.duplicate_line();
        assert_eq!(tab.lines[2], "println!(\"hi\");");

        assert!(tab.move_line_down());
        assert_eq!(tab.cursor_line, 3);

        tab.delete_line();
        assert_eq!(tab.lines, vec!["fn main() {", "println!(\"hi\");", "}"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_selection_cuts_and_restores_multiline_text() {
        let path = temp_file("selection.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "abc\ndef\nghi\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, 1);
        tab.set_cursor_selecting(1, 2);

        assert_eq!(tab.selected_text().as_deref(), Some("bc\nde"));
        let cut = tab.delete_selection().unwrap();
        assert_eq!(cut, "bc\nde");
        assert_eq!(tab.lines, vec!["af", "ghi"]);
        assert_eq!(tab.cursor_position(), (0, 1));

        tab.insert_text(&cut);
        assert_eq!(tab.lines, vec!["abc", "def", "ghi"]);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_auto_pairs_skip_backspace_and_wrap_selection() {
        let path = temp_file("pairs.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.insert_char('(');
        assert_eq!(tab.lines, vec!["()"]);
        assert_eq!(tab.cursor_position(), (0, 1));

        tab.insert_char(')');
        assert_eq!(tab.lines, vec!["()"]);
        assert_eq!(tab.cursor_position(), (0, 2));

        tab.set_cursor(0, 1);
        tab.backspace();
        assert_eq!(tab.lines, vec![""]);
        assert_eq!(tab.cursor_position(), (0, 0));

        tab.insert_text("alpha");
        tab.set_cursor(0, 0);
        tab.set_cursor_selecting(0, 5);
        tab.insert_char('"');
        assert_eq!(tab.lines, vec!["\"alpha\""]);
        assert_eq!(tab.selected_text().as_deref(), Some("alpha"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_enter_preserves_indent_and_splits_pairs() {
        let path = temp_file("auto-indent.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "fn main() {}\n    let x = 1;").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, "fn main() {".chars().count());
        tab.newline();
        assert_eq!(
            tab.lines,
            vec!["fn main() {", "    ", "}", "    let x = 1;"]
        );
        assert_eq!(tab.cursor_position(), (1, 4));

        tab.set_cursor(3, tab.lines[3].chars().count());
        tab.newline();
        assert_eq!(
            tab.lines,
            vec!["fn main() {", "    ", "}", "    let x = 1;", "    "]
        );
        assert_eq!(tab.cursor_position(), (4, 4));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_shortcuts_copy_cut_and_paste_internal_clipboard() {
        let root = std::env::temp_dir().join(format!("tscode-test-edit-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "alpha\nbeta\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.active_tab().unwrap().selection_range().is_some());

        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.editor_clipboard.as_deref(), Some("alpha\nbeta"));
        assert_eq!(app.take_clipboard_export().as_deref(), Some("alpha\nbeta"));

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_tab().unwrap().lines, vec![""]);
        assert_eq!(app.take_clipboard_export().as_deref(), Some("alpha\nbeta"));

        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_tab().unwrap().lines, vec!["alpha", "beta"]);
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_path_commands_export_absolute_and_relative_paths() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-path-copy-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let file = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&file);

        app.copy_active_file_path_to_clipboard(false);
        assert_eq!(
            app.take_clipboard_export(),
            Some(file.to_string_lossy().into_owned())
        );

        app.copy_active_file_path_to_clipboard(true);
        assert_eq!(app.take_clipboard_export().as_deref(), Some("src/main.rs"));

        app.reveal_path(&file).unwrap();
        app.copy_selected_explorer_path_to_clipboard(false);
        assert_eq!(
            app.take_clipboard_export(),
            Some(file.to_string_lossy().into_owned())
        );

        app.copy_selected_explorer_path_to_clipboard(true);
        assert_eq!(app.take_clipboard_export().as_deref(), Some("src/main.rs"));

        assert!(app.explorer_clipboard.is_none());
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_replace_current_and_all_are_undoable() {
        let root = std::env::temp_dir().join(format!("tscode-test-replace-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "alpha beta alpha\nbeta\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.search_needle = Some("alpha".to_owned());
        assert_eq!(app.active_search_match_count(), Some(2));

        app.replace_next_active_match("alpha".to_owned(), "gamma".to_owned());
        assert_eq!(
            app.active_tab().unwrap().lines,
            vec!["gamma beta alpha", "beta"]
        );
        assert_eq!(app.active_search_match_count(), Some(1));

        app.replace_all_active_matches("beta".to_owned(), "delta".to_owned());
        assert_eq!(
            app.active_tab().unwrap().lines,
            vec!["gamma delta alpha", "delta"]
        );

        app.undo_active_tab();
        assert_eq!(
            app.active_tab().unwrap().lines,
            vec!["gamma beta alpha", "beta"]
        );
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn search_wraps_forward_and_backward() {
        let path = temp_file("search.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha\nbeta alpha\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, 0);
        assert_eq!(find_forward(&tab, "alpha"), Some((1, 5)));

        tab.set_cursor(1, 5);
        assert_eq!(find_forward(&tab, "alpha"), Some((0, 0)));
        assert_eq!(find_backward(&tab, "alpha"), Some((0, 0)));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_ctrl_word_navigation_moves_by_words_and_selects() {
        let path = temp_file("word-nav.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha beta\n  gamma\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.move_word(true, false);
        assert_eq!(tab.cursor_position(), (0, 5));

        tab.move_word(true, false);
        assert_eq!(tab.cursor_position(), (0, 10));

        tab.move_word(true, false);
        assert_eq!(tab.cursor_position(), (1, 7));

        tab.move_word(false, true);
        assert_eq!(tab.cursor_position(), (1, 2));
        assert_eq!(tab.selected_text().as_deref(), Some("gamma"));

        tab.move_word(false, false);
        assert_eq!(tab.cursor_position(), (0, 6));
        assert!(tab.selection_range().is_none());
        let _ = fs::remove_file(path);
    }

    #[test]
    fn fuzzy_score_matches_path_fragments_in_order() {
        assert!(fuzzy_score("src/main.rs", "smr").is_some());
        assert!(fuzzy_score("src/main.rs", "zzz").is_none());
    }

    #[test]
    fn command_palette_finds_executable_commands() {
        let root = std::env::temp_dir().join(format!("tscode-test-command-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::F(1), KeyModifiers::NONE))
            .unwrap();
        assert!(
            app.quick_panel
                .as_ref()
                .is_some_and(|panel| panel.kind == QuickPanelKind::CommandPalette)
        );
        app.quick_panel = None;

        let commands = app.command_palette_items("restart term");
        assert!(commands.iter().any(|item| {
            item.command == Some(CommandAction::RestartTerminal) && item.label == "Restart Terminal"
        }));

        let commands = app.command_palette_items("go line");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::GotoLine))
        );

        let commands = app.command_palette_items("replace all");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ReplaceAllInFile))
        );
        let commands = app.command_palette_items("copy relative path");
        assert!(commands.iter().any(|item| {
            item.command == Some(CommandAction::CopyActiveFileRelativePath)
                || item.command == Some(CommandAction::CopySelectedExplorerRelativePath)
        }));
        let commands = app.command_palette_items("new terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::NewTerminal))
        );
        let commands = app.command_palette_items("next terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::NextTerminal))
        );
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_sessions_can_be_created_switched_and_closed() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminals-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);

        app.new_terminal().unwrap();
        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.active_terminal, 1);
        assert_eq!(app.active_terminal().title, "term 2");

        app.previous_terminal();
        assert_eq!(app.active_terminal, 0);
        app.next_terminal();
        assert_eq!(app.active_terminal, 1);

        app.close_active_terminal().unwrap();
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);
        assert_eq!(app.active_terminal().title, "term 1");

        app.close_active_terminal().unwrap();
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn parse_line_col_uses_one_based_input() {
        assert_eq!(parse_line_col("12"), Some((11, 0)));
        assert_eq!(parse_line_col("12:4"), Some((11, 3)));
        assert_eq!(parse_line_col("0"), None);
        assert_eq!(parse_line_col(""), None);
    }

    #[test]
    fn terminal_file_reference_opens_existing_paths_with_line_columns() {
        let root = std::env::temp_dir().join(format!("tscode-test-termref-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("README.md"), "# docs\n").unwrap();
        let root = root.canonicalize().unwrap();
        let main = root.join("src/main.rs");
        let readme = root.join("README.md");

        let cargo_line = "error: here --> src/main.rs:12:5";
        let cargo_ref =
            terminal_file_reference_at(cargo_line, cargo_line.find("main.rs").unwrap(), &root)
                .unwrap();
        assert_eq!(cargo_ref.path, main);
        assert_eq!(cargo_ref.line, Some(11));
        assert_eq!(cargo_ref.col, Some(4));

        let grep_line = "src/main.rs:7:fn main()";
        let grep_ref =
            terminal_file_reference_at(grep_line, grep_line.find("7").unwrap(), &root).unwrap();
        assert_eq!(grep_ref.path, root.join("src/main.rs"));
        assert_eq!(grep_ref.line, Some(6));
        assert_eq!(grep_ref.col, None);

        let simple_line = "open README.md";
        let simple_ref =
            terminal_file_reference_at(simple_line, simple_line.find("README").unwrap(), &root)
                .unwrap();
        assert_eq!(simple_ref.path, readme);
        assert_eq!(simple_ref.line, None);

        assert!(terminal_file_reference_at("missing.rs:1:1", 2, &root).is_none());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_file_search_reads_real_files() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-workspace-{}", std::process::id()));
        let nested = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        fs::write(
            nested.join("main.rs"),
            "fn main() {\n    println!(\"needle\");\n}\n",
        )
        .unwrap();
        fs::write(root.join("README.md"), "other\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let open_items = app.quick_open_items("main").unwrap();
        assert!(open_items.iter().any(|item| item.detail == "src/main.rs"));

        let search_items = app.workspace_search_items("needle").unwrap();
        assert_eq!(search_items.len(), 1);
        assert_eq!(search_items[0].detail, "src/main.rs");
        assert_eq!(search_items[0].line, Some(1));

        app.open_file(&search_items[0].path);
        assert!(app.active_tab().is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_visibility_hides_generated_by_default_and_can_filter() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-visibility-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("target/generated.rs"), "generated\n").unwrap();
        fs::write(root.join(".env"), "SECRET=1\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        let names = app
            .visible_nodes()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"src".to_owned()));
        assert!(names.contains(&".env".to_owned()));
        assert!(!names.contains(&"target".to_owned()));

        app.toggle_ignored_files();
        let names = app
            .visible_nodes()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"target".to_owned()));

        app.toggle_hidden_files();
        let names = app
            .visible_nodes()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(!names.contains(&".env".to_owned()));

        app.set_explorer_filter("src".to_owned());
        let names = app
            .visible_nodes()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"src".to_owned()));
        assert!(!names.contains(&"target".to_owned()));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn copy_path_recursive_copies_directories_and_unique_names() {
        let root = std::env::temp_dir().join(format!("tscode-test-copy-{}", std::process::id()));
        let source = root.join("src");
        let nested = source.join("nested");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("file.txt"), "copied").unwrap();

        let destination = root.join("src copy");
        copy_path_recursive(&source, &destination).unwrap();
        assert_eq!(
            fs::read_to_string(destination.join("nested/file.txt")).unwrap(),
            "copied"
        );

        assert_eq!(unique_copy_path(&source), root.join("src copy 2"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn cut_paste_updates_open_tab_paths() {
        let root = std::env::temp_dir().join(format!("tscode-test-cut-{}", std::process::id()));
        let src_dir = root.join("src");
        let dst_dir = root.join("dst");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src_dir).unwrap();
        fs::create_dir_all(&dst_dir).unwrap();
        fs::write(src_dir.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let source = canonical_root.join("src/main.rs");
        let destination = canonical_root.join("dst/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&source);
        app.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Cut,
            path: source.clone(),
        });
        app.explorer.reveal(&canonical_root.join("dst")).unwrap();
        app.paste_into_selected().unwrap();

        assert!(!source.exists());
        assert!(destination.exists());
        assert_eq!(app.active_tab().unwrap().path, destination);
        let _ = fs::remove_dir_all(root);
    }
}
