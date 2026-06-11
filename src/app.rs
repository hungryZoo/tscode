use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{Context, Result, anyhow};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ignore::WalkBuilder;
use ratatui::layout::Rect;
use serde_json::Value;

use crate::{
    fs_tree::{ExplorerSortMode, FsTree, VisibleNode},
    lsp::{self, DocumentPosition},
    shell::{ShellExitStatus, ShellPanel, TerminalSearchMatch},
    syntax::SyntaxHighlighter,
};

const MAX_QUICK_ITEMS: usize = 200;
const MAX_FILE_SCAN_BYTES: u64 = 1_000_000;
const MAX_FILE_OPEN_BYTES: u64 = 5_000_000;
const READ_ONLY_PREVIEW_BYTES: usize = 4096;
const MAX_OSC52_CLIPBOARD_BYTES: usize = 512 * 1024;
const MAX_NAVIGATION_HISTORY: usize = 200;
const MAX_CLOSED_TABS: usize = 100;
const MAX_TERMINAL_COMMAND_HISTORY: usize = 100;
const WORKSPACE_TREE_CHECK_INTERVAL: Duration = Duration::from_millis(750);

#[derive(Debug, Clone, PartialEq, Eq)]
struct LspRenameSummary {
    server: String,
    edit_count: usize,
    open_count: usize,
    file_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TextReplacement {
    start: usize,
    end: usize,
    new_text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPanel {
    Explorer,
    Editor,
    Terminal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SidebarMode {
    Files,
    Outline,
}

impl SidebarMode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Files => "files",
            Self::Outline => "outline",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HoverTarget {
    #[default]
    None,
    Explorer,
    ExplorerRow(usize),
    Outline,
    OutlineRow(usize),
    Editor,
    EditorPane(usize),
    Tab(usize),
    TabClose(usize),
    TerminalTab(usize),
    TerminalTabClose(usize),
    TerminalNew,
    TerminalResize,
    QuickRow(usize),
    Terminal,
    TerminalInput,
}

#[derive(Debug, Clone, Default)]
pub struct HitRegions {
    pub explorer_area: Option<Rect>,
    pub outline_area: Option<Rect>,
    pub editor_area: Option<Rect>,
    pub editor_body: Option<Rect>,
    pub editor_panes: Vec<(Rect, usize, usize)>,
    pub terminal_area: Option<Rect>,
    pub terminal_body: Option<Rect>,
    pub terminal_input: Option<Rect>,
    pub terminal_resize: Option<Rect>,
    pub terminal_bodies: Vec<(Rect, usize)>,
    pub explorer_rows: Vec<(Rect, usize)>,
    pub outline_rows: Vec<(Rect, usize)>,
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

        if self
            .terminal_resize
            .is_some_and(|rect| contains(rect, x, y))
        {
            return HoverTarget::TerminalResize;
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

        for (rect, index) in &self.outline_rows {
            if contains(*rect, x, y) {
                return HoverTarget::OutlineRow(*index);
            }
        }

        if self.explorer_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Explorer;
        }

        if self.outline_area.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::Outline;
        }

        for (rect, pane, _) in &self.editor_panes {
            if contains(*rect, x, y) {
                return HoverTarget::EditorPane(*pane);
            }
        }

        for (rect, _) in &self.terminal_bodies {
            if contains(*rect, x, y) {
                return HoverTarget::TerminalInput;
            }
        }

        if self.terminal_input.is_some_and(|rect| contains(rect, x, y)) {
            return HoverTarget::TerminalInput;
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

fn is_editor_target(target: &HoverTarget) -> bool {
    matches!(target, HoverTarget::Editor | HoverTarget::EditorPane(_))
}

fn terminal_mouse_cell_in_body(mouse: MouseEvent, body: Rect) -> Option<(u16, u16)> {
    if body.width == 0 || body.height == 0 {
        return None;
    }
    let row = mouse
        .row
        .saturating_sub(body.y)
        .min(body.height.saturating_sub(1));
    let col = mouse
        .column
        .saturating_sub(body.x)
        .min(body.width.saturating_sub(1));
    Some((row, col))
}

fn terminal_host_mouse_override(modifiers: KeyModifiers) -> bool {
    modifiers.contains(KeyModifiers::SHIFT)
}

fn is_scroll_mouse_event(kind: MouseEventKind) -> bool {
    matches!(
        kind,
        MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight
    )
}

#[derive(Debug, Clone)]
struct EditorSnapshot {
    lines: Vec<String>,
    cursor_line: usize,
    cursor_col: usize,
    selection_anchor: Option<(usize, usize)>,
    extra_selections: Vec<EditorSelection>,
    extra_cursors: Vec<(usize, usize)>,
    trailing_newline: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorSelection {
    pub start: (usize, usize),
    pub end: (usize, usize),
}

type EditorReplacement = ((usize, usize), (usize, usize), String);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalFileState {
    Clean,
    Modified,
    Deleted,
}

impl ExternalFileState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Clean => "clean",
            Self::Modified => "modified on disk",
            Self::Deleted => "deleted on disk",
        }
    }

    fn is_clean(self) -> bool {
        self == Self::Clean
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    len: u64,
    modified: Option<SystemTime>,
}

type WorkspaceSnapshot = Vec<WorkspaceEntryStamp>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceEntryStamp {
    path: PathBuf,
    is_dir: bool,
    len: u64,
    modified: Option<SystemTime>,
    readonly: bool,
}

impl EditorSelection {
    fn new(start: (usize, usize), end: (usize, usize)) -> Option<Self> {
        let selection = if start <= end {
            Self { start, end }
        } else {
            Self {
                start: end,
                end: start,
            }
        };
        (selection.start != selection.end).then_some(selection)
    }
}

#[derive(Debug, Clone)]
pub struct EditorTab {
    pub path: PathBuf,
    pub title: String,
    pub lines: Vec<String>,
    pub scroll: usize,
    pub horizontal_scroll: usize,
    pub cursor_line: usize,
    pub cursor_col: usize,
    pub selection_anchor: Option<(usize, usize)>,
    pub extra_selections: Vec<EditorSelection>,
    pub extra_cursors: Vec<(usize, usize)>,
    pub folded_lines: BTreeSet<usize>,
    pub bookmarks: BTreeSet<usize>,
    pub document_highlights: Vec<lsp::LspDocumentHighlight>,
    pub dirty: bool,
    pub external_state: ExternalFileState,
    pub untitled: bool,
    pub read_only: bool,
    disk_stamp: Option<FileStamp>,
    trailing_newline: bool,
    undo_stack: Vec<EditorSnapshot>,
    redo_stack: Vec<EditorSnapshot>,
}

impl EditorTab {
    fn open(path: PathBuf) -> Result<Self> {
        let title = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("[file]")
            .to_owned();
        let metadata = fs::metadata(&path)?;
        if metadata.len() > MAX_FILE_OPEN_BYTES {
            let preview = read_file_prefix(&path, READ_ONLY_PREVIEW_BYTES)?;
            let text =
                guarded_file_preview(&path, FileOpenGuard::TooLarge(metadata.len()), &preview);
            return Ok(Self::read_only(path, title, &text));
        }

        let bytes = fs::read(&path)?;
        if bytes.contains(&0) {
            let text = guarded_file_preview(&path, FileOpenGuard::Binary, &bytes);
            return Ok(Self::read_only(path, title, &text));
        }

        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                let text =
                    guarded_file_preview(&path, FileOpenGuard::InvalidUtf8, &error.into_bytes());
                return Ok(Self::read_only(path, title, &text));
            }
        };
        let (lines, trailing_newline) = split_editor_text(&text);
        let disk_stamp = file_stamp(&path);

        Ok(Self {
            path,
            title,
            lines,
            scroll: 0,
            horizontal_scroll: 0,
            cursor_line: 0,
            cursor_col: 0,
            selection_anchor: None,
            extra_selections: Vec::new(),
            extra_cursors: Vec::new(),
            folded_lines: BTreeSet::new(),
            bookmarks: BTreeSet::new(),
            document_highlights: Vec::new(),
            dirty: false,
            external_state: ExternalFileState::Clean,
            untitled: false,
            read_only: false,
            disk_stamp,
            trailing_newline,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
    }

    fn read_only(path: PathBuf, title: String, text: &str) -> Self {
        let (lines, trailing_newline) = split_editor_text(text);
        Self {
            path,
            title,
            lines,
            scroll: 0,
            horizontal_scroll: 0,
            cursor_line: 0,
            cursor_col: 0,
            selection_anchor: None,
            extra_selections: Vec::new(),
            extra_cursors: Vec::new(),
            folded_lines: BTreeSet::new(),
            bookmarks: BTreeSet::new(),
            document_highlights: Vec::new(),
            dirty: false,
            external_state: ExternalFileState::Clean,
            untitled: false,
            read_only: true,
            disk_stamp: None,
            trailing_newline,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    fn untitled(id: usize, root: &Path) -> Self {
        Self {
            path: root.join(format!("Untitled-{id}")),
            title: format!("Untitled-{id}"),
            lines: vec![String::new()],
            scroll: 0,
            horizontal_scroll: 0,
            cursor_line: 0,
            cursor_col: 0,
            selection_anchor: None,
            extra_selections: Vec::new(),
            extra_cursors: Vec::new(),
            folded_lines: BTreeSet::new(),
            bookmarks: BTreeSet::new(),
            document_highlights: Vec::new(),
            dirty: false,
            external_state: ExternalFileState::Clean,
            untitled: true,
            read_only: false,
            disk_stamp: None,
            trailing_newline: false,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        }
    }

    fn text(&self) -> String {
        let mut text = self.lines.join("\n");
        if self.trailing_newline && !text.ends_with('\n') {
            text.push('\n');
        }
        text
    }

    fn set_clean_text(&mut self, text: &str) {
        let (lines, trailing_newline) = split_editor_text(text);
        self.lines = lines;
        self.trailing_newline = trailing_newline;
        self.cursor_line = self.cursor_line.min(self.lines.len().saturating_sub(1));
        self.clamp_cursor_col();
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
        self.horizontal_scroll = 0;
        self.selection_anchor = None;
        self.extra_selections.clear();
        self.extra_cursors.clear();
        self.folded_lines.clear();
        self.prune_bookmarks();
        self.document_highlights.clear();
        self.dirty = false;
        self.external_state = ExternalFileState::Clean;
        self.disk_stamp = file_stamp(&self.path);
        self.undo_stack.clear();
        self.redo_stack.clear();
    }

    fn refresh_disk_stamp(&mut self) {
        self.untitled = false;
        self.external_state = ExternalFileState::Clean;
        self.disk_stamp = file_stamp(&self.path);
    }

    fn current_disk_state(&self) -> ExternalFileState {
        if self.untitled || self.read_only {
            return ExternalFileState::Clean;
        }
        match file_stamp(&self.path) {
            None => ExternalFileState::Deleted,
            Some(stamp) if Some(stamp) != self.disk_stamp => ExternalFileState::Modified,
            Some(_) => ExternalFileState::Clean,
        }
    }

    fn replace_entire_text_as_edit(&mut self, text: &str) -> bool {
        if self.text() == text {
            return false;
        }

        self.push_undo();
        let (lines, trailing_newline) = split_editor_text(text);
        self.lines = lines;
        self.trailing_newline = trailing_newline;
        self.cursor_line = self.cursor_line.min(self.lines.len().saturating_sub(1));
        self.clamp_cursor_col();
        self.scroll = self.scroll.min(self.lines.len().saturating_sub(1));
        self.horizontal_scroll = 0;
        self.selection_anchor = None;
        self.extra_selections.clear();
        self.extra_cursors.clear();
        self.folded_lines.clear();
        self.prune_bookmarks();
        self.document_highlights.clear();
        self.dirty = true;
        true
    }

    fn save(&mut self) -> Result<()> {
        fs::write(&self.path, self.text())?;
        self.dirty = false;
        self.external_state = ExternalFileState::Clean;
        self.disk_stamp = file_stamp(&self.path);
        Ok(())
    }

    fn insert_char(&mut self, c: char) {
        if !self.has_selection()
            && !self.has_extra_cursors()
            && self.char_at_cursor() == Some(c)
            && is_pair_close(c)
        {
            self.cursor_col += 1;
            return;
        }

        if !self.has_extra_cursors()
            && let Some(close) = auto_pair_close(c)
        {
            if self.has_selection() {
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

        if self.has_selection() {
            self.replace_selection_with(&c.to_string());
            return;
        }

        if self.has_extra_cursors() {
            self.insert_text_at_cursors(&c.to_string());
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

        if self.has_selection() {
            self.replace_selection_with(text);
            return;
        }

        if self.has_extra_cursors() {
            self.insert_text_at_cursors(text);
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
        if self.has_selection() {
            self.replace_selection_with("\n");
            return;
        }

        if self.has_extra_cursors() {
            self.insert_text_at_cursors("\n");
            return;
        }

        self.push_undo();
        self.newline_auto_indent_raw();
        self.dirty = true;
    }

    fn newline_raw(&mut self) {
        let cursor_col = self.cursor_col;
        let insert_at = self.cursor_line + 1;
        let line = self.current_line_mut();
        let byte = byte_index_for_char(line, cursor_col);
        let rest = line.split_off(byte);
        self.cursor_line += 1;
        self.cursor_col = 0;
        self.lines.insert(self.cursor_line, rest);
        self.shift_bookmarks_for_insert(insert_at, 1);
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

        let insert_at = self.cursor_line + 1;
        self.lines[self.cursor_line] = before;
        self.cursor_line += 1;
        self.cursor_col = inner_indent.chars().count();
        if should_split_pair {
            self.lines.insert(self.cursor_line, inner_indent);
            self.lines
                .insert(self.cursor_line + 1, format!("{base_indent}{after}"));
            self.shift_bookmarks_for_insert(insert_at, 2);
        } else {
            self.lines
                .insert(self.cursor_line, format!("{inner_indent}{after}"));
            self.shift_bookmarks_for_insert(insert_at, 1);
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
        let (start, end, had_selection) = self.command_line_range();
        self.push_undo();
        for line_index in start..=end {
            self.lines[line_index].insert_str(0, "    ");
        }
        if had_selection {
            self.select_line_range(start, end);
        } else {
            self.cursor_col = self.cursor_col.saturating_add(4);
        }
        self.dirty = true;
    }

    fn outdent_line(&mut self) -> bool {
        let (start, end, had_selection) = self.command_line_range();
        let removals = (start..=end)
            .filter_map(|line_index| {
                let remove_count = leading_indent_width(&self.lines[line_index]);
                (remove_count > 0).then_some((line_index, remove_count))
            })
            .collect::<Vec<_>>();
        if removals.is_empty() {
            return false;
        }

        self.push_undo();
        let cursor_removed = removals
            .iter()
            .find_map(|(line_index, remove_count)| {
                (*line_index == self.cursor_line).then_some(*remove_count)
            })
            .unwrap_or(0);
        for (line_index, remove_count) in removals {
            let end_byte = byte_index_for_char(&self.lines[line_index], remove_count);
            self.lines[line_index].replace_range(0..end_byte, "");
        }
        if had_selection {
            self.select_line_range(start, end);
        } else {
            self.cursor_col = self.cursor_col.saturating_sub(cursor_removed);
        }
        self.dirty = true;
        true
    }

    fn duplicate_line(&mut self) {
        let (start, end, had_selection) = self.command_line_range();
        self.push_undo();
        let duplicates = self.lines[start..=end].to_vec();
        let insert_at = end + 1;
        for (offset, duplicate) in duplicates.iter().cloned().enumerate() {
            self.lines.insert(insert_at + offset, duplicate);
        }
        self.shift_bookmarks_for_insert(insert_at, duplicates.len());
        if had_selection {
            let duplicated_start = insert_at;
            let duplicated_end = insert_at + duplicates.len() - 1;
            self.select_line_range(duplicated_start, duplicated_end);
        } else {
            self.cursor_line += 1;
            self.clamp_cursor_col();
        }
        self.dirty = true;
    }

    fn delete_line(&mut self) {
        let (start, end, _) = self.command_line_range();
        self.push_undo();
        if start == 0 && end + 1 >= self.lines.len() {
            self.lines.clear();
            self.lines.push(String::new());
            self.bookmarks.clear();
            self.cursor_line = 0;
            self.cursor_col = 0;
        } else {
            self.lines.drain(start..=end);
            self.shift_bookmarks_for_delete(start, end);
            self.cursor_line = start.min(self.lines.len().saturating_sub(1));
            self.cursor_col = 0;
        }
        self.clear_selection();
        self.dirty = true;
    }

    fn move_line_up(&mut self) -> bool {
        let (start, end, had_selection) = self.command_line_range();
        if start == 0 {
            return false;
        }

        self.push_undo();
        let previous = self.lines.remove(start - 1);
        self.lines.insert(end, previous);
        self.shift_bookmarks_for_move_up(start, end);
        if had_selection {
            self.select_line_range(start - 1, end - 1);
        } else {
            self.cursor_line -= 1;
        }
        self.dirty = true;
        true
    }

    fn move_line_down(&mut self) -> bool {
        let (start, end, had_selection) = self.command_line_range();
        if end + 1 >= self.lines.len() {
            return false;
        }

        self.push_undo();
        let next = self.lines.remove(end + 1);
        self.lines.insert(start, next);
        self.shift_bookmarks_for_move_down(start, end);
        if had_selection {
            self.select_line_range(start + 1, end + 1);
        } else {
            self.cursor_line += 1;
        }
        self.dirty = true;
        true
    }

    fn toggle_line_comment(&mut self) -> bool {
        let Some(token) = comment_token_for_path(&self.path) else {
            return false;
        };

        let (start, end, had_selection) = self.command_line_range();
        let token_with_space = format!("{token} ");
        let should_uncomment = (start..=end)
            .all(|line_index| comment_removal_range(&self.lines[line_index], token).is_some());

        self.push_undo();
        if should_uncomment {
            let cursor_removed = comment_removal_range(&self.lines[self.cursor_line], token)
                .map(|(_, _, count)| count)
                .unwrap_or(0);
            for line_index in start..=end {
                if let Some((remove_start, remove_end, _)) =
                    comment_removal_range(&self.lines[line_index], token)
                {
                    self.lines[line_index].replace_range(remove_start..remove_end, "");
                }
            }
            if had_selection {
                self.select_line_range(start, end);
            } else {
                self.cursor_col = self.cursor_col.saturating_sub(cursor_removed);
            }
        } else {
            let cursor_line = self.cursor_line;
            let mut cursor_add = 0usize;
            for line_index in start..=end {
                let indent_chars = self.lines[line_index]
                    .chars()
                    .take_while(|c| c.is_whitespace())
                    .count();
                let indent_byte = byte_index_for_char(&self.lines[line_index], indent_chars);
                self.lines[line_index].insert_str(indent_byte, &token_with_space);
                if line_index == cursor_line && self.cursor_col >= indent_chars {
                    cursor_add = token_with_space.chars().count();
                }
            }
            if had_selection {
                self.select_line_range(start, end);
            } else {
                self.cursor_col = self.cursor_col.saturating_add(cursor_add);
            }
        }

        self.dirty = true;
        true
    }

    fn toggle_block_comment(&mut self) -> Option<bool> {
        let (open, close) = block_comment_tokens_for_path(&self.path)?;
        let mut ranges = self.selection_ranges();
        if ranges.is_empty() {
            let line = self.cursor_line;
            let start_col = self.lines[line]
                .chars()
                .take_while(|c| c.is_whitespace())
                .count();
            let end_col = self.lines[line].chars().count();
            ranges.push(EditorSelection {
                start: (line, start_col),
                end: (line, end_col),
            });
        }

        let should_uncomment = ranges.iter().all(|range| {
            uncomment_block_text(&self.text_in_range(range.start, range.end), open, close).is_some()
        });

        let replacements = ranges
            .iter()
            .map(|range| {
                let selected = self.text_in_range(range.start, range.end);
                let replacement = if should_uncomment {
                    uncomment_block_text(&selected, open, close).unwrap_or(selected)
                } else {
                    comment_block_text(&selected, open, close)
                };
                (range.start, range.end, replacement)
            })
            .collect::<Vec<_>>();

        self.push_undo();
        self.replace_distinct_ranges_raw(&replacements, false);
        self.dirty = true;
        Some(!should_uncomment)
    }

    fn trim_trailing_whitespace(&mut self) -> usize {
        let changed_lines = self
            .lines
            .iter()
            .filter(|line| line.ends_with(' ') || line.ends_with('\t'))
            .count();
        if changed_lines == 0 {
            return 0;
        }

        self.push_undo();
        for line in &mut self.lines {
            let trimmed_len = line.trim_end_matches([' ', '\t']).len();
            if trimmed_len != line.len() {
                line.truncate(trimmed_len);
            }
        }
        self.clamp_cursor_col();
        self.clear_selection();
        self.dirty = true;
        changed_lines
    }

    fn move_cursor_with_selection(&mut self, line_delta: isize, col_delta: isize, selecting: bool) {
        let previous = self.cursor_position();
        if selecting {
            self.extra_selections.clear();
            self.extra_cursors.clear();
        }
        if selecting && self.selection_anchor.is_none() {
            self.selection_anchor = Some(previous);
        } else if !selecting {
            self.clear_selection();
        }

        self.cursor_line =
            add_signed(self.cursor_line, line_delta).min(self.lines.len().saturating_sub(1));
        self.cursor_col = add_signed(self.cursor_col, col_delta);
        self.clamp_cursor_col();
        self.clear_collapsed_selection();
    }

    fn move_word(&mut self, forward: bool, selecting: bool) {
        let previous = self.cursor_position();
        if selecting {
            self.extra_selections.clear();
            self.extra_cursors.clear();
        }
        if selecting && self.selection_anchor.is_none() {
            self.selection_anchor = Some(previous);
        } else if !selecting {
            self.clear_selection();
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
        self.clear_selection();
        self.set_cursor_raw(line, col);
    }

    fn toggle_cursor_at(&mut self, line: usize, col: usize) -> usize {
        let target = self.clamped_position(line, col);
        let mut cursors = self.cursor_positions();
        if let Some(index) = cursors.iter().position(|cursor| *cursor == target) {
            if cursors.len() > 1 {
                cursors.remove(index);
            }
        } else {
            cursors.push(target);
        }
        cursors.sort();
        cursors.dedup();

        let primary = if cursors.contains(&target) {
            target
        } else if cursors.contains(&self.cursor_position()) {
            self.cursor_position()
        } else {
            cursors[0]
        };

        self.selection_anchor = None;
        self.extra_selections.clear();
        self.cursor_line = primary.0;
        self.cursor_col = primary.1;
        self.extra_cursors = cursors
            .into_iter()
            .filter(|cursor| *cursor != primary)
            .collect();
        self.selection_count()
    }

    fn set_cursor_selecting(&mut self, line: usize, col: usize) {
        self.extra_selections.clear();
        self.extra_cursors.clear();
        if self.selection_anchor.is_none() {
            self.selection_anchor = Some(self.cursor_position());
        }
        self.set_cursor_raw(line, col);
        self.clear_collapsed_selection();
    }

    fn set_cursor_raw(&mut self, line: usize, col: usize) {
        let (line, col) = self.clamped_position(line, col);
        self.cursor_line = line;
        self.cursor_col = col;
    }

    fn cursor_position(&self) -> (usize, usize) {
        (self.cursor_line, self.cursor_col)
    }

    fn clamped_position(&self, line: usize, col: usize) -> (usize, usize) {
        let line = line.min(self.lines.len().saturating_sub(1));
        let col = col.min(self.lines[line].chars().count());
        (line, col)
    }

    pub fn cursor_positions(&self) -> Vec<(usize, usize)> {
        let mut cursors = self.extra_cursors.clone();
        cursors.push(self.cursor_position());
        cursors.sort();
        cursors.dedup();
        cursors
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

    fn char_at_position(&self, position: (usize, usize)) -> Option<char> {
        self.lines.get(position.0)?.chars().nth(position.1)
    }

    fn bracket_candidate_at_cursor(&self) -> Option<((usize, usize), char)> {
        let cursor = self.cursor_position();
        if let Some(ch) = self.char_at_cursor()
            && bracket_pair(ch).is_some()
        {
            return Some((cursor, ch));
        }
        if self.cursor_col > 0
            && let Some(ch) = self.char_before_cursor()
            && bracket_pair(ch).is_some()
        {
            return Some(((self.cursor_line, self.cursor_col - 1), ch));
        }
        None
    }

    fn matching_bracket_position(&self) -> Option<BracketMatch> {
        let (source, source_ch) = self.bracket_candidate_at_cursor()?;
        let (open, close, forward) = bracket_pair(source_ch)?;
        let target = if forward {
            self.find_matching_bracket_forward(source, open, close)
        } else {
            self.find_matching_bracket_backward(source, open, close)
        }?;
        let target_ch = self.char_at_position(target)?;

        Some(BracketMatch {
            source,
            target,
            source_ch,
            target_ch,
        })
    }

    fn find_matching_bracket_forward(
        &self,
        source: (usize, usize),
        open: char,
        close: char,
    ) -> Option<(usize, usize)> {
        let mut depth = 0usize;
        for line_index in source.0..self.lines.len() {
            for (col, ch) in self.lines[line_index].chars().enumerate() {
                if line_index == source.0 && col < source.1 {
                    continue;
                }
                if ch == open {
                    depth = depth.saturating_add(1);
                } else if ch == close && depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        return Some((line_index, col));
                    }
                }
            }
        }
        None
    }

    fn find_matching_bracket_backward(
        &self,
        source: (usize, usize),
        open: char,
        close: char,
    ) -> Option<(usize, usize)> {
        let mut depth = 0usize;
        for line_index in (0..=source.0).rev() {
            let chars = self.lines[line_index]
                .chars()
                .enumerate()
                .collect::<Vec<_>>();
            for (col, ch) in chars.into_iter().rev() {
                if line_index == source.0 && col > source.1 {
                    continue;
                }
                if ch == close {
                    depth = depth.saturating_add(1);
                } else if ch == open && depth > 0 {
                    depth -= 1;
                    if depth == 0 {
                        return Some((line_index, col));
                    }
                }
            }
        }
        None
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

    pub fn selection_ranges(&self) -> Vec<EditorSelection> {
        let mut ranges = Vec::new();
        if let Some((start, end)) = self.selection_range()
            && let Some(selection) = EditorSelection::new(start, end)
        {
            ranges.push(selection);
        }
        ranges.extend(self.extra_selections.iter().copied());
        normalize_editor_selections(ranges)
    }

    pub fn selection_count(&self) -> usize {
        let selected = self.selection_ranges().len();
        if selected > 0 {
            selected
        } else if self.has_extra_cursors() {
            self.extra_cursors.len() + 1
        } else {
            0
        }
    }

    pub fn document_highlight_ranges_for_line(
        &self,
        line_index: usize,
    ) -> Vec<lsp::LspDocumentHighlight> {
        self.document_highlights
            .iter()
            .filter(|highlight| highlight.line == line_index)
            .cloned()
            .collect()
    }

    fn has_selection(&self) -> bool {
        !self.selection_ranges().is_empty()
    }

    fn has_extra_cursors(&self) -> bool {
        !self.extra_cursors.is_empty()
    }

    fn clear_selection(&mut self) {
        self.selection_anchor = None;
        self.extra_selections.clear();
        self.extra_cursors.clear();
    }

    fn clear_collapsed_selection(&mut self) {
        if self.selection_anchor == Some(self.cursor_position()) {
            self.selection_anchor = None;
        }
    }

    fn select_all(&mut self) {
        self.selection_anchor = Some((0, 0));
        self.extra_selections.clear();
        self.extra_cursors.clear();
        self.cursor_line = self.lines.len().saturating_sub(1);
        self.cursor_col = self.lines[self.cursor_line].chars().count();
    }

    pub fn fold_end_for_line(&self, line_index: usize) -> Option<usize> {
        if line_index + 1 >= self.lines.len() {
            return None;
        }

        self.brace_fold_end_for_line(line_index)
            .or_else(|| self.indent_fold_end_for_line(line_index))
            .filter(|end| *end > line_index)
    }

    pub fn is_line_folded(&self, line_index: usize) -> bool {
        self.folded_lines.contains(&line_index) && self.fold_end_for_line(line_index).is_some()
    }

    pub fn toggle_fold_at_line(&mut self, line_index: usize) -> Option<bool> {
        let line_index = line_index.min(self.lines.len().saturating_sub(1));
        self.fold_end_for_line(line_index)?;
        if !self.folded_lines.insert(line_index) {
            self.folded_lines.remove(&line_index);
            Some(false)
        } else {
            Some(true)
        }
    }

    pub fn fold_all(&mut self) -> usize {
        self.folded_lines.clear();
        for line_index in 0..self.lines.len() {
            if self.fold_end_for_line(line_index).is_some() {
                self.folded_lines.insert(line_index);
            }
        }
        self.folded_lines.len()
    }

    pub fn unfold_all(&mut self) -> usize {
        let count = self.folded_lines.len();
        self.folded_lines.clear();
        count
    }

    pub fn has_bookmark(&self, line_index: usize) -> bool {
        self.bookmarks.contains(&line_index)
    }

    pub fn toggle_bookmark_at_line(&mut self, line_index: usize) -> Option<bool> {
        if self.lines.is_empty() {
            return None;
        }
        let line_index = line_index.min(self.lines.len().saturating_sub(1));
        if !self.bookmarks.insert(line_index) {
            self.bookmarks.remove(&line_index);
            Some(false)
        } else {
            Some(true)
        }
    }

    fn clear_bookmarks(&mut self) -> usize {
        let count = self.bookmarks.len();
        self.bookmarks.clear();
        count
    }

    fn prune_bookmarks(&mut self) {
        let line_count = self.lines.len();
        self.bookmarks.retain(|line| *line < line_count);
    }

    fn shift_bookmarks_for_insert(&mut self, at: usize, count: usize) {
        if count == 0 || self.bookmarks.is_empty() {
            return;
        }
        self.bookmarks = self
            .bookmarks
            .iter()
            .map(|line| if *line >= at { *line + count } else { *line })
            .collect();
        self.prune_bookmarks();
    }

    fn shift_bookmarks_for_delete(&mut self, start: usize, end: usize) {
        if start > end || self.bookmarks.is_empty() {
            return;
        }
        let count = end - start + 1;
        self.bookmarks = self
            .bookmarks
            .iter()
            .filter_map(|line| {
                if *line < start {
                    Some(*line)
                } else if *line > end {
                    Some(line.saturating_sub(count))
                } else {
                    None
                }
            })
            .collect();
        self.prune_bookmarks();
    }

    fn shift_bookmarks_for_move_up(&mut self, start: usize, end: usize) {
        if start == 0 || self.bookmarks.is_empty() {
            return;
        }
        self.bookmarks = self
            .bookmarks
            .iter()
            .map(|line| {
                if *line == start - 1 {
                    end
                } else if (start..=end).contains(line) {
                    line.saturating_sub(1)
                } else {
                    *line
                }
            })
            .collect();
    }

    fn shift_bookmarks_for_move_down(&mut self, start: usize, end: usize) {
        if end + 1 >= self.lines.len() || self.bookmarks.is_empty() {
            return;
        }
        self.bookmarks = self
            .bookmarks
            .iter()
            .map(|line| {
                if *line == end + 1 {
                    start
                } else if (start..=end).contains(line) {
                    *line + 1
                } else {
                    *line
                }
            })
            .collect();
        self.prune_bookmarks();
    }

    pub fn visible_line_indices(&self) -> Vec<usize> {
        let mut indices = Vec::with_capacity(self.lines.len());
        let mut line_index = 0;
        while line_index < self.lines.len() {
            indices.push(line_index);
            if self.folded_lines.contains(&line_index)
                && let Some(end) = self.fold_end_for_line(line_index)
            {
                line_index = end.saturating_add(1);
            } else {
                line_index += 1;
            }
        }
        indices
    }

    pub fn visible_line_at(&self, row: usize) -> Option<usize> {
        self.visible_line_indices().get(row).copied()
    }

    pub fn visible_row_for_line(&self, target_line: usize) -> Option<usize> {
        let mut row = 0;
        let mut line_index = 0;
        while line_index < self.lines.len() {
            if line_index == target_line {
                return Some(row);
            }
            if self.folded_lines.contains(&line_index)
                && let Some(end) = self.fold_end_for_line(line_index)
            {
                if target_line <= end {
                    return Some(row);
                }
                line_index = end.saturating_add(1);
            } else {
                line_index += 1;
            }
            row += 1;
        }
        None
    }

    fn unfold_line_containing(&mut self, target_line: usize) -> bool {
        let folded = self.folded_lines.iter().copied().collect::<Vec<_>>();
        let mut changed = false;
        for start in folded {
            if start < target_line
                && self
                    .fold_end_for_line(start)
                    .is_some_and(|end| target_line <= end)
            {
                changed |= self.folded_lines.remove(&start);
            }
        }
        changed
    }

    fn brace_fold_end_for_line(&self, line_index: usize) -> Option<usize> {
        for (opener, closer) in [('{', '}'), ('[', ']'), ('(', ')')] {
            if let Some(end) = self.matching_delimiter_end(line_index, opener, closer) {
                return Some(end);
            }
        }
        None
    }

    fn matching_delimiter_end(
        &self,
        line_index: usize,
        opener: char,
        closer: char,
    ) -> Option<usize> {
        let mut depth = 0usize;
        let mut started = false;
        for (index, line) in self.lines.iter().enumerate().skip(line_index) {
            for ch in line.chars() {
                if ch == opener {
                    depth += 1;
                    started = true;
                } else if ch == closer && started {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        return (index > line_index).then_some(index);
                    }
                }
            }
            if index == line_index && !started {
                return None;
            }
        }
        None
    }

    fn indent_fold_end_for_line(&self, line_index: usize) -> Option<usize> {
        let base_indent = leading_whitespace(self.lines.get(line_index)?)
            .chars()
            .count();
        if self.lines[line_index].trim().is_empty() {
            return None;
        }

        let mut end = None;
        for index in line_index + 1..self.lines.len() {
            let line = &self.lines[index];
            if line.trim().is_empty() {
                if end.is_some() {
                    end = Some(index);
                }
                continue;
            }
            let indent = leading_whitespace(line).chars().count();
            if indent <= base_indent {
                break;
            }
            end = Some(index);
        }
        end
    }

    fn command_line_range(&self) -> (usize, usize, bool) {
        let Some((start, end)) = self.selection_range() else {
            return (self.cursor_line, self.cursor_line, false);
        };
        let start_line = start.0.min(self.lines.len().saturating_sub(1));
        let mut end_line = end.0.min(self.lines.len().saturating_sub(1));
        if end.1 == 0 && end_line > start_line {
            end_line -= 1;
        }
        (start_line, end_line, true)
    }

    fn select_line_range(&mut self, start: usize, end: usize) {
        let start = start.min(self.lines.len().saturating_sub(1));
        let end = end.min(self.lines.len().saturating_sub(1));
        self.selection_anchor = Some((start, 0));
        self.extra_selections.clear();
        self.extra_cursors.clear();
        self.cursor_line = end;
        self.cursor_col = self.lines[end].chars().count();
    }

    fn select_line_range_from_anchor(&mut self, anchor: usize, active: usize) {
        let anchor = anchor.min(self.lines.len().saturating_sub(1));
        let active = active.min(self.lines.len().saturating_sub(1));
        self.extra_selections.clear();
        self.extra_cursors.clear();
        if active < anchor {
            self.selection_anchor = Some((anchor, self.lines[anchor].chars().count()));
            self.cursor_line = active;
            self.cursor_col = 0;
        } else {
            self.selection_anchor = Some((anchor, 0));
            self.cursor_line = active;
            self.cursor_col = self.lines[active].chars().count();
        }
    }

    pub fn selected_text(&self) -> Option<String> {
        let ranges = self.selection_ranges();
        if ranges.is_empty() {
            return None;
        }
        Some(
            ranges
                .into_iter()
                .map(|range| self.text_in_range(range.start, range.end))
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    fn current_line_clipboard_text(&self) -> Option<String> {
        let mut text = self.lines.get(self.cursor_line)?.clone();
        text.push('\n');
        Some(text)
    }

    fn delete_selection(&mut self) -> Option<String> {
        let ranges = self.selection_ranges();
        if ranges.is_empty() {
            return None;
        }
        let deleted = ranges
            .iter()
            .map(|range| self.text_in_range(range.start, range.end))
            .collect::<Vec<_>>()
            .join("\n");
        self.push_undo();
        self.replace_ranges_raw(&ranges, "", true);
        self.dirty = true;
        Some(deleted)
    }

    fn replace_selection_with(&mut self, text: &str) {
        let ranges = self.selection_ranges();
        if ranges.is_empty() {
            return;
        }
        self.push_undo();
        self.replace_ranges_raw(&ranges, text, true);
        self.dirty = true;
    }

    fn replace_range_as_edit(
        &mut self,
        start: (usize, usize),
        end: (usize, usize),
        replacement: &str,
    ) -> bool {
        if start > end
            || start.0 >= self.lines.len()
            || end.0 >= self.lines.len()
            || start.1 > self.lines[start.0].chars().count()
            || end.1 > self.lines[end.0].chars().count()
        {
            return false;
        }

        if self.text_in_range(start, end) == replacement {
            self.set_cursor_raw(end.0, end.1);
            return false;
        }

        self.push_undo();
        self.delete_range_raw(start, end);
        self.insert_text_raw(replacement);
        self.dirty = true;
        true
    }

    fn wrap_selection_with(&mut self, open: char, close: char) {
        let ranges = self.selection_ranges();
        if ranges.is_empty() {
            return;
        };
        self.push_undo();
        let replacements = ranges
            .iter()
            .map(|range| {
                let selected = self.text_in_range(range.start, range.end);
                (range.start, range.end, format!("{open}{selected}{close}"))
            })
            .collect::<Vec<_>>();
        self.replace_distinct_ranges_raw(&replacements, false);
        let first = ranges[0];
        let open_width = open.to_string().chars().count();
        self.selection_anchor = Some((first.start.0, first.start.1 + open_width));
        self.cursor_line = first.end.0;
        self.cursor_col = first.end.1 + open_width;
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

    fn add_next_occurrence_selection(&mut self) -> Option<(usize, String)> {
        let (needle, whole_word, primary) = self.occurrence_seed()?;
        if self.selection_range().is_none() {
            self.selection_anchor = Some(primary.start);
            self.cursor_line = primary.end.0;
            self.cursor_col = primary.end.1;
        }

        let selected = self.selection_ranges();
        let after = selected
            .last()
            .map(|selection| selection.end)
            .unwrap_or(primary.end);
        let next = find_next_occurrence_after(&self.lines, &needle, after, whole_word, &selected)?;
        self.extra_selections = normalize_editor_selections(selected);
        self.selection_anchor = Some(next.start);
        self.cursor_line = next.end.0;
        self.cursor_col = next.end.1;
        Some((self.selection_count(), needle))
    }

    fn select_all_occurrences(&mut self) -> Option<(usize, String)> {
        let (needle, whole_word, primary) = self.occurrence_seed()?;
        let mut ranges = occurrence_ranges(&self.lines, &needle, whole_word);
        if ranges.is_empty() {
            ranges.push(primary);
        }
        let primary_index = ranges
            .iter()
            .position(|selection| *selection == primary)
            .unwrap_or(0);
        let primary = ranges.remove(primary_index);
        self.selection_anchor = Some(primary.start);
        self.cursor_line = primary.end.0;
        self.cursor_col = primary.end.1;
        self.extra_selections = normalize_editor_selections(ranges);
        Some((self.selection_count(), needle))
    }

    fn occurrence_seed(&self) -> Option<(String, bool, EditorSelection)> {
        if let Some((start, end)) = self.selection_range() {
            let selected = self.text_in_range(start, end);
            if selected.is_empty() || selected.contains('\n') {
                return None;
            }
            return Some((
                selected.clone(),
                is_identifier_token(&selected),
                EditorSelection { start, end },
            ));
        }

        let line = self.lines.get(self.cursor_line)?;
        let (start_col, end_col, token) = identifier_range_at_char(line, self.cursor_col)?;
        Some((
            token,
            true,
            EditorSelection {
                start: (self.cursor_line, start_col),
                end: (self.cursor_line, end_col),
            },
        ))
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
        if start_line != end_line {
            self.shift_bookmarks_for_delete(start_line + 1, end_line);
        }
        self.cursor_line = start_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = start_col;
        self.clamp_cursor_col();
        self.clear_selection();
    }

    fn insert_text_at_cursors(&mut self, text: &str) {
        let mut replacements = self
            .extra_cursors
            .iter()
            .map(|cursor| (*cursor, *cursor, text.to_owned()))
            .collect::<Vec<_>>();
        replacements.push((
            self.cursor_position(),
            self.cursor_position(),
            text.to_owned(),
        ));
        self.push_undo();
        self.replace_distinct_ranges_raw(&replacements, !text.contains('\n'));
        self.dirty = true;
    }

    fn replace_ranges_raw(
        &mut self,
        ranges: &[EditorSelection],
        replacement: &str,
        keep_cursors: bool,
    ) {
        let replacements = ranges
            .iter()
            .map(|range| (range.start, range.end, replacement.to_owned()))
            .collect::<Vec<_>>();
        self.replace_distinct_ranges_raw(
            &replacements,
            keep_cursors && !replacement.contains('\n'),
        );
    }

    fn replace_distinct_ranges_raw(
        &mut self,
        replacements: &[EditorReplacement],
        keep_cursors: bool,
    ) {
        if replacements.is_empty() {
            return;
        }

        let mut ordered = replacements.to_vec();
        ordered.sort_by_key(|replacement| std::cmp::Reverse(replacement.0));
        let mut final_cursors = Vec::new();
        let first_start = ordered
            .iter()
            .map(|(start, _, _)| *start)
            .min()
            .unwrap_or((0, 0));
        let first_replacement = ordered
            .iter()
            .find(|(start, _, _)| *start == first_start)
            .map(|(_, _, replacement)| replacement.clone())
            .unwrap_or_default();

        for (start, end, replacement) in ordered {
            if keep_cursors {
                shift_cursor_positions_for_replacement(
                    &mut final_cursors,
                    start,
                    end,
                    &replacement,
                );
                final_cursors.push(replacement_end_position(start, &replacement));
            }
            self.delete_range_raw(start, end);
            self.insert_text_raw(&replacement);
        }

        let final_cursor = replacement_end_position(first_start, &first_replacement);
        self.clear_selection();
        if keep_cursors && !final_cursors.is_empty() {
            final_cursors.sort();
            final_cursors.dedup();
            self.set_cursor_raw(final_cursors[0].0, final_cursors[0].1);
            self.extra_cursors = final_cursors.into_iter().skip(1).collect();
        } else {
            self.set_cursor_raw(final_cursor.0, final_cursor.1);
        }
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
            selection_anchor: self.selection_anchor,
            extra_selections: self.extra_selections.clone(),
            extra_cursors: self.extra_cursors.clone(),
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
        self.selection_anchor = snapshot.selection_anchor;
        self.extra_selections = snapshot.extra_selections;
        self.extra_cursors = snapshot.extra_cursors;
        self.folded_lines.clear();
        self.document_highlights.clear();
        self.prune_bookmarks();
        self.clamp_cursor_col();
        self.clamp_selections();
    }

    fn apply_view_state_from(&mut self, other: &Self) {
        self.scroll = other.scroll;
        self.horizontal_scroll = other.horizontal_scroll;
        self.cursor_line = other.cursor_line.min(self.lines.len().saturating_sub(1));
        self.cursor_col = other.cursor_col;
        self.selection_anchor = other.selection_anchor;
        self.extra_selections = other.extra_selections.clone();
        self.extra_cursors = other.extra_cursors.clone();
        self.folded_lines = other.folded_lines.clone();
        self.bookmarks = other.bookmarks.clone();
        self.prune_bookmarks();
        self.clamp_cursor_col();
        self.clamp_selections();
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > 200 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
        self.folded_lines.clear();
        self.document_highlights.clear();
    }

    fn clamp_selections(&mut self) {
        if let Some((line, col)) = self.selection_anchor {
            let line = line.min(self.lines.len().saturating_sub(1));
            let col = col.min(self.lines[line].chars().count());
            self.selection_anchor = Some((line, col));
        }
        self.extra_selections = normalize_editor_selections(
            self.extra_selections
                .iter()
                .filter_map(|selection| {
                    let start_line = selection.start.0.min(self.lines.len().saturating_sub(1));
                    let end_line = selection.end.0.min(self.lines.len().saturating_sub(1));
                    let start_col = selection
                        .start
                        .1
                        .min(self.lines[start_line].chars().count());
                    let end_col = selection.end.1.min(self.lines[end_line].chars().count());
                    EditorSelection::new((start_line, start_col), (end_line, end_col))
                })
                .collect(),
        );
        self.extra_cursors = self
            .extra_cursors
            .iter()
            .map(|(line, col)| {
                let line = (*line).min(self.lines.len().saturating_sub(1));
                let col = (*col).min(self.lines[line].chars().count());
                (line, col)
            })
            .collect();
        self.extra_cursors.sort();
        self.extra_cursors.dedup();
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptKind {
    NewFile,
    NewDir,
    Rename(PathBuf),
    DeletePaths(Vec<PathBuf>),
    ExplorerFilter,
    OpenFolder,
    Search,
    ReplaceFind { all: bool },
    ReplaceWith { needle: String, all: bool },
    WorkspaceReplaceFind,
    WorkspaceReplaceWith { needle: String },
    RenameSymbol { old: String },
    SaveAs,
    SaveAsClose { index: usize },
    CreateGitBranch,
    CommitStagedSourceControlChanges,
    CommitAllSourceControlChanges(Vec<PathBuf>),
    DiscardSourceControlPath(PathBuf),
    DiscardAllSourceControlChanges(Vec<PathBuf>),
    TerminalSearch,
    RunTerminalCommand,
    RenameTerminal,
    GotoLine,
    QuitDirty,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptState {
    pub kind: PromptKind,
    pub input: String,
    pub cursor: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuickPanelKind {
    OpenFile,
    OpenEditors,
    Completions,
    CodeActions,
    DirtyClose { index: usize },
    ExplorerContextMenu,
    EditorContextMenu,
    TerminalContextMenu,
    WorkspaceSearch,
    DocumentSymbols,
    WorkspaceSymbols,
    LspHover,
    SignatureHelp,
    Definitions,
    TypeDefinitions,
    Implementations,
    IncomingCalls,
    OutgoingCalls,
    References,
    LspReferences,
    Problems,
    Bookmarks,
    SourceControl,
    Branches,
    Tasks,
    TerminalCommandHistory,
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
pub enum ProblemSeverity {
    Error,
    Warning,
    Note,
    Help,
    Problem,
}

impl ProblemSeverity {
    pub fn from_label(label: &str) -> Self {
        match label.split_whitespace().next().unwrap_or("problem") {
            "error" => Self::Error,
            "warning" => Self::Warning,
            "note" => Self::Note,
            "help" => Self::Help,
            _ => Self::Problem,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Note => "note",
            Self::Help => "help",
            Self::Problem => "problem",
        }
    }

    fn rank(self) -> usize {
        match self {
            Self::Error => 0,
            Self::Warning => 1,
            Self::Problem => 2,
            Self::Note => 3,
            Self::Help => 4,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineProblemSummary {
    pub severity: ProblemSeverity,
    pub count: usize,
    pub col: usize,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EditorVisualRow {
    pub line: usize,
    pub start_col: usize,
    pub continuation: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BracketMatch {
    source: (usize, usize),
    target: (usize, usize),
    source_ch: char,
    target_ch: char,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorLocation {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    QuickOpen,
    OpenFolder,
    ShowExplorerFiles,
    ShowOutline,
    ToggleSidebarMode,
    TriggerSuggest,
    WorkspaceSearch,
    DocumentSymbols,
    WorkspaceSymbols,
    ShowHover,
    SignatureHelp,
    GoToDefinition,
    GoToTypeDefinition,
    GoToImplementation,
    GoToMatchingBracket,
    ShowIncomingCalls,
    ShowOutgoingCalls,
    HighlightSymbol,
    ClearDocumentHighlights,
    FindReferences,
    CodeAction,
    GoBack,
    GoForward,
    RenameSymbol,
    WorkspaceReplace,
    RunWorkspaceCheck,
    RunLspDiagnostics,
    ShowProblems,
    ShowBookmarks,
    ToggleBookmark,
    NextBookmark,
    PreviousBookmark,
    ClearBookmarks,
    ShowSourceControl,
    ShowGitBranches,
    CheckoutGitBranch,
    CreateGitBranch,
    OpenSourceControlDiff,
    StageSourceControlItem,
    UnstageSourceControlItem,
    StageAllChanges,
    UnstageAllChanges,
    CommitStagedChanges,
    CommitAllChanges,
    DiscardSourceControlItem,
    DiscardAllChanges,
    RunTask,
    RunActiveFileInTerminal,
    RunSelectedExplorerFileInTerminal,
    NewUntitledFile,
    ShowOpenEditors,
    SelectEditorTab(usize),
    SaveFile,
    SaveAs,
    SaveAll,
    RevertFile,
    FormatDocument,
    CloseActiveTab,
    ReopenClosedEditor,
    CloseAllTabs,
    CloseOtherTabs,
    CloseTabsToRight,
    OpenActiveTabToSide,
    OpenSelectedExplorerItemToSide,
    CloseEditorSplit,
    SaveAndCloseTab(usize),
    DiscardAndCloseTab(usize),
    CancelCloseTab,
    CloseSavedTabs,
    OpenSelectedExplorerItem,
    OpenSelectedFolderAsWorkspace,
    NewFile,
    NewFolder,
    RenameSelected,
    DeleteSelected,
    CopySelectedExplorerItem,
    CutSelectedExplorerItem,
    PasteIntoSelectedExplorerItem,
    DuplicateSelectedExplorerItem,
    CompareSelectedFiles,
    RefreshExplorer,
    CollapseExplorer,
    CycleExplorerSort,
    SortExplorerByName,
    SortExplorerByType,
    SortExplorerByModified,
    SortExplorerBySize,
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
    AddSelectionToNextMatch,
    SelectAllOccurrences,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    ToggleLineComment,
    ToggleBlockComment,
    ToggleWordWrap,
    ToggleFold,
    FoldAll,
    UnfoldAll,
    TrimTrailingWhitespace,
    IndentLine,
    OutdentLine,
    SelectAll,
    CopySelection,
    CutSelection,
    PasteClipboard,
    RunSelectionInTerminal,
    CopyTerminalSelection,
    CopyTerminalOutput,
    PasteClipboardToTerminal,
    FindInTerminal,
    TerminalSearchNext,
    TerminalSearchPrevious,
    RunTerminalCommand,
    RunRecentTerminalCommand,
    FocusExplorer,
    FocusEditor,
    FocusTerminal,
    ClearTerminal,
    RestartTerminal,
    RenameTerminal,
    NewTerminal,
    NewTerminalHere,
    SplitTerminal,
    CloseTerminal,
    NextTerminal,
    PreviousTerminal,
    ToggleTerminalFocus,
    ToggleTerminalMaximized,
    ScrollTerminalToBottom,
    IncreaseTerminalHeight,
    DecreaseTerminalHeight,
}

#[derive(Debug, Clone)]
pub struct QuickPanel {
    pub kind: QuickPanelKind,
    pub query: String,
    pub query_cursor: usize,
    pub items: Vec<QuickItem>,
    pub selected: usize,
    pub scroll: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletionState {
    pub path: PathBuf,
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub prefix: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorHoverInfo {
    pub symbol: String,
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
    pub definition_count: usize,
    pub reference_count: usize,
    pub definition: Option<EditorLocation>,
    pub definition_detail: Option<String>,
    pub definition_preview: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardAction {
    Copy,
    Cut,
}

#[derive(Debug, Clone)]
pub struct ExplorerClipboard {
    pub action: ClipboardAction,
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
struct ExplorerDragState {
    source_paths: Vec<PathBuf>,
    source_index: usize,
    target_index: Option<usize>,
    moved: bool,
    copy: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalSelection {
    pub terminal_id: usize,
    pub anchor: (u16, u16),
    pub head: (u16, u16),
}

#[derive(Debug, Clone)]
pub struct TerminalSearchState {
    pub terminal_id: usize,
    pub needle: String,
    pub matches: Vec<TerminalSearchMatch>,
    pub selected: usize,
}

pub struct TerminalSession {
    pub id: usize,
    pub title: String,
    pub title_locked: bool,
    pub cwd: PathBuf,
    pub shell: ShellPanel,
    pub exited: bool,
    pub exit_status: Option<ShellExitStatus>,
}

impl TerminalSession {
    fn new(id: usize, cwd: PathBuf) -> Result<Self> {
        Self::with_title(id, format!("term {id}"), cwd)
    }

    fn here(id: usize, cwd: PathBuf) -> Result<Self> {
        let title = cwd
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| format!("term {id}: {name}"))
            .unwrap_or_else(|| format!("term {id}: /"));
        Self::with_title(id, title, cwd)
    }

    fn with_title(id: usize, title: String, cwd: PathBuf) -> Result<Self> {
        Self::with_title_lock(id, title, cwd, false)
    }

    fn with_locked_title(id: usize, title: String, cwd: PathBuf) -> Result<Self> {
        Self::with_title_lock(id, title, cwd, true)
    }

    fn with_title_lock(id: usize, title: String, cwd: PathBuf, title_locked: bool) -> Result<Self> {
        Ok(Self {
            id,
            title,
            title_locked,
            shell: ShellPanel::new(cwd.clone())?,
            cwd,
            exited: false,
            exit_status: None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitStatusKind {
    Modified,
    Added,
    Deleted,
    Renamed,
    Untracked,
    Conflicted,
}

impl GitStatusKind {
    fn from_porcelain(x: u8, y: u8) -> Option<Self> {
        match (x, y) {
            (b'?', b'?') => Some(Self::Untracked),
            _ if [x, y].contains(&b'U') => Some(Self::Conflicted),
            _ if [x, y].contains(&b'A') => Some(Self::Added),
            _ if [x, y].contains(&b'R') || [x, y].contains(&b'C') => Some(Self::Renamed),
            _ if [x, y].contains(&b'D') => Some(Self::Deleted),
            _ if [x, y].contains(&b'M') || [x, y].contains(&b'T') => Some(Self::Modified),
            _ => None,
        }
    }

    pub fn marker(self) -> &'static str {
        match self {
            Self::Modified => "git:M",
            Self::Added => "git:A",
            Self::Deleted => "git:D",
            Self::Renamed => "git:R",
            Self::Untracked => "git:?",
            Self::Conflicted => "git:!",
        }
    }

    fn short_label(self) -> &'static str {
        match self {
            Self::Modified => "M",
            Self::Added => "A",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Untracked => "?",
            Self::Conflicted => "!",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Modified => "Modified",
            Self::Added => "Added",
            Self::Deleted => "Deleted",
            Self::Renamed => "Renamed",
            Self::Untracked => "Untracked",
            Self::Conflicted => "Conflicted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitStatusEntry {
    path: PathBuf,
    kind: GitStatusKind,
    index: u8,
    worktree: u8,
}

impl GitStatusEntry {
    fn can_stage(&self) -> bool {
        self.index == b'?' || self.worktree == b'?' || self.worktree != b' '
    }

    fn can_unstage(&self) -> bool {
        self.index != b' ' && self.index != b'?'
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingKeyChord {
    CtrlK,
}

pub struct App {
    pub root: PathBuf,
    pub explorer: FsTree,
    pub tabs: Vec<EditorTab>,
    pub active_tab: Option<usize>,
    pub closed_tabs: Vec<EditorTab>,
    pub editor_split: Option<usize>,
    pub active_editor_pane: usize,
    pub sidebar_mode: SidebarMode,
    pub outline_selected: usize,
    pub outline_scroll: usize,
    pub focus: FocusPanel,
    pub hover: HoverTarget,
    pub hit_regions: HitRegions,
    pub terminals: Vec<TerminalSession>,
    pub active_terminal: usize,
    pub split_terminal: Option<usize>,
    next_terminal_id: usize,
    next_untitled_id: usize,
    pub syntax: SyntaxHighlighter,
    pub should_quit: bool,
    pub explorer_height: usize,
    pub editor_height: usize,
    pub editor_width: usize,
    pub word_wrap: bool,
    pub terminal_height: usize,
    pub terminal_rows: u16,
    pub terminal_maximized: bool,
    pub terminal_resize_dragging: bool,
    pub editor_selection_dragging: bool,
    pub editor_gutter_dragging: Option<usize>,
    pub last_error: Option<String>,
    pub prompt: Option<PromptState>,
    pub message: Option<String>,
    pub search_needle: Option<String>,
    pub explorer_filter: Option<String>,
    pub show_hidden: bool,
    pub show_ignored: bool,
    pub quick_panel: Option<QuickPanel>,
    pub completion_state: Option<CompletionState>,
    pub lsp_completion_items: Vec<QuickItem>,
    pub lsp_document_symbol_path: Option<PathBuf>,
    pub lsp_document_symbol_items: Vec<QuickItem>,
    pub lsp_workspace_symbol_query: Option<String>,
    pub lsp_workspace_symbol_items: Vec<QuickItem>,
    pub lsp_code_actions: Vec<lsp::LspCodeAction>,
    pub editor_hover: Option<EditorHoverInfo>,
    pub quick_panel_height: usize,
    pub explorer_multi_selection: BTreeSet<PathBuf>,
    pub explorer_selection_anchor: Option<PathBuf>,
    pub explorer_clipboard: Option<ExplorerClipboard>,
    explorer_drag: Option<ExplorerDragState>,
    pub editor_clipboard: Option<String>,
    pub git_statuses: HashMap<PathBuf, GitStatusKind>,
    pub git_dirty_dirs: HashSet<PathBuf>,
    pub git_branch: Option<String>,
    pub navigation_back: Vec<EditorLocation>,
    pub navigation_forward: Vec<EditorLocation>,
    pub terminal_selection: Option<TerminalSelection>,
    pub terminal_search: Option<TerminalSearchState>,
    pub terminal_command_history: Vec<String>,
    pub problems: Vec<QuickItem>,
    pending_clipboard_export: Option<String>,
    pending_key_chord: Option<PendingKeyChord>,
    workspace_snapshot: Option<WorkspaceSnapshot>,
    workspace_visible_paths: HashSet<PathBuf>,
    last_workspace_tree_check: Instant,
}

impl App {
    pub fn new(root: PathBuf) -> Result<Self> {
        let requested_path = root.canonicalize().unwrap_or(root);
        let (root, initial_file) = if requested_path.is_file() {
            let parent = requested_path
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or(env::current_dir()?);
            (parent, Some(requested_path))
        } else {
            (requested_path, None)
        };
        let explorer = FsTree::new(root.clone())?;
        let terminal = TerminalSession::new(1, root.clone())?;
        let (git_statuses, git_dirty_dirs) = load_git_status(&root);
        let git_branch = git_top_level(&root).and_then(|top_level| git_current_branch(&top_level));
        let workspace_snapshot = workspace_snapshot(&root, true, false).ok();
        let workspace_visible_paths =
            workspace_visible_paths(&root, true, false).unwrap_or_else(|_| {
                let mut paths = HashSet::new();
                paths.insert(root.clone());
                paths
            });
        let mut app = Self {
            root: root.clone(),
            explorer,
            tabs: Vec::new(),
            active_tab: None,
            closed_tabs: Vec::new(),
            editor_split: None,
            active_editor_pane: 0,
            sidebar_mode: SidebarMode::Files,
            outline_selected: 0,
            outline_scroll: 0,
            focus: FocusPanel::Explorer,
            hover: HoverTarget::None,
            hit_regions: HitRegions::default(),
            terminals: vec![terminal],
            active_terminal: 0,
            split_terminal: None,
            next_terminal_id: 2,
            next_untitled_id: 1,
            syntax: SyntaxHighlighter::new(),
            should_quit: false,
            explorer_height: 0,
            editor_height: 0,
            editor_width: 0,
            word_wrap: false,
            terminal_height: 0,
            terminal_rows: 10,
            terminal_maximized: false,
            terminal_resize_dragging: false,
            editor_selection_dragging: false,
            editor_gutter_dragging: None,
            last_error: None,
            prompt: None,
            message: Some("F1/Ctrl-Shift-P commands | Ctrl-P files | Editor: Ctrl-A/C/X/V selection | Terminal: Ctrl-Q quit".to_owned()),
            search_needle: None,
            explorer_filter: None,
            show_hidden: true,
            show_ignored: false,
            quick_panel: None,
            completion_state: None,
            lsp_completion_items: Vec::new(),
            lsp_document_symbol_path: None,
            lsp_document_symbol_items: Vec::new(),
            lsp_workspace_symbol_query: None,
            lsp_workspace_symbol_items: Vec::new(),
            lsp_code_actions: Vec::new(),
            editor_hover: None,
            quick_panel_height: 0,
            explorer_multi_selection: BTreeSet::new(),
            explorer_selection_anchor: None,
            explorer_clipboard: None,
            explorer_drag: None,
            editor_clipboard: None,
            git_statuses,
            git_dirty_dirs,
            git_branch,
            navigation_back: Vec::new(),
            navigation_forward: Vec::new(),
            terminal_selection: None,
            terminal_search: None,
            terminal_command_history: Vec::new(),
            problems: Vec::new(),
            pending_clipboard_export: None,
            pending_key_chord: None,
            workspace_snapshot,
            workspace_visible_paths,
            last_workspace_tree_check: Instant::now(),
        };

        if let Some(file) = initial_file {
            app.open_file(&file);
            let _ = app.explorer.reveal(&file);
            if app.active_tab().is_some_and(|tab| !tab.read_only) {
                app.message = Some(format!("opened {}", file.display()));
            }
        }

        Ok(app)
    }

    pub fn visible_nodes(&self) -> Vec<VisibleNode> {
        filtered_visible_nodes(
            self.explorer.visible_nodes(),
            &self.root,
            self.show_hidden,
            self.show_ignored,
            &self.workspace_visible_paths,
            self.explorer_filter.as_deref(),
        )
    }

    pub fn visible_outline_items(&self) -> Vec<QuickItem> {
        self.document_symbol_items("")
    }

    fn show_files_sidebar(&mut self) {
        self.sidebar_mode = SidebarMode::Files;
        self.focus = FocusPanel::Explorer;
        self.message = Some("sidebar: files".to_owned());
    }

    fn show_outline(&mut self) -> Result<()> {
        self.sidebar_mode = SidebarMode::Outline;
        self.focus = FocusPanel::Explorer;
        self.refresh_outline_symbols()?;
        self.sync_outline_selection_to_cursor();
        let count = self.visible_outline_items().len();
        self.message = if count == 0 {
            Some("outline: no symbols in active editor".to_owned())
        } else {
            Some(format!("outline: {count} symbol(s)"))
        };
        Ok(())
    }

    fn toggle_sidebar_mode(&mut self) -> Result<()> {
        if self.sidebar_mode == SidebarMode::Outline {
            self.show_files_sidebar();
            Ok(())
        } else {
            self.show_outline()
        }
    }

    fn refresh_outline_symbols(&mut self) -> Result<()> {
        self.refresh_lsp_document_symbol_cache()?;
        self.outline_selected = self
            .outline_selected
            .min(self.visible_outline_items().len().saturating_sub(1));
        self.ensure_outline_selection_visible();
        Ok(())
    }

    fn refresh_lsp_document_symbol_cache(&mut self) -> Result<()> {
        self.lsp_document_symbol_items.clear();
        self.lsp_document_symbol_path = self.active_tab().map(|tab| tab.path.clone());
        if self.active_tab().is_none() {
            return Ok(());
        }
        match self.lsp_document_symbol_items_for_active_tab() {
            Ok(items) => {
                self.lsp_document_symbol_items = items;
            }
            Err(error) => {
                self.lsp_document_symbol_path = None;
                self.last_error = Some(format!("LSP document symbols unavailable: {error}"));
            }
        }
        Ok(())
    }

    fn cached_lsp_document_symbol_items_for_active_tab(&self) -> Option<Vec<QuickItem>> {
        let tab = self.active_tab()?;
        if tab.dirty {
            return None;
        }
        if self.lsp_document_symbol_path.as_ref()? != &tab.path {
            return None;
        }
        (!self.lsp_document_symbol_items.is_empty()).then(|| self.lsp_document_symbol_items.clone())
    }

    fn move_outline_selection(&mut self, delta: isize) {
        let len = self.visible_outline_items().len();
        if len == 0 {
            self.outline_selected = 0;
            self.outline_scroll = 0;
            return;
        }
        self.outline_selected = add_signed(self.outline_selected, delta).min(len.saturating_sub(1));
        self.ensure_outline_selection_visible();
    }

    fn set_outline_selection(&mut self, index: usize) {
        let len = self.visible_outline_items().len();
        if len == 0 {
            self.outline_selected = 0;
            self.outline_scroll = 0;
            return;
        }
        self.outline_selected = index.min(len - 1);
        self.ensure_outline_selection_visible();
    }

    fn ensure_outline_selection_visible(&mut self) {
        let height = self.explorer_height.max(1);
        if self.outline_selected < self.outline_scroll {
            self.outline_scroll = self.outline_selected;
        } else if self.outline_selected >= self.outline_scroll + height {
            self.outline_scroll = self.outline_selected.saturating_sub(height - 1);
        }
    }

    fn sync_outline_selection_to_cursor(&mut self) {
        let Some(tab) = self.active_tab() else {
            self.outline_selected = 0;
            self.outline_scroll = 0;
            return;
        };
        let cursor_line = tab.cursor_line;
        let items = self.visible_outline_items();
        let Some((index, _)) = items
            .iter()
            .enumerate()
            .filter_map(|(index, item)| item.line.map(|line| (index, line)))
            .take_while(|(_, line)| *line <= cursor_line)
            .last()
        else {
            self.set_outline_selection(0);
            return;
        };
        self.set_outline_selection(index);
    }

    fn jump_to_selected_outline_symbol(&mut self) {
        self.jump_to_outline_symbol(self.outline_selected);
    }

    fn jump_to_outline_symbol(&mut self, index: usize) {
        let Some(item) = self.visible_outline_items().get(index).cloned() else {
            self.message = Some("outline has no symbol to open".to_owned());
            return;
        };
        self.outline_selected = index;
        self.ensure_outline_selection_visible();
        let label = item.label.clone();
        self.open_quick_item(item, None);
        self.sidebar_mode = SidebarMode::Outline;
        self.message = Some(format!("outline jumped to {label}"));
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

        if let Some(chord) = self.pending_key_chord.take()
            && self.handle_pending_key_chord(chord, key)?
        {
            return Ok(());
        }

        if !matches!(self.focus, FocusPanel::Terminal) && key.code == KeyCode::F(1) {
            self.open_quick_panel(QuickPanelKind::CommandPalette)?;
            return Ok(());
        }
        let terminal_child_owns_keyboard = self.terminal_child_owns_keyboard();
        if key.code == KeyCode::F(6) {
            self.toggle_terminal_focus();
            return Ok(());
        }
        if terminal_child_owns_keyboard
            && matches!(
                key.code,
                KeyCode::F(7) | KeyCode::F(8) | KeyCode::F(9) | KeyCode::F(12)
            )
        {
            return self.handle_terminal_key(key);
        }
        if key.code == KeyCode::F(12) {
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

        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('5')
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && !terminal_child_owns_keyboard =>
                {
                    self.split_terminal()?;
                    return Ok(());
                }
                KeyCode::Char('`') | KeyCode::Char('~')
                    if key.modifiers.contains(KeyModifiers::SHIFT)
                        && !terminal_child_owns_keyboard =>
                {
                    self.new_terminal()?;
                    return Ok(());
                }
                KeyCode::Char('`') if !key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.toggle_terminal_focus();
                    return Ok(());
                }
                KeyCode::Char('j') if !terminal_child_owns_keyboard => {
                    self.toggle_terminal_maximized();
                    return Ok(());
                }
                KeyCode::Char('k')
                    if key.modifiers == KeyModifiers::CONTROL
                        && !matches!(self.focus, FocusPanel::Terminal) =>
                {
                    self.pending_key_chord = Some(PendingKeyChord::CtrlK);
                    self.message = Some("Ctrl-K chord: press S to Save All".to_owned());
                    return Ok(());
                }
                KeyCode::PageDown if matches!(self.focus, FocusPanel::Terminal) => {
                    if !terminal_child_owns_keyboard {
                        self.next_terminal();
                        return Ok(());
                    }
                }
                KeyCode::PageUp if matches!(self.focus, FocusPanel::Terminal) => {
                    if !terminal_child_owns_keyboard {
                        self.previous_terminal();
                        return Ok(());
                    }
                }
                KeyCode::PageDown => {
                    self.next_tab();
                    return Ok(());
                }
                KeyCode::PageUp => {
                    self.previous_tab();
                    return Ok(());
                }
                _ if matches!(self.focus, FocusPanel::Terminal) => {}
                KeyCode::Char('P') => {
                    self.open_quick_panel(QuickPanelKind::CommandPalette)?;
                    return Ok(());
                }
                KeyCode::Char('O') => {
                    self.open_quick_panel(QuickPanelKind::DocumentSymbols)?;
                    return Ok(());
                }
                KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.open_quick_panel(QuickPanelKind::CommandPalette)?;
                    return Ok(());
                }
                KeyCode::Char('o') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.open_quick_panel(QuickPanelKind::DocumentSymbols)?;
                    return Ok(());
                }
                KeyCode::Char('E') => {
                    self.highlight_symbol_under_cursor()?;
                    return Ok(());
                }
                KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.highlight_symbol_under_cursor()?;
                    return Ok(());
                }
                KeyCode::Char('p') => {
                    self.open_quick_panel(QuickPanelKind::OpenFile)?;
                    return Ok(());
                }
                KeyCode::Char(' ') | KeyCode::Null if self.focus == FocusPanel::Editor => {
                    if key.modifiers.contains(KeyModifiers::SHIFT) {
                        self.show_lsp_signature_help_under_cursor()?;
                    } else {
                        self.trigger_suggest()?;
                    }
                    return Ok(());
                }
                KeyCode::Char('T') => {
                    self.reopen_closed_editor();
                    return Ok(());
                }
                KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.reopen_closed_editor();
                    return Ok(());
                }
                KeyCode::Char('t') => {
                    self.open_quick_panel(QuickPanelKind::WorkspaceSymbols)?;
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
                KeyCode::Char('H') => {
                    self.start_workspace_replace_prompt();
                    return Ok(());
                }
                KeyCode::Char('h') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.start_workspace_replace_prompt();
                    return Ok(());
                }
                KeyCode::Char('B') => {
                    self.open_quick_panel(QuickPanelKind::Tasks)?;
                    return Ok(());
                }
                KeyCode::Char('b') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.open_quick_panel(QuickPanelKind::Tasks)?;
                    return Ok(());
                }
                KeyCode::Char('n') | KeyCode::Char('N') => {
                    self.new_untitled_file();
                    return Ok(());
                }
                _ => {}
            }
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('q') {
            self.request_quit();
            return Ok(());
        }
        if self.focus != FocusPanel::Terminal
            && key.modifiers == KeyModifiers::CONTROL
            && key.code == KeyCode::Char('\\')
        {
            self.open_active_tab_to_side();
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
                } else if !self.explorer_multi_selection.is_empty() {
                    self.clear_explorer_multi_selection();
                    self.message = Some("explorer selection cleared".to_owned());
                } else {
                    self.request_quit();
                }
            }
            KeyCode::Char('q') if self.focus != FocusPanel::Terminal => self.request_quit(),
            _ => match self.focus {
                FocusPanel::Explorer if self.sidebar_mode == SidebarMode::Outline => {
                    self.handle_outline_key(key)?
                }
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
        self.update_editor_hover_for_mouse(&target, mouse.kind);

        if self.explorer_drag.is_some()
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
            && self.handle_explorer_drag_mouse(mouse, target.clone())?
        {
            return Ok(());
        }

        if self.terminal_selection.is_some()
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
            && self.handle_terminal_selection_mouse(mouse)?
        {
            return Ok(());
        }

        if self.editor_gutter_dragging.is_some()
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            self.handle_editor_gutter_selection_mouse(mouse);
            return Ok(());
        }

        if self.editor_selection_dragging
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            self.handle_editor_selection_mouse(mouse);
            return Ok(());
        }

        if self.terminal_resize_dragging {
            match mouse.kind {
                MouseEventKind::Drag(MouseButton::Left)
                | MouseEventKind::Down(MouseButton::Left) => {
                    self.resize_terminal_from_mouse(mouse.row);
                    self.focus = FocusPanel::Terminal;
                    return Ok(());
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    self.resize_terminal_from_mouse(mouse.row);
                    self.terminal_resize_dragging = false;
                    self.message = Some(format!(
                        "terminal height set to {} rows",
                        self.terminal_rows
                    ));
                    return Ok(());
                }
                _ => {
                    self.terminal_resize_dragging = false;
                }
            }
        }

        if matches!(target, HoverTarget::Terminal | HoverTarget::TerminalInput)
            && !terminal_host_mouse_override(mouse.modifiers)
            && self.forward_terminal_mouse_event(mouse.kind, mouse.modifiers)?
        {
            self.focus = FocusPanel::Terminal;
            return Ok(());
        }

        if matches!(target, HoverTarget::Terminal | HoverTarget::TerminalInput)
            && matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.start_terminal_selection_from_mouse(mouse)
        {
            return Ok(());
        }

        match mouse.kind {
            MouseEventKind::Moved => {}
            MouseEventKind::Down(MouseButton::Left) if target == HoverTarget::TerminalResize => {
                self.terminal_resize_dragging = true;
                self.resize_terminal_from_mouse(mouse.row);
                self.focus = FocusPanel::Terminal;
            }
            MouseEventKind::Down(MouseButton::Left)
                if is_editor_target(&target) && mouse.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.activate_editor_target(&target);
                self.editor_selection_dragging = false;
                self.toggle_editor_cursor_from_mouse();
            }
            MouseEventKind::Down(MouseButton::Left)
                if matches!(target, HoverTarget::OutlineRow(_)) =>
            {
                if let HoverTarget::OutlineRow(index) = target {
                    self.focus = FocusPanel::Explorer;
                    self.jump_to_outline_symbol(index);
                }
            }
            MouseEventKind::Down(MouseButton::Left)
                if matches!(target, HoverTarget::ExplorerRow(_))
                    && mouse.modifiers.contains(KeyModifiers::SHIFT) =>
            {
                if let HoverTarget::ExplorerRow(index) = target {
                    self.focus = FocusPanel::Explorer;
                    self.extend_explorer_multi_selection_to(index);
                }
            }
            MouseEventKind::Down(MouseButton::Left)
                if matches!(target, HoverTarget::ExplorerRow(_))
                    && explorer_multi_select_modifier(mouse.modifiers) =>
            {
                if let HoverTarget::ExplorerRow(index) = target {
                    self.focus = FocusPanel::Explorer;
                    self.explorer.selected = index;
                    self.toggle_explorer_multi_selection();
                }
            }
            MouseEventKind::Down(MouseButton::Left)
                if matches!(target, HoverTarget::ExplorerRow(_)) =>
            {
                if let HoverTarget::ExplorerRow(index) = target {
                    self.start_explorer_drag_or_click(index, mouse.modifiers);
                }
            }
            MouseEventKind::Down(MouseButton::Left) if is_editor_target(&target) => {
                self.activate_editor_target(&target);
                self.start_editor_selection_from_mouse();
            }
            MouseEventKind::Down(MouseButton::Left) => self.activate_target(target)?,
            MouseEventKind::Down(MouseButton::Right) => {
                self.open_context_menu_for_target(target)?
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if is_editor_target(&target) {
                    self.activate_editor_target(&target);
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
            MouseEventKind::ScrollUp => self.handle_scroll(target, -3, true, mouse.modifiers)?,
            MouseEventKind::ScrollDown => self.handle_scroll(target, 3, false, mouse.modifiers)?,
            MouseEventKind::ScrollLeft => self.scroll_target_horizontal(target, -8),
            MouseEventKind::ScrollRight => self.scroll_target_horizontal(target, 8),
            MouseEventKind::Drag(_) | MouseEventKind::Up(_) => {}
        }

        Ok(())
    }

    fn start_explorer_drag_or_click(&mut self, index: usize, modifiers: KeyModifiers) {
        let Some(node) = self.visible_nodes().get(index).cloned() else {
            return;
        };
        self.focus = FocusPanel::Explorer;
        self.explorer.selected = index;

        let source_paths = if !self.explorer_multi_selection.is_empty()
            && self.explorer_multi_selection.contains(&node.path)
        {
            self.selected_explorer_paths()
        } else {
            self.clear_explorer_multi_selection();
            self.explorer_selection_anchor = Some(node.path.clone());
            vec![node.path]
        };
        let source_paths = normalize_file_op_paths(source_paths);
        if source_paths.is_empty() {
            return;
        }

        self.explorer_drag = Some(ExplorerDragState {
            source_paths,
            source_index: index,
            target_index: Some(index),
            moved: false,
            copy: modifiers.contains(KeyModifiers::ALT),
        });
    }

    fn handle_explorer_drag_mouse(
        &mut self,
        mouse: MouseEvent,
        target: HoverTarget,
    ) -> Result<bool> {
        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => {
                let (count, copy, target_index) = {
                    let Some(drag) = self.explorer_drag.as_mut() else {
                        return Ok(false);
                    };
                    drag.moved = true;
                    drag.copy = mouse.modifiers.contains(KeyModifiers::ALT);
                    drag.target_index = match target {
                        HoverTarget::ExplorerRow(index) => Some(index),
                        _ => None,
                    };
                    (drag.source_paths.len(), drag.copy, drag.target_index)
                };
                let action = if copy { "copy" } else { "move" };
                let destination = self
                    .explorer_drop_target_label(target_index)
                    .unwrap_or_else(|| "the explorer".to_owned());
                self.message = Some(format!(
                    "drop to {action} {count} item(s) into {destination}"
                ));
                Ok(true)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                let Some(drag) = self.explorer_drag.take() else {
                    return Ok(false);
                };
                if !drag.moved {
                    self.explorer.selected = drag.source_index;
                    self.open_or_toggle_selected()?;
                    return Ok(true);
                }

                let Some(target_dir) = self.explorer_drop_target_dir(&target) else {
                    self.message = Some("drop cancelled".to_owned());
                    return Ok(true);
                };
                self.drop_explorer_paths(drag.source_paths, target_dir, drag.copy)?;
                Ok(true)
            }
            _ => {
                self.explorer_drag = None;
                Ok(false)
            }
        }
    }

    pub fn explorer_drag_target_index(&self) -> Option<usize> {
        self.explorer_drag
            .as_ref()
            .filter(|drag| drag.moved)
            .and_then(|drag| drag.target_index)
    }

    fn explorer_drop_target_label(&self, index: Option<usize>) -> Option<String> {
        match index {
            Some(index) => {
                let nodes = self.visible_nodes();
                let node = nodes.get(index)?;
                let target = if node.is_dir {
                    node.path.clone()
                } else {
                    node.path.parent()?.to_path_buf()
                };
                Some(relative_path(&self.root, &target))
            }
            None => Some(relative_path(&self.root, &self.root)),
        }
    }

    fn explorer_drop_target_dir(&self, target: &HoverTarget) -> Option<PathBuf> {
        match target {
            HoverTarget::Explorer => Some(self.root.clone()),
            HoverTarget::ExplorerRow(index) => {
                let nodes = self.visible_nodes();
                let node = nodes.get(*index)?;
                if node.is_dir {
                    Some(node.path.clone())
                } else {
                    node.path.parent().map(Path::to_path_buf)
                }
            }
            _ => None,
        }
    }

    fn drop_explorer_paths(
        &mut self,
        paths: Vec<PathBuf>,
        target_dir: PathBuf,
        copy: bool,
    ) -> Result<()> {
        let sources = normalize_file_op_paths(paths);
        if sources.is_empty() {
            self.message = Some("drop cancelled".to_owned());
            return Ok(());
        }
        if sources.iter().any(|path| path == &self.root) {
            self.message = Some("refusing to drag workspace root".to_owned());
            return Ok(());
        }
        if sources.iter().any(|source| !source.exists()) {
            self.message = Some("one or more dragged sources no longer exist".to_owned());
            return Ok(());
        }

        let target_dir = canonical_existing_path(&target_dir);
        if !target_dir.is_dir() {
            self.message = Some(format!(
                "drop target is not a folder: {}",
                target_dir.display()
            ));
            return Ok(());
        }
        if sources
            .iter()
            .any(|source| source.is_dir() && target_dir.starts_with(source))
        {
            self.message = Some("cannot drop a folder into itself".to_owned());
            return Ok(());
        }

        let mut destinations = Vec::new();
        let mut skipped = 0usize;
        for source in &sources {
            let Some(name) = source.file_name() else {
                continue;
            };
            let candidate = target_dir.join(name);
            if !copy && candidate == *source {
                skipped += 1;
                continue;
            }
            let destination = if candidate.exists() {
                unique_copy_path(&candidate)
            } else {
                candidate
            };
            if copy {
                copy_path_recursive(source, &destination)?;
                destinations.push(destination);
            } else {
                fs::rename(source, &destination)?;
                let destination = destination
                    .canonicalize()
                    .unwrap_or_else(|_| destination.clone());
                self.update_open_tabs_for_move(source, &destination);
                self.update_navigation_for_move(source, &destination);
                destinations.push(destination);
            }
        }

        if destinations.is_empty() {
            self.message = if skipped > 0 {
                Some("drop target is already the current location".to_owned())
            } else {
                Some("drop moved no files".to_owned())
            };
            return Ok(());
        }

        if !copy {
            self.clear_explorer_multi_selection();
        }
        self.refresh_explorer()?;
        if let Some(destination) = destinations.last() {
            self.reveal_path(destination)?;
        }
        let action = if copy { "copied" } else { "moved" };
        self.message = Some(format!(
            "drag-{action} {} item(s) into {}",
            destinations.len(),
            target_dir.display()
        ));
        Ok(())
    }

    fn open_context_menu_for_target(&mut self, target: HoverTarget) -> Result<()> {
        match target {
            HoverTarget::ExplorerRow(index) => {
                self.focus = FocusPanel::Explorer;
                self.explorer.selected = index;
                if let Some(path) = self.current_explorer_path()
                    && !self.explorer_multi_selection.is_empty()
                    && !self.explorer_multi_selection.contains(&path)
                {
                    self.clear_explorer_multi_selection();
                    self.explorer_selection_anchor = Some(path);
                }
                self.open_quick_panel(QuickPanelKind::ExplorerContextMenu)?;
            }
            HoverTarget::Explorer => {
                self.focus = FocusPanel::Explorer;
                self.open_quick_panel(QuickPanelKind::ExplorerContextMenu)?;
            }
            HoverTarget::OutlineRow(index) => {
                self.outline_selected =
                    index.min(self.visible_outline_items().len().saturating_sub(1));
                self.ensure_outline_selection_visible();
                self.focus = FocusPanel::Explorer;
                self.open_quick_panel(QuickPanelKind::EditorContextMenu)?;
            }
            HoverTarget::Outline => {
                self.focus = FocusPanel::Explorer;
                self.open_quick_panel(QuickPanelKind::EditorContextMenu)?;
            }
            HoverTarget::Editor | HoverTarget::EditorPane(_) => {
                self.activate_editor_target(&target);
                self.focus = FocusPanel::Editor;
                self.set_editor_cursor_from_mouse(false);
                self.open_quick_panel(QuickPanelKind::EditorContextMenu)?;
            }
            HoverTarget::Tab(index) | HoverTarget::TabClose(index) => {
                if index < self.tabs.len() {
                    self.active_tab = Some(index);
                }
                self.focus = FocusPanel::Editor;
                self.open_quick_panel(QuickPanelKind::EditorContextMenu)?;
            }
            HoverTarget::Terminal | HoverTarget::TerminalInput => {
                self.activate_terminal_under_mouse();
                self.focus = FocusPanel::Terminal;
                self.open_quick_panel(QuickPanelKind::TerminalContextMenu)?;
            }
            HoverTarget::TerminalTab(index) | HoverTarget::TerminalTabClose(index) => {
                self.select_terminal(index);
                self.focus = FocusPanel::Terminal;
                self.open_quick_panel(QuickPanelKind::TerminalContextMenu)?;
            }
            _ => {}
        }

        Ok(())
    }

    fn activate_target(&mut self, target: HoverTarget) -> Result<()> {
        self.activate_editor_target(&target);
        match target {
            HoverTarget::Explorer | HoverTarget::ExplorerRow(_) => {
                self.focus = FocusPanel::Explorer;
            }
            HoverTarget::Outline | HoverTarget::OutlineRow(_) => {
                self.focus = FocusPanel::Explorer;
            }
            HoverTarget::Tab(_)
            | HoverTarget::TabClose(_)
            | HoverTarget::Editor
            | HoverTarget::EditorPane(_) => {
                self.focus = FocusPanel::Editor;
            }
            HoverTarget::TerminalTab(_)
            | HoverTarget::TerminalTabClose(_)
            | HoverTarget::TerminalNew
            | HoverTarget::TerminalResize => {
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
                self.clear_explorer_multi_selection();
                self.set_explorer_anchor_to_current();
                self.open_or_toggle_selected()?;
            }
            HoverTarget::OutlineRow(index) => self.jump_to_outline_symbol(index),
            HoverTarget::Outline => {}
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
            HoverTarget::TerminalResize => {}
            HoverTarget::QuickRow(index) => self.activate_quick_row(index),
            HoverTarget::Editor | HoverTarget::EditorPane(_)
                if self.toggle_editor_fold_from_mouse() => {}
            HoverTarget::Editor | HoverTarget::EditorPane(_) => {
                self.activate_editor_target(&target);
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
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_selected_to_side()?
            }
            KeyCode::Enter => self.open_or_toggle_selected()?,
            KeyCode::Right => self.expand_or_descend_selected()?,
            KeyCode::Left => self.collapse_or_select_parent(),
            KeyCode::Char('r') => self.refresh_explorer()?,
            KeyCode::Char('s') => self.cycle_explorer_sort_mode(),
            KeyCode::Char('/') => self.start_explorer_filter_prompt(),
            KeyCode::Char('.') => self.toggle_hidden_files(),
            KeyCode::Char('i') => self.toggle_ignored_files(),
            KeyCode::Char('n') => self.start_new_file_prompt(),
            KeyCode::Char('N') => self.start_new_dir_prompt(),
            KeyCode::Char('e') => self.prompt_rename(),
            KeyCode::Char('D') => self.prompt_delete(),
            KeyCode::Char(' ') => self.toggle_explorer_multi_selection(),
            KeyCode::Char('c') => self.copy_selected_path(),
            KeyCode::Char('x') => self.cut_selected_path(),
            KeyCode::Char('p') => self.paste_into_selected()?,
            KeyCode::Char('y') => self.duplicate_selected()?,
            KeyCode::Char('v') => self.compare_selected_files()?,
            KeyCode::Char('o') => self.reveal_active_file()?,
            KeyCode::Char('O') => self.open_selected_folder_as_workspace()?,
            KeyCode::Char('m') => self.show_outline()?,
            KeyCode::Char('t') => self.new_terminal_here()?,
            KeyCode::F(5) => self.run_selected_explorer_file_in_terminal()?,
            _ => {}
        }

        Ok(())
    }

    fn handle_outline_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Up => self.move_outline_selection(-1),
            KeyCode::Down => self.move_outline_selection(1),
            KeyCode::PageUp => self.move_outline_selection(-(self.explorer_height as isize)),
            KeyCode::PageDown => self.move_outline_selection(self.explorer_height as isize),
            KeyCode::Home => self.set_outline_selection(0),
            KeyCode::End => {
                let last = self.visible_outline_items().len().saturating_sub(1);
                self.set_outline_selection(last);
            }
            KeyCode::Enter => self.jump_to_selected_outline_symbol(),
            KeyCode::Char('m') => self.show_files_sidebar(),
            KeyCode::Char('r') => self.refresh_outline_symbols()?,
            KeyCode::Char('o') => self.reveal_active_file()?,
            KeyCode::Char('O') => {
                self.open_quick_panel(QuickPanelKind::DocumentSymbols)?;
            }
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
                KeyCode::Left => self.go_back(),
                KeyCode::Right => self.go_forward(),
                KeyCode::Up => self.move_active_line_up(),
                KeyCode::Down => self.move_active_line_down(),
                KeyCode::Char('[') => self.toggle_active_fold(),
                KeyCode::Char('0') => self.fold_all_active_tab(),
                KeyCode::Char(']') => self.unfold_all_active_tab(),
                KeyCode::Char('b') | KeyCode::Char('B') => self.toggle_bookmark_at_cursor(),
                KeyCode::Char('n') | KeyCode::Char('N') => self.jump_to_relative_bookmark(true),
                KeyCode::Char('p') | KeyCode::Char('P') => self.jump_to_relative_bookmark(false),
                KeyCode::Char('a') | KeyCode::Char('A')
                    if key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.toggle_active_block_comment()
                }
                KeyCode::Char('z') | KeyCode::Char('Z') => self.toggle_word_wrap(),
                KeyCode::Char('f') | KeyCode::Char('F')
                    if key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.format_active_document()?
                }
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
                KeyCode::Enter => {
                    if let Err(error) = self.run_selection_in_terminal() {
                        self.last_error = Some(error.to_string());
                    }
                }
                KeyCode::Char('f') => {
                    let initial = self.search_needle.clone().unwrap_or_default();
                    self.start_prompt(PromptKind::Search, &initial);
                }
                KeyCode::Char('h') => self.start_replace_prompt(false),
                KeyCode::Char('L') => self.select_all_occurrences_in_active_tab(),
                KeyCode::Char('l') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.select_all_occurrences_in_active_tab()
                }
                KeyCode::Char('l') => self.start_prompt(PromptKind::GotoLine, ""),
                KeyCode::Char(']') => {
                    if let Err(error) = self.go_to_definition_under_cursor() {
                        self.last_error = Some(error.to_string());
                    }
                }
                KeyCode::Char('\\') | KeyCode::Char('|')
                    if key.modifiers.contains(KeyModifiers::SHIFT) =>
                {
                    self.go_to_matching_bracket();
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    if let Err(error) = self.find_references_under_cursor() {
                        self.last_error = Some(error.to_string());
                    }
                }
                KeyCode::Char('/') => self.toggle_active_line_comment(),
                KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::SHIFT) => {
                    self.duplicate_active_line()
                }
                KeyCode::Char('D') => self.duplicate_active_line(),
                KeyCode::Char('d') => self.add_selection_to_next_match(),
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
            KeyCode::F(2) => self.start_rename_symbol_prompt(),
            KeyCode::F(3) => self.find_next(true),
            KeyCode::F(5) => self.run_active_file_in_terminal()?,
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
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && key.modifiers.contains(KeyModifiers::SHIFT)
        {
            match key.code {
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.copy_terminal_selection();
                    return Ok(());
                }
                KeyCode::Char('v') | KeyCode::Char('V') => {
                    self.paste_clipboard_to_terminal()?;
                    return Ok(());
                }
                _ => {}
            }
        }

        if self.terminal_child_owns_keyboard() {
            self.terminal_selection = None;
            return self.active_terminal_mut().shell.send_key(key);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('f') | KeyCode::Char('F'))
        {
            self.start_terminal_search_prompt();
            return Ok(());
        }

        match key.code {
            KeyCode::F(3) if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.previous_terminal_search_match();
                return Ok(());
            }
            KeyCode::F(3) => {
                self.next_terminal_search_match();
                return Ok(());
            }
            _ => {}
        }

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

        self.terminal_selection = None;
        self.active_terminal_mut().shell.send_key(key)
    }

    fn terminal_child_owns_keyboard(&self) -> bool {
        if self.focus != FocusPanel::Terminal {
            return false;
        }
        let terminal = self.active_terminal();
        terminal.shell.alternate_screen()
            || terminal.shell.mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    fn handle_prompt_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => self.finish_prompt()?,
            KeyCode::Left => self.edit_prompt_input(PromptEdit::MoveLeft),
            KeyCode::Right => self.edit_prompt_input(PromptEdit::MoveRight),
            KeyCode::Home => self.edit_prompt_input(PromptEdit::MoveStart),
            KeyCode::End => self.edit_prompt_input(PromptEdit::MoveEnd),
            KeyCode::Backspace => self.edit_prompt_input(PromptEdit::Backspace),
            KeyCode::Delete => self.edit_prompt_input(PromptEdit::Delete),
            KeyCode::Char('a') | KeyCode::Char('A')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_prompt_input(PromptEdit::MoveStart)
            }
            KeyCode::Char('e') | KeyCode::Char('E')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_prompt_input(PromptEdit::MoveEnd)
            }
            KeyCode::Char('u') | KeyCode::Char('U')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_prompt_input(PromptEdit::DeleteBeforeCursor)
            }
            KeyCode::Char('k') | KeyCode::Char('K')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_prompt_input(PromptEdit::DeleteAfterCursor)
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.edit_prompt_input(PromptEdit::Insert(&c.to_string()));
            }
            _ => {}
        }
        Ok(())
    }

    fn handle_quick_panel_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                if self
                    .quick_panel
                    .as_ref()
                    .is_some_and(|panel| panel.kind == QuickPanelKind::Completions)
                {
                    self.completion_state = None;
                    self.lsp_completion_items.clear();
                }
                self.quick_panel = None;
            }
            KeyCode::Enter => self.activate_selected_quick_item(),
            KeyCode::Up => self.move_quick_selection(-1),
            KeyCode::Down => self.move_quick_selection(1),
            KeyCode::PageUp => self.move_quick_selection(-(self.quick_panel_height as isize)),
            KeyCode::PageDown => self.move_quick_selection(self.quick_panel_height as isize),
            KeyCode::Home if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.set_quick_selection(0)
            }
            KeyCode::End if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(panel) = &self.quick_panel {
                    self.set_quick_selection(panel.items.len().saturating_sub(1));
                }
            }
            KeyCode::Home => self.edit_quick_query(PromptEdit::MoveStart)?,
            KeyCode::End => self.edit_quick_query(PromptEdit::MoveEnd)?,
            KeyCode::Left => self.edit_quick_query(PromptEdit::MoveLeft)?,
            KeyCode::Right => self.edit_quick_query(PromptEdit::MoveRight)?,
            KeyCode::Backspace => self.edit_quick_query(PromptEdit::Backspace)?,
            KeyCode::Delete => self.edit_quick_query(PromptEdit::Delete)?,
            KeyCode::Char('a') | KeyCode::Char('A')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_quick_query(PromptEdit::MoveStart)?
            }
            KeyCode::Char('e') | KeyCode::Char('E')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_quick_query(PromptEdit::MoveEnd)?
            }
            KeyCode::Char('u') | KeyCode::Char('U')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_quick_query(PromptEdit::DeleteBeforeCursor)?
            }
            KeyCode::Char('k') | KeyCode::Char('K')
                if key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.edit_quick_query(PromptEdit::DeleteAfterCursor)?
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.edit_quick_query(PromptEdit::Insert(&c.to_string()))?;
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
        let prompt_text = text.replace(['\r', '\n'], " ");
        if let Some(panel) = &mut self.quick_panel {
            edit_text_at_cursor(
                &mut panel.query,
                &mut panel.query_cursor,
                PromptEdit::Insert(&prompt_text),
            );
            return self.refresh_quick_panel();
        }

        if let Some(prompt) = &mut self.prompt {
            edit_text_at_cursor(
                &mut prompt.input,
                &mut prompt.cursor,
                PromptEdit::Insert(&prompt_text),
            );
            return Ok(());
        }

        match self.focus {
            FocusPanel::Editor => {
                if !self.ensure_active_tab_writable("paste") {
                    return Ok(());
                }
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

    fn edit_prompt_input(&mut self, edit: PromptEdit<'_>) {
        if let Some(prompt) = &mut self.prompt {
            edit_text_at_cursor(&mut prompt.input, &mut prompt.cursor, edit);
        }
    }

    fn edit_quick_query(&mut self, edit: PromptEdit<'_>) -> Result<()> {
        if let Some(panel) = &mut self.quick_panel {
            edit_text_at_cursor(&mut panel.query, &mut panel.query_cursor, edit);
        }
        self.refresh_quick_panel()
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

    fn current_explorer_path(&self) -> Option<PathBuf> {
        self.visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone())
    }

    pub fn selected_explorer_paths(&self) -> Vec<PathBuf> {
        let nodes = self.visible_nodes();
        if self.explorer_multi_selection.is_empty() {
            return nodes
                .get(self.explorer.selected)
                .map(|node| vec![node.path.clone()])
                .unwrap_or_default();
        }

        let paths = nodes
            .iter()
            .filter(|node| self.explorer_multi_selection.contains(&node.path))
            .map(|node| node.path.clone())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            nodes
                .get(self.explorer.selected)
                .map(|node| vec![node.path.clone()])
                .unwrap_or_default()
        } else {
            paths
        }
    }

    fn selected_explorer_label(&self, paths: &[PathBuf]) -> String {
        if paths.len() == 1 {
            paths
                .first()
                .map(|path| relative_path(&self.root, path))
                .unwrap_or_else(|| "selection".to_owned())
        } else {
            format!("{} selected items", paths.len())
        }
    }

    fn set_explorer_anchor_to_current(&mut self) {
        self.explorer_selection_anchor = self.current_explorer_path();
    }

    fn clear_explorer_multi_selection(&mut self) {
        self.explorer_multi_selection.clear();
        self.explorer_selection_anchor = None;
    }

    fn toggle_explorer_multi_selection(&mut self) {
        let Some(path) = self.current_explorer_path() else {
            return;
        };
        if !self.explorer_multi_selection.remove(&path) {
            self.explorer_multi_selection.insert(path.clone());
        }
        self.explorer_selection_anchor = Some(path);
        let count = self.explorer_multi_selection.len();
        self.message = if count == 0 {
            Some("explorer selection cleared".to_owned())
        } else {
            Some(format!("selected {count} explorer item(s)"))
        };
    }

    fn extend_explorer_multi_selection_to(&mut self, index: usize) {
        let nodes = self.visible_nodes();
        if nodes.is_empty() {
            return;
        }
        let index = index.min(nodes.len().saturating_sub(1));
        let anchor_index = self
            .explorer_selection_anchor
            .as_ref()
            .and_then(|anchor| nodes.iter().position(|node| &node.path == anchor))
            .unwrap_or(self.explorer.selected.min(nodes.len().saturating_sub(1)));
        let (start, end) = if anchor_index <= index {
            (anchor_index, index)
        } else {
            (index, anchor_index)
        };
        self.explorer_multi_selection = nodes[start..=end]
            .iter()
            .map(|node| node.path.clone())
            .collect();
        self.explorer.selected = index;
        self.ensure_explorer_selection_visible();
        self.message = Some(format!(
            "selected {} explorer item(s)",
            self.explorer_multi_selection.len()
        ));
    }

    fn prune_explorer_multi_selection(&mut self) {
        if self.explorer_multi_selection.is_empty() && self.explorer_selection_anchor.is_none() {
            return;
        }
        let visible_paths = self
            .visible_nodes()
            .into_iter()
            .map(|node| node.path)
            .collect::<HashSet<_>>();
        self.explorer_multi_selection
            .retain(|path| visible_paths.contains(path) && path.exists());
        if self
            .explorer_selection_anchor
            .as_ref()
            .is_some_and(|path| !visible_paths.contains(path))
        {
            self.explorer_selection_anchor = self.current_explorer_path();
        }
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

    fn expand_or_descend_selected(&mut self) -> Result<()> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return Ok(());
        };

        if !node.is_dir {
            self.open_file(&node.path);
            return Ok(());
        }

        if !node.expanded {
            if let Err(error) = self.explorer.toggle(&node.path) {
                self.last_error = Some(error.to_string());
            }
            self.ensure_explorer_selection_visible();
            return Ok(());
        }

        let nodes = self.visible_nodes();
        if let Some(child_index) = nodes
            .iter()
            .enumerate()
            .skip(self.explorer.selected + 1)
            .take_while(|(_, candidate)| candidate.depth > node.depth)
            .find_map(|(index, candidate)| (candidate.depth == node.depth + 1).then_some(index))
        {
            self.explorer.selected = child_index;
            self.set_explorer_anchor_to_current();
            self.ensure_explorer_selection_visible();
        }

        Ok(())
    }

    fn collapse_or_select_parent(&mut self) {
        let nodes = self.visible_nodes();
        let Some(node) = nodes.get(self.explorer.selected).cloned() else {
            return;
        };

        if node.is_dir && node.expanded {
            self.explorer.collapse(&node.path);
            self.prune_explorer_multi_selection();
            self.ensure_explorer_selection_visible();
            return;
        }

        if node.depth == 0 {
            return;
        }

        if let Some(parent_index) = nodes[..self.explorer.selected]
            .iter()
            .enumerate()
            .rev()
            .find_map(|(index, candidate)| {
                (candidate.depth < node.depth && node.path.starts_with(&candidate.path))
                    .then_some(index)
            })
        {
            self.explorer.selected = parent_index;
            self.set_explorer_anchor_to_current();
            self.ensure_explorer_selection_visible();
        }
    }

    fn open_file(&mut self, path: &Path) {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active_tab = Some(index);
            self.focus = FocusPanel::Editor;
            self.normalize_editor_split();
            return;
        }

        match EditorTab::open(path) {
            Ok(tab) => {
                let read_only = tab.read_only;
                let title = tab.title.clone();
                self.tabs.push(tab);
                self.active_tab = Some(self.tabs.len() - 1);
                self.focus = FocusPanel::Editor;
                self.normalize_editor_split();
                if read_only {
                    self.message = Some(format!("opened read-only preview for {title}"));
                }
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn open_file_to_side(&mut self, path: &Path) {
        let previous = self.active_tab;
        self.open_file(path);
        let Some(active) = self.active_tab else {
            return;
        };
        self.editor_split = previous.or(Some(active));
        self.active_editor_pane = 1;
        self.focus = FocusPanel::Editor;
        self.normalize_editor_split();
        let title = self
            .tabs
            .get(active)
            .map(|tab| tab.title.clone())
            .unwrap_or_else(|| "file".to_owned());
        self.message = Some(format!("opened {title} to side"));
    }

    fn open_selected_to_side(&mut self) -> Result<()> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            self.message = Some("no explorer item selected".to_owned());
            return Ok(());
        };
        if node.is_dir {
            self.open_or_toggle_selected()?;
        } else {
            self.open_file_to_side(&node.path);
        }
        Ok(())
    }

    fn open_selected_folder_as_workspace(&mut self) -> Result<()> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            self.message = Some("select a folder to open as workspace".to_owned());
            return Ok(());
        };
        if !node.is_dir {
            self.message = Some(format!(
                "select a folder to open as workspace, not {}",
                relative_path(&self.root, &node.path)
            ));
            return Ok(());
        }
        self.open_workspace_root(node.path)
    }

    fn open_folder_from_prompt(&mut self, input: String) -> Result<()> {
        let Some(path) = resolve_prompt_path(&self.root, &input) else {
            self.message = Some("open folder cancelled".to_owned());
            return Ok(());
        };
        self.open_workspace_root(path)
    }

    fn open_workspace_root(&mut self, path: PathBuf) -> Result<()> {
        let root = match path.canonicalize() {
            Ok(path) => path,
            Err(error) => {
                self.message = Some(format!("open folder failed: {error}"));
                return Ok(());
            }
        };
        if !root.is_dir() {
            self.message = Some(format!(
                "open folder target is not a folder: {}",
                root.display()
            ));
            return Ok(());
        }
        if root == self.root {
            self.focus = FocusPanel::Explorer;
            self.message = Some(format!("folder already open: {}", root.display()));
            return Ok(());
        }
        if let Some(label) = self.dirty_tab_label() {
            self.message = Some(format!(
                "open folder blocked by unsaved editor tab: {label}; save or close it first"
            ));
            return Ok(());
        }

        let explorer = FsTree::new(root.clone())?;
        let terminal = TerminalSession::new(1, root.clone())?;
        let (git_statuses, git_dirty_dirs) = load_git_status(&root);
        let git_branch = git_top_level(&root).and_then(|top_level| git_current_branch(&top_level));
        let workspace_snapshot =
            workspace_snapshot(&root, self.show_hidden, self.show_ignored).ok();
        let workspace_visible_paths =
            workspace_visible_paths(&root, self.show_hidden, self.show_ignored).unwrap_or_else(
                |_| {
                    let mut paths = HashSet::new();
                    paths.insert(root.clone());
                    paths
                },
            );

        self.kill_terminal_sessions();
        self.root = root.clone();
        self.explorer = explorer;
        self.tabs.clear();
        self.active_tab = None;
        self.closed_tabs.clear();
        self.editor_split = None;
        self.active_editor_pane = 0;
        self.focus = FocusPanel::Explorer;
        self.hover = HoverTarget::None;
        self.hit_regions.clear();
        self.terminals = vec![terminal];
        self.active_terminal = 0;
        self.split_terminal = None;
        self.next_terminal_id = 2;
        self.next_untitled_id = 1;
        self.terminal_maximized = false;
        self.terminal_resize_dragging = false;
        self.editor_selection_dragging = false;
        self.last_error = None;
        self.prompt = None;
        self.search_needle = None;
        self.explorer_filter = None;
        self.quick_panel = None;
        self.completion_state = None;
        self.lsp_completion_items.clear();
        self.lsp_document_symbol_items.clear();
        self.lsp_workspace_symbol_query = None;
        self.lsp_workspace_symbol_items.clear();
        self.lsp_code_actions.clear();
        self.editor_hover = None;
        self.quick_panel_height = 0;
        self.explorer_multi_selection.clear();
        self.explorer_selection_anchor = None;
        self.explorer_clipboard = None;
        self.explorer_drag = None;
        self.git_statuses = git_statuses;
        self.git_dirty_dirs = git_dirty_dirs;
        self.git_branch = git_branch;
        self.navigation_back.clear();
        self.navigation_forward.clear();
        self.terminal_selection = None;
        self.terminal_search = None;
        self.terminal_command_history.clear();
        self.problems.clear();
        self.pending_clipboard_export = None;
        self.workspace_snapshot = workspace_snapshot;
        self.workspace_visible_paths = workspace_visible_paths;
        self.last_workspace_tree_check = Instant::now();
        self.message = Some(format!("opened folder {}", root.display()));
        Ok(())
    }

    fn open_active_tab_to_side(&mut self) {
        let Some(active) = self.active_tab else {
            self.message = Some("open a file before splitting the editor".to_owned());
            return;
        };
        self.editor_split = Some(active);
        self.active_editor_pane = 1;
        self.focus = FocusPanel::Editor;
        self.normalize_editor_split();
        let title = self
            .tabs
            .get(active)
            .map(|tab| tab.title.clone())
            .unwrap_or_else(|| "active tab".to_owned());
        self.message = Some(format!("split editor: {title}"));
    }

    fn close_editor_split(&mut self) {
        if self.editor_split.is_some() {
            self.editor_split = None;
            self.active_editor_pane = 0;
            self.message = Some("editor split closed".to_owned());
        } else {
            self.message = Some("editor is already single-pane".to_owned());
        }
    }

    fn open_read_only_text_tab(&mut self, path: PathBuf, title: String, text: String) {
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.tabs[index].set_clean_text(&text);
            self.tabs[index].title = title;
            self.tabs[index].read_only = true;
            self.active_tab = Some(index);
        } else {
            self.tabs.push(EditorTab::read_only(path, title, &text));
            self.active_tab = Some(self.tabs.len() - 1);
        }
        self.focus = FocusPanel::Editor;
        self.normalize_editor_split();
        if let Some(tab) = self.active_tab_mut()
            && let Some(line) = first_diff_hunk_line(&tab.lines)
        {
            tab.set_cursor(line, 0);
            self.ensure_editor_cursor_visible();
        }
    }

    fn new_untitled_file(&mut self) {
        let id = self.next_untitled_id;
        self.next_untitled_id += 1;
        let title = format!("Untitled-{id}");
        self.tabs.push(EditorTab::untitled(id, &self.root));
        self.active_tab = Some(self.tabs.len() - 1);
        self.focus = FocusPanel::Editor;
        self.normalize_editor_split();
        self.message = Some(format!("{title} ready; use Save As to write it to disk"));
    }

    fn current_editor_location(&self) -> Option<EditorLocation> {
        let tab = self.active_tab()?;
        Some(EditorLocation {
            path: tab.path.clone(),
            line: tab.cursor_line,
            col: tab.cursor_col,
        })
    }

    fn navigation_target_location(
        &self,
        path: &Path,
        line: Option<usize>,
        col: Option<usize>,
    ) -> EditorLocation {
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let (line, col) = match line {
            Some(line) => (line, col.unwrap_or(0)),
            None => self
                .tabs
                .iter()
                .find(|tab| tab.path == path)
                .map(EditorTab::cursor_position)
                .unwrap_or((0, 0)),
        };
        EditorLocation { path, line, col }
    }

    fn push_navigation_stack(stack: &mut Vec<EditorLocation>, location: EditorLocation) {
        if stack.last().is_some_and(|last| last == &location) {
            return;
        }
        if stack.len() >= MAX_NAVIGATION_HISTORY {
            stack.remove(0);
        }
        stack.push(location);
    }

    fn push_navigation_location_for_jump(
        &mut self,
        path: &Path,
        line: Option<usize>,
        col: Option<usize>,
    ) {
        let Some(current) = self.current_editor_location() else {
            return;
        };
        let target = self.navigation_target_location(path, line, col);
        if current != target {
            Self::push_navigation_stack(&mut self.navigation_back, current);
            self.navigation_forward.clear();
        }
    }

    fn jump_to_editor_location(&mut self, location: &EditorLocation) -> bool {
        self.open_file(&location.path);
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        if tab.path != location.path {
            return false;
        }
        tab.set_cursor(location.line, location.col);
        self.ensure_editor_cursor_visible();
        self.focus = FocusPanel::Editor;
        true
    }

    fn navigation_label(&self, location: &EditorLocation) -> String {
        format!(
            "{}:{}:{}",
            relative_path(&self.root, &location.path),
            location.line + 1,
            location.col + 1
        )
    }

    fn go_back(&mut self) {
        let Some(current) = self.current_editor_location() else {
            self.message = Some("no active editor location".to_owned());
            return;
        };

        while let Some(target) = self.navigation_back.pop() {
            if target == current {
                continue;
            }
            Self::push_navigation_stack(&mut self.navigation_forward, current.clone());
            if self.jump_to_editor_location(&target) {
                self.message = Some(format!("back to {}", self.navigation_label(&target)));
            } else {
                let _ = self.navigation_forward.pop();
                self.message = Some(format!("could not reopen {}", target.path.display()));
            }
            return;
        }

        self.message = Some("no previous editor location".to_owned());
    }

    fn go_forward(&mut self) {
        let Some(current) = self.current_editor_location() else {
            self.message = Some("no active editor location".to_owned());
            return;
        };

        while let Some(target) = self.navigation_forward.pop() {
            if target == current {
                continue;
            }
            Self::push_navigation_stack(&mut self.navigation_back, current.clone());
            if self.jump_to_editor_location(&target) {
                self.message = Some(format!("forward to {}", self.navigation_label(&target)));
            } else {
                let _ = self.navigation_back.pop();
                self.message = Some(format!("could not reopen {}", target.path.display()));
            }
            return;
        }

        self.message = Some("no next editor location".to_owned());
    }

    fn go_to_matching_bracket(&mut self) {
        let Some((path, bracket_match)) = self.active_tab().and_then(|tab| {
            tab.matching_bracket_position()
                .map(|bracket_match| (tab.path.clone(), bracket_match))
        }) else {
            self.message = Some("no matching bracket at cursor".to_owned());
            return;
        };

        self.push_navigation_location_for_jump(
            &path,
            Some(bracket_match.target.0),
            Some(bracket_match.target.1),
        );
        if let Some(tab) = self.active_tab_mut() {
            tab.set_cursor(bracket_match.target.0, bracket_match.target.1);
        }
        self.ensure_editor_cursor_visible();
        self.focus = FocusPanel::Editor;
        self.message = Some(format!(
            "matched {} at {}:{} to {} at {}:{}",
            bracket_match.source_ch,
            bracket_match.source.0 + 1,
            bracket_match.source.1 + 1,
            bracket_match.target_ch,
            bracket_match.target.0 + 1,
            bracket_match.target.1 + 1
        ));
    }

    pub fn drain_terminal(&mut self) -> bool {
        let mut changed = false;
        let mut exited = Vec::new();
        let mut clipboard_updates = Vec::new();
        for terminal in &mut self.terminals {
            changed |= terminal.shell.drain();
            if let Some(cwd) = terminal.shell.take_cwd_update()
                && terminal.cwd != cwd
            {
                terminal.cwd = cwd;
                changed = true;
            }
            if let Some(title) = terminal.shell.take_title_update()
                && !terminal.title_locked
                && terminal.title != title
            {
                terminal.title = title;
                changed = true;
            }
            if let Some(text) = terminal.shell.take_clipboard_update() {
                clipboard_updates.push((terminal.title.clone(), text));
                changed = true;
            }
            if !terminal.exited
                && let Some(status) = terminal.shell.child_exit_status()
            {
                terminal.exited = true;
                terminal.exit_status = Some(status.clone());
                exited.push(format!("{} ({})", terminal.title, status.label()));
            }
        }

        let mut notifications = Vec::new();
        if !exited.is_empty() {
            notifications.push(if self.terminals.len() == 1 {
                format!("terminal shell exited {}", exited[0])
            } else {
                format!("terminal exited: {}", exited.join(", "))
            });
            changed = true;
        }
        if let Some((title, text)) = clipboard_updates.pop() {
            let count = text.chars().count();
            self.editor_clipboard = Some(text.clone());
            if self.queue_clipboard_export(&text) {
                notifications.push(format!(
                    "terminal {title} copied {count} char(s) through OSC52"
                ));
            } else {
                notifications.push(format!(
                    "terminal {title} copied {count} char(s) internally; OSC52 export too large"
                ));
            }
            changed = true;
        }
        if !notifications.is_empty() {
            self.message = Some(notifications.join("; "));
        }
        self.normalize_terminal_split();
        changed
    }

    pub fn check_external_file_changes(&mut self) -> bool {
        let mut changed = false;
        let mut reloaded = 0usize;
        let mut modified_conflicts = 0usize;
        let mut deleted_conflicts = 0usize;
        let mut deleted_clean = 0usize;
        let mut read_errors = Vec::new();

        for index in 0..self.tabs.len() {
            if self.tabs[index].untitled {
                continue;
            }
            let state = self.tabs[index].current_disk_state();

            if state == ExternalFileState::Clean {
                if self.tabs[index].external_state != ExternalFileState::Clean {
                    self.tabs[index].external_state = ExternalFileState::Clean;
                    changed = true;
                }
                continue;
            }

            if self.tabs[index].dirty {
                if self.tabs[index].external_state != state {
                    self.tabs[index].external_state = state;
                    changed = true;
                }
                match state {
                    ExternalFileState::Modified => modified_conflicts += 1,
                    ExternalFileState::Deleted => deleted_conflicts += 1,
                    ExternalFileState::Clean => {}
                }
                continue;
            }

            match state {
                ExternalFileState::Modified => {
                    let path = self.tabs[index].path.clone();
                    let title = self.tabs[index].title.clone();
                    match read_text_lossy(&path) {
                        Ok(text) => {
                            self.tabs[index].set_clean_text(&text);
                            reloaded += 1;
                            changed = true;
                        }
                        Err(error) => {
                            self.tabs[index].external_state = ExternalFileState::Modified;
                            read_errors.push(format!("{title}: {error}"));
                            changed = true;
                        }
                    }
                }
                ExternalFileState::Deleted => {
                    if self.tabs[index].external_state != ExternalFileState::Deleted {
                        self.tabs[index].external_state = ExternalFileState::Deleted;
                        changed = true;
                    }
                    deleted_clean += 1;
                }
                ExternalFileState::Clean => {}
            }
        }

        if changed {
            if reloaded > 0 || deleted_clean > 0 {
                self.refresh_git_status();
            }
            let mut parts = Vec::new();
            if reloaded > 0 {
                parts.push(format!("reloaded {reloaded} clean tab(s) from disk"));
            }
            if modified_conflicts > 0 {
                parts.push(format!(
                    "external changes detected in {modified_conflicts} dirty tab(s)"
                ));
            }
            if deleted_conflicts > 0 {
                parts.push(format!(
                    "{deleted_conflicts} dirty tab(s) were deleted on disk"
                ));
            }
            if deleted_clean > 0 {
                parts.push(format!("{deleted_clean} clean tab(s) deleted on disk"));
            }
            if !read_errors.is_empty() {
                parts.push(format!("reload failed: {}", read_errors.join(", ")));
            }
            if !parts.is_empty() {
                self.message = Some(parts.join("; "));
            }
        }

        changed
    }

    pub fn check_workspace_tree_changes(&mut self) -> bool {
        if self.last_workspace_tree_check.elapsed() < WORKSPACE_TREE_CHECK_INTERVAL {
            return false;
        }
        self.last_workspace_tree_check = Instant::now();
        self.force_check_workspace_tree_changes()
    }

    fn force_check_workspace_tree_changes(&mut self) -> bool {
        let current = match workspace_snapshot(&self.root, self.show_hidden, self.show_ignored) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return false;
            }
        };

        if self.workspace_snapshot.as_ref() == Some(&current) {
            return false;
        }

        self.workspace_snapshot = Some(current);
        if let Err(error) = self.refresh_explorer_preserving_selection(false) {
            self.last_error = Some(error.to_string());
            return false;
        }

        true
    }

    pub fn active_tab(&self) -> Option<&EditorTab> {
        self.active_tab.and_then(|index| self.tabs.get(index))
    }

    pub fn active_tab_mut(&mut self) -> Option<&mut EditorTab> {
        self.active_tab.and_then(|index| self.tabs.get_mut(index))
    }

    pub fn repair_runtime_state(&mut self) -> Result<()> {
        let mut repaired = Vec::new();

        if self.terminals.is_empty() {
            let id = self.next_terminal_id;
            self.next_terminal_id += 1;
            self.terminals
                .push(TerminalSession::new(id, self.root.clone())?);
            self.active_terminal = 0;
            self.split_terminal = None;
            self.terminal_selection = None;
            repaired.push("terminal session");
        } else {
            let previous = self.active_terminal;
            self.normalize_terminal_split();
            if self.active_terminal != previous {
                repaired.push("active terminal index");
            }
        }

        if let Some(active) = self.active_tab
            && active >= self.tabs.len()
        {
            self.active_tab = self.tabs.len().checked_sub(1);
            repaired.push("active editor index");
        }
        let previous_split = self.editor_split;
        self.normalize_editor_split();
        if self.editor_split != previous_split {
            repaired.push("editor split index");
        }

        let visible_nodes = self.visible_nodes();
        if visible_nodes.is_empty() {
            self.explorer.selected = 0;
            self.explorer.scroll = 0;
        } else {
            let previous_selected = self.explorer.selected;
            self.explorer.selected = self.explorer.selected.min(visible_nodes.len() - 1);
            let max_scroll = visible_nodes
                .len()
                .saturating_sub(self.explorer_height.max(1));
            self.explorer.scroll = self.explorer.scroll.min(max_scroll);
            if self.explorer.selected != previous_selected {
                repaired.push("explorer selection");
            }
        }

        if self.sidebar_mode == SidebarMode::Outline {
            let outline_len = self.visible_outline_items().len();
            if outline_len == 0 {
                self.outline_selected = 0;
                self.outline_scroll = 0;
            } else {
                self.outline_selected = self.outline_selected.min(outline_len - 1);
                let max_scroll = outline_len.saturating_sub(self.explorer_height.max(1));
                self.outline_scroll = self.outline_scroll.min(max_scroll);
            }
        }

        if let Some(panel) = &mut self.quick_panel {
            panel.query_cursor = panel.query_cursor.min(panel.query.chars().count());
            if panel.items.is_empty() {
                panel.selected = 0;
                panel.scroll = 0;
            } else {
                panel.selected = panel.selected.min(panel.items.len() - 1);
                let max_scroll = panel
                    .items
                    .len()
                    .saturating_sub(self.quick_panel_height.max(1));
                panel.scroll = panel.scroll.min(max_scroll);
            }
        }

        if let Some(prompt) = &mut self.prompt {
            prompt.cursor = prompt.cursor.min(prompt.input.chars().count());
        }

        if !repaired.is_empty() {
            self.last_error = Some(format!("recovered invalid state: {}", repaired.join(", ")));
        }

        Ok(())
    }

    pub fn editor_split_active(&self) -> bool {
        self.active_tab.is_some()
            && self
                .editor_split
                .is_some_and(|index| index < self.tabs.len())
    }

    pub fn editor_visible_panes(&self) -> Vec<(usize, usize)> {
        let Some(active) = self.active_tab else {
            return Vec::new();
        };
        let Some(other) = self.editor_split.filter(|index| *index < self.tabs.len()) else {
            return vec![(0, active)];
        };
        if self.active_editor_pane == 0 {
            vec![(0, active), (1, other)]
        } else {
            vec![(0, other), (1, active)]
        }
    }

    fn normalize_editor_split(&mut self) {
        if self.active_tab.is_none() || self.tabs.is_empty() {
            self.editor_split = None;
            self.active_editor_pane = 0;
            return;
        }
        if self
            .editor_split
            .is_some_and(|index| index >= self.tabs.len())
        {
            self.editor_split = None;
            self.active_editor_pane = 0;
        }
        self.active_editor_pane = self.active_editor_pane.min(1);
    }

    fn adjust_editor_split_after_tab_removed(&mut self, removed: usize) {
        self.editor_split = self.editor_split.and_then(|index| {
            if index == removed {
                None
            } else if index > removed {
                Some(index - 1)
            } else {
                Some(index)
            }
        });
        self.normalize_editor_split();
    }

    fn activate_editor_pane(&mut self, pane: usize) {
        let Some((body, _, tab_index)) = self
            .hit_regions
            .editor_panes
            .iter()
            .find(|(_, candidate, _)| *candidate == pane)
            .copied()
        else {
            return;
        };
        if tab_index >= self.tabs.len() {
            return;
        }
        let previous = self.active_tab;
        self.active_tab = Some(tab_index);
        self.active_editor_pane = pane.min(1);
        if previous.is_some_and(|index| index != tab_index) {
            self.editor_split = previous;
        }
        self.hit_regions.editor_body = Some(body);
        self.editor_height = body.height as usize;
        self.editor_width = body.width as usize;
        self.focus = FocusPanel::Editor;
        self.normalize_editor_split();
    }

    fn activate_editor_target(&mut self, target: &HoverTarget) {
        if let HoverTarget::EditorPane(pane) = target {
            self.activate_editor_pane(*pane);
        }
    }

    fn ensure_active_tab_writable(&mut self, action: &str) -> bool {
        let Some(tab) = self.active_tab() else {
            self.message = Some(format!("{action} requires an active editor tab"));
            return false;
        };
        if tab.read_only {
            self.message = Some(format!("{} is read-only; {action} skipped", tab.title));
            return false;
        }
        true
    }

    pub fn active_terminal(&self) -> &TerminalSession {
        &self.terminals[self.active_terminal]
    }

    pub fn active_terminal_mut(&mut self) -> &mut TerminalSession {
        &mut self.terminals[self.active_terminal]
    }

    pub fn visible_terminal_indices(&self) -> Vec<usize> {
        let mut indices = Vec::new();
        if let Some(split) = self.split_terminal
            && split < self.terminals.len()
            && split != self.active_terminal
        {
            indices.push(split);
        }
        if self.active_terminal < self.terminals.len() {
            indices.push(self.active_terminal);
        }
        if indices.is_empty() && !self.terminals.is_empty() {
            indices.push(0);
        }
        indices
    }

    pub fn terminal_split_active(&self) -> bool {
        self.visible_terminal_indices().len() > 1
    }

    fn normalize_terminal_split(&mut self) {
        self.active_terminal = self
            .active_terminal
            .min(self.terminals.len().saturating_sub(1));
        self.split_terminal = self
            .split_terminal
            .filter(|index| *index < self.terminals.len() && *index != self.active_terminal);
    }

    fn set_active_terminal(&mut self, index: usize) {
        if index >= self.terminals.len() {
            return;
        }
        let previous = self.active_terminal;
        if self.split_terminal == Some(index) && previous != index {
            self.split_terminal = Some(previous);
        }
        self.active_terminal = index;
        self.normalize_terminal_split();
    }

    fn active_terminal_body(&self) -> Option<Rect> {
        self.hit_regions
            .terminal_bodies
            .iter()
            .find_map(|(rect, index)| (*index == self.active_terminal).then_some(*rect))
            .or(self.hit_regions.terminal_body)
    }

    fn terminal_body_under_mouse(&self) -> Option<(Rect, usize)> {
        let x = self.hit_regions.last_mouse_x;
        let y = self.hit_regions.last_mouse_y;
        self.hit_regions
            .terminal_bodies
            .iter()
            .find_map(|(rect, index)| contains(*rect, x, y).then_some((*rect, *index)))
            .or_else(|| {
                self.hit_regions
                    .terminal_body
                    .filter(|rect| contains(*rect, x, y))
                    .map(|rect| (rect, self.active_terminal))
            })
    }

    fn activate_terminal_under_mouse(&mut self) -> Option<Rect> {
        let (body, index) = self.terminal_body_under_mouse()?;
        self.set_active_terminal(index);
        Some(body)
    }

    fn handle_scroll(
        &mut self,
        target: HoverTarget,
        amount: isize,
        up: bool,
        modifiers: KeyModifiers,
    ) -> Result<()> {
        if matches!(target, HoverTarget::Terminal | HoverTarget::TerminalInput)
            && !terminal_host_mouse_override(modifiers)
            && self.send_terminal_mouse_wheel(up)?
        {
            return Ok(());
        }

        self.activate_editor_target(&target);
        self.scroll_target(target, amount);
        Ok(())
    }

    fn scroll_target(&mut self, target: HoverTarget, amount: isize) {
        match target {
            HoverTarget::Explorer | HoverTarget::ExplorerRow(_) => self.scroll_explorer(amount),
            HoverTarget::Outline | HoverTarget::OutlineRow(_) => self.scroll_outline(amount),
            HoverTarget::Editor
            | HoverTarget::EditorPane(_)
            | HoverTarget::Tab(_)
            | HoverTarget::TabClose(_) => self.scroll_editor(amount),
            HoverTarget::QuickRow(_) => self.scroll_quick_panel(amount),
            HoverTarget::Terminal
            | HoverTarget::TerminalInput
            | HoverTarget::TerminalTab(_)
            | HoverTarget::TerminalTabClose(_)
            | HoverTarget::TerminalNew
            | HoverTarget::TerminalResize => self.scroll_terminal(amount),
            HoverTarget::None => match self.focus {
                FocusPanel::Explorer if self.sidebar_mode == SidebarMode::Outline => {
                    self.scroll_outline(amount)
                }
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

    fn scroll_outline(&mut self, amount: isize) {
        let len = self.visible_outline_items().len();
        let max_scroll = len.saturating_sub(self.explorer_height.max(1));
        self.outline_scroll = add_signed(self.outline_scroll, amount).min(max_scroll);
        self.outline_selected = self.outline_selected.min(len.saturating_sub(1));
    }

    fn scroll_editor(&mut self, amount: isize) {
        let height = self.editor_height.max(1);
        let editor_width = self.editor_width;
        let word_wrap = self.word_wrap;
        if let Some(tab) = self.active_tab_mut() {
            let code_width = editor_code_width(tab, editor_width);
            let max_scroll = editor_visual_rows(tab, code_width, word_wrap)
                .len()
                .saturating_sub(height);
            tab.scroll = add_signed(tab.scroll, amount).min(max_scroll);
        }
    }

    fn scroll_target_horizontal(&mut self, target: HoverTarget, amount: isize) {
        self.activate_editor_target(&target);
        match target {
            HoverTarget::Editor
            | HoverTarget::EditorPane(_)
            | HoverTarget::Tab(_)
            | HoverTarget::TabClose(_) => self.scroll_editor_horizontal(amount),
            HoverTarget::None if self.focus == FocusPanel::Editor => {
                self.scroll_editor_horizontal(amount)
            }
            _ => {}
        }
    }

    fn scroll_editor_horizontal(&mut self, amount: isize) {
        if self.word_wrap {
            if let Some(tab) = self.active_tab_mut() {
                tab.horizontal_scroll = 0;
            }
            return;
        }
        let editor_width = self.editor_width;
        if let Some(tab) = self.active_tab_mut() {
            let code_width = editor_code_width(tab, editor_width);
            let max_scroll = max_editor_horizontal_scroll(tab, code_width);
            tab.horizontal_scroll = add_signed(tab.horizontal_scroll, amount).min(max_scroll);
        }
    }

    fn scroll_terminal(&mut self, amount: isize) {
        self.active_terminal_mut()
            .shell
            .scroll(amount.saturating_neg());
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

    fn resize_terminal_from_mouse(&mut self, row: u16) {
        if self.terminal_maximized {
            return;
        }

        let Some(terminal_area) = self.hit_regions.terminal_area else {
            return;
        };
        let Some(editor_area) = self.hit_regions.editor_area else {
            return;
        };

        let total_height = terminal_area.height.saturating_add(editor_area.height);
        let min_terminal_rows = 4;
        let max_terminal_rows = total_height.saturating_sub(8).max(min_terminal_rows);
        let proposed = terminal_area.bottom().saturating_sub(row);
        self.terminal_rows = proposed.clamp(min_terminal_rows, max_terminal_rows);
    }
}

#[derive(Debug, Clone, Copy)]
enum PromptEdit<'a> {
    Insert(&'a str),
    MoveLeft,
    MoveRight,
    MoveStart,
    MoveEnd,
    Backspace,
    Delete,
    DeleteBeforeCursor,
    DeleteAfterCursor,
}

fn edit_text_at_cursor(text: &mut String, cursor: &mut usize, edit: PromptEdit<'_>) {
    *cursor = (*cursor).min(text.chars().count());
    match edit {
        PromptEdit::Insert(inserted) => {
            if inserted.is_empty() {
                return;
            }
            let byte = byte_index_for_char(text, *cursor);
            text.insert_str(byte, inserted);
            *cursor += inserted.chars().count();
        }
        PromptEdit::MoveLeft => {
            *cursor = cursor.saturating_sub(1);
        }
        PromptEdit::MoveRight => {
            *cursor = (*cursor + 1).min(text.chars().count());
        }
        PromptEdit::MoveStart => {
            *cursor = 0;
        }
        PromptEdit::MoveEnd => {
            *cursor = text.chars().count();
        }
        PromptEdit::Backspace => {
            if *cursor == 0 {
                return;
            }
            let end = byte_index_for_char(text, *cursor);
            let start = byte_index_for_char(text, *cursor - 1);
            text.replace_range(start..end, "");
            *cursor -= 1;
        }
        PromptEdit::Delete => {
            let char_len = text.chars().count();
            if *cursor >= char_len {
                return;
            }
            let start = byte_index_for_char(text, *cursor);
            let end = byte_index_for_char(text, *cursor + 1);
            text.replace_range(start..end, "");
        }
        PromptEdit::DeleteBeforeCursor => {
            if *cursor == 0 {
                return;
            }
            let end = byte_index_for_char(text, *cursor);
            text.replace_range(..end, "");
            *cursor = 0;
        }
        PromptEdit::DeleteAfterCursor => {
            let char_len = text.chars().count();
            if *cursor >= char_len {
                return;
            }
            let start = byte_index_for_char(text, *cursor);
            text.replace_range(start.., "");
        }
    }
}

pub(crate) fn editable_text_with_cursor(text: &str, cursor: usize) -> String {
    let cursor = cursor.min(text.chars().count());
    let byte = byte_index_for_char(text, cursor);
    let mut rendered = String::with_capacity(text.len() + 1);
    rendered.push_str(&text[..byte]);
    rendered.push('|');
    rendered.push_str(&text[byte..]);
    rendered
}

fn add_signed(value: usize, amount: isize) -> usize {
    if amount.is_negative() {
        value.saturating_sub(amount.unsigned_abs())
    } else {
        value.saturating_add(amount as usize)
    }
}

fn explorer_multi_select_modifier(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::META)
}

fn normalize_file_op_paths(mut paths: Vec<PathBuf>) -> Vec<PathBuf> {
    paths.sort();
    paths.dedup();
    paths.sort_by(|a, b| {
        a.components()
            .count()
            .cmp(&b.components().count())
            .then_with(|| a.cmp(b))
    });

    let mut normalized: Vec<PathBuf> = Vec::new();
    'path: for path in paths {
        for kept in &normalized {
            if path != *kept && path.starts_with(kept) {
                continue 'path;
            }
        }
        normalized.push(path);
    }
    normalized
}

fn normalize_editor_selections(mut ranges: Vec<EditorSelection>) -> Vec<EditorSelection> {
    ranges.sort_by(|a, b| a.start.cmp(&b.start).then(a.end.cmp(&b.end)));
    let mut normalized = Vec::new();
    for range in ranges {
        if range.start == range.end {
            continue;
        }
        if normalized
            .last()
            .is_some_and(|previous: &EditorSelection| range.start < previous.end)
        {
            continue;
        }
        if normalized.last() == Some(&range) {
            continue;
        }
        normalized.push(range);
    }
    normalized
}

fn replacement_end_position(start: (usize, usize), replacement: &str) -> (usize, usize) {
    let normalized = replacement.replace("\r\n", "\n").replace('\r', "\n");
    let mut parts = normalized.split('\n').collect::<Vec<_>>();
    if parts.len() <= 1 {
        return (start.0, start.1 + normalized.chars().count());
    }
    let last = parts.pop().unwrap_or_default();
    (start.0 + parts.len(), last.chars().count())
}

fn shift_cursor_positions_for_replacement(
    cursors: &mut [(usize, usize)],
    start: (usize, usize),
    end: (usize, usize),
    replacement: &str,
) {
    if replacement.contains('\n') || start.0 != end.0 {
        return;
    }

    let removed = end.1.saturating_sub(start.1);
    let inserted = replacement.chars().count();
    for cursor in cursors {
        if cursor.0 == start.0 && cursor.1 >= end.1 {
            if inserted >= removed {
                cursor.1 = cursor.1.saturating_add(inserted - removed);
            } else {
                cursor.1 = cursor.1.saturating_sub(removed - inserted);
            }
        }
    }
}

fn editor_code_width(tab: &EditorTab, editor_width: usize) -> usize {
    editor_width.saturating_sub(editor_gutter_width(tab.lines.len()))
}

fn editor_gutter_width(line_count: usize) -> usize {
    line_count.max(1).to_string().len().max(3) + 3
}

pub fn editor_visual_rows(
    tab: &EditorTab,
    code_width: usize,
    word_wrap: bool,
) -> Vec<EditorVisualRow> {
    let visible_lines = tab.visible_line_indices();
    if !word_wrap || code_width == 0 {
        return visible_lines
            .into_iter()
            .map(|line| EditorVisualRow {
                line,
                start_col: 0,
                continuation: false,
            })
            .collect();
    }

    let mut rows = Vec::new();
    for line in visible_lines {
        let line_len = tab
            .lines
            .get(line)
            .map(|text| text.chars().count())
            .unwrap_or(0);
        if line_len == 0 {
            rows.push(EditorVisualRow {
                line,
                start_col: 0,
                continuation: false,
            });
            continue;
        }
        let mut start_col = 0usize;
        while start_col < line_len {
            rows.push(EditorVisualRow {
                line,
                start_col,
                continuation: start_col > 0,
            });
            start_col = start_col.saturating_add(code_width.max(1));
        }
    }
    rows
}

pub fn editor_visual_row_for_line_col(
    tab: &EditorTab,
    line: usize,
    col: usize,
    code_width: usize,
    word_wrap: bool,
) -> Option<usize> {
    if !word_wrap || code_width == 0 {
        return tab.visible_row_for_line(line);
    }

    let mut row = 0usize;
    for visible_line in tab.visible_line_indices() {
        if visible_line == line {
            let line_len = tab
                .lines
                .get(line)
                .map(|text| text.chars().count())
                .unwrap_or(0);
            if line_len == 0 {
                return Some(row);
            }
            let cursor_col = col.min(line_len.saturating_sub(1));
            return Some(row + cursor_col / code_width.max(1));
        }
        row += visual_row_count_for_line(tab, visible_line, code_width, word_wrap);
    }
    None
}

fn visual_row_count_for_line(
    tab: &EditorTab,
    line: usize,
    code_width: usize,
    word_wrap: bool,
) -> usize {
    if !word_wrap || code_width == 0 {
        return 1;
    }
    let line_len = tab
        .lines
        .get(line)
        .map(|text| text.chars().count())
        .unwrap_or(0);
    line_len.saturating_sub(1) / code_width.max(1) + 1
}

fn max_editor_horizontal_scroll(tab: &EditorTab, code_width: usize) -> usize {
    if code_width == 0 {
        return 0;
    }
    tab.lines
        .iter()
        .map(|line| line.chars().count().saturating_sub(code_width))
        .max()
        .unwrap_or(0)
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
    workspace_visible_paths: &HashSet<PathBuf>,
    filter: Option<&str>,
) -> Vec<VisibleNode> {
    let base_visible = nodes
        .into_iter()
        .filter(|node| {
            node_passes_explorer_visibility(
                node,
                root,
                show_hidden,
                show_ignored,
                workspace_visible_paths,
            )
        })
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
    workspace_visible_paths: &HashSet<PathBuf>,
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
    if !workspace_visible_paths.is_empty() && !workspace_visible_paths.contains(&node.path) {
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

fn explorer_path_filter_matches(path: &Path, root: &Path, filter: &str) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.to_lowercase().contains(filter))
        || relative_path(root, path).to_lowercase().contains(filter)
}

#[derive(Debug, Clone, Copy)]
struct CommandSpec {
    label: &'static str,
    detail: &'static str,
    shortcut: &'static str,
    action: CommandAction,
}

#[derive(Debug, Clone)]
struct ContextMenuAction {
    label: &'static str,
    detail: String,
    shortcut: &'static str,
    action: CommandAction,
}

fn context_menu_items(path: PathBuf, specs: Vec<ContextMenuAction>, query: &str) -> Vec<QuickItem> {
    let query = query.trim();
    if query.is_empty() {
        return specs
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|spec| QuickItem {
                label: spec.label.to_owned(),
                detail: spec.detail,
                path: path.clone(),
                line: None,
                col: None,
                preview: (!spec.shortcut.is_empty()).then(|| spec.shortcut.to_owned()),
                command: Some(spec.action),
            })
            .collect();
    }

    let mut scored = specs
        .into_iter()
        .filter_map(|spec| {
            let haystack = format!("{} {}", spec.label, spec.detail);
            fuzzy_score(&haystack, query).map(|score| {
                (
                    score,
                    QuickItem {
                        label: spec.label.to_owned(),
                        detail: spec.detail,
                        path: path.clone(),
                        line: None,
                        col: None,
                        preview: (!spec.shortcut.is_empty()).then(|| spec.shortcut.to_owned()),
                        command: Some(spec.action),
                    },
                )
            })
        })
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.label.cmp(&b.1.label)));
    scored
        .into_iter()
        .take(MAX_QUICK_ITEMS)
        .map(|(_, item)| item)
        .collect()
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
            label: "Open Folder",
            detail: "Switch the current workspace root to an existing folder",
            shortcut: "",
            action: CommandAction::OpenFolder,
        },
        CommandSpec {
            label: "Show Explorer Files",
            detail: "Show the workspace file tree in the left sidebar",
            shortcut: "Sidebar m",
            action: CommandAction::ShowExplorerFiles,
        },
        CommandSpec {
            label: "Show Outline",
            detail: "Show active-file symbols in the left sidebar and jump by clicking rows",
            shortcut: "Sidebar m",
            action: CommandAction::ShowOutline,
        },
        CommandSpec {
            label: "Toggle Sidebar Mode",
            detail: "Switch the left sidebar between file explorer and active-file outline",
            shortcut: "Sidebar m",
            action: CommandAction::ToggleSidebarMode,
        },
        CommandSpec {
            label: "Trigger Suggest",
            detail: "Complete the identifier at the editor cursor from workspace symbols and words",
            shortcut: "Ctrl-Space",
            action: CommandAction::TriggerSuggest,
        },
        CommandSpec {
            label: "Search Workspace",
            detail: "Search text across workspace files and unsaved open buffers",
            shortcut: "Ctrl-Shift-F",
            action: CommandAction::WorkspaceSearch,
        },
        CommandSpec {
            label: "Document Symbols",
            detail: "List functions, types, and classes from the active editor buffer",
            shortcut: "Ctrl-Shift-O",
            action: CommandAction::DocumentSymbols,
        },
        CommandSpec {
            label: "Workspace Symbols",
            detail: "Search workspace symbols from functions, types, classes, and modules",
            shortcut: "Ctrl-T",
            action: CommandAction::WorkspaceSymbols,
        },
        CommandSpec {
            label: "Show Hover",
            detail: "Ask an installed language server for hover information at the editor cursor",
            shortcut: "",
            action: CommandAction::ShowHover,
        },
        CommandSpec {
            label: "Signature Help",
            detail: "Ask an installed language server for call signature information at the editor cursor",
            shortcut: "Ctrl-Shift-Space",
            action: CommandAction::SignatureHelp,
        },
        CommandSpec {
            label: "Go to Definition",
            detail: "Jump from the symbol under the editor cursor using LSP first, then workspace scan",
            shortcut: "Ctrl-]",
            action: CommandAction::GoToDefinition,
        },
        CommandSpec {
            label: "Go to Type Definition",
            detail: "Jump to the language-server type definition for the symbol under the editor cursor",
            shortcut: "",
            action: CommandAction::GoToTypeDefinition,
        },
        CommandSpec {
            label: "Go to Implementation",
            detail: "Jump to language-server implementations for the symbol under the editor cursor",
            shortcut: "",
            action: CommandAction::GoToImplementation,
        },
        CommandSpec {
            label: "Go to Matching Bracket",
            detail: "Jump between matching (), [], and {} brackets in the active editor buffer",
            shortcut: "Ctrl-Shift-\\",
            action: CommandAction::GoToMatchingBracket,
        },
        CommandSpec {
            label: "Show Incoming Calls",
            detail: "Ask an installed language server which functions call the symbol under the cursor",
            shortcut: "",
            action: CommandAction::ShowIncomingCalls,
        },
        CommandSpec {
            label: "Show Outgoing Calls",
            detail: "Ask an installed language server which functions are called by the symbol under the cursor",
            shortcut: "",
            action: CommandAction::ShowOutgoingCalls,
        },
        CommandSpec {
            label: "Highlight Symbol",
            detail: "Ask an installed language server to highlight reads and writes for the symbol under the cursor",
            shortcut: "Ctrl-Shift-E",
            action: CommandAction::HighlightSymbol,
        },
        CommandSpec {
            label: "Clear Symbol Highlights",
            detail: "Clear active language-server document highlights from the editor",
            shortcut: "",
            action: CommandAction::ClearDocumentHighlights,
        },
        CommandSpec {
            label: "Find References",
            detail: "List whole-word workspace references for the symbol under the editor cursor",
            shortcut: "Ctrl-R",
            action: CommandAction::FindReferences,
        },
        CommandSpec {
            label: "Code Action",
            detail: "Ask an installed language server for quick fixes and refactors at the editor cursor",
            shortcut: "",
            action: CommandAction::CodeAction,
        },
        CommandSpec {
            label: "Go Back",
            detail: "Return to the previous editor navigation location",
            shortcut: "Alt-Left",
            action: CommandAction::GoBack,
        },
        CommandSpec {
            label: "Go Forward",
            detail: "Move forward in editor navigation history after going back",
            shortcut: "Alt-Right",
            action: CommandAction::GoForward,
        },
        CommandSpec {
            label: "Rename Symbol",
            detail: "Rename the identifier under the editor cursor across workspace files",
            shortcut: "F2",
            action: CommandAction::RenameSymbol,
        },
        CommandSpec {
            label: "Replace in Files",
            detail: "Replace text across real workspace files, skipping dirty open buffers",
            shortcut: "Ctrl-Shift-H",
            action: CommandAction::WorkspaceReplace,
        },
        CommandSpec {
            label: "Run Workspace Check",
            detail: "Run the detected project checker and collect compiler diagnostics",
            shortcut: "",
            action: CommandAction::RunWorkspaceCheck,
        },
        CommandSpec {
            label: "Run LSP Diagnostics",
            detail: "Ask the installed language server for diagnostics on the active editor buffer",
            shortcut: "",
            action: CommandAction::RunLspDiagnostics,
        },
        CommandSpec {
            label: "Show Problems",
            detail: "Show the last collected workspace or language-server diagnostics",
            shortcut: "",
            action: CommandAction::ShowProblems,
        },
        CommandSpec {
            label: "Show Bookmarks",
            detail: "List all editor bookmarks across open tabs and jump to one",
            shortcut: "",
            action: CommandAction::ShowBookmarks,
        },
        CommandSpec {
            label: "Toggle Bookmark",
            detail: "Add or remove a bookmark on the active editor line",
            shortcut: "Alt-B",
            action: CommandAction::ToggleBookmark,
        },
        CommandSpec {
            label: "Next Bookmark",
            detail: "Jump to the next editor bookmark across open tabs",
            shortcut: "Alt-N",
            action: CommandAction::NextBookmark,
        },
        CommandSpec {
            label: "Previous Bookmark",
            detail: "Jump to the previous editor bookmark across open tabs",
            shortcut: "Alt-P",
            action: CommandAction::PreviousBookmark,
        },
        CommandSpec {
            label: "Clear Bookmarks",
            detail: "Remove every bookmark from the active editor tab",
            shortcut: "",
            action: CommandAction::ClearBookmarks,
        },
        CommandSpec {
            label: "Source Control",
            detail: "Show Git changed files and diff hunks",
            shortcut: "",
            action: CommandAction::ShowSourceControl,
        },
        CommandSpec {
            label: "Git Checkout Branch",
            detail: "List local Git branches and switch the workspace branch",
            shortcut: "",
            action: CommandAction::ShowGitBranches,
        },
        CommandSpec {
            label: "Git Create Branch",
            detail: "Prompt for a branch name, create it, and switch to it",
            shortcut: "",
            action: CommandAction::CreateGitBranch,
        },
        CommandSpec {
            label: "Stage All Changes",
            detail: "Stage every current Git working tree change",
            shortcut: "",
            action: CommandAction::StageAllChanges,
        },
        CommandSpec {
            label: "Unstage All Changes",
            detail: "Remove every staged Git change from the index",
            shortcut: "",
            action: CommandAction::UnstageAllChanges,
        },
        CommandSpec {
            label: "Commit Staged Changes",
            detail: "Prompt for a message, then commit the staged Git index",
            shortcut: "",
            action: CommandAction::CommitStagedChanges,
        },
        CommandSpec {
            label: "Commit All Changes",
            detail: "Prompt for a message, stage all Git changes, then commit",
            shortcut: "",
            action: CommandAction::CommitAllChanges,
        },
        CommandSpec {
            label: "Discard All Changes",
            detail: "Prompt, then discard every Git working tree change and untracked file",
            shortcut: "",
            action: CommandAction::DiscardAllChanges,
        },
        CommandSpec {
            label: "Run Task",
            detail: "Detect workspace tasks and run one in a real integrated PTY terminal",
            shortcut: "Ctrl-Shift-B",
            action: CommandAction::RunTask,
        },
        CommandSpec {
            label: "Run Active File in Terminal",
            detail: "Start a new PTY terminal and run the saved active editor file",
            shortcut: "F5",
            action: CommandAction::RunActiveFileInTerminal,
        },
        CommandSpec {
            label: "New Untitled File",
            detail: "Create a scratch editor tab that can be saved with Save As",
            shortcut: "Ctrl-N",
            action: CommandAction::NewUntitledFile,
        },
        CommandSpec {
            label: "Show Open Editors",
            detail: "List open editor tabs, including dirty and Untitled buffers, and switch to one",
            shortcut: "",
            action: CommandAction::ShowOpenEditors,
        },
        CommandSpec {
            label: "Save File",
            detail: "Write the active editor tab to disk",
            shortcut: "Ctrl-S",
            action: CommandAction::SaveFile,
        },
        CommandSpec {
            label: "Save As",
            detail: "Write the active editor buffer to a new path and retarget the tab",
            shortcut: "",
            action: CommandAction::SaveAs,
        },
        CommandSpec {
            label: "Save All",
            detail: "Write all dirty editor tabs to disk",
            shortcut: "Ctrl-K S",
            action: CommandAction::SaveAll,
        },
        CommandSpec {
            label: "Revert File",
            detail: "Discard editor changes and reload the active file from disk",
            shortcut: "",
            action: CommandAction::RevertFile,
        },
        CommandSpec {
            label: "Reload File From Disk",
            detail: "Reload the active tab from disk and clear disk-change warnings",
            shortcut: "",
            action: CommandAction::RevertFile,
        },
        CommandSpec {
            label: "Format Document",
            detail: "Format the active editor buffer with LSP or an installed formatter",
            shortcut: "Shift-Alt-F",
            action: CommandAction::FormatDocument,
        },
        CommandSpec {
            label: "Close Active Tab",
            detail: "Close the active tab, asking how to handle unsaved edits",
            shortcut: "Ctrl-W",
            action: CommandAction::CloseActiveTab,
        },
        CommandSpec {
            label: "Reopen Closed Editor",
            detail: "Restore the most recently closed editor tab with its buffer and view state",
            shortcut: "Ctrl-Shift-T",
            action: CommandAction::ReopenClosedEditor,
        },
        CommandSpec {
            label: "Close All Editors",
            detail: "Close every clean editor tab and keep dirty tabs open",
            shortcut: "",
            action: CommandAction::CloseAllTabs,
        },
        CommandSpec {
            label: "Close Other Editors",
            detail: "Close every clean editor except the active tab, keeping dirty tabs open",
            shortcut: "",
            action: CommandAction::CloseOtherTabs,
        },
        CommandSpec {
            label: "Close Editors to the Right",
            detail: "Close clean editor tabs to the right of the active tab",
            shortcut: "",
            action: CommandAction::CloseTabsToRight,
        },
        CommandSpec {
            label: "Open Active Editor to Side",
            detail: "Show the active editor tab in a side-by-side editor pane",
            shortcut: "Ctrl-\\",
            action: CommandAction::OpenActiveTabToSide,
        },
        CommandSpec {
            label: "Close Editor Split",
            detail: "Return the editor to a single active pane",
            shortcut: "",
            action: CommandAction::CloseEditorSplit,
        },
        CommandSpec {
            label: "Close Saved Editors",
            detail: "Close every clean editor tab and keep dirty tabs open",
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
            label: "Open Selected Explorer Item to Side",
            detail: "Open the selected file in a side-by-side editor pane",
            shortcut: "Explorer Ctrl-Enter",
            action: CommandAction::OpenSelectedExplorerItemToSide,
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
            label: "Compare Selected Files",
            detail: "Open a read-only unified diff for two selected explorer files",
            shortcut: "Explorer v",
            action: CommandAction::CompareSelectedFiles,
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
            label: "Cycle Explorer Sort",
            detail: "Cycle the explorer through name, type, modified time, and size sorting",
            shortcut: "s",
            action: CommandAction::CycleExplorerSort,
        },
        CommandSpec {
            label: "Sort Explorer by Name",
            detail: "Show folders first, then sort entries case-insensitively by name",
            shortcut: "",
            action: CommandAction::SortExplorerByName,
        },
        CommandSpec {
            label: "Sort Explorer by Type",
            detail: "Show folders first, then sort files by extension and name",
            shortcut: "",
            action: CommandAction::SortExplorerByType,
        },
        CommandSpec {
            label: "Sort Explorer by Modified Time",
            detail: "Show folders first, then sort newest entries before older entries",
            shortcut: "",
            action: CommandAction::SortExplorerByModified,
        },
        CommandSpec {
            label: "Sort Explorer by Size",
            detail: "Show folders first, then sort larger files before smaller files",
            shortcut: "",
            action: CommandAction::SortExplorerBySize,
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
            label: "Add Selection to Next Match",
            detail: "Select the next occurrence of the current word or selection in the active file",
            shortcut: "Ctrl-D",
            action: CommandAction::AddSelectionToNextMatch,
        },
        CommandSpec {
            label: "Select All Occurrences",
            detail: "Select every occurrence of the current word or selection in the active file",
            shortcut: "Ctrl-Shift-L",
            action: CommandAction::SelectAllOccurrences,
        },
        CommandSpec {
            label: "Duplicate Line",
            detail: "Duplicate the current editor line",
            shortcut: "Ctrl-Shift-D",
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
            label: "Toggle Block Comment",
            detail: "Wrap or unwrap the selection or current line with the file type's block comment tokens",
            shortcut: "Shift-Alt-A",
            action: CommandAction::ToggleBlockComment,
        },
        CommandSpec {
            label: "Toggle Word Wrap",
            detail: "Wrap long editor lines to the visible pane width without changing file contents",
            shortcut: "Alt-Z",
            action: CommandAction::ToggleWordWrap,
        },
        CommandSpec {
            label: "Toggle Fold",
            detail: "Fold or unfold the code block at the editor cursor",
            shortcut: "Alt-[",
            action: CommandAction::ToggleFold,
        },
        CommandSpec {
            label: "Fold All",
            detail: "Fold every detected block in the active editor tab",
            shortcut: "Alt-0",
            action: CommandAction::FoldAll,
        },
        CommandSpec {
            label: "Unfold All",
            detail: "Show every folded block in the active editor tab",
            shortcut: "Alt-]",
            action: CommandAction::UnfoldAll,
        },
        CommandSpec {
            label: "Trim Trailing Whitespace",
            detail: "Remove spaces and tabs at line ends in the active editor tab",
            shortcut: "",
            action: CommandAction::TrimTrailingWhitespace,
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
            label: "Run Selection in Terminal",
            detail: "Send the editor selection or current line to the active PTY shell",
            shortcut: "Ctrl-Enter",
            action: CommandAction::RunSelectionInTerminal,
        },
        CommandSpec {
            label: "Copy Terminal Selection",
            detail: "Copy the active terminal text selection to the internal and terminal clipboard",
            shortcut: "Ctrl-Shift-C",
            action: CommandAction::CopyTerminalSelection,
        },
        CommandSpec {
            label: "Copy Terminal Output",
            detail: "Copy the active terminal viewport and retained scrollback",
            shortcut: "",
            action: CommandAction::CopyTerminalOutput,
        },
        CommandSpec {
            label: "Paste Clipboard to Terminal",
            detail: "Paste the internal clipboard into the active PTY shell",
            shortcut: "Ctrl-Shift-V",
            action: CommandAction::PasteClipboardToTerminal,
        },
        CommandSpec {
            label: "Find in Terminal",
            detail: "Search the active terminal viewport and scrollback",
            shortcut: "Terminal Ctrl-F",
            action: CommandAction::FindInTerminal,
        },
        CommandSpec {
            label: "Run Terminal Command",
            detail: "Prompt for a shell command and send it to the active PTY terminal",
            shortcut: "",
            action: CommandAction::RunTerminalCommand,
        },
        CommandSpec {
            label: "Run Recent Terminal Command",
            detail: "Pick a tscode-submitted shell command and send it to the active PTY terminal again",
            shortcut: "",
            action: CommandAction::RunRecentTerminalCommand,
        },
        CommandSpec {
            label: "Next Terminal Search Match",
            detail: "Jump to the next match in the active terminal scrollback",
            shortcut: "Terminal F3",
            action: CommandAction::TerminalSearchNext,
        },
        CommandSpec {
            label: "Previous Terminal Search Match",
            detail: "Jump to the previous match in the active terminal scrollback",
            shortcut: "Terminal Shift-F3",
            action: CommandAction::TerminalSearchPrevious,
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
            label: "Rename Terminal",
            detail: "Change the active terminal tab title without restarting its PTY shell",
            shortcut: "",
            action: CommandAction::RenameTerminal,
        },
        CommandSpec {
            label: "New Terminal",
            detail: "Create a new integrated PTY terminal session",
            shortcut: "Ctrl-Shift-` / F7",
            action: CommandAction::NewTerminal,
        },
        CommandSpec {
            label: "New Terminal Here",
            detail: "Create a new PTY terminal in the selected explorer folder",
            shortcut: "Explorer t",
            action: CommandAction::NewTerminalHere,
        },
        CommandSpec {
            label: "Split Terminal",
            detail: "Create a side-by-side PTY terminal pane from the active terminal cwd",
            shortcut: "Ctrl-Shift-5",
            action: CommandAction::SplitTerminal,
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
            shortcut: "Ctrl-PageDown / F8",
            action: CommandAction::NextTerminal,
        },
        CommandSpec {
            label: "Previous Terminal",
            detail: "Switch to the previous integrated terminal session",
            shortcut: "Ctrl-PageUp",
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
            label: "Scroll Terminal to Bottom",
            detail: "Return the active terminal viewport to the live shell output",
            shortcut: "",
            action: CommandAction::ScrollTerminalToBottom,
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
            cursor: initial.chars().count(),
        });
    }

    fn start_replace_prompt(&mut self, all: bool) {
        let initial = self.search_needle.clone().unwrap_or_default();
        self.start_prompt(PromptKind::ReplaceFind { all }, &initial);
    }

    fn start_workspace_replace_prompt(&mut self) {
        let initial = self.search_needle.clone().unwrap_or_default();
        self.start_prompt(PromptKind::WorkspaceReplaceFind, &initial);
    }

    fn start_open_folder_prompt(&mut self) {
        self.start_prompt(PromptKind::OpenFolder, "");
    }

    fn start_new_file_prompt(&mut self) {
        let initial = self.new_item_prompt_prefix();
        self.start_prompt(PromptKind::NewFile, &initial);
    }

    fn start_new_dir_prompt(&mut self) {
        let initial = self.new_item_prompt_prefix();
        self.start_prompt(PromptKind::NewDir, &initial);
    }

    fn new_item_prompt_prefix(&self) -> String {
        let base = self.selected_base_dir();
        let relative = relative_path(&self.root, &base);
        if relative.is_empty() {
            String::new()
        } else {
            format!("{relative}/")
        }
    }

    fn start_save_as_prompt(&mut self) {
        let Some(tab) = self.active_tab() else {
            self.message = Some("no active file to save as".to_owned());
            return;
        };
        let initial = relative_path(&self.root, &tab.path);
        self.start_prompt(PromptKind::SaveAs, &initial);
    }

    fn trigger_suggest(&mut self) -> Result<()> {
        let Some(state) = self.active_tab().map(completion_state_for_tab) else {
            self.message = Some("no active editor for suggestions".to_owned());
            return Ok(());
        };
        let query = state.prefix.clone();
        self.lsp_completion_items = self.lsp_completion_items_for_state(&state)?;
        self.completion_state = Some(state);
        self.open_quick_panel_with_query(QuickPanelKind::Completions, query)
    }

    fn open_quick_panel(&mut self, kind: QuickPanelKind) -> Result<()> {
        self.open_quick_panel_with_query(kind, String::new())
    }

    fn open_quick_panel_with_query(&mut self, kind: QuickPanelKind, query: String) -> Result<()> {
        if kind != QuickPanelKind::Completions {
            self.completion_state = None;
            self.lsp_completion_items.clear();
        }
        if kind == QuickPanelKind::DocumentSymbols {
            self.refresh_lsp_document_symbol_cache()?;
        }
        self.lsp_workspace_symbol_query = None;
        self.lsp_workspace_symbol_items.clear();
        if kind != QuickPanelKind::CodeActions {
            self.lsp_code_actions.clear();
        }
        self.quick_panel = Some(QuickPanel {
            kind,
            query_cursor: query.chars().count(),
            query,
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
        let existing_items = panel.items.clone();
        let items = match kind {
            QuickPanelKind::OpenFile => self.quick_open_items(&query)?,
            QuickPanelKind::OpenEditors => self.open_editor_items(&query),
            QuickPanelKind::Completions => self.completion_items(&query)?,
            QuickPanelKind::CodeActions => self.code_action_items(&query),
            QuickPanelKind::DirtyClose { index } => self.dirty_close_items(index, &query),
            QuickPanelKind::ExplorerContextMenu => self.explorer_context_menu_items(&query),
            QuickPanelKind::EditorContextMenu => self.editor_context_menu_items(&query),
            QuickPanelKind::TerminalContextMenu => self.terminal_context_menu_items(&query),
            QuickPanelKind::WorkspaceSearch => self.workspace_search_items(&query)?,
            QuickPanelKind::DocumentSymbols => self.document_symbol_items(&query),
            QuickPanelKind::WorkspaceSymbols => self.workspace_symbol_items(&query)?,
            QuickPanelKind::LspHover => filter_existing_quick_items(existing_items, &query),
            QuickPanelKind::SignatureHelp => filter_existing_quick_items(existing_items, &query),
            QuickPanelKind::Definitions => self.definition_items(&query)?,
            QuickPanelKind::TypeDefinitions | QuickPanelKind::Implementations => {
                filter_existing_quick_items(existing_items, &query)
            }
            QuickPanelKind::IncomingCalls | QuickPanelKind::OutgoingCalls => {
                filter_existing_quick_items(existing_items, &query)
            }
            QuickPanelKind::References => self.reference_items(&query)?,
            QuickPanelKind::LspReferences => filter_existing_quick_items(existing_items, &query),
            QuickPanelKind::Problems => self.problem_items(&query),
            QuickPanelKind::Bookmarks => self.bookmark_items(&query),
            QuickPanelKind::SourceControl => self.source_control_items(&query)?,
            QuickPanelKind::Branches => self.branch_items(&query)?,
            QuickPanelKind::Tasks => self.task_items(&query),
            QuickPanelKind::TerminalCommandHistory => self.terminal_command_history_items(&query),
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

    fn open_editor_items(&self, query: &str) -> Vec<QuickItem> {
        if self.tabs.is_empty() {
            return vec![QuickItem {
                label: "No Open Editors".to_owned(),
                detail: "Open a file or create an Untitled editor first".to_owned(),
                path: self.root.clone(),
                line: None,
                col: None,
                preview: None,
                command: None,
            }];
        }

        let query = query.trim();
        let mut scored = Vec::new();
        for (index, tab) in self.tabs.iter().enumerate() {
            let active = self.active_tab == Some(index);
            let mut states = Vec::new();
            if active {
                states.push("active");
            }
            if tab.dirty {
                states.push("dirty");
            }
            if tab.untitled {
                states.push("untitled");
            }
            if tab.read_only {
                states.push("read-only");
            }
            if !tab.external_state.is_clean() {
                states.push(tab.external_state.label());
            }

            let location = if tab.untitled {
                "Untitled editor".to_owned()
            } else {
                relative_path(&self.root, &tab.path)
            };
            let detail = if states.is_empty() {
                location.clone()
            } else {
                format!("{} | {}", location, states.join(", "))
            };
            let mut prefix = String::new();
            prefix.push(if active { '>' } else { ' ' });
            prefix.push(if tab.dirty { '*' } else { ' ' });
            let item = QuickItem {
                label: format!("{prefix} {}", tab.title),
                detail: detail.clone(),
                path: tab.path.clone(),
                line: Some(tab.cursor_line),
                col: Some(tab.cursor_col),
                preview: Some(format!(
                    "line {}, col {}",
                    tab.cursor_line + 1,
                    tab.cursor_col + 1
                )),
                command: Some(CommandAction::SelectEditorTab(index)),
            };

            if query.is_empty() {
                scored.push((0, index, item));
            } else {
                let haystack = format!("{} {} {}", tab.title, detail, index + 1);
                if let Some(score) = fuzzy_score(&haystack, query) {
                    scored.push((score, index, item));
                }
            }
        }

        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, _, item)| item)
            .collect()
    }

    fn dirty_close_items(&self, index: usize, query: &str) -> Vec<QuickItem> {
        let Some(tab) = self.tabs.get(index) else {
            return Vec::new();
        };
        let label = if tab.untitled {
            tab.title.clone()
        } else {
            relative_path(&self.root, &tab.path)
        };
        let specs = [
            (
                "Save and Close",
                format!("Save {label} before closing this tab"),
                "Enter",
                CommandAction::SaveAndCloseTab(index),
            ),
            (
                "Don't Save",
                format!("Discard unsaved edits in {label} and close this tab"),
                "",
                CommandAction::DiscardAndCloseTab(index),
            ),
            (
                "Cancel",
                format!("Keep {label} open with its unsaved edits"),
                "Esc",
                CommandAction::CancelCloseTab,
            ),
        ];
        let query = query.trim();
        let mut scored = specs
            .into_iter()
            .enumerate()
            .filter_map(|(order, (item_label, detail, shortcut, action))| {
                let score = if query.is_empty() {
                    Some(order)
                } else {
                    fuzzy_score(&format!("{item_label} {detail}"), query)
                }?;
                Some((
                    score,
                    order,
                    QuickItem {
                        label: item_label.to_owned(),
                        detail,
                        path: tab.path.clone(),
                        line: None,
                        col: None,
                        preview: (!shortcut.is_empty()).then(|| shortcut.to_owned()),
                        command: Some(action),
                    },
                ))
            })
            .collect::<Vec<_>>();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        scored.into_iter().map(|(_, _, item)| item).collect()
    }

    fn explorer_context_menu_items(&self, query: &str) -> Vec<QuickItem> {
        let Some(node) = self.visible_nodes().get(self.explorer.selected).cloned() else {
            return Vec::new();
        };
        let selected_paths = self.selected_explorer_paths();
        let relative = self.selected_explorer_label(&selected_paths);
        let target_kind = if node.is_dir { "folder" } else { "file" };
        let base = if node.is_dir {
            relative_path(&self.root, &node.path)
        } else {
            node.path
                .parent()
                .map(|parent| relative_path(&self.root, parent))
                .unwrap_or_else(|| ".".to_owned())
        };
        let open_label = if node.is_dir {
            if node.expanded {
                "Collapse Folder"
            } else {
                "Expand Folder"
            }
        } else {
            "Open File"
        };
        let paste_detail = self
            .explorer_clipboard
            .as_ref()
            .map(|clipboard| {
                let action = match clipboard.action {
                    ClipboardAction::Copy => "Copy",
                    ClipboardAction::Cut => "Move",
                };
                let source = if clipboard.paths.len() == 1 {
                    relative_path(&self.root, &clipboard.paths[0])
                } else {
                    format!("{} items", clipboard.paths.len())
                };
                format!("{action} {source} into {base}")
            })
            .unwrap_or_else(|| "Paste the explorer clipboard into the selected folder".to_owned());
        let current_sort = self.explorer.sort_mode().label();

        let mut specs = vec![
            ContextMenuAction {
                label: open_label,
                detail: format!("{} {}", open_label.to_lowercase(), relative),
                shortcut: "Enter",
                action: CommandAction::OpenSelectedExplorerItem,
            },
            ContextMenuAction {
                label: "Open to Side",
                detail: format!("Open {relative} in the side editor pane"),
                shortcut: "Ctrl-Enter",
                action: CommandAction::OpenSelectedExplorerItemToSide,
            },
            ContextMenuAction {
                label: "New File",
                detail: format!("Create a file under {base}"),
                shortcut: "n",
                action: CommandAction::NewFile,
            },
            ContextMenuAction {
                label: "New Folder",
                detail: format!("Create a folder under {base}"),
                shortcut: "N",
                action: CommandAction::NewFolder,
            },
            ContextMenuAction {
                label: "New Terminal Here",
                detail: format!("Open a PTY shell in this {target_kind}'s directory"),
                shortcut: "t",
                action: CommandAction::NewTerminalHere,
            },
            ContextMenuAction {
                label: "Copy Path",
                detail: "Copy the absolute path to the terminal clipboard".to_owned(),
                shortcut: "",
                action: CommandAction::CopySelectedExplorerPath,
            },
            ContextMenuAction {
                label: "Copy Relative Path",
                detail: "Copy the workspace-relative path to the terminal clipboard".to_owned(),
                shortcut: "",
                action: CommandAction::CopySelectedExplorerRelativePath,
            },
            ContextMenuAction {
                label: "Copy",
                detail: format!("Copy {relative} to the explorer clipboard"),
                shortcut: "c",
                action: CommandAction::CopySelectedExplorerItem,
            },
            ContextMenuAction {
                label: "Paste",
                detail: paste_detail,
                shortcut: "p",
                action: CommandAction::PasteIntoSelectedExplorerItem,
            },
            ContextMenuAction {
                label: "Refresh Explorer",
                detail: "Reload the workspace file tree from disk".to_owned(),
                shortcut: "r",
                action: CommandAction::RefreshExplorer,
            },
            ContextMenuAction {
                label: "Collapse Explorer Folders",
                detail: "Collapse all expanded explorer folders".to_owned(),
                shortcut: "",
                action: CommandAction::CollapseExplorer,
            },
            ContextMenuAction {
                label: "Cycle Explorer Sort",
                detail: format!("Current sort: {current_sort}"),
                shortcut: "s",
                action: CommandAction::CycleExplorerSort,
            },
            ContextMenuAction {
                label: "Sort by Name",
                detail: "Folders first, then entries by case-insensitive name".to_owned(),
                shortcut: "",
                action: CommandAction::SortExplorerByName,
            },
            ContextMenuAction {
                label: "Sort by Type",
                detail: "Folders first, then files by extension and name".to_owned(),
                shortcut: "",
                action: CommandAction::SortExplorerByType,
            },
            ContextMenuAction {
                label: "Sort by Modified Time",
                detail: "Folders first, then newest entries before older entries".to_owned(),
                shortcut: "",
                action: CommandAction::SortExplorerByModified,
            },
            ContextMenuAction {
                label: "Sort by Size",
                detail: "Folders first, then larger files before smaller files".to_owned(),
                shortcut: "",
                action: CommandAction::SortExplorerBySize,
            },
            ContextMenuAction {
                label: "Toggle Hidden Files",
                detail: "Show or hide dot-prefixed files and folders".to_owned(),
                shortcut: ".",
                action: CommandAction::ToggleHiddenFiles,
            },
            ContextMenuAction {
                label: "Toggle Generated Folders",
                detail: "Show or hide target, dist, build, node_modules, and similar folders"
                    .to_owned(),
                shortcut: "i",
                action: CommandAction::ToggleIgnoredFiles,
            },
        ];

        if node.is_dir {
            specs.insert(
                2,
                ContextMenuAction {
                    label: "Open Folder as Workspace",
                    detail: format!("Switch the workspace root to {relative}"),
                    shortcut: "O",
                    action: CommandAction::OpenSelectedFolderAsWorkspace,
                },
            );
        }

        if !node.is_dir {
            specs.insert(
                5,
                ContextMenuAction {
                    label: "Run File in Terminal",
                    detail: format!("Start a PTY terminal and run {relative}"),
                    shortcut: "F5",
                    action: CommandAction::RunSelectedExplorerFileInTerminal,
                },
            );
        }

        if !selected_paths.iter().any(|path| path == &self.root) {
            let mut selection_actions = vec![
                ContextMenuAction {
                    label: "Cut",
                    detail: format!("Move {relative} through the explorer clipboard"),
                    shortcut: "x",
                    action: CommandAction::CutSelectedExplorerItem,
                },
                ContextMenuAction {
                    label: "Duplicate",
                    detail: format!("Create a copy next to {relative}"),
                    shortcut: "y",
                    action: CommandAction::DuplicateSelectedExplorerItem,
                },
            ];
            if selected_paths.len() == 1 {
                selection_actions.push(ContextMenuAction {
                    label: "Rename",
                    detail: format!("Rename {relative}"),
                    shortcut: "e",
                    action: CommandAction::RenameSelected,
                });
            }
            if selected_paths.len() == 2 && selected_paths.iter().all(|path| path.is_file()) {
                selection_actions.push(ContextMenuAction {
                    label: "Compare Selected Files",
                    detail: "Open a read-only unified diff for the two selected files".to_owned(),
                    shortcut: "v",
                    action: CommandAction::CompareSelectedFiles,
                });
            }
            selection_actions.push(ContextMenuAction {
                label: "Delete",
                detail: format!("Delete {relative} after confirmation"),
                shortcut: "D",
                action: CommandAction::DeleteSelected,
            });
            let paste_index = specs
                .iter()
                .position(|spec| spec.label == "Paste")
                .unwrap_or(specs.len());
            specs.splice(paste_index..paste_index, selection_actions);
        }

        context_menu_items(node.path, specs, query)
    }

    fn editor_context_menu_items(&self, query: &str) -> Vec<QuickItem> {
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        let relative = if tab.untitled {
            tab.title.clone()
        } else {
            relative_path(&self.root, &tab.path)
        };
        let selection_detail = tab
            .selected_text()
            .map(|text| format!("{} selected char(s)", text.chars().count()))
            .unwrap_or_else(|| "current editor selection".to_owned());
        let symbol = self
            .active_identifier_under_cursor()
            .unwrap_or_else(|| "symbol under cursor".to_owned());
        let specs = vec![
            ContextMenuAction {
                label: "Save File",
                detail: format!("Write {relative} to disk"),
                shortcut: "Ctrl-S",
                action: CommandAction::SaveFile,
            },
            ContextMenuAction {
                label: "Save All",
                detail: "Write every dirty file-backed editor tab to disk".to_owned(),
                shortcut: "Ctrl-K S",
                action: CommandAction::SaveAll,
            },
            ContextMenuAction {
                label: "Open to Side",
                detail: format!("Show {relative} in a side-by-side editor pane"),
                shortcut: "Ctrl-\\",
                action: CommandAction::OpenActiveTabToSide,
            },
            ContextMenuAction {
                label: "Close Editor Split",
                detail: "Return to a single active editor pane".to_owned(),
                shortcut: "",
                action: CommandAction::CloseEditorSplit,
            },
            ContextMenuAction {
                label: "Copy",
                detail: format!("Copy {selection_detail}"),
                shortcut: "Ctrl-C",
                action: CommandAction::CopySelection,
            },
            ContextMenuAction {
                label: "Cut",
                detail: format!("Cut {selection_detail}"),
                shortcut: "Ctrl-X",
                action: CommandAction::CutSelection,
            },
            ContextMenuAction {
                label: "Paste",
                detail: "Paste the internal editor clipboard at the cursor".to_owned(),
                shortcut: "Ctrl-V",
                action: CommandAction::PasteClipboard,
            },
            ContextMenuAction {
                label: "Select All",
                detail: "Select the entire active editor buffer".to_owned(),
                shortcut: "Ctrl-A",
                action: CommandAction::SelectAll,
            },
            ContextMenuAction {
                label: "Find in File",
                detail: format!("Search inside {relative}"),
                shortcut: "Ctrl-F",
                action: CommandAction::FindInFile,
            },
            ContextMenuAction {
                label: "Replace in File",
                detail: format!("Replace text inside {relative}"),
                shortcut: "Ctrl-H",
                action: CommandAction::ReplaceInFile,
            },
            ContextMenuAction {
                label: "Go to Line",
                detail: "Jump to a line or line:column in the active file".to_owned(),
                shortcut: "Ctrl-L",
                action: CommandAction::GotoLine,
            },
            ContextMenuAction {
                label: "Toggle Bookmark",
                detail: "Add or remove a bookmark on the current editor line".to_owned(),
                shortcut: "Alt-B",
                action: CommandAction::ToggleBookmark,
            },
            ContextMenuAction {
                label: "Show Bookmarks",
                detail: "List editor bookmarks across open tabs".to_owned(),
                shortcut: "",
                action: CommandAction::ShowBookmarks,
            },
            ContextMenuAction {
                label: "Next Bookmark",
                detail: "Jump to the next editor bookmark".to_owned(),
                shortcut: "Alt-N",
                action: CommandAction::NextBookmark,
            },
            ContextMenuAction {
                label: "Previous Bookmark",
                detail: "Jump to the previous editor bookmark".to_owned(),
                shortcut: "Alt-P",
                action: CommandAction::PreviousBookmark,
            },
            ContextMenuAction {
                label: "Clear Bookmarks",
                detail: "Remove every bookmark from this editor tab".to_owned(),
                shortcut: "",
                action: CommandAction::ClearBookmarks,
            },
            ContextMenuAction {
                label: "Show Hover",
                detail: format!("Show language-server hover for {symbol}"),
                shortcut: "",
                action: CommandAction::ShowHover,
            },
            ContextMenuAction {
                label: "Signature Help",
                detail: format!("Show call signature help for {symbol}"),
                shortcut: "Ctrl-Shift-Space",
                action: CommandAction::SignatureHelp,
            },
            ContextMenuAction {
                label: "Go to Definition",
                detail: format!("Jump to LSP/workspace definition for {symbol}"),
                shortcut: "Ctrl-]",
                action: CommandAction::GoToDefinition,
            },
            ContextMenuAction {
                label: "Go to Type Definition",
                detail: format!("Jump to LSP type definition for {symbol}"),
                shortcut: "",
                action: CommandAction::GoToTypeDefinition,
            },
            ContextMenuAction {
                label: "Go to Implementation",
                detail: format!("Jump to LSP implementations for {symbol}"),
                shortcut: "",
                action: CommandAction::GoToImplementation,
            },
            ContextMenuAction {
                label: "Go to Matching Bracket",
                detail: "Jump between matching (), [], and {} brackets in this buffer".to_owned(),
                shortcut: "Ctrl-Shift-\\",
                action: CommandAction::GoToMatchingBracket,
            },
            ContextMenuAction {
                label: "Show Incoming Calls",
                detail: format!("List LSP callers for {symbol}"),
                shortcut: "",
                action: CommandAction::ShowIncomingCalls,
            },
            ContextMenuAction {
                label: "Show Outgoing Calls",
                detail: format!("List LSP callees used by {symbol}"),
                shortcut: "",
                action: CommandAction::ShowOutgoingCalls,
            },
            ContextMenuAction {
                label: "Highlight Symbol",
                detail: format!("Highlight LSP read/write ranges for {symbol}"),
                shortcut: "Ctrl-Shift-E",
                action: CommandAction::HighlightSymbol,
            },
            ContextMenuAction {
                label: "Clear Symbol Highlights",
                detail: "Clear active LSP document highlights from this editor tab".to_owned(),
                shortcut: "",
                action: CommandAction::ClearDocumentHighlights,
            },
            ContextMenuAction {
                label: "Find References",
                detail: format!("List references for {symbol}"),
                shortcut: "Ctrl-R",
                action: CommandAction::FindReferences,
            },
            ContextMenuAction {
                label: "Code Action",
                detail: format!("Request quick fixes and refactors for {symbol}"),
                shortcut: "",
                action: CommandAction::CodeAction,
            },
            ContextMenuAction {
                label: "Rename Symbol",
                detail: format!("Rename {symbol} across workspace files"),
                shortcut: "F2",
                action: CommandAction::RenameSymbol,
            },
            ContextMenuAction {
                label: "Trigger Suggest",
                detail: "Open LSP, workspace symbol, and keyword suggestions".to_owned(),
                shortcut: "Ctrl-Space",
                action: CommandAction::TriggerSuggest,
            },
            ContextMenuAction {
                label: "Run LSP Diagnostics",
                detail: "Collect language-server diagnostics for this buffer".to_owned(),
                shortcut: "",
                action: CommandAction::RunLspDiagnostics,
            },
            ContextMenuAction {
                label: "Format Document",
                detail: "Format the active buffer with LSP or an installed formatter".to_owned(),
                shortcut: "Shift-Alt-F",
                action: CommandAction::FormatDocument,
            },
            ContextMenuAction {
                label: "Toggle Line Comment",
                detail: "Comment or uncomment the current line or selected lines".to_owned(),
                shortcut: "Ctrl-/",
                action: CommandAction::ToggleLineComment,
            },
            ContextMenuAction {
                label: "Toggle Block Comment",
                detail: "Wrap or unwrap the current selection with block comment tokens".to_owned(),
                shortcut: "Shift-Alt-A",
                action: CommandAction::ToggleBlockComment,
            },
            ContextMenuAction {
                label: "Toggle Word Wrap",
                detail: "Wrap long visual lines to the current editor pane width".to_owned(),
                shortcut: "Alt-Z",
                action: CommandAction::ToggleWordWrap,
            },
            ContextMenuAction {
                label: "Toggle Fold",
                detail: "Fold or unfold the code block at the cursor".to_owned(),
                shortcut: "Alt-[",
                action: CommandAction::ToggleFold,
            },
            ContextMenuAction {
                label: "Fold All",
                detail: "Fold every detected block in this file".to_owned(),
                shortcut: "Alt-0",
                action: CommandAction::FoldAll,
            },
            ContextMenuAction {
                label: "Unfold All",
                detail: "Show every folded block in this file".to_owned(),
                shortcut: "Alt-]",
                action: CommandAction::UnfoldAll,
            },
            ContextMenuAction {
                label: "Run Selection in Terminal",
                detail: "Send the selection or current line to the active PTY shell".to_owned(),
                shortcut: "Ctrl-Enter",
                action: CommandAction::RunSelectionInTerminal,
            },
            ContextMenuAction {
                label: "Run File in Terminal",
                detail: format!("Start a PTY terminal and run {relative}"),
                shortcut: "F5",
                action: CommandAction::RunActiveFileInTerminal,
            },
            ContextMenuAction {
                label: "Copy File Path",
                detail: "Copy the active file absolute path to the terminal clipboard".to_owned(),
                shortcut: "",
                action: CommandAction::CopyActiveFilePath,
            },
            ContextMenuAction {
                label: "Copy Relative File Path",
                detail: "Copy the active file path relative to the workspace".to_owned(),
                shortcut: "",
                action: CommandAction::CopyActiveFileRelativePath,
            },
            ContextMenuAction {
                label: "Revert File",
                detail: "Reload the active file from disk and discard in-memory edits".to_owned(),
                shortcut: "",
                action: CommandAction::RevertFile,
            },
            ContextMenuAction {
                label: "Close Active Tab",
                detail: "Close the active tab, asking how to handle unsaved edits".to_owned(),
                shortcut: "Ctrl-W",
                action: CommandAction::CloseActiveTab,
            },
            ContextMenuAction {
                label: "Close Other Editors",
                detail: "Close clean editor tabs except the active tab".to_owned(),
                shortcut: "",
                action: CommandAction::CloseOtherTabs,
            },
            ContextMenuAction {
                label: "Close Editors to the Right",
                detail: "Close clean editor tabs to the right of the active tab".to_owned(),
                shortcut: "",
                action: CommandAction::CloseTabsToRight,
            },
            ContextMenuAction {
                label: "Close All Editors",
                detail: "Close every clean editor tab and keep dirty tabs open".to_owned(),
                shortcut: "",
                action: CommandAction::CloseAllTabs,
            },
            ContextMenuAction {
                label: "Reopen Closed Editor",
                detail: "Restore the most recently closed editor tab".to_owned(),
                shortcut: "Ctrl-Shift-T",
                action: CommandAction::ReopenClosedEditor,
            },
        ];

        context_menu_items(tab.path.clone(), specs, query)
    }

    fn terminal_context_menu_items(&self, query: &str) -> Vec<QuickItem> {
        let terminal = self.active_terminal();
        let cwd = terminal_cwd_detail(&terminal.cwd, &self.root);
        let selection_detail = if self.terminal_selection_for_active().is_some() {
            "Copy the active terminal selection".to_owned()
        } else {
            "Copy selected terminal text after dragging a selection".to_owned()
        };
        let specs = vec![
            ContextMenuAction {
                label: "Copy",
                detail: selection_detail,
                shortcut: "Ctrl-Shift-C",
                action: CommandAction::CopyTerminalSelection,
            },
            ContextMenuAction {
                label: "Copy All Output",
                detail: "Copy the active terminal viewport and retained scrollback".to_owned(),
                shortcut: "",
                action: CommandAction::CopyTerminalOutput,
            },
            ContextMenuAction {
                label: "Paste",
                detail: "Paste the internal clipboard into the PTY shell".to_owned(),
                shortcut: "Ctrl-Shift-V",
                action: CommandAction::PasteClipboardToTerminal,
            },
            ContextMenuAction {
                label: "Find in Terminal",
                detail: "Search the active terminal viewport and scrollback".to_owned(),
                shortcut: "Ctrl-F",
                action: CommandAction::FindInTerminal,
            },
            ContextMenuAction {
                label: "Run Command",
                detail: format!("Send a shell command to '{}'", terminal.title),
                shortcut: "",
                action: CommandAction::RunTerminalCommand,
            },
            ContextMenuAction {
                label: "Run Recent Command",
                detail: "Pick a tscode-submitted command and run it again in the active PTY"
                    .to_owned(),
                shortcut: "",
                action: CommandAction::RunRecentTerminalCommand,
            },
            ContextMenuAction {
                label: "Clear Terminal",
                detail: "Clear the active terminal viewport and scrollback".to_owned(),
                shortcut: "",
                action: CommandAction::ClearTerminal,
            },
            ContextMenuAction {
                label: "Restart Terminal",
                detail: format!("Restart the shell in {cwd}"),
                shortcut: "",
                action: CommandAction::RestartTerminal,
            },
            ContextMenuAction {
                label: "Rename Terminal",
                detail: format!("Rename the active terminal tab '{}'", terminal.title),
                shortcut: "",
                action: CommandAction::RenameTerminal,
            },
            ContextMenuAction {
                label: "New Terminal",
                detail: "Create a new workspace-root PTY terminal session".to_owned(),
                shortcut: "Ctrl-Shift-` / F7",
                action: CommandAction::NewTerminal,
            },
            ContextMenuAction {
                label: "Split Terminal",
                detail: "Create a side-by-side PTY pane from the active terminal cwd".to_owned(),
                shortcut: "Ctrl-Shift-5",
                action: CommandAction::SplitTerminal,
            },
            ContextMenuAction {
                label: "Close Terminal",
                detail: "Close the active integrated terminal session".to_owned(),
                shortcut: "F9",
                action: CommandAction::CloseTerminal,
            },
            ContextMenuAction {
                label: "Next Terminal",
                detail: "Switch to the next integrated terminal session".to_owned(),
                shortcut: "Ctrl-PageDown / F8",
                action: CommandAction::NextTerminal,
            },
            ContextMenuAction {
                label: "Previous Terminal",
                detail: "Switch to the previous integrated terminal session".to_owned(),
                shortcut: "Ctrl-PageUp",
                action: CommandAction::PreviousTerminal,
            },
            ContextMenuAction {
                label: "Toggle Terminal Maximize",
                detail: "Expand or restore the integrated terminal panel".to_owned(),
                shortcut: "F12",
                action: CommandAction::ToggleTerminalMaximized,
            },
            ContextMenuAction {
                label: "Scroll to Bottom",
                detail: "Return the active terminal viewport to the live shell output".to_owned(),
                shortcut: "",
                action: CommandAction::ScrollTerminalToBottom,
            },
            ContextMenuAction {
                label: "Increase Terminal Height",
                detail: "Give the terminal panel more rows".to_owned(),
                shortcut: "",
                action: CommandAction::IncreaseTerminalHeight,
            },
            ContextMenuAction {
                label: "Decrease Terminal Height",
                detail: "Give the editor more rows by shrinking the terminal".to_owned(),
                shortcut: "",
                action: CommandAction::DecreaseTerminalHeight,
            },
            ContextMenuAction {
                label: "Focus Editor",
                detail: "Move focus back to the editor".to_owned(),
                shortcut: "",
                action: CommandAction::FocusEditor,
            },
            ContextMenuAction {
                label: "Focus Explorer",
                detail: "Move focus to the file explorer".to_owned(),
                shortcut: "",
                action: CommandAction::FocusExplorer,
            },
        ];

        context_menu_items(terminal.cwd.clone(), specs, query)
    }

    fn completion_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let Some(active_tab) = self.active_tab() else {
            return Ok(Vec::new());
        };
        let active_path = active_tab.path.clone();
        let query = query.trim();
        let mut candidates = HashMap::<String, (CompletionRank, QuickItem)>::new();

        for item in &self.lsp_completion_items {
            let Some(rank) =
                completion_rank(&item.label, query, 0, 0, item.line.unwrap_or(usize::MAX))
            else {
                continue;
            };
            upsert_completion_item(&mut candidates, rank, item.clone());
        }

        for file in self.workspace_text_files()? {
            let source_rank = usize::from(file.path != active_path);
            for symbol in extract_code_symbols(&file.path, &file.text) {
                let Some(rank) = completion_rank(&symbol.name, query, source_rank, 0, symbol.line)
                else {
                    continue;
                };
                let detail = if file.path == active_path {
                    format!("{}  line {}", symbol.kind, symbol.line + 1)
                } else {
                    format!("{}:{}  {}", file.relative, symbol.line + 1, symbol.kind)
                };
                upsert_completion_item(
                    &mut candidates,
                    rank,
                    QuickItem {
                        label: symbol.name,
                        detail,
                        path: file.path.clone(),
                        line: Some(symbol.line),
                        col: Some(symbol.col),
                        preview: Some(symbol.preview),
                        command: None,
                    },
                );
            }

            for token in identifier_completion_tokens(&file.text) {
                let Some(rank) = completion_rank(&token.name, query, source_rank, 1, token.line)
                else {
                    continue;
                };
                let detail = if file.path == active_path {
                    format!("identifier  line {}", token.line + 1)
                } else {
                    format!("{}:{}  identifier", file.relative, token.line + 1)
                };
                upsert_completion_item(
                    &mut candidates,
                    rank,
                    QuickItem {
                        label: token.name,
                        detail,
                        path: file.path.clone(),
                        line: Some(token.line),
                        col: Some(token.col),
                        preview: Some(token.preview),
                        command: None,
                    },
                );
            }
        }

        for &keyword in completion_keywords_for_path(&active_path) {
            let Some(rank) = keyword_completion_rank(keyword, query, 0, 2, usize::MAX) else {
                continue;
            };
            upsert_completion_item(
                &mut candidates,
                rank,
                QuickItem {
                    label: keyword.to_owned(),
                    detail: "keyword".to_owned(),
                    path: active_path.clone(),
                    line: None,
                    col: None,
                    preview: None,
                    command: None,
                },
            );
        }

        let mut scored = candidates.into_values().collect::<Vec<_>>();
        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.label.cmp(&b.1.label)));
        Ok(scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, item)| item)
            .collect())
    }

    fn lsp_completion_items_for_state(&self, state: &CompletionState) -> Result<Vec<QuickItem>> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            return Ok(Vec::new());
        };
        if position.path != state.path || position.line != state.line {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();
        for completion in lsp::completions(&position)? {
            let label = completion.label.clone();
            let insert_text = completion
                .insert_text
                .as_deref()
                .filter(|text| lsp_completion_insert_text_is_plain(text))
                .map(str::to_owned)
                .unwrap_or_else(|| label.clone());
            if !is_completion_candidate_name(&insert_text) {
                continue;
            }
            let detail = completion
                .detail
                .as_deref()
                .filter(|detail| !detail.trim().is_empty())
                .map(|detail| format!("LSP {}  {}", completion.server, compact_preview(detail)))
                .unwrap_or_else(|| format!("LSP {} completion", completion.server));
            let preview = (insert_text != label).then_some(label);
            items.push(QuickItem {
                label: insert_text,
                detail,
                path: state.path.clone(),
                line: Some(state.line),
                col: Some(state.start_col),
                preview,
                command: None,
            });
            if items.len() >= MAX_QUICK_ITEMS {
                break;
            }
        }
        Ok(items)
    }

    fn code_action_items(&self, query: &str) -> Vec<QuickItem> {
        let active_path = self
            .active_tab()
            .map(|tab| tab.path.clone())
            .unwrap_or_else(|| self.root.clone());
        let items = self
            .lsp_code_actions
            .iter()
            .enumerate()
            .map(|(index, action)| {
                let path = action
                    .edit
                    .as_ref()
                    .and_then(|edit| edit.edits.first())
                    .map(|edit| edit.path.clone())
                    .unwrap_or_else(|| active_path.clone());
                QuickItem {
                    label: action.title.clone(),
                    detail: code_action_detail(action),
                    path,
                    line: Some(index),
                    col: None,
                    preview: code_action_preview(action),
                    command: None,
                }
            })
            .take(MAX_QUICK_ITEMS)
            .collect::<Vec<_>>();
        filter_existing_quick_items(items, query)
    }

    fn workspace_search_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let needle = query.trim();
        if needle.is_empty() {
            return Ok(Vec::new());
        }

        let needle_lower = needle.to_lowercase();
        let dirty_paths = self
            .tabs
            .iter()
            .filter(|tab| tab.dirty)
            .map(|tab| tab.path.clone())
            .collect::<HashSet<_>>();
        let mut items = Vec::new();
        for file in self.workspace_text_files()? {
            if items.len() >= MAX_QUICK_ITEMS {
                break;
            }

            let dirty = dirty_paths.contains(&file.path);
            let detail = if dirty {
                format!("{} (unsaved)", file.relative)
            } else {
                file.relative.clone()
            };
            for (line_index, line) in file.text.lines().enumerate() {
                let line_lower = line.to_lowercase();
                let Some(byte_col) = line_lower.find(&needle_lower) else {
                    continue;
                };
                let col = line[..byte_col].chars().count();
                items.push(QuickItem {
                    label: format!("{}:{}", file.relative, line_index + 1),
                    detail: detail.clone(),
                    path: file.path.clone(),
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

    fn document_symbol_items(&self, query: &str) -> Vec<QuickItem> {
        if let Some(items) = self.cached_lsp_document_symbol_items_for_active_tab() {
            return filter_existing_quick_items(items, query)
                .into_iter()
                .take(MAX_QUICK_ITEMS)
                .collect();
        }
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        let relative = if tab.untitled {
            tab.title.clone()
        } else {
            relative_path(&self.root, &tab.path)
        };
        let items = symbols_to_quick_items(&tab.path, &tab.text(), &relative, query, false);
        items.into_iter().take(MAX_QUICK_ITEMS).collect()
    }

    fn lsp_document_symbol_items_for_active_tab(&self) -> Result<Vec<QuickItem>> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            return Ok(Vec::new());
        };
        let mut items = lsp::document_symbols(&position)?
            .into_iter()
            .map(lsp_document_symbol_to_quick_item)
            .collect::<Vec<_>>();
        items.truncate(MAX_QUICK_ITEMS);
        Ok(items)
    }

    fn workspace_symbol_items(&mut self, query: &str) -> Result<Vec<QuickItem>> {
        let lsp_items = self.lsp_workspace_symbol_items_for_query(query)?;
        if !lsp_items.is_empty() {
            return Ok(lsp_items);
        }

        let mut scored = Vec::new();

        for file in self.workspace_text_files()? {
            scored.extend(symbols_to_quick_items(
                &file.path,
                &file.text,
                &file.relative,
                query,
                true,
            ));
            if query.trim().is_empty() && scored.len() >= MAX_QUICK_ITEMS {
                break;
            }
        }

        if !query.trim().is_empty() {
            scored.sort_by(|a, b| {
                fuzzy_score(&format!("{} {}", a.label, a.detail), query)
                    .unwrap_or(usize::MAX)
                    .cmp(
                        &fuzzy_score(&format!("{} {}", b.label, b.detail), query)
                            .unwrap_or(usize::MAX),
                    )
                    .then(a.detail.cmp(&b.detail))
            });
        }

        Ok(scored.into_iter().take(MAX_QUICK_ITEMS).collect())
    }

    fn lsp_workspace_symbol_items_for_query(&mut self, query: &str) -> Result<Vec<QuickItem>> {
        let query = query.trim().to_owned();
        if self.lsp_workspace_symbol_query.as_deref() == Some(query.as_str()) {
            return Ok(self.lsp_workspace_symbol_items.clone());
        }

        self.lsp_workspace_symbol_query = Some(query.clone());
        self.lsp_workspace_symbol_items.clear();
        let Some(position) = self.active_lsp_position_at_cursor() else {
            return Ok(Vec::new());
        };

        let mut items = lsp::workspace_symbols(&position, &query)?
            .into_iter()
            .map(lsp_document_symbol_to_quick_item)
            .collect::<Vec<_>>();
        items.truncate(MAX_QUICK_ITEMS);
        self.lsp_workspace_symbol_items = items.clone();
        Ok(items)
    }

    fn definition_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let symbol = query.trim();
        if symbol.is_empty() {
            return Ok(Vec::new());
        }
        let symbol_lower = symbol.to_lowercase();
        let mut exact = Vec::new();
        let mut insensitive = Vec::new();

        for file in self.workspace_text_files()? {
            for code_symbol in extract_code_symbols(&file.path, &file.text) {
                if code_symbol.name == symbol {
                    exact.push(symbol_to_quick_item(
                        &file.path,
                        &file.relative,
                        code_symbol,
                        true,
                    ));
                } else if code_symbol.name.to_lowercase() == symbol_lower {
                    insensitive.push(symbol_to_quick_item(
                        &file.path,
                        &file.relative,
                        code_symbol,
                        true,
                    ));
                }
            }
        }

        if !exact.is_empty() {
            return Ok(exact.into_iter().take(MAX_QUICK_ITEMS).collect());
        }
        Ok(insensitive.into_iter().take(MAX_QUICK_ITEMS).collect())
    }

    fn reference_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let symbol = query.trim();
        if symbol.is_empty() {
            return Ok(Vec::new());
        }

        let mut items = Vec::new();
        for file in self.workspace_text_files()? {
            for (line_index, line) in file.text.lines().enumerate() {
                for col in identifier_occurrences(line, symbol) {
                    items.push(QuickItem {
                        label: format!("{}:{}", file.relative, line_index + 1),
                        detail: format!("col {}", col + 1),
                        path: file.path.clone(),
                        line: Some(line_index),
                        col: Some(col),
                        preview: Some(line.trim().to_owned()),
                        command: None,
                    });
                    if items.len() >= MAX_QUICK_ITEMS {
                        return Ok(items);
                    }
                }
            }
        }
        Ok(items)
    }

    fn problem_items(&self, query: &str) -> Vec<QuickItem> {
        let query = query.trim();
        let mut items = self.problems.clone();
        if !query.is_empty() {
            items.retain(|item| {
                let haystack = format!(
                    "{} {} {}",
                    item.label,
                    item.detail,
                    item.preview.as_deref().unwrap_or_default()
                );
                fuzzy_score(&haystack, query).is_some()
            });
            items.sort_by(|a, b| {
                let a_haystack = format!(
                    "{} {} {}",
                    a.label,
                    a.detail,
                    a.preview.as_deref().unwrap_or_default()
                );
                let b_haystack = format!(
                    "{} {} {}",
                    b.label,
                    b.detail,
                    b.preview.as_deref().unwrap_or_default()
                );
                fuzzy_score(&a_haystack, query)
                    .unwrap_or(usize::MAX)
                    .cmp(&fuzzy_score(&b_haystack, query).unwrap_or(usize::MAX))
                    .then(a.label.cmp(&b.label))
            });
        }
        items.into_iter().take(MAX_QUICK_ITEMS).collect()
    }

    pub fn problem_summaries_for_path(&self, path: &Path) -> HashMap<usize, LineProblemSummary> {
        let mut summaries = HashMap::new();
        for problem in &self.problems {
            if problem.path != path {
                continue;
            }
            let Some(line) = problem.line else {
                continue;
            };
            let severity = ProblemSeverity::from_label(&problem.label);
            let col = problem.col.unwrap_or(0);
            let message = problem.detail.clone();
            summaries
                .entry(line)
                .and_modify(|summary: &mut LineProblemSummary| {
                    summary.count += 1;
                    if severity.rank() < summary.severity.rank()
                        || (severity == summary.severity && col < summary.col)
                    {
                        summary.severity = severity;
                        summary.col = col;
                        summary.message = message.clone();
                    }
                })
                .or_insert(LineProblemSummary {
                    severity,
                    count: 1,
                    col,
                    message,
                });
        }
        summaries
    }

    pub fn active_file_problem_count(&self) -> usize {
        let Some(tab) = self.active_tab() else {
            return 0;
        };
        self.problems
            .iter()
            .filter(|problem| problem.path == tab.path)
            .count()
    }

    pub fn active_line_problem_summary(&self) -> Option<LineProblemSummary> {
        let tab = self.active_tab()?;
        self.problem_summaries_for_path(&tab.path)
            .remove(&tab.cursor_line)
    }

    fn show_bookmarks(&mut self) -> Result<()> {
        self.open_quick_panel(QuickPanelKind::Bookmarks)?;
        if self
            .quick_panel
            .as_ref()
            .is_some_and(|panel| panel.items.is_empty())
        {
            self.message = Some("no editor bookmarks yet".to_owned());
        }
        Ok(())
    }

    fn toggle_bookmark_at_cursor(&mut self) {
        let Some(index) = self.active_tab else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        let line = self.tabs[index].cursor_line;
        let title = self.tabs[index].title.clone();
        match self.tabs[index].toggle_bookmark_at_line(line) {
            Some(true) => {
                self.focus = FocusPanel::Editor;
                self.message = Some(format!("bookmarked {title}:{}", line + 1));
            }
            Some(false) => {
                self.focus = FocusPanel::Editor;
                self.message = Some(format!("removed bookmark {title}:{}", line + 1));
            }
            None => {
                self.message = Some("no line to bookmark".to_owned());
            }
        }
    }

    fn clear_active_bookmarks(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        let title = tab.title.clone();
        let count = tab.clear_bookmarks();
        self.focus = FocusPanel::Editor;
        self.message = Some(format!("cleared {count} bookmark(s) in {title}"));
    }

    fn bookmark_locations(&self) -> Vec<(usize, usize)> {
        let mut locations = self
            .tabs
            .iter()
            .enumerate()
            .flat_map(|(tab_index, tab)| {
                tab.bookmarks
                    .iter()
                    .copied()
                    .map(move |line| (tab_index, line))
            })
            .collect::<Vec<_>>();
        locations.sort();
        locations
    }

    fn jump_to_relative_bookmark(&mut self, forward: bool) {
        let locations = self.bookmark_locations();
        if locations.is_empty() {
            self.message = Some("no editor bookmarks yet".to_owned());
            return;
        }

        let current_tab = self.active_tab.unwrap_or(0);
        let current_line = self
            .tabs
            .get(current_tab)
            .map(|tab| tab.cursor_line)
            .unwrap_or(0);
        let current = (current_tab, current_line);
        let target = if forward {
            locations
                .iter()
                .copied()
                .find(|location| *location > current)
                .unwrap_or(locations[0])
        } else {
            locations
                .iter()
                .copied()
                .rev()
                .find(|location| *location < current)
                .unwrap_or_else(|| *locations.last().unwrap())
        };
        self.jump_to_bookmark_location(target.0, target.1);
    }

    fn jump_to_bookmark_item(&mut self, item: QuickItem) {
        let Some(line) = item.line else {
            self.message = Some("bookmark item has no line".to_owned());
            return;
        };
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == item.path) {
            self.jump_to_bookmark_location(index, line);
        } else if item.path.is_file() {
            self.open_quick_item(item, None);
        } else {
            self.message = Some("bookmark tab is no longer open".to_owned());
        }
    }

    fn jump_to_bookmark_location(&mut self, tab_index: usize, line: usize) {
        if tab_index >= self.tabs.len() {
            self.message = Some("bookmark tab is no longer open".to_owned());
            return;
        }
        let path = self.tabs[tab_index].path.clone();
        let line = line.min(self.tabs[tab_index].lines.len().saturating_sub(1));
        self.push_navigation_location_for_jump(&path, Some(line), Some(0));
        self.active_tab = Some(tab_index);
        self.tabs[tab_index].set_cursor(line, 0);
        self.ensure_editor_cursor_visible();
        self.focus = FocusPanel::Editor;
        let label = if self.tabs[tab_index].untitled {
            self.tabs[tab_index].title.clone()
        } else {
            relative_path(&self.root, &self.tabs[tab_index].path)
        };
        self.message = Some(format!("jumped to bookmark {label}:{}", line + 1));
    }

    fn bookmark_items(&self, query: &str) -> Vec<QuickItem> {
        let query = query.trim();
        let mut scored = Vec::new();
        for (tab_index, tab) in self.tabs.iter().enumerate() {
            let detail = if tab.untitled {
                tab.title.clone()
            } else {
                relative_path(&self.root, &tab.path)
            };
            for line in &tab.bookmarks {
                let preview = tab
                    .lines
                    .get(*line)
                    .map(|line| line.trim().to_owned())
                    .filter(|line| !line.is_empty());
                let label = format!("{}:{}", tab.title, line + 1);
                let haystack = format!(
                    "{label} {detail} {}",
                    preview.as_deref().unwrap_or_default()
                );
                let score = if query.is_empty() {
                    Some(tab_index.saturating_mul(10_000).saturating_add(*line))
                } else {
                    fuzzy_score(&haystack, query)
                };
                if let Some(score) = score {
                    scored.push((
                        score,
                        tab_index,
                        *line,
                        QuickItem {
                            label,
                            detail: detail.clone(),
                            path: tab.path.clone(),
                            line: Some(*line),
                            col: Some(0),
                            preview,
                            command: None,
                        },
                    ));
                }
            }
        }
        scored.sort_by(|a, b| {
            a.0.cmp(&b.0)
                .then(a.1.cmp(&b.1))
                .then(a.2.cmp(&b.2))
                .then(a.3.label.cmp(&b.3.label))
        });
        scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, _, _, item)| item)
            .collect()
    }

    fn source_control_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let Some(top_level) = git_top_level(&self.root) else {
            return Ok(Vec::new());
        };

        let mut entries = load_git_status_entries(&self.root);
        let mut items = Vec::new();
        entries.sort_by(|left, right| {
            relative_path(&top_level, &left.path).cmp(&relative_path(&top_level, &right.path))
        });
        let current_branch = git_current_branch(&top_level);

        items.push(QuickItem {
            label: format!(
                "B Checkout Branch{}",
                current_branch
                    .as_ref()
                    .map(|branch| format!(" ({branch})"))
                    .unwrap_or_default()
            ),
            detail: "list local branches and switch with dirty-buffer protection".to_owned(),
            path: top_level.clone(),
            line: None,
            col: None,
            preview: None,
            command: Some(CommandAction::ShowGitBranches),
        });
        items.push(QuickItem {
            label: "+ Create Branch".to_owned(),
            detail: "prompt for a new branch name and check it out".to_owned(),
            path: top_level.clone(),
            line: None,
            col: None,
            preview: None,
            command: Some(CommandAction::CreateGitBranch),
        });

        if entries.iter().any(GitStatusEntry::can_stage) {
            items.push(QuickItem {
                label: "+ Stage All Changes".to_owned(),
                detail: "git add -A".to_owned(),
                path: top_level.clone(),
                line: None,
                col: None,
                preview: None,
                command: Some(CommandAction::StageAllChanges),
            });
        }
        if entries.iter().any(GitStatusEntry::can_unstage) {
            items.push(QuickItem {
                label: "- Unstage All Changes".to_owned(),
                detail: "git restore --staged .".to_owned(),
                path: top_level.clone(),
                line: None,
                col: None,
                preview: None,
                command: Some(CommandAction::UnstageAllChanges),
            });
        }
        if git_has_staged_changes(&top_level) {
            items.push(QuickItem {
                label: "C Commit Staged Changes".to_owned(),
                detail: "prompt for a commit message, then run git commit".to_owned(),
                path: top_level.clone(),
                line: None,
                col: None,
                preview: None,
                command: Some(CommandAction::CommitStagedChanges),
            });
        }
        if !entries.is_empty() {
            items.push(QuickItem {
                label: "C Commit All Changes".to_owned(),
                detail: "prompt for a commit message, stage all changes, then commit".to_owned(),
                path: top_level.clone(),
                line: None,
                col: None,
                preview: None,
                command: Some(CommandAction::CommitAllChanges),
            });
        }
        if !entries.is_empty() {
            items.push(QuickItem {
                label: "! Discard All Changes".to_owned(),
                detail: "type discard to run git restore/clean for every change".to_owned(),
                path: top_level.clone(),
                line: None,
                col: None,
                preview: None,
                command: Some(CommandAction::DiscardAllChanges),
            });
        }

        for entry in entries {
            let path = entry.path.clone();
            let status = entry.kind;
            let relative = relative_path(&top_level, &path);
            let preview = (!path.is_file()).then(|| "not available in working tree".to_owned());
            items.push(QuickItem {
                label: format!("{} {relative}", status.short_label()),
                detail: format!("{} - open diff", status.description()),
                path: path.clone(),
                line: None,
                col: None,
                preview,
                command: Some(CommandAction::OpenSourceControlDiff),
            });
            if entry.can_stage() {
                items.push(QuickItem {
                    label: format!("+ Stage {relative}"),
                    detail: "git add".to_owned(),
                    path: path.clone(),
                    line: None,
                    col: None,
                    preview: None,
                    command: Some(CommandAction::StageSourceControlItem),
                });
            }
            if entry.can_unstage() {
                items.push(QuickItem {
                    label: format!("- Unstage {relative}"),
                    detail: "git restore --staged".to_owned(),
                    path: path.clone(),
                    line: None,
                    col: None,
                    preview: None,
                    command: Some(CommandAction::UnstageSourceControlItem),
                });
            }
            items.push(QuickItem {
                label: format!("! Discard {relative}"),
                detail: "type discard to restore this path from HEAD or remove it if untracked"
                    .to_owned(),
                path,
                line: None,
                col: None,
                preview: None,
                command: Some(CommandAction::DiscardSourceControlItem),
            });
        }

        for hunk in load_git_diff_hunks(&self.root, &top_level) {
            let relative = relative_path(&top_level, &hunk.path);
            let line = hunk.new_start.max(1);
            items.push(QuickItem {
                label: format!("~ {relative}:{line}"),
                detail: format!("+{} -{}", hunk.new_count, hunk.old_count),
                path: hunk.path,
                line: Some(line.saturating_sub(1)),
                col: Some(0),
                preview: (!hunk.preview.is_empty()).then_some(hunk.preview),
                command: None,
            });
        }

        let query = query.trim();
        if !query.is_empty() {
            items.retain(|item| {
                let haystack = format!(
                    "{} {} {}",
                    item.label,
                    item.detail,
                    item.preview.as_deref().unwrap_or_default()
                );
                fuzzy_score(&haystack, query).is_some()
            });
            items.sort_by(|a, b| {
                let a_haystack = format!(
                    "{} {} {}",
                    a.label,
                    a.detail,
                    a.preview.as_deref().unwrap_or_default()
                );
                let b_haystack = format!(
                    "{} {} {}",
                    b.label,
                    b.detail,
                    b.preview.as_deref().unwrap_or_default()
                );
                fuzzy_score(&a_haystack, query)
                    .unwrap_or(usize::MAX)
                    .cmp(&fuzzy_score(&b_haystack, query).unwrap_or(usize::MAX))
                    .then(a.label.cmp(&b.label))
            });
        }

        Ok(items.into_iter().take(MAX_QUICK_ITEMS).collect())
    }

    fn branch_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let Some(top_level) = git_top_level(&self.root) else {
            return Ok(Vec::new());
        };

        let current = git_current_branch(&top_level);
        let mut items = vec![QuickItem {
            label: "+ Create Branch".to_owned(),
            detail: "prompt for a new branch name and check it out".to_owned(),
            path: top_level.clone(),
            line: None,
            col: None,
            preview: None,
            command: Some(CommandAction::CreateGitBranch),
        }];
        for branch in git_local_branches(&top_level)? {
            let is_current = current.as_deref() == Some(branch.as_str());
            items.push(QuickItem {
                label: if is_current {
                    format!("* {branch}")
                } else {
                    format!("  {branch}")
                },
                detail: branch,
                path: top_level.clone(),
                line: None,
                col: None,
                preview: is_current.then(|| "current branch".to_owned()),
                command: Some(CommandAction::CheckoutGitBranch),
            });
        }

        Ok(filter_existing_quick_items(items, query))
    }

    fn task_items(&self, query: &str) -> Vec<QuickItem> {
        let mut items = collect_workspace_tasks(&self.root)
            .into_iter()
            .map(|task| QuickItem {
                label: task.label,
                detail: task.command,
                path: task.cwd,
                line: None,
                col: None,
                preview: Some(task.source),
                command: None,
            })
            .collect::<Vec<_>>();

        let query = query.trim();
        if !query.is_empty() {
            items.retain(|item| {
                let haystack = format!(
                    "{} {} {}",
                    item.label,
                    item.detail,
                    item.preview.as_deref().unwrap_or_default()
                );
                fuzzy_score(&haystack, query).is_some()
            });
            items.sort_by(|a, b| {
                let a_haystack = format!(
                    "{} {} {}",
                    a.label,
                    a.detail,
                    a.preview.as_deref().unwrap_or_default()
                );
                let b_haystack = format!(
                    "{} {} {}",
                    b.label,
                    b.detail,
                    b.preview.as_deref().unwrap_or_default()
                );
                fuzzy_score(&a_haystack, query)
                    .unwrap_or(usize::MAX)
                    .cmp(&fuzzy_score(&b_haystack, query).unwrap_or(usize::MAX))
                    .then(a.label.cmp(&b.label))
            });
        }

        items.into_iter().take(MAX_QUICK_ITEMS).collect()
    }

    fn terminal_command_history_items(&self, query: &str) -> Vec<QuickItem> {
        let query = query.trim();
        let mut scored = self
            .terminal_command_history
            .iter()
            .enumerate()
            .filter_map(|(index, command)| {
                let score = if query.is_empty() {
                    Some(index)
                } else {
                    fuzzy_score(command, query)
                }?;
                let line_count = command.lines().count().max(1);
                Some((
                    score,
                    index,
                    QuickItem {
                        label: truncate_chars(command, 80),
                        detail: command.clone(),
                        path: self.active_terminal().cwd.clone(),
                        line: None,
                        col: None,
                        preview: Some(format!("{line_count} line(s)")),
                        command: None,
                    },
                ))
            })
            .collect::<Vec<_>>();

        scored.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        scored
            .into_iter()
            .take(MAX_QUICK_ITEMS)
            .map(|(_, _, item)| item)
            .collect()
    }

    fn workspace_text_files(&self) -> Result<Vec<WorkspaceTextFile>> {
        let open_texts = self
            .tabs
            .iter()
            .filter(|tab| !tab.read_only)
            .map(|tab| (tab.path.clone(), tab.text()))
            .collect::<HashMap<_, _>>();
        let mut files = Vec::new();

        for path in collect_workspace_files(&self.root, self.show_hidden, self.show_ignored)? {
            let relative = relative_path(&self.root, &path);
            let text = if let Some(text) = open_texts.get(&path) {
                text.clone()
            } else {
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
                String::from_utf8_lossy(&bytes).into_owned()
            };
            files.push(WorkspaceTextFile {
                path,
                relative,
                text,
            });
        }

        Ok(files)
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
        let paths = self.selected_explorer_paths();
        if paths.len() > 1 {
            self.message = Some("rename works on one explorer item at a time".to_owned());
            return;
        };
        let Some(path) = paths.first().cloned() else {
            return;
        };
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("")
            .to_owned();
        self.start_prompt(PromptKind::Rename(path), &name);
    }

    fn prompt_delete(&mut self) {
        let paths = normalize_file_op_paths(self.selected_explorer_paths());
        if paths.is_empty() {
            return;
        };
        let label = self.selected_explorer_label(&paths);
        self.start_prompt(PromptKind::DeletePaths(paths), "");
        self.message = Some(format!("type yes to delete {label}"));
    }

    fn copy_selected_path(&mut self) {
        let paths = normalize_file_op_paths(self.selected_explorer_paths());
        if paths.is_empty() {
            return;
        };
        let label = self.selected_explorer_label(&paths);
        self.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Copy,
            paths,
        });
        self.message = Some(format!("copied {label}"));
    }

    fn cut_selected_path(&mut self) {
        let paths = normalize_file_op_paths(self.selected_explorer_paths());
        if paths.is_empty() {
            return;
        };
        if paths.iter().any(|path| path == &self.root) {
            self.message = Some("refusing to cut workspace root".to_owned());
            return;
        }
        let label = self.selected_explorer_label(&paths);
        self.explorer_clipboard = Some(ExplorerClipboard {
            action: ClipboardAction::Cut,
            paths,
        });
        self.message = Some(format!("cut {label}"));
    }

    fn paste_into_selected(&mut self) -> Result<()> {
        let Some(clipboard) = self.explorer_clipboard.clone() else {
            self.message = Some("clipboard empty".to_owned());
            return Ok(());
        };
        let sources = normalize_file_op_paths(clipboard.paths);
        if sources.is_empty() {
            self.message = Some("clipboard empty".to_owned());
            self.explorer_clipboard = None;
            return Ok(());
        }
        if sources.iter().any(|source| !source.exists()) {
            self.message = Some("one or more clipboard sources no longer exist".to_owned());
            self.explorer_clipboard = None;
            return Ok(());
        }

        let target_dir = self.selected_base_dir();
        if sources
            .iter()
            .any(|source| source.is_dir() && target_dir.starts_with(source))
        {
            self.message = Some("cannot paste a folder into itself".to_owned());
            return Ok(());
        }
        if clipboard.action == ClipboardAction::Cut && sources.iter().any(|path| path == &self.root)
        {
            self.message = Some("refusing to move workspace root".to_owned());
            self.explorer_clipboard = None;
            return Ok(());
        }

        let mut destinations = Vec::new();
        for source in &sources {
            let Some(name) = source.file_name() else {
                continue;
            };
            let destination = unique_copy_path(&target_dir.join(name));
            match clipboard.action {
                ClipboardAction::Copy => {
                    copy_path_recursive(source, &destination)?;
                    destinations.push(destination);
                }
                ClipboardAction::Cut => {
                    fs::rename(source, &destination)?;
                    let destination = destination
                        .canonicalize()
                        .unwrap_or_else(|_| destination.clone());
                    self.update_open_tabs_for_move(source, &destination);
                    self.update_navigation_for_move(source, &destination);
                    destinations.push(destination);
                }
            }
        }

        if clipboard.action == ClipboardAction::Cut {
            self.explorer_clipboard = None;
            self.clear_explorer_multi_selection();
        }
        self.refresh_explorer()?;
        if let Some(destination) = destinations.last() {
            self.reveal_path(destination)?;
        }
        let action = match clipboard.action {
            ClipboardAction::Copy => "copied",
            ClipboardAction::Cut => "moved",
        };
        self.message = if destinations.len() == 1 {
            destinations
                .first()
                .map(|destination| format!("{action} to {}", destination.display()))
        } else {
            Some(format!(
                "{action} {} item(s) into {}",
                destinations.len(),
                target_dir.display()
            ))
        };
        Ok(())
    }

    fn duplicate_selected(&mut self) -> Result<()> {
        let paths = normalize_file_op_paths(self.selected_explorer_paths());
        if paths.is_empty() {
            return Ok(());
        };
        if paths.iter().any(|path| path == &self.root) {
            self.message = Some("refusing to duplicate workspace root".to_owned());
            return Ok(());
        }
        let mut destinations = Vec::new();
        for path in &paths {
            let Some(parent) = path.parent() else {
                continue;
            };
            let destination = unique_copy_path(
                &parent.join(
                    path.file_name()
                        .and_then(|name| name.to_str())
                        .map(copy_name)
                        .unwrap_or_else(|| "copy".to_owned()),
                ),
            );
            copy_path_recursive(path, &destination)?;
            destinations.push(destination);
        }
        self.refresh_explorer()?;
        if let Some(destination) = destinations.last() {
            self.reveal_path(destination)?;
        }
        self.message = if destinations.len() == 1 {
            destinations
                .first()
                .map(|destination| format!("duplicated to {}", destination.display()))
        } else {
            Some(format!("duplicated {} item(s)", destinations.len()))
        };
        Ok(())
    }

    fn compare_selected_files(&mut self) -> Result<()> {
        let paths = normalize_file_op_paths(self.selected_explorer_paths());
        if paths.len() != 2 {
            self.message = Some("compare expects exactly two selected explorer files".to_owned());
            return Ok(());
        }
        let left = canonical_existing_path(&paths[0]);
        let right = canonical_existing_path(&paths[1]);
        if left == right {
            self.message = Some("compare expects two different files".to_owned());
            return Ok(());
        }
        if !left.is_file() || !right.is_file() {
            self.message = Some("compare selected files only supports regular files".to_owned());
            return Ok(());
        }

        let left_text = match read_compare_text_file(&left) {
            Ok(text) => text,
            Err(error) => {
                self.message = Some(error.to_string());
                return Ok(());
            }
        };
        let right_text = match read_compare_text_file(&right) {
            Ok(text) => text,
            Err(error) => {
                self.message = Some(error.to_string());
                return Ok(());
            }
        };
        let text = build_compare_diff(&self.root, &left, &right, &left_text, &right_text);
        let tab_path = compare_diff_tab_path(&self.root, &left, &right);
        let left_name = left
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("left");
        let right_name = right
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("right");
        let title = format!("Compare {left_name} <-> {right_name}");
        let left_relative = relative_path(&self.root, &left);
        let right_relative = relative_path(&self.root, &right);
        self.open_read_only_text_tab(tab_path, title, text);
        self.message = Some(format!("compared {left_relative} and {right_relative}"));
        Ok(())
    }

    fn reveal_active_file(&mut self) -> Result<()> {
        let Some(tab) = self.active_tab() else {
            self.message = Some("no active file to reveal".to_owned());
            return Ok(());
        };
        if tab.untitled {
            self.message = Some(format!("{} has not been saved to disk", tab.title));
            return Ok(());
        }
        let path = tab.path.clone();
        self.reveal_path(&path)?;
        self.focus = FocusPanel::Explorer;
        self.message = Some(format!("revealed {}", path.display()));
        Ok(())
    }

    fn copy_active_file_path_to_clipboard(&mut self, relative: bool) {
        let Some(tab) = self.active_tab() else {
            self.message = Some("no active file path to copy".to_owned());
            return;
        };
        if tab.untitled {
            self.message = Some(format!("{} has no file path yet", tab.title));
            return;
        }
        self.copy_path_to_clipboard(&tab.path.clone(), relative, "active file");
    }

    fn copy_selected_explorer_path_to_clipboard(&mut self, relative: bool) {
        let paths = self.selected_explorer_paths();
        if paths.is_empty() {
            self.message = Some("no explorer path to copy".to_owned());
            return;
        }
        if paths.len() == 1 {
            self.copy_path_to_clipboard(&paths[0], relative, "explorer item");
            return;
        }

        let text = paths
            .iter()
            .map(|path| {
                if relative {
                    relative_path(&self.root, path)
                } else {
                    path.to_string_lossy().into_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let display_kind = if relative { "relative paths" } else { "paths" };
        if self.queue_clipboard_export(&text) {
            self.message = Some(format!(
                "copied {} explorer item {display_kind}",
                paths.len()
            ));
        } else {
            self.message = Some(format!(
                "{} explorer item {display_kind} too large for terminal clipboard",
                paths.len()
            ));
        }
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
            PromptKind::DeletePaths(paths) => {
                if prompt.input == "yes" {
                    self.delete_paths(paths)?;
                } else {
                    self.message = Some("delete cancelled".to_owned());
                }
            }
            PromptKind::ExplorerFilter => self.set_explorer_filter(prompt.input),
            PromptKind::OpenFolder => self.open_folder_from_prompt(prompt.input)?,
            PromptKind::Search => self.search_active(prompt.input),
            PromptKind::ReplaceFind { all } => self.replace_find_from_prompt(prompt.input, all),
            PromptKind::ReplaceWith { needle, all } => {
                if all {
                    self.replace_all_active_matches(needle, prompt.input);
                } else {
                    self.replace_next_active_match(needle, prompt.input);
                }
            }
            PromptKind::WorkspaceReplaceFind => {
                self.workspace_replace_find_from_prompt(prompt.input)
            }
            PromptKind::WorkspaceReplaceWith { needle } => {
                self.replace_workspace_matches(needle, prompt.input)?;
            }
            PromptKind::RenameSymbol { old } => {
                self.rename_symbol_from_prompt(old, prompt.input)?;
            }
            PromptKind::SaveAs => self.save_as_from_prompt(prompt.input)?,
            PromptKind::SaveAsClose { index } => {
                if let Some(saved_index) = self.save_tab_as_from_prompt(index, prompt.input)? {
                    self.close_tab_without_prompt(saved_index, "saved and closed");
                }
            }
            PromptKind::CreateGitBranch => {
                self.create_git_branch_from_prompt(prompt.input)?;
            }
            PromptKind::CommitStagedSourceControlChanges => {
                self.commit_staged_source_control_changes_confirmed(prompt.input)?;
            }
            PromptKind::CommitAllSourceControlChanges(paths) => {
                self.commit_all_source_control_changes_confirmed(paths, prompt.input)?;
            }
            PromptKind::DiscardSourceControlPath(path) => {
                if prompt.input == "discard" {
                    self.discard_source_control_path_confirmed(path)?;
                } else {
                    self.message = Some("discard cancelled".to_owned());
                }
            }
            PromptKind::DiscardAllSourceControlChanges(paths) => {
                if prompt.input == "discard" {
                    self.discard_all_source_control_changes_confirmed(paths)?;
                } else {
                    self.message = Some("discard cancelled".to_owned());
                }
            }
            PromptKind::TerminalSearch => self.terminal_search_from_prompt(prompt.input),
            PromptKind::RunTerminalCommand => {
                self.run_terminal_command_from_prompt(prompt.input)?
            }
            PromptKind::RenameTerminal => self.rename_terminal_from_prompt(prompt.input),
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
        let filter = input.trim().to_owned();
        let selected_path = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.explorer_filter = (!filter.is_empty()).then_some(filter.clone());
        self.expand_active_explorer_filter_matches();
        self.restore_explorer_selection(selected_path);
        self.prune_explorer_multi_selection();
        self.message = match &self.explorer_filter {
            Some(filter) => Some(format!("explorer filter: {filter}")),
            None => Some("explorer filter cleared".to_owned()),
        };
    }

    fn clear_explorer_filter(&mut self) {
        self.explorer_filter = None;
        self.restore_explorer_selection(None);
        self.prune_explorer_multi_selection();
        self.message = Some("explorer filter cleared".to_owned());
    }

    fn toggle_hidden_files(&mut self) {
        let selected_path = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.show_hidden = !self.show_hidden;
        self.refresh_workspace_visibility_cache();
        self.expand_active_explorer_filter_matches();
        self.restore_explorer_selection(selected_path);
        self.prune_explorer_multi_selection();
        self.update_workspace_snapshot();
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
        self.refresh_workspace_visibility_cache();
        self.expand_active_explorer_filter_matches();
        self.restore_explorer_selection(selected_path);
        self.prune_explorer_multi_selection();
        self.update_workspace_snapshot();
        self.message = Some(format!(
            "{} generated/ignored folders",
            if self.show_ignored {
                "showing"
            } else {
                "hiding"
            }
        ));
    }

    fn cycle_explorer_sort_mode(&mut self) {
        self.set_explorer_sort_mode(self.explorer.sort_mode().next());
    }

    fn set_explorer_sort_mode(&mut self, sort_mode: ExplorerSortMode) {
        let selected_path = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.explorer.set_sort_mode(sort_mode);
        self.restore_explorer_selection(selected_path);
        self.prune_explorer_multi_selection();
        self.message = Some(format!("explorer sorted by {}", sort_mode.label()));
    }

    fn expand_active_explorer_filter_matches(&mut self) {
        let Some(filter) = self
            .explorer_filter
            .as_deref()
            .map(str::trim)
            .filter(|filter| !filter.is_empty())
        else {
            return;
        };
        let filter = filter.to_lowercase();

        let paths = match collect_workspace_paths(&self.root, self.show_hidden, self.show_ignored) {
            Ok(paths) => paths,
            Err(error) => {
                self.last_error = Some(error.to_string());
                return;
            }
        };

        for path in paths
            .into_iter()
            .filter(|path| explorer_path_filter_matches(path, &self.root, &filter))
            .take(MAX_QUICK_ITEMS)
        {
            let _ = self.explorer.reveal(&path);
        }
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
        self.message = None;
        let Some(path) = self.resolve_explorer_create_path(&name) else {
            if self.message.is_none() {
                self.message = Some("new file cancelled".to_owned());
            }
            return Ok(());
        };
        if path == self.root {
            self.message = Some("new file requires a file name".to_owned());
            return Ok(());
        }
        if path.exists() {
            if path.is_dir() {
                self.message = Some(format!("new file target is a folder: {}", path.display()));
                return Ok(());
            }
            self.open_file(&path);
            self.message = Some(format!("opened existing file {}", path.display()));
            return Ok(());
        }
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&path, "")?;
        self.refresh_explorer()?;
        self.open_file(&path);
        self.message = Some(format!("created {}", path.display()));
        Ok(())
    }

    fn create_dir_from_prompt(&mut self, name: String) -> Result<()> {
        self.message = None;
        let Some(path) = self.resolve_explorer_create_path(&name) else {
            if self.message.is_none() {
                self.message = Some("new folder cancelled".to_owned());
            }
            return Ok(());
        };
        if path == self.root {
            self.message = Some("new folder requires a folder name".to_owned());
            return Ok(());
        }
        if path.is_file() {
            self.message = Some(format!(
                "new folder target is an existing file: {}",
                path.display()
            ));
            return Ok(());
        }
        fs::create_dir_all(&path)?;
        self.refresh_explorer()?;
        self.reveal_path(&path)?;
        self.message = Some(format!("created {}", path.display()));
        Ok(())
    }

    fn rename_from_prompt(&mut self, path: PathBuf, name: String) -> Result<()> {
        let name = name.trim();
        if name.is_empty() {
            self.message = Some("rename cancelled".to_owned());
            return Ok(());
        }
        if path == self.root {
            self.message = Some("refusing to rename workspace root".to_owned());
            return Ok(());
        }
        if !is_simple_file_name(name) {
            self.message = Some("rename expects a single file or folder name".to_owned());
            return Ok(());
        }
        let new_path = path
            .parent()
            .map(|parent| parent.join(name))
            .unwrap_or_else(|| self.root.join(name));
        if new_path == path {
            self.message = Some("rename unchanged".to_owned());
            return Ok(());
        }
        if new_path.exists() {
            self.message = Some(format!(
                "rename target already exists: {}",
                new_path.display()
            ));
            return Ok(());
        }
        fs::rename(&path, &new_path)?;
        let new_path = new_path.canonicalize().unwrap_or(new_path);
        self.update_open_tabs_for_move(&path, &new_path);
        self.update_navigation_for_move(&path, &new_path);
        self.refresh_explorer()?;
        self.reveal_path(&new_path)?;
        self.message = Some(format!("renamed to {}", new_path.display()));
        Ok(())
    }

    fn resolve_explorer_create_path(&mut self, input: &str) -> Option<PathBuf> {
        let input = input.trim();
        if input.is_empty() {
            return None;
        }
        let requested = PathBuf::from(input);
        if requested.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir | std::path::Component::Prefix(_)
            )
        }) {
            self.message = Some("explorer paths cannot use '..'".to_owned());
            return None;
        }

        let target = if requested.is_absolute() {
            requested
        } else if input.contains('/') || input.contains('\\') || path_mentions_directory(&requested)
        {
            self.root.join(requested)
        } else {
            self.selected_base_dir().join(requested)
        };

        if !target.starts_with(&self.root) {
            self.message = Some("explorer create target must stay inside the workspace".to_owned());
            return None;
        }

        Some(target)
    }

    fn delete_paths(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        let paths = normalize_file_op_paths(paths);
        if paths.is_empty() {
            self.message = Some("nothing to delete".to_owned());
            return Ok(());
        }
        if paths.iter().any(|path| path == &self.root) {
            self.message = Some("refusing to delete workspace root".to_owned());
            return Ok(());
        }

        let dirty_open_tabs = self
            .tabs
            .iter()
            .filter(|tab| {
                !tab.untitled && tab.dirty && paths.iter().any(|path| tab.path.starts_with(path))
            })
            .map(|tab| relative_path(&self.root, &tab.path))
            .collect::<Vec<_>>();
        if !dirty_open_tabs.is_empty() {
            let first = &dirty_open_tabs[0];
            let suffix = dirty_open_tabs
                .len()
                .checked_sub(1)
                .filter(|count| *count > 0)
                .map(|count| format!(" and {count} more"))
                .unwrap_or_default();
            self.message = Some(format!(
                "delete blocked by unsaved tab: {first}{suffix}; save, close, or discard it first"
            ));
            return Ok(());
        }

        let active_path = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.path.clone());
        let active_index = self.active_tab;
        let split_path = self
            .editor_split
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.path.clone());

        for path in &paths {
            if path.is_dir() {
                fs::remove_dir_all(path)?;
            } else {
                fs::remove_file(path)?;
            }
        }
        self.tabs
            .retain(|tab| tab.untitled || !paths.iter().any(|path| tab.path.starts_with(path)));
        for path in &paths {
            self.prune_navigation_for_deleted_path(path);
        }
        if self.explorer_clipboard.as_ref().is_some_and(|clipboard| {
            clipboard
                .paths
                .iter()
                .any(|clipboard_path| paths.iter().any(|path| clipboard_path.starts_with(path)))
        }) {
            self.explorer_clipboard = None;
        }
        self.explorer_multi_selection
            .retain(|selected| !paths.iter().any(|path| selected.starts_with(path)));
        self.active_tab = if self.tabs.is_empty() {
            None
        } else if let Some(active_path) = active_path {
            self.tabs
                .iter()
                .position(|tab| tab.path == active_path)
                .or_else(|| {
                    active_index.map(|index| index.saturating_sub(1).min(self.tabs.len() - 1))
                })
        } else {
            Some(0)
        };
        self.editor_split =
            split_path.and_then(|path| self.tabs.iter().position(|tab| tab.path == path));
        self.normalize_editor_split();
        self.refresh_explorer()?;
        self.message = if paths.len() == 1 {
            paths
                .first()
                .map(|path| format!("deleted {}", path.display()))
        } else {
            Some(format!("deleted {} item(s)", paths.len()))
        };
        Ok(())
    }

    fn update_open_tabs_for_move(&mut self, old_path: &Path, new_path: &Path) {
        for tab in &mut self.tabs {
            if tab.untitled {
                continue;
            }
            if let Ok(relative) = tab.path.strip_prefix(old_path) {
                tab.path = new_path.join(relative);
                tab.title = tab
                    .path
                    .file_name()
                    .and_then(|file_name| file_name.to_str())
                    .unwrap_or("[file]")
                    .to_owned();
                tab.refresh_disk_stamp();
            }
        }
    }

    fn update_navigation_for_move(&mut self, old_path: &Path, new_path: &Path) {
        for location in self
            .navigation_back
            .iter_mut()
            .chain(self.navigation_forward.iter_mut())
        {
            if let Ok(relative) = location.path.strip_prefix(old_path) {
                location.path = new_path.join(relative);
            }
        }
    }

    fn prune_navigation_for_deleted_path(&mut self, path: &Path) {
        self.navigation_back
            .retain(|location| !location.path.starts_with(path));
        self.navigation_forward
            .retain(|location| !location.path.starts_with(path));
    }

    fn reveal_path(&mut self, path: &Path) -> Result<()> {
        self.explorer.reveal(path)?;
        self.ensure_explorer_selection_visible();
        Ok(())
    }

    fn refresh_explorer(&mut self) -> Result<()> {
        self.refresh_explorer_preserving_selection(true)
    }

    fn refresh_explorer_preserving_selection(&mut self, show_message: bool) -> Result<()> {
        let selected = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.explorer.refresh()?;
        self.refresh_git_status();
        self.update_workspace_snapshot();
        self.expand_active_explorer_filter_matches();
        if let Some(path) = selected
            && let Some(index) = self
                .visible_nodes()
                .iter()
                .position(|node| node.path == path)
        {
            self.explorer.selected = index;
        }
        self.ensure_explorer_selection_visible();
        self.prune_explorer_multi_selection();
        if show_message {
            self.message = Some("explorer refreshed".to_owned());
        }
        Ok(())
    }

    fn update_workspace_snapshot(&mut self) {
        self.refresh_workspace_visibility_cache();
        match workspace_snapshot(&self.root, self.show_hidden, self.show_ignored) {
            Ok(snapshot) => self.workspace_snapshot = Some(snapshot),
            Err(error) => self.last_error = Some(error.to_string()),
        }
        self.last_workspace_tree_check = Instant::now();
    }

    fn refresh_workspace_visibility_cache(&mut self) {
        match workspace_visible_paths(&self.root, self.show_hidden, self.show_ignored) {
            Ok(paths) => self.workspace_visible_paths = paths,
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn refresh_git_status(&mut self) {
        let (statuses, dirty_dirs) = load_git_status(&self.root);
        self.git_branch =
            git_top_level(&self.root).and_then(|top_level| git_current_branch(&top_level));
        self.git_statuses = statuses;
        self.git_dirty_dirs = dirty_dirs;
    }

    fn collapse_explorer(&mut self) {
        self.explorer.collapse_all();
        self.explorer.selected = 0;
        self.explorer.scroll = 0;
        self.prune_explorer_multi_selection();
        self.message = Some("explorer collapsed".to_owned());
    }

    fn save_active_tab(&mut self) {
        let Some(index) = self.active_tab else {
            return;
        };
        if self.tabs[index].read_only {
            self.message = Some(format!("{} is read-only", self.tabs[index].title));
            return;
        }
        if self.tabs[index].untitled {
            self.start_save_as_prompt();
            self.message = Some(format!("{} needs Save As", self.tabs[index].title));
            return;
        }
        self.check_external_file_changes();
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        if !tab.external_state.is_clean() {
            self.message = Some(format!(
                "{} {}; use Revert File or Save As before overwriting",
                tab.title,
                tab.external_state.label()
            ));
            return;
        }

        let path = self.tabs[index].path.clone();
        match self.tabs[index].save() {
            Ok(()) => {
                self.refresh_git_status();
                self.message = Some(format!("saved {}", path.display()));
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
    }

    fn save_as_from_prompt(&mut self, input: String) -> Result<()> {
        let Some(index) = self.active_tab else {
            self.message = Some("no active file to save as".to_owned());
            return Ok(());
        };
        let _ = self.save_tab_as_from_prompt(index, input)?;
        Ok(())
    }

    fn save_tab_as_from_prompt(
        &mut self,
        mut index: usize,
        input: String,
    ) -> Result<Option<usize>> {
        if index >= self.tabs.len() {
            self.message = Some("tab to save is no longer open".to_owned());
            return Ok(None);
        }
        if self.tabs[index].read_only {
            self.message = Some(format!("{} is read-only", self.tabs[index].title));
            return Ok(None);
        }
        let Some(target) = resolve_prompt_path(&self.root, &input) else {
            self.message = Some("save as cancelled".to_owned());
            return Ok(None);
        };
        if target.is_dir() {
            self.message = Some(format!(
                "save as target is a directory: {}",
                target.display()
            ));
            return Ok(None);
        }

        let canonical_target = target.canonicalize().unwrap_or_else(|_| target.clone());
        if let Some(existing_index) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(other_index, tab)| {
                *other_index != index
                    && tab.dirty
                    && tab.path.canonicalize().unwrap_or_else(|_| tab.path.clone())
                        == canonical_target
            })
            .map(|(other_index, _)| other_index)
        {
            self.message = Some(format!(
                "save as target is already open with unsaved edits: {}",
                self.tabs[existing_index].path.display()
            ));
            return Ok(None);
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        let text = self.tabs[index].text();
        fs::write(&target, text)?;
        let saved_path = target.canonicalize().unwrap_or(target);

        if let Some(existing_index) = self
            .tabs
            .iter()
            .enumerate()
            .find(|(other_index, tab)| *other_index != index && tab.path == saved_path)
            .map(|(other_index, _)| other_index)
        {
            self.tabs.remove(existing_index);
            if existing_index < index {
                index -= 1;
            }
        }

        let title = saved_path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("[file]")
            .to_owned();
        self.tabs[index].path = saved_path.clone();
        self.tabs[index].title = title;
        self.tabs[index].dirty = false;
        self.tabs[index].refresh_disk_stamp();
        self.active_tab = Some(index);

        self.refresh_explorer()?;
        if saved_path.starts_with(&self.root) {
            self.reveal_path(&saved_path)?;
        }
        self.focus = FocusPanel::Editor;
        self.refresh_git_status();
        self.message = Some(format!("saved as {}", saved_path.display()));
        Ok(Some(index))
    }

    fn revert_active_tab(&mut self) -> Result<()> {
        let Some(index) = self.active_tab else {
            self.message = Some("no active file to revert".to_owned());
            return Ok(());
        };

        if self.tabs[index].read_only {
            self.message = Some(format!("{} is read-only", self.tabs[index].title));
            return Ok(());
        }

        if self.tabs[index].untitled {
            let title = self.tabs[index].title.clone();
            self.tabs[index].set_clean_text("");
            self.tabs[index].untitled = true;
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("cleared {title}"));
            return Ok(());
        }

        let path = self.tabs[index].path.clone();
        let title = self.tabs[index].title.clone();
        let previous_text = self.tabs[index].text();
        let text = read_text_lossy(&path)?;
        let changed = previous_text != text;

        self.tabs[index].set_clean_text(&text);
        self.ensure_editor_cursor_visible();
        self.refresh_git_status();
        self.message = if changed {
            Some(format!("reverted {title} from disk"))
        } else {
            Some(format!("{title} already matches disk"))
        };
        Ok(())
    }

    fn save_all_tabs(&mut self) {
        self.check_external_file_changes();
        let mut saved = 0usize;
        let mut skipped = 0usize;
        let mut untitled = 0usize;
        for tab in &mut self.tabs {
            if !tab.dirty {
                continue;
            }
            if tab.read_only {
                continue;
            }
            if tab.untitled {
                untitled += 1;
                continue;
            }
            if !tab.external_state.is_clean() {
                skipped += 1;
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
        if saved > 0 {
            self.refresh_git_status();
        }
        self.message = if skipped > 0 || untitled > 0 {
            let mut parts = vec![format!("saved {saved} dirty tab(s)")];
            if skipped > 0 {
                parts.push(format!("skipped {skipped} tab(s) with disk changes"));
            }
            if untitled > 0 {
                parts.push(format!("skipped {untitled} untitled tab(s); use Save As"));
            }
            Some(parts.join("; "))
        } else {
            Some(format!("saved {saved} dirty tab(s)"))
        };
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
        if !self.ensure_active_tab_writable("replace") {
            return;
        }
        let needle = needle.trim().to_owned();
        if needle.is_empty() {
            self.message = Some("replace requires a search string".to_owned());
            return;
        }

        self.search_needle = Some(needle.clone());
        self.start_prompt(PromptKind::ReplaceWith { needle, all }, "");
    }

    fn replace_next_active_match(&mut self, needle: String, replacement: String) {
        if !self.ensure_active_tab_writable("replace") {
            return;
        }
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
        if !self.ensure_active_tab_writable("replace all") {
            return;
        }
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

    fn workspace_replace_find_from_prompt(&mut self, needle: String) {
        let needle = needle.trim().to_owned();
        if needle.is_empty() {
            self.message = Some("replace in files requires a search string".to_owned());
            return;
        }

        self.search_needle = Some(needle.clone());
        self.start_prompt(PromptKind::WorkspaceReplaceWith { needle }, "");
    }

    fn start_rename_symbol_prompt(&mut self) {
        if !self.ensure_active_tab_writable("rename symbol") {
            return;
        }
        let Some(symbol) = self.active_identifier_under_cursor() else {
            self.message = Some("no symbol under cursor".to_owned());
            return;
        };

        self.start_prompt(
            PromptKind::RenameSymbol {
                old: symbol.clone(),
            },
            &symbol,
        );
        self.message = Some(format!("rename symbol: {symbol}"));
    }

    fn rename_symbol_from_prompt(&mut self, old: String, new_name: String) -> Result<()> {
        let new_name = new_name.trim().to_owned();
        if !is_identifier_token(&new_name) {
            self.message = Some("rename symbol requires a valid identifier".to_owned());
            return Ok(());
        }
        if let Some(summary) = self.try_lsp_rename_symbol(&new_name)? {
            self.search_needle = Some(new_name.clone());
            self.message = Some(format!(
                "LSP rename via {}: {} edit(s), {} open buffer(s), {} saved file(s)",
                summary.server, summary.edit_count, summary.open_count, summary.file_count
            ));
            return Ok(());
        }
        self.rename_symbol_occurrences(old, new_name)
    }

    fn try_lsp_rename_symbol(&mut self, new_name: &str) -> Result<Option<LspRenameSummary>> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            return Ok(None);
        };
        let Some(workspace_edit) = lsp::rename(&position, new_name)? else {
            return Ok(None);
        };
        self.apply_lsp_workspace_edit(workspace_edit)
    }

    fn try_lsp_code_action_command(
        &mut self,
        action: &lsp::LspCodeAction,
    ) -> Result<Option<LspRenameSummary>> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            return Ok(None);
        };
        let Some(workspace_edit) = lsp::execute_code_action_command(&position, action)? else {
            return Ok(None);
        };
        self.apply_lsp_workspace_edit(workspace_edit)
    }

    fn apply_lsp_workspace_edit(
        &mut self,
        workspace_edit: lsp::LspWorkspaceEdit,
    ) -> Result<Option<LspRenameSummary>> {
        let mut edits_by_path: HashMap<PathBuf, Vec<lsp::LspTextEdit>> = HashMap::new();
        for edit in workspace_edit.edits {
            let key = canonical_existing_path(&edit.path);
            edits_by_path.entry(key).or_default().push(edit);
        }

        let mut edit_count = 0usize;
        let mut open_count = 0usize;
        let mut file_count = 0usize;

        for (path, edits) in edits_by_path {
            if let Some(tab_index) = self
                .tabs
                .iter()
                .position(|tab| canonical_existing_path(&tab.path) == path)
            {
                if self.tabs[tab_index].read_only {
                    continue;
                }
                let original = self.tabs[tab_index].text();
                let Some((updated, count)) = apply_lsp_text_edits_to_text(&original, &edits) else {
                    continue;
                };
                if count == 0 {
                    continue;
                }
                if self.tabs[tab_index].replace_entire_text_as_edit(&updated) {
                    edit_count += count;
                    open_count += 1;
                }
                continue;
            }

            let Ok(metadata) = fs::metadata(&path) else {
                continue;
            };
            if !metadata.is_file() || metadata.len() > MAX_FILE_SCAN_BYTES {
                continue;
            }
            let Ok(bytes) = fs::read(&path) else {
                continue;
            };
            if bytes.contains(&0) {
                continue;
            }
            let Ok(original) = String::from_utf8(bytes) else {
                continue;
            };
            let Some((updated, count)) = apply_lsp_text_edits_to_text(&original, &edits) else {
                continue;
            };
            if count == 0 || updated == original {
                continue;
            }
            fs::write(&path, updated.as_bytes())?;
            edit_count += count;
            file_count += 1;
        }

        if edit_count == 0 {
            return Ok(None);
        }
        if file_count > 0 {
            self.refresh_git_status();
        }
        Ok(Some(LspRenameSummary {
            server: workspace_edit.server,
            edit_count,
            open_count,
            file_count,
        }))
    }

    fn rename_symbol_occurrences(&mut self, old: String, new_name: String) -> Result<()> {
        let old = old.trim().to_owned();
        if !is_identifier_token(&old) {
            self.message = Some("rename symbol requires a symbol under cursor".to_owned());
            return Ok(());
        }
        if !is_identifier_token(&new_name) {
            self.message = Some("rename symbol requires a valid identifier".to_owned());
            return Ok(());
        }
        if old == new_name {
            self.message = Some("rename symbol unchanged".to_owned());
            return Ok(());
        }

        let open_paths = self
            .tabs
            .iter()
            .map(|tab| tab.path.clone())
            .collect::<HashSet<_>>();
        let mut match_count = 0usize;
        let mut open_count = 0usize;
        let mut file_count = 0usize;

        for tab in &mut self.tabs {
            if tab.read_only {
                continue;
            }
            let (replaced, count) =
                replace_identifier_occurrences_in_text(&tab.text(), &old, &new_name);
            if count == 0 {
                continue;
            }
            if tab.replace_entire_text_as_edit(&replaced) {
                match_count += count;
                open_count += 1;
            }
        }

        for path in collect_workspace_files(&self.root, self.show_hidden, self.show_ignored)? {
            if open_paths.contains(&path) {
                continue;
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
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            let (replaced, count) = replace_identifier_occurrences_in_text(&text, &old, &new_name);
            if count == 0 {
                continue;
            }

            fs::write(&path, replaced.as_bytes())?;
            match_count += count;
            file_count += 1;
        }

        if file_count > 0 {
            self.refresh_git_status();
        }
        self.search_needle = Some(new_name.clone());
        self.message = if match_count == 0 {
            Some(format!("rename symbol found no matches for '{old}'"))
        } else {
            Some(format!(
                "renamed symbol '{old}' to '{new_name}': {match_count} occurrence(s), {open_count} open buffer(s), {file_count} saved file(s)"
            ))
        };
        Ok(())
    }

    fn replace_workspace_matches(&mut self, needle: String, replacement: String) -> Result<()> {
        let needle = needle.trim().to_owned();
        if needle.is_empty() {
            self.message = Some("replace in files requires a search string".to_owned());
            return Ok(());
        }

        let dirty_paths = self
            .tabs
            .iter()
            .filter(|tab| tab.dirty)
            .map(|tab| tab.path.clone())
            .collect::<HashSet<_>>();
        let mut changed = Vec::new();
        let mut match_count = 0usize;
        let mut skipped_dirty = 0usize;

        for path in collect_workspace_files(&self.root, self.show_hidden, self.show_ignored)? {
            if dirty_paths.contains(&path) {
                if self
                    .tabs
                    .iter()
                    .any(|tab| tab.path == path && tab.text().contains(&needle))
                {
                    skipped_dirty += 1;
                }
                continue;
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
            let Ok(text) = String::from_utf8(bytes) else {
                continue;
            };
            let count = text.matches(&needle).count();
            if count == 0 {
                continue;
            }

            let replaced = text.replace(&needle, &replacement);
            fs::write(&path, replaced.as_bytes())?;
            match_count += count;
            changed.push((path, replaced));
        }

        for (path, text) in &changed {
            for tab in &mut self.tabs {
                if tab.path == *path && !tab.dirty {
                    tab.set_clean_text(text);
                }
            }
        }

        if !changed.is_empty() {
            self.refresh_git_status();
        }
        self.search_needle = Some(needle.clone());
        let mut message = if changed.is_empty() {
            format!("replace in files found no writable matches for '{needle}'")
        } else {
            format!(
                "replace in files: {match_count} match(es) in {} file(s)",
                changed.len()
            )
        };
        if skipped_dirty > 0 {
            message.push_str(&format!("; skipped {skipped_dirty} dirty open file(s)"));
        }
        self.message = Some(message);
        Ok(())
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

    pub fn git_status_marker(&self, path: &Path, is_dir: bool) -> Option<&'static str> {
        if let Some(status) = self.git_statuses.get(path) {
            return Some(status.marker());
        }

        if is_dir && self.git_dirty_dirs.contains(path) {
            return Some("git:*");
        }

        None
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
        if !self.ensure_active_tab_writable("insert") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.insert_char(c);
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_newline(&mut self) {
        if !self.ensure_active_tab_writable("insert newline") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.newline();
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_backspace(&mut self) {
        if !self.ensure_active_tab_writable("backspace") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.backspace();
            self.ensure_editor_cursor_visible();
        }
    }

    fn edit_delete(&mut self) {
        if !self.ensure_active_tab_writable("delete") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.delete();
            self.ensure_editor_cursor_visible();
        }
    }

    fn indent_active_line(&mut self) {
        if !self.ensure_active_tab_writable("indent") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.indent_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("indented line".to_owned());
        }
    }

    fn outdent_active_line(&mut self) {
        if !self.ensure_active_tab_writable("outdent") {
            return;
        }
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
        if !self.ensure_active_tab_writable("duplicate line") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.duplicate_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("duplicated line".to_owned());
        }
    }

    fn delete_active_line(&mut self) {
        if !self.ensure_active_tab_writable("delete line") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            tab.delete_line();
            self.ensure_editor_cursor_visible();
            self.message = Some("deleted line".to_owned());
        }
    }

    fn move_active_line_up(&mut self) {
        if !self.ensure_active_tab_writable("move line") {
            return;
        }
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
        if !self.ensure_active_tab_writable("move line") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            if tab.move_line_down() {
                self.ensure_editor_cursor_visible();
                self.message = Some("moved line down".to_owned());
            } else {
                self.message = Some("line already at bottom".to_owned());
            }
        }
    }

    fn toggle_active_fold(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            match tab.toggle_fold_at_line(tab.cursor_line) {
                Some(true) => {
                    self.ensure_editor_cursor_visible();
                    self.message = Some("folded block".to_owned());
                }
                Some(false) => {
                    self.ensure_editor_cursor_visible();
                    self.message = Some("unfolded block".to_owned());
                }
                None => {
                    self.message = Some("no foldable block at cursor".to_owned());
                }
            }
        }
    }

    fn toggle_word_wrap(&mut self) {
        self.word_wrap = !self.word_wrap;
        if self.word_wrap
            && let Some(tab) = self.active_tab_mut()
        {
            tab.horizontal_scroll = 0;
        }
        self.ensure_editor_cursor_visible();
        self.focus = FocusPanel::Editor;
        self.message = Some(if self.word_wrap {
            "word wrap enabled".to_owned()
        } else {
            "word wrap disabled".to_owned()
        });
    }

    fn fold_all_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            let count = tab.fold_all();
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("folded {count} block(s)"));
        }
    }

    fn unfold_all_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            let count = tab.unfold_all();
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("unfolded {count} block(s)"));
        }
    }

    fn toggle_active_line_comment(&mut self) {
        if !self.ensure_active_tab_writable("toggle line comment") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            if tab.toggle_line_comment() {
                self.ensure_editor_cursor_visible();
                self.message = Some("toggled line comment".to_owned());
            } else {
                self.message = Some("no line comment token for file type".to_owned());
            }
        }
    }

    fn toggle_active_block_comment(&mut self) {
        if !self.ensure_active_tab_writable("toggle block comment") {
            return;
        }
        if let Some(tab) = self.active_tab_mut() {
            match tab.toggle_block_comment() {
                Some(true) => {
                    self.ensure_editor_cursor_visible();
                    self.message = Some("added block comment".to_owned());
                }
                Some(false) => {
                    self.ensure_editor_cursor_visible();
                    self.message = Some("removed block comment".to_owned());
                }
                None => {
                    self.message = Some("no block comment token for file type".to_owned());
                }
            }
        }
    }

    fn trim_active_trailing_whitespace(&mut self) {
        if !self.ensure_active_tab_writable("trim trailing whitespace") {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        let changed_lines = tab.trim_trailing_whitespace();
        self.ensure_editor_cursor_visible();
        self.message = if changed_lines == 0 {
            Some("no trailing whitespace".to_owned())
        } else {
            Some(format!(
                "trimmed trailing whitespace on {changed_lines} line(s)"
            ))
        };
    }

    fn format_active_document(&mut self) -> Result<()> {
        let Some(index) = self.active_tab else {
            self.message = Some("no active editor tab".to_owned());
            return Ok(());
        };
        if self.tabs[index].read_only {
            self.message = Some(format!("{} is read-only", self.tabs[index].title));
            return Ok(());
        }

        let path = self.tabs[index].path.clone();
        let title = self.tabs[index].title.clone();
        if let Some(summary) = self.try_lsp_format_document()? {
            self.ensure_editor_cursor_visible();
            self.message = Some(format!(
                "formatted {title} via {}: {} edit(s)",
                summary.server, summary.edit_count
            ));
            return Ok(());
        }

        let Some(formatter) = formatter_command_for_path(&path, &self.root) else {
            self.message = Some(format!(
                "no formatter configured for {}",
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("[file]")
            ));
            return Ok(());
        };

        let original = self.tabs[index].text();
        let formatted = run_formatter_command(&formatter, &original)?;
        if self.tabs[index].replace_entire_text_as_edit(&formatted) {
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("formatted {title} with {}", formatter.label));
        } else {
            self.message = Some(format!("already formatted with {}", formatter.label));
        }
        Ok(())
    }

    fn try_lsp_format_document(&mut self) -> Result<Option<LspRenameSummary>> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            return Ok(None);
        };
        let Some(workspace_edit) = lsp::formatting(&position)? else {
            return Ok(None);
        };
        self.apply_lsp_workspace_edit(workspace_edit)
    }

    fn run_workspace_check(&mut self) -> Result<()> {
        let Some(command) = workspace_check_command(&self.root) else {
            self.message = Some("no workspace checker detected".to_owned());
            return Ok(());
        };

        self.message = Some(format!("running {}", command.label));
        let output = Command::new(command.program)
            .args(command.args)
            .current_dir(&self.root)
            .env("NO_COLOR", "1")
            .env("CLICOLOR", "0")
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .with_context(|| format!("failed to run {}", command.label))?;

        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&output.stderr));
        if !output.stdout.is_empty() {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&String::from_utf8_lossy(&output.stdout));
        }

        self.problems = parse_problem_items(&combined, &self.root);
        let problem_count = self.problems.len();
        self.open_quick_panel(QuickPanelKind::Problems)?;

        let dirty_note = if self.tabs.iter().any(|tab| tab.dirty) {
            "; unsaved buffers were not checked"
        } else {
            ""
        };
        self.message = if problem_count == 0 && output.status.success() {
            Some(format!(
                "{} completed: no problems{dirty_note}",
                command.label
            ))
        } else if problem_count == 0 {
            Some(format!(
                "{} failed but no parseable file diagnostics were found{dirty_note}",
                command.label
            ))
        } else {
            Some(format!(
                "{} found {} problem(s){dirty_note}",
                command.label, problem_count
            ))
        };
        Ok(())
    }

    fn run_lsp_diagnostics(&mut self) -> Result<()> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            self.message = Some("no language server configured for active editor".to_owned());
            return Ok(());
        };
        let server = lsp::server_name_for_path(&position.path).unwrap_or_else(|| "LSP".to_owned());
        self.message = Some(format!("running LSP diagnostics via {server}"));

        let diagnostics = lsp::diagnostics(&position)?;
        self.problems = lsp_diagnostics_to_problem_items(&self.root, diagnostics);
        let problem_count = self.problems.len();
        self.open_quick_panel(QuickPanelKind::Problems)?;

        self.message = if problem_count == 0 {
            Some(format!("LSP diagnostics via {server}: no problems"))
        } else {
            Some(format!(
                "LSP diagnostics via {server}: {problem_count} problem(s)"
            ))
        };
        Ok(())
    }

    fn run_code_actions(&mut self) -> Result<()> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            self.message = Some("no language server configured for active editor".to_owned());
            return Ok(());
        };
        let server = lsp::server_name_for_path(&position.path).unwrap_or_else(|| "LSP".to_owned());
        self.message = Some(format!("requesting code actions via {server}"));

        let diagnostics = lsp::diagnostics(&position).unwrap_or_default();
        self.lsp_code_actions = lsp::code_actions(&position, &diagnostics)?;
        let count = self.lsp_code_actions.len();
        self.open_quick_panel(QuickPanelKind::CodeActions)?;
        self.message = if count == 0 {
            Some(format!("no code actions returned by {server}"))
        } else {
            Some(format!("LSP code actions via {server}: {count} action(s)"))
        };
        Ok(())
    }

    fn select_all_active_tab(&mut self) {
        if let Some(tab) = self.active_tab_mut() {
            tab.select_all();
            self.ensure_editor_cursor_visible();
            self.message = Some("selected all".to_owned());
        }
    }

    fn add_selection_to_next_match(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        match tab.add_next_occurrence_selection() {
            Some((count, needle)) => {
                self.search_needle = Some(needle.clone());
                self.ensure_editor_cursor_visible();
                self.message = Some(format!("selected {count} occurrence(s) of '{needle}'"));
            }
            None => {
                self.message = Some("no next occurrence for current word or selection".to_owned());
            }
        }
    }

    fn select_all_occurrences_in_active_tab(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        match tab.select_all_occurrences() {
            Some((count, needle)) => {
                self.search_needle = Some(needle.clone());
                self.ensure_editor_cursor_visible();
                self.message = Some(format!("selected all {count} occurrence(s) of '{needle}'"));
            }
            None => {
                self.message = Some("no word or single-line selection to select".to_owned());
            }
        }
    }

    fn copy_editor_selection(&mut self) {
        let Some(tab) = self.active_tab() else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        let copied_line = tab.selected_text().is_none();
        let Some(text) = tab
            .selected_text()
            .or_else(|| tab.current_line_clipboard_text())
        else {
            self.message = Some("no editor text to copy".to_owned());
            return;
        };
        let count = text.chars().count();
        self.editor_clipboard = Some(text.clone());
        if self.queue_clipboard_export(&text) {
            self.message = if copied_line {
                Some("copied current line to clipboard".to_owned())
            } else {
                Some(format!("copied {count} char(s) to clipboard"))
            };
        } else {
            self.message = Some(format!(
                "copied {count} char(s) internally; selection too large for terminal clipboard"
            ));
        }
    }

    fn cut_editor_selection(&mut self) {
        if !self.ensure_active_tab_writable("cut") {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let mut cut_line = false;
        let Some(text) = tab.delete_selection().or_else(|| {
            cut_line = true;
            let text = tab.current_line_clipboard_text();
            if text.is_some() {
                tab.delete_line();
            }
            text
        }) else {
            self.message = Some("no editor text to cut".to_owned());
            return;
        };
        let count = text.chars().count();
        self.editor_clipboard = Some(text.clone());
        self.ensure_editor_cursor_visible();
        if self.queue_clipboard_export(&text) {
            self.message = if cut_line {
                Some("cut current line to clipboard".to_owned())
            } else {
                Some(format!("cut {count} char(s) to clipboard"))
            };
        } else {
            self.message = Some(format!(
                "cut {count} char(s) internally; selection too large for terminal clipboard"
            ));
        }
    }

    fn paste_editor_clipboard(&mut self) {
        if !self.ensure_active_tab_writable("paste") {
            return;
        }
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

    fn run_selection_in_terminal(&mut self) -> Result<()> {
        let Some(text) = self.editor_text_for_terminal_submission() else {
            self.message = Some("no editor text to run in terminal".to_owned());
            return Ok(());
        };

        self.record_terminal_command(&text);
        let submitted = terminal_submission_text(&text);
        self.active_terminal_mut().shell.send_text(&submitted)?;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!(
            "sent {} line(s) to terminal",
            text.lines().count().max(1)
        ));
        Ok(())
    }

    fn run_active_file_in_terminal(&mut self) -> Result<()> {
        let Some(tab) = self.active_tab() else {
            self.message = Some("open a file before running it".to_owned());
            return Ok(());
        };
        if tab.untitled {
            self.message = Some("save the Untitled file before running it".to_owned());
            return Ok(());
        }
        if tab.dirty {
            self.message = Some(format!("save {} before running it", tab.title));
            return Ok(());
        }
        if !tab.external_state.is_clean() {
            self.message = Some(format!(
                "reload or save {} before running it ({})",
                tab.title,
                tab.external_state.label()
            ));
            return Ok(());
        }

        self.run_file_path_in_terminal(tab.path.clone())
    }

    fn run_selected_explorer_file_in_terminal(&mut self) -> Result<()> {
        let paths = normalize_file_op_paths(self.selected_explorer_paths());
        if paths.len() != 1 {
            self.message = Some("select one explorer file to run".to_owned());
            return Ok(());
        }
        let path = paths[0].clone();
        if !path.is_file() {
            self.message = Some(format!(
                "select a file to run, not {}",
                relative_path(&self.root, &path)
            ));
            return Ok(());
        }

        self.run_file_path_in_terminal(path)
    }

    fn run_file_path_in_terminal(&mut self, path: PathBuf) -> Result<()> {
        let path = path.canonicalize().unwrap_or(path);
        let Some(run) = run_command_for_file(&self.root, &path) else {
            self.message = Some(format!(
                "no run command for {}",
                relative_path(&self.root, &path)
            ));
            return Ok(());
        };

        let title = format!(
            "run: {}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("file")
        );
        let title = truncate_chars(&title, 32);
        let command = run.command;
        self.start_terminal_command(title, run.cwd, &command)?;
        self.message = Some(format!(
            "running {}: {}",
            relative_path(&self.root, &path),
            command
        ));
        Ok(())
    }

    fn run_task_item(&mut self, item: QuickItem) -> Result<()> {
        let command = item.detail.trim().to_owned();
        if command.is_empty() {
            self.message = Some(format!("task has no command: {}", item.label));
            return Ok(());
        }

        let cwd = if item.path.is_dir() {
            item.path.canonicalize().unwrap_or(item.path)
        } else {
            self.root.clone()
        };
        let title = format!("task: {}", truncate_chars(&item.label, 28));
        self.start_terminal_command(title.clone(), cwd, &command)?;
        self.message = Some(format!("started {title}: {command}"));
        Ok(())
    }

    fn start_terminal_command(&mut self, title: String, cwd: PathBuf, command: &str) -> Result<()> {
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        let terminal = TerminalSession::with_locked_title(id, title, cwd)?;
        self.terminals.push(terminal);
        self.set_active_terminal(self.terminals.len() - 1);
        self.focus = FocusPanel::Terminal;
        self.terminal_selection = None;

        self.record_terminal_command(command);
        let submitted = terminal_submission_text(command);
        self.active_terminal_mut().shell.send_text(&submitted)?;
        Ok(())
    }

    fn editor_text_for_terminal_submission(&self) -> Option<String> {
        let tab = self.active_tab()?;
        if let Some(text) = tab.selected_text()
            && !text.trim().is_empty()
        {
            return Some(text);
        }

        let line = tab.lines.get(tab.cursor_line)?.clone();
        (!line.trim().is_empty()).then_some(line)
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

    fn go_to_definition_under_cursor(&mut self) -> Result<()> {
        let Some(symbol) = self.active_identifier_under_cursor() else {
            self.message = Some("no symbol under cursor".to_owned());
            return Ok(());
        };
        let mut definitions = self.lsp_definition_items()?;
        let used_lsp = !definitions.is_empty();
        if definitions.is_empty() {
            definitions = self.definition_items(&symbol)?;
        }

        match definitions.len() {
            0 => {
                self.message = Some(format!("definition not found: {symbol}"));
            }
            1 => {
                let item = definitions.into_iter().next().unwrap();
                self.open_quick_item(item, None);
                self.focus = FocusPanel::Editor;
                self.message = Some(if used_lsp {
                    format!("jumped to LSP definition: {symbol}")
                } else {
                    format!("jumped to definition: {symbol}")
                });
            }
            _ => {
                self.quick_panel = Some(QuickPanel {
                    kind: QuickPanelKind::Definitions,
                    query_cursor: symbol.chars().count(),
                    query: symbol.clone(),
                    items: definitions,
                    selected: 0,
                    scroll: 0,
                });
                self.message = Some(if used_lsp {
                    format!("multiple LSP definitions: {symbol}")
                } else {
                    format!("multiple definitions: {symbol}")
                });
            }
        }

        Ok(())
    }

    fn lsp_definition_items(&self) -> Result<Vec<QuickItem>> {
        self.lsp_location_items(lsp::definitions)
    }

    fn go_to_type_definition_under_cursor(&mut self) -> Result<()> {
        self.go_to_lsp_location_under_cursor(
            "type definition",
            QuickPanelKind::TypeDefinitions,
            lsp::type_definitions,
        )
    }

    fn go_to_implementation_under_cursor(&mut self) -> Result<()> {
        self.go_to_lsp_location_under_cursor(
            "implementation",
            QuickPanelKind::Implementations,
            lsp::implementations,
        )
    }

    fn go_to_lsp_location_under_cursor(
        &mut self,
        label: &str,
        panel_kind: QuickPanelKind,
        request: fn(&DocumentPosition) -> Result<Vec<lsp::LspLocation>>,
    ) -> Result<()> {
        let Some(symbol) = self.active_identifier_under_cursor() else {
            self.message = Some("no symbol under cursor".to_owned());
            return Ok(());
        };
        let locations = self.lsp_location_items(request)?;

        match locations.len() {
            0 => {
                self.message = Some(format!("LSP {label} not found: {symbol}"));
            }
            1 => {
                let item = locations.into_iter().next().unwrap();
                self.open_quick_item(item, None);
                self.focus = FocusPanel::Editor;
                self.message = Some(format!("jumped to LSP {label}: {symbol}"));
            }
            count => {
                self.quick_panel = Some(QuickPanel {
                    kind: panel_kind,
                    query_cursor: symbol.chars().count(),
                    query: symbol.clone(),
                    items: locations,
                    selected: 0,
                    scroll: 0,
                });
                self.message = Some(format!("multiple LSP {label}s: {symbol} ({count})"));
            }
        }

        Ok(())
    }

    fn lsp_location_items(
        &self,
        request: fn(&DocumentPosition) -> Result<Vec<lsp::LspLocation>>,
    ) -> Result<Vec<QuickItem>> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            return Ok(Vec::new());
        };
        let mut items = Vec::new();
        for location in request(&position)? {
            if !location.path.is_file() {
                continue;
            }
            let relative = relative_path(&self.root, &location.path);
            items.push(QuickItem {
                label: format!("{}:{}", relative, location.line + 1),
                detail: format!("LSP {}  col {}", location.server, location.col + 1),
                path: location.path,
                line: Some(location.line),
                col: Some(location.col),
                preview: location.preview,
                command: None,
            });
            if items.len() >= MAX_QUICK_ITEMS {
                break;
            }
        }
        Ok(items)
    }

    fn show_lsp_hover_under_cursor(&mut self) -> Result<()> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            self.message = Some("no language server configured for the active file".to_owned());
            return Ok(());
        };
        let Some(hover) = lsp::hover(&position)? else {
            let server = lsp::server_name_for_path(&position.path)
                .unwrap_or_else(|| "language server".to_owned());
            self.message = Some(format!("no LSP hover returned by {server}"));
            return Ok(());
        };

        let (summary, preview) = hover_quick_text(&hover.contents);
        self.quick_panel = Some(QuickPanel {
            kind: QuickPanelKind::LspHover,
            query: String::new(),
            query_cursor: 0,
            items: vec![QuickItem {
                label: "LSP Hover".to_owned(),
                detail: format!("{}  {}", hover.server, summary),
                path: position.path,
                line: Some(position.line),
                col: Some(position.col),
                preview,
                command: None,
            }],
            selected: 0,
            scroll: 0,
        });
        self.message = Some(format!("LSP hover from {}", hover.server));
        Ok(())
    }

    fn show_lsp_signature_help_under_cursor(&mut self) -> Result<()> {
        let Some(position) = self.active_lsp_position_at_cursor() else {
            self.message = Some("no language server configured for the active file".to_owned());
            return Ok(());
        };
        let Some(help) = lsp::signature_help(&position)? else {
            let server = lsp::server_name_for_path(&position.path)
                .unwrap_or_else(|| "language server".to_owned());
            self.message = Some(format!("no LSP signature help returned by {server}"));
            return Ok(());
        };

        let items = lsp_signature_help_items(&help, position.path, position.line, position.col);
        if items.is_empty() {
            self.message = Some(format!("no LSP signature help returned by {}", help.server));
            return Ok(());
        }
        let count = items.len();
        let selected = help
            .active_signature
            .unwrap_or(0)
            .min(count.saturating_sub(1));
        self.quick_panel = Some(QuickPanel {
            kind: QuickPanelKind::SignatureHelp,
            query: String::new(),
            query_cursor: 0,
            items,
            selected,
            scroll: 0,
        });
        self.message = Some(format!("LSP signature help from {}", help.server));
        Ok(())
    }

    fn show_lsp_call_hierarchy_under_cursor(
        &mut self,
        label: &str,
        kind: QuickPanelKind,
        request: fn(&DocumentPosition) -> Result<Vec<lsp::LspCallHierarchyEntry>>,
    ) -> Result<()> {
        let Some(symbol) = self.active_identifier_under_cursor() else {
            self.message = Some("no symbol under cursor".to_owned());
            return Ok(());
        };
        let Some(position) = self.active_lsp_position_at_cursor() else {
            self.message = Some("no language server configured for the active file".to_owned());
            return Ok(());
        };
        let entries = request(&position)?;
        if entries.is_empty() {
            let server = lsp::server_name_for_path(&position.path)
                .unwrap_or_else(|| "language server".to_owned());
            self.message = Some(format!("no LSP {label}s returned by {server}: {symbol}"));
            return Ok(());
        }

        let items = lsp_call_hierarchy_items(entries, &self.root);
        if items.is_empty() {
            self.message = Some(format!("no on-disk LSP {label}s returned for {symbol}"));
            return Ok(());
        }
        let count = items.len();
        self.quick_panel = Some(QuickPanel {
            kind,
            query_cursor: symbol.chars().count(),
            query: symbol.clone(),
            items,
            selected: 0,
            scroll: 0,
        });
        self.message = Some(format!("LSP {label}s: {symbol} ({count})"));
        Ok(())
    }

    fn highlight_symbol_under_cursor(&mut self) -> Result<()> {
        let Some(symbol) = self.active_identifier_under_cursor() else {
            self.message = Some("no symbol under cursor".to_owned());
            return Ok(());
        };
        let Some(position) = self.active_lsp_position_at_cursor() else {
            self.message = Some("no language server configured for the active file".to_owned());
            return Ok(());
        };
        let highlights = lsp::document_highlights(&position)?;
        if highlights.is_empty() {
            let server = lsp::server_name_for_path(&position.path)
                .unwrap_or_else(|| "language server".to_owned());
            if let Some(tab) = self.active_tab_mut() {
                tab.document_highlights.clear();
            }
            self.message = Some(format!(
                "no LSP document highlights returned by {server}: {symbol}"
            ));
            return Ok(());
        }

        let server = highlights
            .first()
            .map(|highlight| highlight.server.clone())
            .unwrap_or_else(|| "language server".to_owned());
        let read_count = highlights
            .iter()
            .filter(|highlight| highlight.kind == lsp::LspDocumentHighlightKind::Read)
            .count();
        let write_count = highlights
            .iter()
            .filter(|highlight| highlight.kind == lsp::LspDocumentHighlightKind::Write)
            .count();
        let count = highlights.len();
        if let Some(tab) = self.active_tab_mut() {
            tab.document_highlights = highlights;
        }
        self.focus = FocusPanel::Editor;
        self.message = Some(format!(
            "LSP highlights from {server}: {symbol} ({count}, read:{read_count}, write:{write_count})"
        ));
        Ok(())
    }

    fn clear_document_highlights(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        let count = tab.document_highlights.len();
        tab.document_highlights.clear();
        self.message = if count == 0 {
            Some("no symbol highlights to clear".to_owned())
        } else {
            Some(format!("cleared {count} symbol highlight(s)"))
        };
    }

    fn find_references_under_cursor(&mut self) -> Result<()> {
        let Some(symbol) = self.active_identifier_under_cursor() else {
            self.message = Some("no symbol under cursor".to_owned());
            return Ok(());
        };
        let references = self.lsp_reference_items()?;
        if !references.is_empty() {
            let count = references.len();
            self.quick_panel = Some(QuickPanel {
                kind: QuickPanelKind::LspReferences,
                query_cursor: symbol.chars().count(),
                query: symbol.clone(),
                items: references,
                selected: 0,
                scroll: 0,
            });
            self.message = Some(format!("LSP references: {symbol} ({count})"));
            return Ok(());
        }
        self.open_quick_panel_with_query(QuickPanelKind::References, symbol)
    }

    fn lsp_reference_items(&self) -> Result<Vec<QuickItem>> {
        self.lsp_location_items(lsp::references)
    }

    fn active_lsp_position_at_cursor(&self) -> Option<DocumentPosition> {
        let tab = self.active_tab()?;
        lsp::server_name_for_path(&tab.path)?;
        Some(DocumentPosition {
            root: self.root.clone(),
            path: tab.path.clone(),
            text: tab.text(),
            line: tab.cursor_line,
            col: tab.cursor_col,
        })
    }

    fn active_identifier_under_cursor(&self) -> Option<String> {
        let tab = self.active_tab()?;
        if let Some(selected) = tab.selected_text() {
            let selected = selected.trim();
            if is_identifier_token(selected) {
                return Some(selected.to_owned());
            }
        }

        let line = tab.lines.get(tab.cursor_line)?;
        identifier_at_char(line, tab.cursor_col)
    }

    fn ensure_editor_cursor_visible(&mut self) {
        let height = self.editor_height.max(1);
        let editor_width = self.editor_width;
        let word_wrap = self.word_wrap;
        if let Some(tab) = self.active_tab_mut() {
            tab.unfold_line_containing(tab.cursor_line);
            let code_width = editor_code_width(tab, editor_width);
            let cursor_row = editor_visual_row_for_line_col(
                tab,
                tab.cursor_line,
                tab.cursor_col,
                code_width,
                word_wrap,
            )
            .unwrap_or(0);
            if cursor_row < tab.scroll {
                tab.scroll = cursor_row;
            } else if cursor_row >= tab.scroll + height {
                tab.scroll = cursor_row.saturating_sub(height - 1);
            }

            if word_wrap {
                tab.horizontal_scroll = 0;
            } else if code_width > 0 {
                if tab.cursor_col < tab.horizontal_scroll {
                    tab.horizontal_scroll = tab.cursor_col;
                } else if tab.cursor_col >= tab.horizontal_scroll.saturating_add(code_width) {
                    tab.horizontal_scroll = tab.cursor_col.saturating_sub(code_width - 1);
                }
                tab.horizontal_scroll = tab
                    .horizontal_scroll
                    .min(max_editor_horizontal_scroll(tab, code_width));
            }
        }
    }

    fn set_editor_cursor_from_mouse(&mut self, selecting: bool) {
        if !is_editor_target(&self.hover) {
            return;
        }
        if let Some((line, col)) = self.editor_mouse_position()
            && let Some(tab) = self.active_tab_mut()
        {
            if selecting {
                tab.set_cursor_selecting(line, col);
            } else {
                tab.set_cursor(line, col);
            }
            self.ensure_editor_cursor_visible();
        }
    }

    fn start_editor_selection_from_mouse(&mut self) {
        self.focus = FocusPanel::Editor;
        if self.toggle_editor_fold_from_mouse() {
            self.editor_selection_dragging = false;
            self.editor_gutter_dragging = None;
            return;
        }

        if self.toggle_editor_bookmark_from_mouse() {
            self.editor_selection_dragging = false;
            self.editor_gutter_dragging = None;
            return;
        }

        if self.start_editor_gutter_selection_from_mouse() {
            return;
        }

        self.editor_selection_dragging = true;
        self.set_editor_cursor_from_mouse(false);
    }

    fn start_editor_gutter_selection_from_mouse(&mut self) -> bool {
        let Some(line) = self.editor_mouse_gutter_line() else {
            self.editor_gutter_dragging = None;
            return false;
        };
        let Some(tab) = self.active_tab_mut() else {
            self.editor_gutter_dragging = None;
            return false;
        };

        tab.select_line_range_from_anchor(line, line);
        self.editor_selection_dragging = false;
        self.editor_gutter_dragging = Some(line);
        self.ensure_editor_cursor_visible();
        self.message = Some(format!("selected line {}", line + 1));
        true
    }

    fn handle_editor_gutter_selection_mouse(&mut self, mouse: MouseEvent) {
        self.focus = FocusPanel::Editor;
        self.scroll_editor_for_drag(mouse);
        let anchor = self.editor_gutter_dragging.unwrap_or(0);
        if let Some(active) = self.editor_mouse_gutter_line_clamped()
            && let Some(tab) = self.active_tab_mut()
        {
            tab.select_line_range_from_anchor(anchor, active);
            self.ensure_editor_cursor_visible();
        }

        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            self.editor_gutter_dragging = None;
            if let Some((start, end)) = self.active_tab().and_then(|tab| {
                let (start, end, had_selection) = tab.command_line_range();
                had_selection.then_some((start, end))
            }) {
                let count = end.saturating_sub(start) + 1;
                self.message = Some(format!("{count} editor line(s) selected"));
            }
        }
    }

    fn handle_editor_selection_mouse(&mut self, mouse: MouseEvent) {
        self.focus = FocusPanel::Editor;
        self.scroll_editor_for_drag(mouse);
        self.set_editor_cursor_from_mouse_clamped(true);

        if matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left)) {
            self.editor_selection_dragging = false;
            if let Some(tab) = self.active_tab() {
                let count = tab.selection_count();
                if count > 0 {
                    self.message = Some(format!("{count} editor selection(s)"));
                }
            }
        }
    }

    fn set_editor_cursor_from_mouse_clamped(&mut self, selecting: bool) {
        if let Some((line, col)) = self.editor_mouse_position_clamped()
            && let Some(tab) = self.active_tab_mut()
        {
            if selecting {
                tab.set_cursor_selecting(line, col);
            } else {
                tab.set_cursor(line, col);
            }
            self.ensure_editor_cursor_visible();
        }
    }

    fn scroll_editor_for_drag(&mut self, mouse: MouseEvent) {
        let Some(body) = self.hit_regions.editor_body else {
            return;
        };
        if body.width == 0 || body.height == 0 {
            return;
        }

        if mouse.row < body.y {
            self.scroll_editor(-1);
        } else if mouse.row >= body.bottom() {
            self.scroll_editor(1);
        }

        if self.word_wrap {
            return;
        }

        let gutter_width = self
            .active_tab()
            .map(|tab| editor_gutter_width(tab.lines.len()) as u16)
            .unwrap_or(0);
        let code_start = body
            .x
            .saturating_add(gutter_width)
            .min(body.right().saturating_sub(1));
        if mouse.column < code_start {
            self.scroll_editor_horizontal(-1);
        } else if mouse.column >= body.right() {
            self.scroll_editor_horizontal(1);
        }
    }

    fn toggle_editor_cursor_from_mouse(&mut self) {
        let Some((line, col)) = self.editor_mouse_position() else {
            return;
        };
        if let Some(tab) = self.active_tab_mut() {
            let count = tab.toggle_cursor_at(line, col);
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("{count} editor cursor(s)"));
        }
    }

    fn editor_visual_row_at(&self, body: Rect, visual_row: usize) -> Option<EditorVisualRow> {
        let tab = self.active_tab()?;
        if !self.word_wrap {
            return tab.visible_line_at(visual_row).map(|line| EditorVisualRow {
                line,
                start_col: 0,
                continuation: false,
            });
        }
        let code_width = editor_code_width(tab, body.width as usize);
        editor_visual_rows(tab, code_width, self.word_wrap)
            .get(visual_row)
            .copied()
    }

    fn editor_visual_row_at_or_last(
        &self,
        body: Rect,
        visual_row: usize,
    ) -> Option<EditorVisualRow> {
        let tab = self.active_tab()?;
        if !self.word_wrap {
            return tab
                .visible_line_at(visual_row)
                .or_else(|| tab.visible_line_indices().last().copied())
                .map(|line| EditorVisualRow {
                    line,
                    start_col: 0,
                    continuation: false,
                });
        }
        let code_width = editor_code_width(tab, body.width as usize);
        let rows = editor_visual_rows(tab, code_width, self.word_wrap);
        rows.get(visual_row)
            .copied()
            .or_else(|| rows.last().copied())
    }

    fn editor_mouse_position(&self) -> Option<(usize, usize)> {
        let body = self.hit_regions.editor_body?;
        let tab = self.active_tab()?;
        let gutter_width = editor_gutter_width(tab.lines.len());
        let local_x = self.hit_regions.last_mouse_x.saturating_sub(body.x) as usize;
        let visible_row =
            tab.scroll + self.hit_regions.last_mouse_y.saturating_sub(body.y) as usize;
        let row = self.editor_visual_row_at(body, visible_row)?;
        let col = if local_x < gutter_width {
            row.start_col
        } else if self.word_wrap {
            row.start_col + local_x.saturating_sub(gutter_width)
        } else {
            local_x.saturating_sub(gutter_width) + tab.horizontal_scroll
        };
        let line = row.line;
        Some((line, col))
    }

    fn editor_mouse_gutter_line(&self) -> Option<usize> {
        let body = self.hit_regions.editor_body?;
        let tab = self.active_tab()?;
        let gutter_width = editor_gutter_width(tab.lines.len());
        let local_x = self.hit_regions.last_mouse_x.saturating_sub(body.x) as usize;
        if local_x >= gutter_width.saturating_sub(1) {
            return None;
        }
        if local_x == 0 {
            return None;
        }
        let visible_row =
            tab.scroll + self.hit_regions.last_mouse_y.saturating_sub(body.y) as usize;
        self.editor_visual_row_at(body, visible_row)
            .map(|row| row.line)
    }

    fn editor_mouse_gutter_line_clamped(&self) -> Option<usize> {
        let body = self.hit_regions.editor_body?;
        if body.width == 0 || body.height == 0 {
            return None;
        }
        let tab = self.active_tab()?;
        let max_y = body.y.saturating_add(body.height.saturating_sub(1));
        let mouse_y = self.hit_regions.last_mouse_y.clamp(body.y, max_y);
        let visible_row = tab.scroll + mouse_y.saturating_sub(body.y) as usize;
        self.editor_visual_row_at_or_last(body, visible_row)
            .map(|row| row.line)
    }

    fn editor_mouse_position_clamped(&self) -> Option<(usize, usize)> {
        let body = self.hit_regions.editor_body?;
        if body.width == 0 || body.height == 0 {
            return None;
        }
        let tab = self.active_tab()?;
        let gutter_width = editor_gutter_width(tab.lines.len());
        let max_x = body.x.saturating_add(body.width.saturating_sub(1));
        let max_y = body.y.saturating_add(body.height.saturating_sub(1));
        let mouse_x = self.hit_regions.last_mouse_x.clamp(body.x, max_x);
        let mouse_y = self.hit_regions.last_mouse_y.clamp(body.y, max_y);
        let local_x = mouse_x.saturating_sub(body.x) as usize;
        let visible_row = tab.scroll + mouse_y.saturating_sub(body.y) as usize;
        let row = self.editor_visual_row_at_or_last(body, visible_row)?;
        let col = if local_x < gutter_width {
            row.start_col
        } else if self.word_wrap {
            row.start_col + local_x.saturating_sub(gutter_width)
        } else {
            local_x.saturating_sub(gutter_width) + tab.horizontal_scroll
        };
        let line = row.line;
        Some((line, col))
    }

    fn update_editor_hover_for_mouse(&mut self, target: &HoverTarget, kind: MouseEventKind) {
        self.editor_hover = if is_editor_target(target)
            && !is_scroll_mouse_event(kind)
            && self.prompt.is_none()
            && self.quick_panel.is_none()
        {
            self.editor_hover_at_mouse()
        } else {
            None
        };
    }

    fn editor_hover_at_mouse(&self) -> Option<EditorHoverInfo> {
        let (line, col) = self.editor_mouse_position()?;
        let tab = self.active_tab()?;
        let source_line = tab.lines.get(line)?;
        let (_, _, symbol) = identifier_range_at_char(source_line, col)?;
        if is_control_symbol_name(&symbol) {
            return None;
        }

        let definitions = self.definition_items(&symbol).ok()?;
        let references = self.reference_items(&symbol).ok()?;
        let first_definition = definitions.first();
        let definition = first_definition.and_then(|item| {
            Some(EditorLocation {
                path: item.path.clone(),
                line: item.line?,
                col: item.col.unwrap_or(0),
            })
        });

        Some(EditorHoverInfo {
            symbol,
            path: tab.path.clone(),
            line,
            col,
            definition_count: definitions.len(),
            reference_count: references.len(),
            definition,
            definition_detail: first_definition.map(|item| item.detail.clone()),
            definition_preview: first_definition.and_then(|item| item.preview.clone()),
        })
    }

    fn editor_mouse_fold_line(&self) -> Option<usize> {
        let body = self.hit_regions.editor_body?;
        let tab = self.active_tab()?;
        let local_x = self.hit_regions.last_mouse_x.saturating_sub(body.x) as usize;
        let gutter_width = editor_gutter_width(tab.lines.len());
        if local_x + 1 != gutter_width {
            return None;
        }
        let visible_row =
            tab.scroll + self.hit_regions.last_mouse_y.saturating_sub(body.y) as usize;
        let row = self.editor_visual_row_at(body, visible_row)?;
        if row.continuation {
            return None;
        }
        let line = row.line;
        tab.fold_end_for_line(line).map(|_| line)
    }

    fn toggle_editor_fold_from_mouse(&mut self) -> bool {
        let Some(line) = self.editor_mouse_fold_line() else {
            return false;
        };
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        match tab.toggle_fold_at_line(line) {
            Some(true) => {
                self.ensure_editor_cursor_visible();
                self.message = Some(format!("folded line {}", line + 1));
                true
            }
            Some(false) => {
                self.ensure_editor_cursor_visible();
                self.message = Some(format!("unfolded line {}", line + 1));
                true
            }
            None => false,
        }
    }

    fn editor_mouse_bookmark_line(&self) -> Option<usize> {
        let body = self.hit_regions.editor_body?;
        let tab = self.active_tab()?;
        let local_x = self.hit_regions.last_mouse_x.saturating_sub(body.x) as usize;
        if local_x != 0 {
            return None;
        }
        let visible_row =
            tab.scroll + self.hit_regions.last_mouse_y.saturating_sub(body.y) as usize;
        let row = self.editor_visual_row_at(body, visible_row)?;
        (!row.continuation).then_some(row.line)
    }

    fn toggle_editor_bookmark_from_mouse(&mut self) -> bool {
        let Some(line) = self.editor_mouse_bookmark_line() else {
            return false;
        };
        let Some(tab) = self.active_tab_mut() else {
            return false;
        };
        match tab.toggle_bookmark_at_line(line) {
            Some(true) => {
                self.focus = FocusPanel::Editor;
                self.message = Some(format!("bookmarked line {}", line + 1));
                true
            }
            Some(false) => {
                self.focus = FocusPanel::Editor;
                self.message = Some(format!("removed bookmark line {}", line + 1));
                true
            }
            None => false,
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
        let Some((kind, item)) = self.quick_panel.as_ref().and_then(|panel| {
            panel
                .items
                .get(panel.selected)
                .map(|item| (panel.kind.clone(), item.clone()))
        }) else {
            return;
        };

        if kind == QuickPanelKind::Tasks {
            self.quick_panel = None;
            if let Err(error) = self.run_task_item(item) {
                self.last_error = Some(error.to_string());
            }
            return;
        }

        if kind == QuickPanelKind::TerminalCommandHistory {
            self.quick_panel = None;
            if let Err(error) = self.submit_terminal_command(&item.detail) {
                self.last_error = Some(error.to_string());
            }
            return;
        }

        if kind == QuickPanelKind::Completions {
            self.quick_panel = None;
            self.apply_completion_item(item);
            return;
        }

        if kind == QuickPanelKind::CodeActions {
            self.quick_panel = None;
            if let Err(error) = self.apply_code_action_item(item) {
                self.last_error = Some(error.to_string());
            }
            return;
        }

        if matches!(
            kind,
            QuickPanelKind::LspHover | QuickPanelKind::SignatureHelp
        ) {
            self.quick_panel = None;
            self.focus = FocusPanel::Editor;
            self.message = Some(match kind {
                QuickPanelKind::LspHover => "closed LSP hover".to_owned(),
                QuickPanelKind::SignatureHelp => "closed signature help".to_owned(),
                _ => unreachable!(),
            });
            return;
        }

        if kind == QuickPanelKind::Bookmarks {
            self.quick_panel = None;
            self.jump_to_bookmark_item(item);
            return;
        }

        if kind == QuickPanelKind::OpenEditors && item.command.is_none() {
            self.quick_panel = None;
            self.message = Some("no open editors".to_owned());
            return;
        }

        if kind == QuickPanelKind::SourceControl {
            match item.command {
                Some(CommandAction::OpenSourceControlDiff) => {
                    self.quick_panel = None;
                    if let Err(error) = self.open_source_control_diff(&item.path) {
                        self.last_error = Some(error.to_string());
                    }
                    return;
                }
                Some(CommandAction::StageSourceControlItem) => {
                    self.quick_panel = None;
                    if let Err(error) = self.stage_source_control_path(&item.path) {
                        self.last_error = Some(error.to_string());
                    }
                    return;
                }
                Some(CommandAction::UnstageSourceControlItem) => {
                    self.quick_panel = None;
                    if let Err(error) = self.unstage_source_control_path(&item.path) {
                        self.last_error = Some(error.to_string());
                    }
                    return;
                }
                Some(CommandAction::DiscardSourceControlItem) => {
                    self.quick_panel = None;
                    if let Err(error) = self.prompt_discard_source_control_path(&item.path) {
                        self.last_error = Some(error.to_string());
                    }
                    return;
                }
                _ => {}
            }
        }

        if kind == QuickPanelKind::Branches {
            match item.command {
                Some(CommandAction::CreateGitBranch) => {
                    self.quick_panel = None;
                    if let Err(error) = self.prompt_create_git_branch() {
                        self.last_error = Some(error.to_string());
                    }
                    return;
                }
                Some(CommandAction::CheckoutGitBranch) => {
                    self.quick_panel = None;
                    if let Err(error) = self.checkout_git_branch(&item.detail) {
                        self.last_error = Some(error.to_string());
                    }
                    return;
                }
                _ => {}
            }
        }

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
        self.open_quick_item(item, search_query);
    }

    fn open_quick_item(&mut self, item: QuickItem, search_query: Option<String>) {
        if !item.path.is_file() {
            self.message = Some(format!("{} is not available on disk", item.path.display()));
            return;
        }

        self.push_navigation_location_for_jump(&item.path, item.line, item.col);
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

    fn prompt_create_git_branch(&mut self) -> Result<()> {
        let _ = git_top_level(&self.root).context("not a git repository")?;
        self.start_prompt(PromptKind::CreateGitBranch, "");
        self.message = Some("enter new Git branch name".to_owned());
        Ok(())
    }

    fn create_git_branch_from_prompt(&mut self, branch: String) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let branch = branch.trim();
        if branch.is_empty() {
            self.message = Some("create branch cancelled: empty branch name".to_owned());
            return Ok(());
        }
        validate_git_branch_name(&top_level, branch)?;
        if let Some(label) = self.dirty_tabs_under_paths(std::slice::from_ref(&top_level)) {
            self.message = Some(format!(
                "create branch blocked by unsaved editor tab: {label}; save or close it first"
            ));
            return Ok(());
        }
        create_and_checkout_git_branch(&top_level, branch)?;
        self.after_git_branch_switch(format!("created and checked out {branch}"))
    }

    fn checkout_git_branch(&mut self, branch: &str) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let branch = branch.trim();
        if branch.is_empty() {
            self.message = Some("select a Git branch to check out".to_owned());
            return Ok(());
        }
        if git_current_branch(&top_level).as_deref() == Some(branch) {
            self.message = Some(format!("already on {branch}"));
            return Ok(());
        }
        if let Some(label) = self.dirty_tabs_under_paths(std::slice::from_ref(&top_level)) {
            self.message = Some(format!(
                "checkout blocked by unsaved editor tab: {label}; save or close it first"
            ));
            return Ok(());
        }
        checkout_git_branch(&top_level, branch)?;
        self.after_git_branch_switch(format!("checked out {branch}"))
    }

    fn after_git_branch_switch(&mut self, message: String) -> Result<()> {
        self.refresh_explorer_preserving_selection(false)?;
        self.update_workspace_snapshot();
        self.check_external_file_changes();
        self.refresh_git_status();
        self.open_quick_panel(QuickPanelKind::Branches)?;
        self.message = Some(message);
        Ok(())
    }

    fn open_source_control_diff(&mut self, path: &Path) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let relative = relative_path(&top_level, path);
        let text = load_git_path_diff(&self.root, &top_level, path)?;
        if text.trim().is_empty() {
            self.message = Some(format!("no diff for {relative}"));
            return Ok(());
        }

        let tab_path = source_control_diff_tab_path(&top_level, path);
        let title = format!("Diff {relative}");
        self.open_read_only_text_tab(tab_path, title, text);
        self.search_needle = None;
        self.message = Some(format!("opened diff for {relative}"));
        Ok(())
    }

    fn stage_source_control_path(&mut self, path: &Path) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let relative = relative_path(&top_level, path);
        stage_git_path(&top_level, path)?;
        self.reopen_source_control_with_message(format!("staged {relative}"))
    }

    fn unstage_source_control_path(&mut self, path: &Path) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let relative = relative_path(&top_level, path);
        unstage_git_path(&top_level, path)?;
        self.reopen_source_control_with_message(format!("unstaged {relative}"))
    }

    fn stage_all_source_control_changes(&mut self) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        stage_all_git_changes(&top_level)?;
        self.reopen_source_control_with_message("staged all changes".to_owned())
    }

    fn unstage_all_source_control_changes(&mut self) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        unstage_all_git_changes(&top_level)?;
        self.reopen_source_control_with_message("unstaged all changes".to_owned())
    }

    fn prompt_commit_staged_source_control_changes(&mut self) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        if !git_has_staged_changes(&top_level) {
            self.message = Some("no staged Source Control changes to commit".to_owned());
            return Ok(());
        }
        self.start_prompt(PromptKind::CommitStagedSourceControlChanges, "");
        self.message = Some("enter commit message for staged changes".to_owned());
        Ok(())
    }

    fn prompt_commit_all_source_control_changes(&mut self) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let paths = source_control_changed_paths(&self.root, &top_level);
        if paths.is_empty() {
            self.message = Some("no Source Control changes to commit".to_owned());
            return Ok(());
        }
        if let Some(label) = self.dirty_tabs_under_paths(&paths) {
            self.message = Some(format!(
                "commit all blocked by unsaved editor tab: {label}; save or close it first"
            ));
            return Ok(());
        }
        self.start_prompt(PromptKind::CommitAllSourceControlChanges(paths.clone()), "");
        self.message = Some(format!(
            "enter commit message to commit {} Git change(s)",
            paths.len()
        ));
        Ok(())
    }

    fn commit_staged_source_control_changes_confirmed(&mut self, message: String) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let message = message.trim();
        if message.is_empty() {
            self.message = Some("commit cancelled: empty message".to_owned());
            return Ok(());
        }
        if !git_has_staged_changes(&top_level) {
            self.reopen_source_control_with_message(
                "no staged Source Control changes to commit".to_owned(),
            )?;
            return Ok(());
        }

        commit_git_staged_changes(&top_level, message)?;
        self.refresh_explorer_preserving_selection(false)?;
        self.check_external_file_changes();
        self.reopen_source_control_with_message(format!("committed staged changes: {message}"))
    }

    fn commit_all_source_control_changes_confirmed(
        &mut self,
        paths: Vec<PathBuf>,
        message: String,
    ) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let message = message.trim();
        if message.is_empty() {
            self.message = Some("commit cancelled: empty message".to_owned());
            return Ok(());
        }
        let paths = if paths.is_empty() {
            source_control_changed_paths(&self.root, &top_level)
        } else {
            paths
        };
        if paths.is_empty() {
            self.reopen_source_control_with_message(
                "no Source Control changes to commit".to_owned(),
            )?;
            return Ok(());
        }
        if let Some(label) = self.dirty_tabs_under_paths(&paths) {
            self.message = Some(format!(
                "commit all blocked by unsaved editor tab: {label}; save or close it first"
            ));
            return Ok(());
        }

        stage_all_git_changes(&top_level)?;
        if !git_has_staged_changes(&top_level) {
            self.reopen_source_control_with_message(
                "no staged changes after staging all".to_owned(),
            )?;
            return Ok(());
        }
        commit_git_staged_changes(&top_level, message)?;
        self.refresh_explorer_preserving_selection(false)?;
        self.check_external_file_changes();
        self.reopen_source_control_with_message(format!("committed all changes: {message}"))
    }

    fn prompt_discard_source_control_path(&mut self, path: &Path) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let relative = relative_path(&top_level, &path);
        if self
            .dirty_tabs_under_paths(std::slice::from_ref(&path))
            .is_some()
        {
            self.message = Some(format!(
                "discard blocked by unsaved editor tab under {relative}; save or close it first"
            ));
            return Ok(());
        }
        self.start_prompt(PromptKind::DiscardSourceControlPath(path), "");
        self.message = Some(format!("type discard to discard Git changes in {relative}"));
        Ok(())
    }

    fn prompt_discard_all_source_control_changes(&mut self) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let paths = source_control_changed_paths(&self.root, &top_level);
        if paths.is_empty() {
            self.message = Some("no Source Control changes to discard".to_owned());
            return Ok(());
        }
        if let Some(label) = self.dirty_tabs_under_paths(&paths) {
            self.message = Some(format!(
                "discard all blocked by unsaved editor tab: {label}; save or close it first"
            ));
            return Ok(());
        }
        self.start_prompt(
            PromptKind::DiscardAllSourceControlChanges(paths.clone()),
            "",
        );
        self.message = Some(format!(
            "type discard to discard {} Git change(s)",
            paths.len()
        ));
        Ok(())
    }

    fn discard_source_control_path_confirmed(&mut self, path: PathBuf) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        if self
            .dirty_tabs_under_paths(std::slice::from_ref(&path))
            .is_some()
        {
            let relative = relative_path(&top_level, &path);
            self.message = Some(format!(
                "discard blocked by unsaved editor tab under {relative}; save or close it first"
            ));
            return Ok(());
        }

        let entries = load_git_status_entries(&self.root);
        let Some(entry) = entries.into_iter().find(|entry| entry.path == path) else {
            let relative = relative_path(&top_level, &path);
            self.reopen_source_control_with_message(format!("no Git changes for {relative}"))?;
            return Ok(());
        };
        let deleted_by_discard = entry_removes_worktree_path(&entry);
        let relative = relative_path(&top_level, &path);
        discard_git_entry(&top_level, &entry)?;
        if deleted_by_discard {
            self.close_clean_tabs_for_removed_paths(std::slice::from_ref(&path));
        }
        self.refresh_explorer_preserving_selection(false)?;
        self.check_external_file_changes();
        self.reopen_source_control_with_message(format!("discarded {relative}"))
    }

    fn discard_all_source_control_changes_confirmed(&mut self, paths: Vec<PathBuf>) -> Result<()> {
        let top_level = git_top_level(&self.root).context("not a git repository")?;
        let entries = load_git_status_entries(&self.root);
        let paths = if paths.is_empty() {
            source_control_changed_paths(&self.root, &top_level)
        } else {
            paths
        };
        if let Some(label) = self.dirty_tabs_under_paths(&paths) {
            self.message = Some(format!(
                "discard all blocked by unsaved editor tab: {label}; save or close it first"
            ));
            return Ok(());
        }

        let removed_paths = entries
            .iter()
            .filter(|entry| entry_removes_worktree_path(entry))
            .map(|entry| entry.path.clone())
            .collect::<Vec<_>>();
        let mut discarded = 0usize;
        for entry in entries {
            discard_git_entry(&top_level, &entry)?;
            discarded += 1;
        }
        self.close_clean_tabs_for_removed_paths(&removed_paths);
        self.refresh_explorer_preserving_selection(false)?;
        self.check_external_file_changes();
        self.reopen_source_control_with_message(format!("discarded {discarded} Git change(s)"))
    }

    fn dirty_tabs_under_paths(&self, paths: &[PathBuf]) -> Option<String> {
        self.tabs
            .iter()
            .filter(|tab| !tab.untitled && tab.dirty)
            .find(|tab| paths.iter().any(|path| tab.path.starts_with(path)))
            .map(|tab| relative_path(&self.root, &tab.path))
    }

    fn dirty_tab_label(&self) -> Option<String> {
        self.tabs.iter().find(|tab| tab.dirty).map(|tab| {
            if tab.untitled {
                tab.title.clone()
            } else {
                relative_path(&self.root, &tab.path)
            }
        })
    }

    fn close_clean_tabs_for_removed_paths(&mut self, paths: &[PathBuf]) {
        if paths.is_empty() {
            return;
        }
        let active_path = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.path.clone());
        let split_path = self
            .editor_split
            .and_then(|index| self.tabs.get(index))
            .map(|tab| tab.path.clone());
        self.tabs.retain(|tab| {
            tab.untitled
                || tab.dirty
                || !paths
                    .iter()
                    .any(|removed_path| tab.path.starts_with(removed_path))
        });
        self.active_tab = if self.tabs.is_empty() {
            None
        } else if let Some(active_path) = active_path {
            self.tabs
                .iter()
                .position(|tab| tab.path == active_path)
                .or(Some(0))
        } else {
            Some(0)
        };
        self.editor_split =
            split_path.and_then(|path| self.tabs.iter().position(|tab| tab.path == path));
        self.normalize_editor_split();
    }

    fn reopen_source_control_with_message(&mut self, message: String) -> Result<()> {
        self.refresh_git_status();
        self.open_quick_panel(QuickPanelKind::SourceControl)?;
        self.message = Some(message);
        Ok(())
    }

    fn apply_completion_item(&mut self, item: QuickItem) {
        self.lsp_completion_items.clear();
        let Some(state) = self.completion_state.take() else {
            self.message = Some("no active completion request".to_owned());
            return;
        };

        if !self.ensure_active_tab_writable("completion") {
            return;
        }
        let Some(tab) = self.active_tab_mut() else {
            self.message = Some("no active editor for completion".to_owned());
            return;
        };

        if tab.path != state.path || state.line >= tab.lines.len() {
            self.message = Some("completion target is no longer active".to_owned());
            return;
        }

        let changed = tab.replace_range_as_edit(
            (state.line, state.start_col),
            (state.line, state.end_col),
            &item.label,
        );
        self.ensure_editor_cursor_visible();
        self.focus = FocusPanel::Editor;
        self.message = Some(if changed {
            format!("completed {}", item.label)
        } else {
            format!("completion unchanged: {}", item.label)
        });
    }

    fn apply_code_action_item(&mut self, item: QuickItem) -> Result<()> {
        let Some(index) = item.line else {
            self.message = Some("code action is no longer available".to_owned());
            return Ok(());
        };
        let Some(action) = self.lsp_code_actions.get(index).cloned() else {
            self.message = Some("code action is no longer available".to_owned());
            return Ok(());
        };
        self.lsp_code_actions.clear();

        let title = action.title.clone();
        if let Some(edit) = action.edit.clone() {
            match self.apply_lsp_workspace_edit(edit)? {
                Some(summary) => {
                    self.focus = FocusPanel::Editor;
                    self.ensure_editor_cursor_visible();
                    self.message = Some(format!(
                        "applied code action '{title}' via {}: {} edit(s), {} open buffer(s), {} saved file(s)",
                        summary.server, summary.edit_count, summary.open_count, summary.file_count
                    ));
                }
                None => {
                    self.focus = FocusPanel::Editor;
                    self.message = Some(format!(
                        "code action '{title}' produced no applicable edits"
                    ));
                }
            }
            return Ok(());
        }

        if action.command.is_none() {
            self.focus = FocusPanel::Editor;
            self.message = Some(format!(
                "code action '{title}' did not include a workspace edit"
            ));
            return Ok(());
        }

        let command_title = action
            .command_title
            .clone()
            .unwrap_or_else(|| "LSP command".to_owned());
        match self.try_lsp_code_action_command(&action)? {
            Some(summary) => {
                self.focus = FocusPanel::Editor;
                self.ensure_editor_cursor_visible();
                self.message = Some(format!(
                    "executed code action '{title}' via {}: {} edit(s), {} open buffer(s), {} saved file(s)",
                    summary.server, summary.edit_count, summary.open_count, summary.file_count
                ));
            }
            None => {
                self.focus = FocusPanel::Editor;
                self.message = Some(format!(
                    "code action '{title}' command '{command_title}' produced no applicable edits"
                ));
            }
        }
        Ok(())
    }

    fn run_command(&mut self, command: CommandAction) -> Result<()> {
        match command {
            CommandAction::QuickOpen => self.open_quick_panel(QuickPanelKind::OpenFile)?,
            CommandAction::OpenFolder => self.start_open_folder_prompt(),
            CommandAction::ShowExplorerFiles => self.show_files_sidebar(),
            CommandAction::ShowOutline => self.show_outline()?,
            CommandAction::ToggleSidebarMode => self.toggle_sidebar_mode()?,
            CommandAction::TriggerSuggest => self.trigger_suggest()?,
            CommandAction::WorkspaceSearch => {
                self.open_quick_panel(QuickPanelKind::WorkspaceSearch)?
            }
            CommandAction::DocumentSymbols => {
                self.open_quick_panel(QuickPanelKind::DocumentSymbols)?
            }
            CommandAction::WorkspaceSymbols => {
                self.open_quick_panel(QuickPanelKind::WorkspaceSymbols)?
            }
            CommandAction::ShowHover => self.show_lsp_hover_under_cursor()?,
            CommandAction::SignatureHelp => self.show_lsp_signature_help_under_cursor()?,
            CommandAction::GoToDefinition => self.go_to_definition_under_cursor()?,
            CommandAction::GoToTypeDefinition => self.go_to_type_definition_under_cursor()?,
            CommandAction::GoToImplementation => self.go_to_implementation_under_cursor()?,
            CommandAction::GoToMatchingBracket => self.go_to_matching_bracket(),
            CommandAction::ShowIncomingCalls => self.show_lsp_call_hierarchy_under_cursor(
                "incoming call",
                QuickPanelKind::IncomingCalls,
                lsp::incoming_calls,
            )?,
            CommandAction::ShowOutgoingCalls => self.show_lsp_call_hierarchy_under_cursor(
                "outgoing call",
                QuickPanelKind::OutgoingCalls,
                lsp::outgoing_calls,
            )?,
            CommandAction::HighlightSymbol => self.highlight_symbol_under_cursor()?,
            CommandAction::ClearDocumentHighlights => self.clear_document_highlights(),
            CommandAction::FindReferences => self.find_references_under_cursor()?,
            CommandAction::CodeAction => self.run_code_actions()?,
            CommandAction::GoBack => self.go_back(),
            CommandAction::GoForward => self.go_forward(),
            CommandAction::RenameSymbol => self.start_rename_symbol_prompt(),
            CommandAction::WorkspaceReplace => self.start_workspace_replace_prompt(),
            CommandAction::RunWorkspaceCheck => self.run_workspace_check()?,
            CommandAction::RunLspDiagnostics => self.run_lsp_diagnostics()?,
            CommandAction::ShowProblems => self.open_quick_panel(QuickPanelKind::Problems)?,
            CommandAction::ShowBookmarks => self.show_bookmarks()?,
            CommandAction::ToggleBookmark => self.toggle_bookmark_at_cursor(),
            CommandAction::NextBookmark => self.jump_to_relative_bookmark(true),
            CommandAction::PreviousBookmark => self.jump_to_relative_bookmark(false),
            CommandAction::ClearBookmarks => self.clear_active_bookmarks(),
            CommandAction::ShowSourceControl => {
                self.refresh_git_status();
                if git_top_level(&self.root).is_none() {
                    self.message = Some("not a git repository".to_owned());
                }
                self.open_quick_panel(QuickPanelKind::SourceControl)?;
            }
            CommandAction::ShowGitBranches => {
                self.refresh_git_status();
                if git_top_level(&self.root).is_none() {
                    self.message = Some("not a git repository".to_owned());
                }
                self.open_quick_panel(QuickPanelKind::Branches)?;
            }
            CommandAction::CheckoutGitBranch => {
                self.message = Some("select a Git branch to check out".to_owned());
            }
            CommandAction::CreateGitBranch => self.prompt_create_git_branch()?,
            CommandAction::OpenSourceControlDiff => {
                self.message = Some("select a Source Control file row to open its diff".to_owned());
            }
            CommandAction::StageSourceControlItem => {
                self.message =
                    Some("select a Source Control file row to stage that path".to_owned());
            }
            CommandAction::UnstageSourceControlItem => {
                self.message =
                    Some("select a Source Control file row to unstage that path".to_owned());
            }
            CommandAction::StageAllChanges => self.stage_all_source_control_changes()?,
            CommandAction::UnstageAllChanges => self.unstage_all_source_control_changes()?,
            CommandAction::CommitStagedChanges => {
                self.prompt_commit_staged_source_control_changes()?
            }
            CommandAction::CommitAllChanges => self.prompt_commit_all_source_control_changes()?,
            CommandAction::DiscardSourceControlItem => {
                self.message =
                    Some("select a Source Control file row to discard that path".to_owned());
            }
            CommandAction::DiscardAllChanges => self.prompt_discard_all_source_control_changes()?,
            CommandAction::RunTask => self.open_quick_panel(QuickPanelKind::Tasks)?,
            CommandAction::RunActiveFileInTerminal => self.run_active_file_in_terminal()?,
            CommandAction::RunSelectedExplorerFileInTerminal => {
                self.run_selected_explorer_file_in_terminal()?
            }
            CommandAction::NewUntitledFile => self.new_untitled_file(),
            CommandAction::ShowOpenEditors => self.open_quick_panel(QuickPanelKind::OpenEditors)?,
            CommandAction::SelectEditorTab(index) => self.select_editor_tab(index),
            CommandAction::SaveFile => self.save_active_tab(),
            CommandAction::SaveAs => self.start_save_as_prompt(),
            CommandAction::SaveAll => self.save_all_tabs(),
            CommandAction::RevertFile => self.revert_active_tab()?,
            CommandAction::FormatDocument => self.format_active_document()?,
            CommandAction::CloseActiveTab => self.close_active_tab(),
            CommandAction::ReopenClosedEditor => self.reopen_closed_editor(),
            CommandAction::CloseAllTabs => self.close_all_tabs(),
            CommandAction::CloseOtherTabs => self.close_other_tabs(),
            CommandAction::CloseTabsToRight => self.close_tabs_to_right(),
            CommandAction::OpenActiveTabToSide => self.open_active_tab_to_side(),
            CommandAction::OpenSelectedExplorerItemToSide => self.open_selected_to_side()?,
            CommandAction::CloseEditorSplit => self.close_editor_split(),
            CommandAction::SaveAndCloseTab(index) => self.save_and_close_tab(index)?,
            CommandAction::DiscardAndCloseTab(index) => self.discard_and_close_tab(index),
            CommandAction::CancelCloseTab => {
                self.message = Some("close cancelled".to_owned());
            }
            CommandAction::CloseSavedTabs => self.close_saved_tabs(),
            CommandAction::OpenSelectedExplorerItem => self.open_or_toggle_selected()?,
            CommandAction::OpenSelectedFolderAsWorkspace => {
                self.open_selected_folder_as_workspace()?
            }
            CommandAction::NewFile => self.start_new_file_prompt(),
            CommandAction::NewFolder => self.start_new_dir_prompt(),
            CommandAction::RenameSelected => self.prompt_rename(),
            CommandAction::DeleteSelected => self.prompt_delete(),
            CommandAction::CopySelectedExplorerItem => self.copy_selected_path(),
            CommandAction::CutSelectedExplorerItem => self.cut_selected_path(),
            CommandAction::PasteIntoSelectedExplorerItem => self.paste_into_selected()?,
            CommandAction::DuplicateSelectedExplorerItem => self.duplicate_selected()?,
            CommandAction::CompareSelectedFiles => self.compare_selected_files()?,
            CommandAction::RefreshExplorer => self.refresh_explorer()?,
            CommandAction::CollapseExplorer => self.collapse_explorer(),
            CommandAction::CycleExplorerSort => self.cycle_explorer_sort_mode(),
            CommandAction::SortExplorerByName => {
                self.set_explorer_sort_mode(ExplorerSortMode::Name)
            }
            CommandAction::SortExplorerByType => {
                self.set_explorer_sort_mode(ExplorerSortMode::Type)
            }
            CommandAction::SortExplorerByModified => {
                self.set_explorer_sort_mode(ExplorerSortMode::Modified)
            }
            CommandAction::SortExplorerBySize => {
                self.set_explorer_sort_mode(ExplorerSortMode::Size)
            }
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
            CommandAction::AddSelectionToNextMatch => self.add_selection_to_next_match(),
            CommandAction::SelectAllOccurrences => self.select_all_occurrences_in_active_tab(),
            CommandAction::DuplicateLine => self.duplicate_active_line(),
            CommandAction::DeleteLine => self.delete_active_line(),
            CommandAction::MoveLineUp => self.move_active_line_up(),
            CommandAction::MoveLineDown => self.move_active_line_down(),
            CommandAction::ToggleLineComment => self.toggle_active_line_comment(),
            CommandAction::ToggleBlockComment => self.toggle_active_block_comment(),
            CommandAction::ToggleWordWrap => self.toggle_word_wrap(),
            CommandAction::ToggleFold => self.toggle_active_fold(),
            CommandAction::FoldAll => self.fold_all_active_tab(),
            CommandAction::UnfoldAll => self.unfold_all_active_tab(),
            CommandAction::TrimTrailingWhitespace => self.trim_active_trailing_whitespace(),
            CommandAction::IndentLine => self.indent_active_line(),
            CommandAction::OutdentLine => self.outdent_active_line(),
            CommandAction::SelectAll => self.select_all_active_tab(),
            CommandAction::CopySelection => self.copy_editor_selection(),
            CommandAction::CutSelection => self.cut_editor_selection(),
            CommandAction::PasteClipboard => self.paste_editor_clipboard(),
            CommandAction::RunSelectionInTerminal => self.run_selection_in_terminal()?,
            CommandAction::CopyTerminalSelection => self.copy_terminal_selection(),
            CommandAction::CopyTerminalOutput => self.copy_terminal_output(),
            CommandAction::PasteClipboardToTerminal => self.paste_clipboard_to_terminal()?,
            CommandAction::FindInTerminal => self.start_terminal_search_prompt(),
            CommandAction::TerminalSearchNext => self.next_terminal_search_match(),
            CommandAction::TerminalSearchPrevious => self.previous_terminal_search_match(),
            CommandAction::RunTerminalCommand => self.start_terminal_command_prompt(),
            CommandAction::RunRecentTerminalCommand => self.open_terminal_command_history()?,
            CommandAction::FocusExplorer => self.focus = FocusPanel::Explorer,
            CommandAction::FocusEditor => self.focus = FocusPanel::Editor,
            CommandAction::FocusTerminal => self.focus = FocusPanel::Terminal,
            CommandAction::ClearTerminal => {
                self.active_terminal_mut().shell.clear();
                self.message = Some("terminal cleared".to_owned());
            }
            CommandAction::RestartTerminal => self.restart_terminal()?,
            CommandAction::RenameTerminal => self.start_terminal_rename_prompt(),
            CommandAction::NewTerminal => self.new_terminal()?,
            CommandAction::NewTerminalHere => self.new_terminal_here()?,
            CommandAction::SplitTerminal => self.split_terminal()?,
            CommandAction::CloseTerminal => self.close_active_terminal()?,
            CommandAction::NextTerminal => self.next_terminal(),
            CommandAction::PreviousTerminal => self.previous_terminal(),
            CommandAction::ToggleTerminalFocus => self.toggle_terminal_focus(),
            CommandAction::ToggleTerminalMaximized => self.toggle_terminal_maximized(),
            CommandAction::ScrollTerminalToBottom => self.scroll_terminal_to_bottom(),
            CommandAction::IncreaseTerminalHeight => self.resize_terminal_panel(2),
            CommandAction::DecreaseTerminalHeight => self.resize_terminal_panel(-2),
        }
        Ok(())
    }

    fn handle_pending_key_chord(&mut self, chord: PendingKeyChord, key: KeyEvent) -> Result<bool> {
        match chord {
            PendingKeyChord::CtrlK => {
                if matches!(self.focus, FocusPanel::Terminal) {
                    self.message = Some("Ctrl-K chord cancelled".to_owned());
                    return Ok(false);
                }

                match key.code {
                    KeyCode::Char('s') | KeyCode::Char('S')
                        if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                    {
                        self.save_all_tabs();
                        Ok(true)
                    }
                    KeyCode::Esc => {
                        self.message = Some("Ctrl-K chord cancelled".to_owned());
                        Ok(true)
                    }
                    _ => {
                        self.message = Some("Ctrl-K chord cancelled".to_owned());
                        Ok(false)
                    }
                }
            }
        }
    }

    fn send_terminal_mouse_click(&mut self) {
        let Some(body) = self.activate_terminal_under_mouse() else {
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
        let Some(body) = self.activate_terminal_under_mouse() else {
            return Ok(false);
        };
        let row = self.hit_regions.last_mouse_y.saturating_sub(body.y);
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x);
        self.active_terminal_mut()
            .shell
            .send_mouse_wheel(row, col, up)
    }

    fn forward_terminal_mouse_event(
        &mut self,
        kind: MouseEventKind,
        modifiers: KeyModifiers,
    ) -> Result<bool> {
        let Some(body) = self.activate_terminal_under_mouse() else {
            return Ok(false);
        };
        let row = self.hit_regions.last_mouse_y.saturating_sub(body.y);
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x);
        self.active_terminal_mut()
            .shell
            .send_mouse_event(kind, row, col, modifiers)
    }

    fn start_terminal_selection_from_mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(body) = self.activate_terminal_under_mouse() else {
            return false;
        };
        let Some(cell) = terminal_mouse_cell_in_body(mouse, body) else {
            return false;
        };
        self.focus = FocusPanel::Terminal;
        self.terminal_selection = Some(TerminalSelection {
            terminal_id: self.active_terminal().id,
            anchor: cell,
            head: cell,
        });
        true
    }

    fn handle_terminal_selection_mouse(&mut self, mouse: MouseEvent) -> Result<bool> {
        let Some(cell) = self.terminal_mouse_cell(mouse) else {
            return Ok(false);
        };
        let active_terminal_id = self.active_terminal().id;
        let Some(selection) = &mut self.terminal_selection else {
            return Ok(false);
        };
        if selection.terminal_id != active_terminal_id {
            self.terminal_selection = None;
            return Ok(false);
        }

        selection.head = cell;
        self.focus = FocusPanel::Terminal;

        match mouse.kind {
            MouseEventKind::Drag(MouseButton::Left) => Ok(true),
            MouseEventKind::Up(MouseButton::Left) => {
                if self
                    .terminal_selection
                    .as_ref()
                    .is_some_and(|selection| selection.anchor == selection.head)
                {
                    self.terminal_selection = None;
                    let _ = self.open_terminal_reference(cell.0, cell.1);
                } else {
                    self.copy_terminal_selection();
                }
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    fn terminal_mouse_cell(&self, mouse: MouseEvent) -> Option<(u16, u16)> {
        let body = self.active_terminal_body()?;
        terminal_mouse_cell_in_body(mouse, body)
    }

    pub fn terminal_selection_for_active(&self) -> Option<&TerminalSelection> {
        let selection = self.terminal_selection.as_ref()?;
        (selection.terminal_id == self.active_terminal().id).then_some(selection)
    }

    pub fn terminal_selection_columns_for_terminal_row(
        &self,
        terminal_index: usize,
        row: u16,
    ) -> Option<(usize, usize)> {
        let selection = self.terminal_selection.as_ref()?;
        let terminal = self.terminals.get(terminal_index)?;
        if selection.terminal_id != terminal.id {
            return None;
        }
        let (_, cols) = terminal.shell.size();
        terminal_selection_columns(selection.anchor, selection.head, row, cols)
    }

    fn terminal_selected_text(&self) -> Option<String> {
        let selection = self.terminal_selection_for_active()?;
        let (_, cols) = self.active_terminal().shell.size();
        terminal_selected_text_from_screen(selection.anchor, selection.head, cols, |row| {
            self.active_terminal().shell.row_text(row)
        })
    }

    fn copy_terminal_selection(&mut self) {
        let Some(text) = self.terminal_selected_text() else {
            self.message = Some("no terminal selection to copy".to_owned());
            return;
        };
        let count = text.chars().count();
        self.editor_clipboard = Some(text.clone());
        if self.queue_clipboard_export(&text) {
            self.message = Some(format!("copied {count} terminal char(s)"));
        } else {
            self.message = Some(format!(
                "copied {count} terminal char(s) internally; selection too large for terminal clipboard"
            ));
        }
    }

    fn copy_terminal_output(&mut self) {
        let text = self.active_terminal_mut().shell.all_text();
        if text.is_empty() {
            self.message = Some("terminal output empty".to_owned());
            return;
        }
        let count = text.chars().count();
        self.editor_clipboard = Some(text.clone());
        if self.queue_clipboard_export(&text) {
            self.message = Some(format!("copied {count} terminal output char(s)"));
        } else {
            self.message = Some(format!(
                "copied {count} terminal output char(s) internally; output too large for terminal clipboard"
            ));
        }
    }

    fn scroll_terminal_to_bottom(&mut self) {
        self.active_terminal_mut().shell.scroll_to_bottom();
        self.focus = FocusPanel::Terminal;
        self.message = Some("terminal scrolled to bottom".to_owned());
    }

    fn paste_clipboard_to_terminal(&mut self) -> Result<()> {
        let Some(text) = self.editor_clipboard.clone() else {
            self.message = Some("clipboard empty".to_owned());
            return Ok(());
        };
        self.active_terminal_mut().shell.send_paste(&text)?;
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!(
            "pasted {} char(s) to terminal",
            text.chars().count()
        ));
        Ok(())
    }

    fn start_terminal_command_prompt(&mut self) {
        self.start_prompt(PromptKind::RunTerminalCommand, "");
        self.message = Some("enter a shell command for the active terminal".to_owned());
    }

    fn run_terminal_command_from_prompt(&mut self, input: String) -> Result<()> {
        let command = input.trim();
        if command.is_empty() {
            self.message = Some("terminal command cancelled".to_owned());
            return Ok(());
        }
        self.submit_terminal_command(command)
    }

    fn submit_terminal_command(&mut self, command: &str) -> Result<()> {
        let command = command.trim();
        if command.is_empty() {
            self.message = Some("terminal command is empty".to_owned());
            return Ok(());
        }
        self.record_terminal_command(command);
        let submitted = terminal_submission_text(command);
        self.active_terminal_mut().shell.send_text(&submitted)?;
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!(
            "sent terminal command: {}",
            truncate_chars(command, 80)
        ));
        Ok(())
    }

    fn record_terminal_command(&mut self, command: &str) {
        let command = normalize_terminal_history_command(command);
        if command.is_empty() {
            return;
        }
        self.terminal_command_history
            .retain(|existing| existing != &command);
        self.terminal_command_history.insert(0, command);
        self.terminal_command_history
            .truncate(MAX_TERMINAL_COMMAND_HISTORY);
    }

    fn open_terminal_command_history(&mut self) -> Result<()> {
        if self.terminal_command_history.is_empty() {
            self.message = Some("no recent terminal commands yet".to_owned());
            return Ok(());
        }
        self.open_quick_panel(QuickPanelKind::TerminalCommandHistory)
    }

    fn start_terminal_search_prompt(&mut self) {
        let active_id = self.active_terminal().id;
        let initial = self
            .terminal_search
            .as_ref()
            .filter(|search| search.terminal_id == active_id)
            .map(|search| search.needle.clone())
            .unwrap_or_default();
        self.start_prompt(PromptKind::TerminalSearch, &initial);
    }

    fn terminal_search_from_prompt(&mut self, input: String) {
        let needle = input.trim().to_owned();
        self.focus = FocusPanel::Terminal;
        self.terminal_selection = None;
        if needle.is_empty() {
            self.terminal_search = None;
            self.message = Some("terminal search cleared".to_owned());
            return;
        }

        let terminal_id = self.active_terminal().id;
        let matches = self.active_terminal_mut().shell.search_matches(&needle);
        if matches.is_empty() {
            self.terminal_search = None;
            self.message = Some(format!("terminal search: no matches for {needle:?}"));
            return;
        }

        let selected = matches.len().saturating_sub(1);
        self.terminal_search = Some(TerminalSearchState {
            terminal_id,
            needle,
            matches,
            selected,
        });
        self.jump_to_terminal_search_selected();
    }

    fn next_terminal_search_match(&mut self) {
        let active_id = self.active_terminal().id;
        let moved = if let Some(search) = &mut self.terminal_search {
            if search.terminal_id == active_id && !search.matches.is_empty() {
                search.selected = (search.selected + 1) % search.matches.len();
                true
            } else {
                false
            }
        } else {
            false
        };

        if moved {
            self.jump_to_terminal_search_selected();
        } else {
            self.message = Some("no active terminal search".to_owned());
        }
    }

    fn previous_terminal_search_match(&mut self) {
        let active_id = self.active_terminal().id;
        let moved = if let Some(search) = &mut self.terminal_search {
            if search.terminal_id == active_id && !search.matches.is_empty() {
                search.selected =
                    (search.selected + search.matches.len() - 1) % search.matches.len();
                true
            } else {
                false
            }
        } else {
            false
        };

        if moved {
            self.jump_to_terminal_search_selected();
        } else {
            self.message = Some("no active terminal search".to_owned());
        }
    }

    fn jump_to_terminal_search_selected(&mut self) {
        let Some((row, selected, count, needle)) =
            self.terminal_search.as_ref().and_then(|search| {
                let item = search.matches.get(search.selected)?;
                Some((
                    item.row,
                    search.selected,
                    search.matches.len(),
                    search.needle.clone(),
                ))
            })
        else {
            return;
        };
        let height = self.terminal_height.max(1);
        let top = row.saturating_sub(height / 2);
        self.active_terminal_mut().shell.scroll_to_global_row(top);
        self.focus = FocusPanel::Terminal;
        self.terminal_selection = None;
        self.message = Some(format!(
            "terminal find {}/{} for {:?}",
            selected + 1,
            count,
            needle
        ));
    }

    pub fn active_terminal_search_summary(&self) -> Option<(usize, usize)> {
        let search = self.terminal_search.as_ref()?;
        (search.terminal_id == self.active_terminal().id && !search.matches.is_empty())
            .then_some((search.selected + 1, search.matches.len()))
    }

    pub fn terminal_search_ranges_for_terminal_row(
        &mut self,
        terminal_index: usize,
        row: u16,
    ) -> Vec<(usize, usize, bool)> {
        if terminal_index >= self.terminals.len() {
            return Vec::new();
        }
        let active_id = self.terminals[terminal_index].id;
        let visible_top = self.terminals[terminal_index].shell.visible_top_row();
        let Some(search) = self.terminal_search.as_ref() else {
            return Vec::new();
        };
        if search.terminal_id != active_id {
            return Vec::new();
        }
        let global_row = visible_top + row as usize;
        search
            .matches
            .iter()
            .enumerate()
            .filter(|(_, item)| item.row == global_row)
            .map(|(index, item)| (item.start, item.end, index == search.selected))
            .collect()
    }

    fn open_terminal_reference(&mut self, row: u16, col: u16) -> bool {
        let Some(line) = self.active_terminal().shell.row_text(row) else {
            return false;
        };
        let Some(candidate) = terminal_link_candidate_at(&line, col as usize, &self.root) else {
            return false;
        };

        match candidate.link {
            TerminalLink::File(reference) => {
                self.push_navigation_location_for_jump(
                    &reference.path,
                    reference.line,
                    reference.col,
                );
                self.open_file(&reference.path);
                if let Some(line) = reference.line
                    && let Some(tab) = self.active_tab_mut()
                {
                    tab.set_cursor(line, reference.col.unwrap_or(0));
                    self.ensure_editor_cursor_visible();
                }
                self.message = Some(format!("opened {}", reference.path.display()));
            }
            TerminalLink::Url(url) => {
                let count = url.chars().count();
                self.editor_clipboard = Some(url.clone());
                if self.queue_clipboard_export(&url) {
                    self.message = Some(format!("copied terminal URL ({count} char(s))"));
                } else {
                    self.message = Some(
                        "copied terminal URL internally; URL too large for terminal clipboard"
                            .to_owned(),
                    );
                }
            }
        }
        true
    }

    pub fn terminal_link_ranges_for_terminal_row(
        &self,
        terminal_index: usize,
        row: u16,
    ) -> Vec<(usize, usize)> {
        let Some(terminal) = self.terminals.get(terminal_index) else {
            return Vec::new();
        };
        if terminal.shell.mouse_protocol_mode() != vt100::MouseProtocolMode::None {
            return Vec::new();
        }
        let Some((body, _)) = self
            .hit_regions
            .terminal_bodies
            .iter()
            .find(|(body, index)| {
                *index == terminal_index
                    && contains(
                        *body,
                        self.hit_regions.last_mouse_x,
                        self.hit_regions.last_mouse_y,
                    )
            })
            .copied()
        else {
            return Vec::new();
        };
        if self.hit_regions.last_mouse_y.saturating_sub(body.y) != row {
            return Vec::new();
        }
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x) as usize;
        let Some(line) = terminal.shell.row_text(row) else {
            return Vec::new();
        };
        terminal_link_candidate_at(&line, col, &self.root)
            .map(|candidate| vec![(candidate.start, candidate.end)])
            .unwrap_or_default()
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

    fn select_editor_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            self.message = Some("editor tab is no longer open".to_owned());
            return;
        };
        let title = tab.title.clone();
        self.active_tab = Some(index);
        self.focus = FocusPanel::Editor;
        self.normalize_editor_split();
        self.ensure_editor_cursor_visible();
        self.message = Some(format!("editor: {title}"));
    }

    fn push_closed_tab(&mut self, tab: EditorTab) {
        self.closed_tabs.push(tab);
        if self.closed_tabs.len() > MAX_CLOSED_TABS {
            let overflow = self.closed_tabs.len() - MAX_CLOSED_TABS;
            self.closed_tabs.drain(0..overflow);
        }
    }

    fn prepare_closed_tab_for_reopen(&self, tab: EditorTab) -> EditorTab {
        if !tab.untitled
            && !tab.dirty
            && tab.path.is_file()
            && let Ok(mut fresh) = EditorTab::open(tab.path.clone())
        {
            fresh.apply_view_state_from(&tab);
            return fresh;
        }
        tab
    }

    fn reopen_closed_editor(&mut self) {
        let Some(tab) = self.closed_tabs.pop() else {
            self.message = Some("no closed editor to reopen".to_owned());
            return;
        };
        if let Some(index) = self
            .tabs
            .iter()
            .position(|open_tab| open_tab.path == tab.path && open_tab.untitled == tab.untitled)
        {
            self.active_tab = Some(index);
            self.focus = FocusPanel::Editor;
            self.normalize_editor_split();
            self.message = Some(format!("{} is already open", self.tabs[index].title));
            return;
        }

        let tab = self.prepare_closed_tab_for_reopen(tab);
        let title = tab.title.clone();
        self.tabs.push(tab);
        self.active_tab = Some(self.tabs.len() - 1);
        self.focus = FocusPanel::Editor;
        self.normalize_editor_split();
        self.ensure_editor_cursor_visible();
        self.message = Some(format!("reopened {title}"));
    }

    fn close_active_tab(&mut self) {
        if let Some(index) = self.active_tab {
            self.close_tab(index);
        }
    }

    fn tab_identity(tab: &EditorTab) -> (PathBuf, bool) {
        (tab.path.clone(), tab.untitled)
    }

    fn find_tab_identity(&self, identity: &(PathBuf, bool)) -> Option<usize> {
        self.tabs
            .iter()
            .position(|tab| tab.path == identity.0 && tab.untitled == identity.1)
    }

    fn close_clean_tabs_at_indices(&mut self, mut indices: Vec<usize>) -> (usize, usize) {
        indices.sort_unstable();
        indices.dedup();

        let active_identity = self
            .active_tab
            .and_then(|index| self.tabs.get(index))
            .map(Self::tab_identity);
        let split_identity = self
            .editor_split
            .and_then(|index| self.tabs.get(index))
            .map(Self::tab_identity);

        let mut removed_tabs = Vec::new();
        let mut dirty_kept = 0;
        for index in indices.into_iter().rev() {
            if index >= self.tabs.len() {
                continue;
            }
            if self.tabs[index].dirty {
                dirty_kept += 1;
            } else {
                removed_tabs.push(self.tabs.remove(index));
            }
        }

        let closed = removed_tabs.len();
        for tab in removed_tabs.into_iter().rev() {
            self.push_closed_tab(tab);
        }

        self.active_tab = active_identity
            .and_then(|identity| self.find_tab_identity(&identity))
            .or_else(|| (!self.tabs.is_empty()).then_some(0));
        self.editor_split = split_identity.and_then(|identity| self.find_tab_identity(&identity));
        self.normalize_editor_split();
        if self.active_tab.is_some() {
            self.ensure_editor_cursor_visible();
        }
        self.focus = FocusPanel::Editor;

        (closed, dirty_kept)
    }

    fn set_batch_close_message(
        &mut self,
        closed: usize,
        dirty_kept: usize,
        closed_label: &str,
        empty_message: &str,
    ) {
        self.message = if closed == 0 && dirty_kept == 0 {
            Some(empty_message.to_owned())
        } else if dirty_kept > 0 {
            Some(format!(
                "closed {closed} {closed_label}; kept {dirty_kept} dirty tab(s) open"
            ))
        } else {
            Some(format!("closed {closed} {closed_label}"))
        };
    }

    fn close_all_tabs(&mut self) {
        let indices = (0..self.tabs.len()).collect();
        let (closed, dirty_kept) = self.close_clean_tabs_at_indices(indices);
        self.set_batch_close_message(
            closed,
            dirty_kept,
            "clean editor tab(s)",
            "no editor tabs to close",
        );
    }

    fn close_other_tabs(&mut self) {
        let Some(active) = self.active_tab.and_then(|index| self.tabs.get(index)) else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        let active_identity = Self::tab_identity(active);
        let indices = self
            .tabs
            .iter()
            .enumerate()
            .filter_map(|(index, tab)| {
                (Self::tab_identity(tab) != active_identity).then_some(index)
            })
            .collect();
        let (closed, dirty_kept) = self.close_clean_tabs_at_indices(indices);
        self.set_batch_close_message(
            closed,
            dirty_kept,
            "other clean editor tab(s)",
            "no other editor tabs to close",
        );
    }

    fn close_tabs_to_right(&mut self) {
        let Some(active) = self.active_tab else {
            self.message = Some("no active editor tab".to_owned());
            return;
        };
        let indices = (active + 1..self.tabs.len()).collect();
        let (closed, dirty_kept) = self.close_clean_tabs_at_indices(indices);
        self.set_batch_close_message(
            closed,
            dirty_kept,
            "clean editor tab(s) to the right",
            "no editor tabs to the right",
        );
    }

    fn close_saved_tabs(&mut self) {
        let indices = (0..self.tabs.len()).collect();
        let (closed, dirty_kept) = self.close_clean_tabs_at_indices(indices);
        self.set_batch_close_message(
            closed,
            dirty_kept,
            "saved editor tab(s)",
            "no saved editor tabs to close",
        );
    }

    fn close_tab(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if self.tabs[index].dirty {
            if let Err(error) = self.open_quick_panel(QuickPanelKind::DirtyClose { index }) {
                self.last_error = Some(error.to_string());
            }
            return;
        }

        self.close_tab_without_prompt(index, "closed");
    }

    fn close_tab_without_prompt(&mut self, index: usize, verb: &str) {
        if index >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(index);
        let title = tab.title.clone();
        self.push_closed_tab(tab);
        self.active_tab = if self.tabs.is_empty() {
            None
        } else {
            Some(index.saturating_sub(1).min(self.tabs.len() - 1))
        };
        self.adjust_editor_split_after_tab_removed(index);
        self.message = Some(format!("{verb} {title}"));
    }

    fn save_and_close_tab(&mut self, index: usize) -> Result<()> {
        if index >= self.tabs.len() {
            self.message = Some("tab to close is no longer open".to_owned());
            return Ok(());
        }

        self.active_tab = Some(index);
        if self.tabs[index].untitled {
            let initial = relative_path(&self.root, &self.tabs[index].path);
            self.start_prompt(PromptKind::SaveAsClose { index }, &initial);
            self.message = Some(format!(
                "{} needs Save As before closing",
                self.tabs[index].title
            ));
            return Ok(());
        }

        self.check_external_file_changes();
        if index >= self.tabs.len() {
            self.message = Some("tab to close is no longer open".to_owned());
            return Ok(());
        }
        if !self.tabs[index].external_state.is_clean() {
            self.message = Some(format!(
                "{} {}; use Revert File or Save As before closing",
                self.tabs[index].title,
                self.tabs[index].external_state.label()
            ));
            return Ok(());
        }

        let path = self.tabs[index].path.clone();
        self.tabs[index].save()?;
        self.refresh_git_status();
        self.close_tab_without_prompt(index, "saved and closed");
        self.message = Some(format!("saved and closed {}", path.display()));
        Ok(())
    }

    fn discard_and_close_tab(&mut self, index: usize) {
        self.close_tab_without_prompt(index, "discarded changes and closed");
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
        let Some(path) = self.active_tab().map(|tab| tab.path.clone()) else {
            return;
        };
        self.push_navigation_location_for_jump(&path, Some(line), Some(col));
        if let Some(tab) = self.active_tab_mut() {
            tab.set_cursor(line, col);
            self.ensure_editor_cursor_visible();
            self.focus = FocusPanel::Editor;
            self.message = Some(format!("jumped to line {}", line + 1));
        }
    }

    fn restart_terminal(&mut self) -> Result<()> {
        let title = self.active_terminal().title.clone();
        let title_locked = self.active_terminal().title_locked;
        let id = self.active_terminal().id;
        let cwd = self.active_terminal().cwd.clone();
        let _ = self.active_terminal_mut().shell.kill();
        self.terminal_selection = None;
        self.terminals[self.active_terminal] = TerminalSession {
            id,
            title: title.clone(),
            title_locked,
            shell: ShellPanel::new(cwd.clone())?,
            cwd,
            exited: false,
            exit_status: None,
        };
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal restarted: {title}"));
        Ok(())
    }

    fn start_terminal_rename_prompt(&mut self) {
        let title = self.active_terminal().title.clone();
        self.start_prompt(PromptKind::RenameTerminal, &title);
    }

    fn rename_terminal_from_prompt(&mut self, input: String) {
        let title = input.trim();
        if title.is_empty() {
            self.message = Some("terminal rename requires a title".to_owned());
            return;
        }

        let old_title = self.active_terminal().title.clone();
        let terminal = self.active_terminal_mut();
        terminal.title = title.to_owned();
        terminal.title_locked = true;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("renamed terminal: {old_title} -> {title}"));
    }

    fn new_terminal(&mut self) -> Result<()> {
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        let terminal = TerminalSession::new(id, self.root.clone())?;
        let title = terminal.title.clone();
        self.terminals.push(terminal);
        self.set_active_terminal(self.terminals.len() - 1);
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("new terminal: {title}"));
        Ok(())
    }

    fn new_terminal_here(&mut self) -> Result<()> {
        let cwd = self
            .selected_base_dir()
            .canonicalize()
            .unwrap_or_else(|_| self.selected_base_dir());
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        let terminal = TerminalSession::here(id, cwd.clone())?;
        let title = terminal.title.clone();
        self.terminals.push(terminal);
        self.set_active_terminal(self.terminals.len() - 1);
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("new terminal in {}: {title}", cwd.display()));
        Ok(())
    }

    fn split_terminal(&mut self) -> Result<()> {
        let partner = self.active_terminal;
        let cwd = self.active_terminal().cwd.clone();
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        let terminal = TerminalSession::here(id, cwd.clone())?;
        let title = terminal.title.clone();
        self.terminals.push(terminal);
        self.split_terminal = Some(partner);
        self.set_active_terminal(self.terminals.len() - 1);
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("split terminal in {}: {title}", cwd.display()));
        Ok(())
    }

    fn select_terminal(&mut self, index: usize) {
        if index >= self.terminals.len() {
            return;
        }
        self.set_active_terminal(index);
        self.terminal_selection = None;
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
        self.split_terminal = self.split_terminal.and_then(|split| {
            if split == index {
                None
            } else if split > index {
                Some(split - 1)
            } else {
                Some(split)
            }
        });
        self.normalize_terminal_split();
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("closed terminal: {title}"));
        Ok(())
    }

    fn next_terminal(&mut self) {
        if self.terminals.is_empty() {
            return;
        }
        self.set_active_terminal((self.active_terminal + 1) % self.terminals.len());
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal: {}", self.active_terminal().title));
    }

    fn previous_terminal(&mut self) {
        if self.terminals.is_empty() {
            return;
        }
        self.set_active_terminal(
            (self.active_terminal + self.terminals.len() - 1) % self.terminals.len(),
        );
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal: {}", self.active_terminal().title));
    }

    fn kill_terminal_sessions(&mut self) {
        for terminal in &mut self.terminals {
            let _ = terminal.shell.kill();
        }
    }

    #[cfg(test)]
    fn kill_all_terminals(&mut self) {
        self.kill_terminal_sessions();
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

fn split_editor_text(text: &str) -> (Vec<String>, bool) {
    let trailing_newline = text.ends_with('\n');
    let mut lines = text.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    (lines, trailing_newline)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileOpenGuard {
    Binary,
    InvalidUtf8,
    TooLarge(u64),
}

fn read_file_prefix(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let mut file = fs::File::open(path)?;
    let mut bytes = vec![0; limit];
    let read = file.read(&mut bytes)?;
    bytes.truncate(read);
    Ok(bytes)
}

fn guarded_file_preview(path: &Path, guard: FileOpenGuard, bytes: &[u8]) -> String {
    let reason = match guard {
        FileOpenGuard::Binary => "binary data was detected",
        FileOpenGuard::InvalidUtf8 => "file is not valid UTF-8 text",
        FileOpenGuard::TooLarge(_) => "file is larger than the editable safety limit",
    };
    let total = match guard {
        FileOpenGuard::TooLarge(total) => total,
        FileOpenGuard::Binary | FileOpenGuard::InvalidUtf8 => bytes.len() as u64,
    };

    let mut lines = vec![
        "Read-only file preview".to_owned(),
        format!("Path: {}", path.display()),
        format!("Reason: {reason}"),
        format!("Size: {total} bytes"),
        String::new(),
        "This tab is protected so Save, edit, replace, rename, and workspace search cannot rewrite the original bytes.".to_owned(),
        format!(
            "Showing the first {} byte(s) as hex/ascii.",
            bytes.len().min(READ_ONLY_PREVIEW_BYTES)
        ),
        String::new(),
    ];
    lines.extend(hex_preview_lines(bytes));
    if total > bytes.len() as u64 {
        lines.push(format!(
            "... {} byte(s) not shown",
            total - bytes.len() as u64
        ));
    }
    lines.join("\n")
}

fn hex_preview_lines(bytes: &[u8]) -> Vec<String> {
    if bytes.is_empty() {
        return vec!["<empty>".to_owned()];
    }

    bytes
        .chunks(16)
        .enumerate()
        .map(|(index, chunk)| {
            let offset = index * 16;
            let mut line = format!("{offset:08x}  ");
            for byte in chunk {
                line.push_str(&format!("{byte:02x} "));
            }
            for _ in chunk.len()..16 {
                line.push_str("   ");
            }
            line.push(' ');
            for byte in chunk {
                let ch = if (0x20..=0x7e).contains(byte) {
                    *byte as char
                } else {
                    '.'
                };
                line.push(ch);
            }
            line
        })
        .collect()
}

fn file_stamp(path: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(path).ok()?;
    Some(FileStamp {
        len: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

fn read_text_lossy(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn canonical_existing_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn apply_lsp_text_edits_to_text(text: &str, edits: &[lsp::LspTextEdit]) -> Option<(String, usize)> {
    let mut replacements = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = byte_offset_for_lsp_position(text, edit.start_line, edit.start_utf16_col)?;
        let end = byte_offset_for_lsp_position(text, edit.end_line, edit.end_utf16_col)?;
        if start > end {
            return None;
        }
        replacements.push(TextReplacement {
            start,
            end,
            new_text: edit.new_text.clone(),
        });
    }
    replacements.sort_by_key(|replacement| replacement.start);
    let mut previous_end = 0usize;
    for replacement in &replacements {
        if replacement.start < previous_end {
            return None;
        }
        previous_end = replacement.end;
    }

    let mut updated = text.to_owned();
    for replacement in replacements.iter().rev() {
        updated.replace_range(replacement.start..replacement.end, &replacement.new_text);
    }
    let count = replacements.len();
    Some((updated, count))
}

fn byte_offset_for_lsp_position(text: &str, line: usize, utf16_col: usize) -> Option<usize> {
    let line_start = line_start_offsets(text).get(line).copied()?;
    let line_end = text[line_start..]
        .find('\n')
        .map(|offset| line_start + offset)
        .unwrap_or(text.len());
    let line_text = &text[line_start..line_end];
    let mut utf16_seen = 0usize;
    for (byte_index, ch) in line_text.char_indices() {
        if utf16_seen == utf16_col {
            return Some(line_start + byte_index);
        }
        let next = utf16_seen + ch.len_utf16();
        if next > utf16_col {
            return Some(line_start + byte_index);
        }
        utf16_seen = next;
    }
    (utf16_seen == utf16_col).then_some(line_end)
}

fn line_start_offsets(text: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, ch) in text.char_indices() {
        if ch == '\n' {
            starts.push(index + 1);
        }
    }
    starts
}

fn terminal_submission_text(text: &str) -> String {
    let mut normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized.replace('\n', "\r")
}

fn normalize_terminal_history_command(command: &str) -> String {
    command
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileRunCommand {
    command: String,
    cwd: PathBuf,
}

fn run_command_for_file(root: &Path, path: &Path) -> Option<FileRunCommand> {
    if !path.is_file() {
        return None;
    }

    let parent = path.parent().unwrap_or(root).to_path_buf();
    let file_name = path.file_name()?.to_string_lossy().to_string();
    let file_arg = shell_escape_task_arg(&file_name);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    let command = match extension.as_str() {
        "sh" => format!("sh {file_arg}"),
        "bash" => format!("bash {file_arg}"),
        "zsh" => format!("zsh {file_arg}"),
        "fish" => format!("fish {file_arg}"),
        "py" => {
            let runner = if root.join("uv.lock").is_file() {
                "uv run python"
            } else {
                "python3"
            };
            format!("{runner} {file_arg}")
        }
        "js" | "mjs" | "cjs" => format!("node {file_arg}"),
        "ts" | "tsx" => format!("npx tsx {file_arg}"),
        "rb" => format!("ruby {file_arg}"),
        "php" => format!("php {file_arg}"),
        "pl" | "pm" => format!("perl {file_arg}"),
        "lua" => format!("lua {file_arg}"),
        "go" => format!("go run {file_arg}"),
        "java" => format!("java {file_arg}"),
        "swift" => format!("swift {file_arg}"),
        "ps1" => format!("pwsh -File {file_arg}"),
        "bat" | "cmd" => file_arg,
        _ if is_executable_file(path) => format!("./{file_arg}"),
        _ => return None,
    };

    Some(FileRunCommand {
        command,
        cwd: parent,
    })
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    fs::metadata(path)
        .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "exe" | "com" | "bat" | "cmd"
            )
        })
}

fn take_chars_owned(s: &str, count: usize) -> String {
    s.chars().take(count).collect()
}

fn truncate_chars(s: &str, count: usize) -> String {
    let mut text = take_chars_owned(s, count);
    if s.chars().count() > count {
        text.push_str("...");
    }
    text
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

fn occurrence_ranges(lines: &[String], needle: &str, whole_word: bool) -> Vec<EditorSelection> {
    if needle.is_empty() || needle.contains('\n') {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    for (line_index, line) in lines.iter().enumerate() {
        for (start, end) in line_occurrence_ranges(line, needle, whole_word) {
            ranges.push(EditorSelection {
                start: (line_index, start),
                end: (line_index, end),
            });
        }
    }
    ranges
}

fn find_next_occurrence_after(
    lines: &[String],
    needle: &str,
    after: (usize, usize),
    whole_word: bool,
    excluded: &[EditorSelection],
) -> Option<EditorSelection> {
    let ranges = occurrence_ranges(lines, needle, whole_word);
    ranges
        .iter()
        .copied()
        .find(|range| range.start >= after && !excluded.contains(range))
        .or_else(|| {
            ranges
                .iter()
                .copied()
                .find(|range| !excluded.contains(range))
        })
}

fn line_occurrence_ranges(line: &str, needle: &str, whole_word: bool) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut start_byte = 0usize;
    while start_byte <= line.len() {
        let Some(found) = line[start_byte..].find(needle) else {
            break;
        };
        let byte = start_byte + found;
        let end_byte = byte + needle.len();
        if !whole_word || has_identifier_boundaries(line, byte, needle.len()) {
            ranges.push((
                line[..byte].chars().count(),
                line[..end_byte].chars().count(),
            ));
        }
        start_byte = end_byte;
    }
    ranges
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
    let mut files = workspace_walk_paths(root, show_hidden, show_ignored)?
        .into_iter()
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    files.sort();
    Ok(files)
}

fn collect_workspace_paths(
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<Vec<PathBuf>> {
    let mut paths = workspace_walk_paths(root, show_hidden, show_ignored)?;
    paths.retain(|path| path != root);
    paths.sort();
    Ok(paths)
}

fn workspace_snapshot(
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<WorkspaceSnapshot> {
    let mut snapshot = Vec::new();
    for path in workspace_walk_paths(root, show_hidden, show_ignored)? {
        let metadata = fs::metadata(&path)?;
        snapshot.push(WorkspaceEntryStamp {
            path,
            is_dir: metadata.is_dir(),
            len: metadata.len(),
            modified: metadata.modified().ok(),
            readonly: metadata.permissions().readonly(),
        });
    }
    snapshot.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(snapshot)
}

fn workspace_visible_paths(
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<HashSet<PathBuf>> {
    Ok(workspace_walk_paths(root, show_hidden, show_ignored)?
        .into_iter()
        .collect())
}

fn workspace_walk_paths(
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<Vec<PathBuf>> {
    let mut builder = WalkBuilder::new(root);
    builder
        .hidden(!show_hidden)
        .ignore(!show_ignored)
        .git_ignore(!show_ignored)
        .git_global(!show_ignored)
        .git_exclude(!show_ignored)
        .parents(!show_ignored)
        .require_git(false);

    let root = root.to_path_buf();
    builder.filter_entry(move |entry| {
        let path = entry.path();
        if path == root {
            return true;
        }
        if !show_hidden && path_has_hidden_component(&root, path) {
            return false;
        }
        if !show_ignored && path_has_generated_component(&root, path) {
            return false;
        }
        true
    });

    let mut paths = Vec::new();
    for entry in builder.build() {
        let entry = entry?;
        let path = entry.path();
        let Some(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() || file_type.is_file() {
            paths.push(path.to_path_buf());
        }
    }
    Ok(paths)
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

fn terminal_cwd_detail(cwd: &Path, root: &Path) -> String {
    if cwd == root {
        ".".to_owned()
    } else {
        relative_path(root, cwd)
    }
}

fn resolve_prompt_path(root: &Path, input: &str) -> Option<PathBuf> {
    let input = input.trim();
    if input.is_empty() {
        return None;
    }
    let path = PathBuf::from(input);
    Some(if path.is_absolute() {
        path
    } else {
        root.join(path)
    })
}

fn path_mentions_directory(path: &Path) -> bool {
    path.components()
        .filter(|component| matches!(component, std::path::Component::Normal(_)))
        .count()
        > 1
}

fn is_simple_file_name(input: &str) -> bool {
    let mut components = Path::new(input).components();
    matches!(components.next(), Some(std::path::Component::Normal(_)))
        && components.next().is_none()
}

fn load_git_status(root: &Path) -> (HashMap<PathBuf, GitStatusKind>, HashSet<PathBuf>) {
    let entries = load_git_status_entries(root);
    let statuses = entries
        .into_iter()
        .map(|entry| (entry.path, entry.kind))
        .collect::<HashMap<_, _>>();
    let dirty_dirs = git_dirty_directories(&statuses, root);
    (statuses, dirty_dirs)
}

fn load_git_status_entries(root: &Path) -> Vec<GitStatusEntry> {
    let Some(top_level) = git_top_level(root) else {
        return Vec::new();
    };

    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    parse_git_status_entries_z(&output.stdout, &top_level)
}

fn git_top_level(root: &Path) -> Option<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if path.is_empty() {
        return None;
    }
    let path = PathBuf::from(path);
    Some(path.canonicalize().unwrap_or(path))
}

fn git_current_branch(top_level: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(top_level)
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    if output.status.success() {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        if !branch.is_empty() {
            return Some(branch);
        }
    }

    let output = Command::new("git")
        .arg("-C")
        .arg(top_level)
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let head = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!head.is_empty()).then(|| format!("HEAD@{head}"))
}

fn git_local_branches(top_level: &Path) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(top_level)
        .args(["for-each-ref", "--format=%(refname:short)", "refs/heads"])
        .output()
        .context("failed to list Git branches")?;
    if !output.status.success() {
        return Err(anyhow!(
            "git for-each-ref failed: {}",
            git_output_message(&output)
        ));
    }
    let mut branches = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    branches.sort();
    branches.dedup();
    Ok(branches)
}

fn validate_git_branch_name(top_level: &Path, branch: &str) -> Result<()> {
    if branch.starts_with('-') || branch.chars().any(char::is_control) {
        return Err(anyhow!("invalid Git branch name: {branch}"));
    }
    run_git_command(top_level, &["check-ref-format", "--branch", branch])
}

fn checkout_git_branch(top_level: &Path, branch: &str) -> Result<()> {
    run_git_command(top_level, &["checkout", branch])
}

fn create_and_checkout_git_branch(top_level: &Path, branch: &str) -> Result<()> {
    run_git_command(top_level, &["checkout", "-b", branch])
}

#[cfg(test)]
fn parse_git_status_z(output: &[u8], top_level: &Path) -> HashMap<PathBuf, GitStatusKind> {
    parse_git_status_entries_z(output, top_level)
        .into_iter()
        .map(|entry| (entry.path, entry.kind))
        .collect()
}

fn parse_git_status_entries_z(output: &[u8], top_level: &Path) -> Vec<GitStatusEntry> {
    let mut entries = Vec::new();
    let mut records = output.split(|byte| *byte == 0);

    while let Some(record) = records.next() {
        if record.is_empty() || record.len() < 4 {
            continue;
        }
        let x = record[0];
        let y = record[1];
        let Some(status) = GitStatusKind::from_porcelain(x, y) else {
            continue;
        };
        let path = top_level.join(String::from_utf8_lossy(&record[3..]).as_ref());
        entries.push(GitStatusEntry {
            path,
            kind: status,
            index: x,
            worktree: y,
        });

        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            let _ = records.next();
        }
    }

    entries
}

fn git_dirty_directories(
    statuses: &HashMap<PathBuf, GitStatusKind>,
    root: &Path,
) -> HashSet<PathBuf> {
    let mut dirs = HashSet::new();
    for path in statuses.keys() {
        let mut current = path.parent();
        while let Some(dir) = current {
            if dir.starts_with(root) {
                dirs.insert(dir.to_path_buf());
            }
            if dir == root {
                break;
            }
            current = dir.parent();
        }
    }
    dirs
}

fn stage_git_path(top_level: &Path, path: &Path) -> Result<()> {
    run_git_command_with_path(top_level, &["add"], path)
}

fn stage_all_git_changes(top_level: &Path) -> Result<()> {
    run_git_command(top_level, &["add", "-A", "--", "."])
}

fn unstage_git_path(top_level: &Path, path: &Path) -> Result<()> {
    if run_git_command_with_path(top_level, &["restore", "--staged"], path).is_ok() {
        return Ok(());
    }
    if run_git_command_with_path(top_level, &["reset", "-q"], path).is_ok() {
        return Ok(());
    }
    run_git_command_with_path(top_level, &["rm", "--cached", "--ignore-unmatch"], path)
}

fn unstage_all_git_changes(top_level: &Path) -> Result<()> {
    if run_git_command(top_level, &["restore", "--staged", "--", "."]).is_ok() {
        return Ok(());
    }
    if run_git_command(top_level, &["reset", "-q", "--", "."]).is_ok() {
        return Ok(());
    }
    run_git_command(
        top_level,
        &["rm", "-r", "--cached", "--ignore-unmatch", "--", "."],
    )
}

fn git_has_staged_changes(top_level: &Path) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(top_level)
        .args(["diff", "--cached", "--name-only", "--"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .is_some_and(|output| !output.stdout.is_empty())
}

fn commit_git_staged_changes(top_level: &Path, message: &str) -> Result<()> {
    run_git_command(top_level, &["commit", "--no-gpg-sign", "-m", message])
}

fn source_control_changed_paths(root: &Path, top_level: &Path) -> Vec<PathBuf> {
    load_git_status_entries(root)
        .into_iter()
        .map(|entry| entry.path)
        .filter(|path| path.starts_with(top_level))
        .collect()
}

fn entry_removes_worktree_path(entry: &GitStatusEntry) -> bool {
    entry.kind == GitStatusKind::Untracked || entry.index == b'A'
}

fn discard_git_entry(top_level: &Path, entry: &GitStatusEntry) -> Result<()> {
    if entry.kind == GitStatusKind::Untracked {
        return clean_git_path(top_level, &entry.path);
    }

    if entry.index == b'A' {
        let _ = unstage_git_path(top_level, &entry.path);
        return clean_git_path(top_level, &entry.path);
    }

    if run_git_command_with_path(
        top_level,
        &["restore", "--source=HEAD", "--staged", "--worktree"],
        &entry.path,
    )
    .is_ok()
    {
        return Ok(());
    }

    if run_git_command_with_path(top_level, &["checkout", "HEAD"], &entry.path).is_ok() {
        let _ = unstage_git_path(top_level, &entry.path);
        return Ok(());
    }

    run_git_command_with_path(top_level, &["restore", "--worktree"], &entry.path)
}

fn clean_git_path(top_level: &Path, path: &Path) -> Result<()> {
    if run_git_command_with_path(top_level, &["clean", "-fd"], path).is_ok() {
        return Ok(());
    }
    run_git_command_with_path(top_level, &["clean", "-f"], path)
}

fn run_git_command(top_level: &Path, args: &[&str]) -> Result<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(top_level)
        .args(args)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "git {} failed: {}",
            args.join(" "),
            git_output_message(&output)
        ))
    }
}

fn run_git_command_with_path(top_level: &Path, args: &[&str], path: &Path) -> Result<()> {
    let path_arg = git_relative_os_arg(top_level, path);
    let output = Command::new("git")
        .arg("-C")
        .arg(top_level)
        .args(args)
        .arg("--")
        .arg(&path_arg)
        .output()
        .with_context(|| {
            format!(
                "failed to run git {} for {}",
                args.join(" "),
                path.display()
            )
        })?;
    if output.status.success() {
        Ok(())
    } else {
        Err(anyhow!(
            "git {} failed for {}: {}",
            args.join(" "),
            path.display(),
            git_output_message(&output)
        ))
    }
}

fn git_relative_os_arg(top_level: &Path, path: &Path) -> PathBuf {
    path.strip_prefix(top_level)
        .map(Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}

fn git_output_message(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    if !stderr.is_empty() {
        return stderr;
    }
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if !stdout.is_empty() {
        return stdout;
    }
    output.status.to_string()
}

fn load_git_path_diff(root: &Path, top_level: &Path, path: &Path) -> Result<String> {
    let attempts = [
        &[
            "diff",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
            "HEAD",
        ][..],
        &[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-color",
            "--find-renames",
        ][..],
        &["diff", "--no-ext-diff", "--no-color", "--find-renames"][..],
    ];

    let mut last_error = None;
    for args in attempts {
        match git_command_output_with_path(root, args, path) {
            Ok(output) if output.status.success() => {
                let text = String::from_utf8_lossy(&output.stdout).into_owned();
                if !text.trim().is_empty() {
                    return Ok(text);
                }
            }
            Ok(output) => last_error = Some(git_output_message(&output)),
            Err(error) => last_error = Some(error.to_string()),
        }
    }

    if path.is_file() {
        return build_untracked_file_diff(top_level, path);
    }

    Err(anyhow!(
        "no diff available for {}{}",
        path.display(),
        last_error
            .map(|error| format!(": {error}"))
            .unwrap_or_default()
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CompareDiffLine {
    Equal(String),
    Delete(String),
    Insert(String),
}

fn read_compare_text_file(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    if !metadata.is_file() {
        return Err(anyhow!(
            "compare selected files only supports regular files: {}",
            path.display()
        ));
    }
    if metadata.len() > MAX_FILE_SCAN_BYTES {
        return Err(anyhow!(
            "compare file is too large: {} ({} byte limit)",
            path.display(),
            MAX_FILE_SCAN_BYTES
        ));
    }

    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.contains(&0) {
        return Err(anyhow!(
            "compare selected files only supports text files: {}",
            path.display()
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        anyhow!(
            "compare selected files only supports valid UTF-8 text: {}",
            path.display()
        )
    })
}

fn build_compare_diff(
    root: &Path,
    left: &Path,
    right: &Path,
    left_text: &str,
    right_text: &str,
) -> String {
    let left_relative = relative_path(root, left);
    let right_relative = relative_path(root, right);
    let left_lines = diff_text_lines(left_text);
    let right_lines = diff_text_lines(right_text);
    let mut diff = format!(
        "diff --tscode a/{left_relative} b/{right_relative}\n--- a/{left_relative}\n+++ b/{right_relative}\n"
    );

    if left_text == right_text {
        diff.push_str("Files are identical.\n");
        return diff;
    }

    diff.push_str(&format!(
        "@@ -{} +{} @@\n",
        unified_range(left_lines.len()),
        unified_range(right_lines.len())
    ));
    for line in compare_diff_lines(&left_lines, &right_lines) {
        match line {
            CompareDiffLine::Equal(text) => {
                diff.push(' ');
                diff.push_str(&text);
            }
            CompareDiffLine::Delete(text) => {
                diff.push('-');
                diff.push_str(&text);
            }
            CompareDiffLine::Insert(text) => {
                diff.push('+');
                diff.push_str(&text);
            }
        }
        diff.push('\n');
    }
    diff
}

fn diff_text_lines(text: &str) -> Vec<String> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.lines().map(ToOwned::to_owned).collect()
    }
}

fn unified_range(count: usize) -> String {
    if count == 0 {
        "0,0".to_owned()
    } else {
        format!("1,{count}")
    }
}

fn compare_diff_lines(left: &[String], right: &[String]) -> Vec<CompareDiffLine> {
    const MAX_EXACT_DIFF_CELLS: usize = 750_000;
    if left.len().saturating_mul(right.len()) <= MAX_EXACT_DIFF_CELLS {
        exact_compare_diff_lines(left, right)
    } else {
        coarse_compare_diff_lines(left, right)
    }
}

fn exact_compare_diff_lines(left: &[String], right: &[String]) -> Vec<CompareDiffLine> {
    let cols = right.len() + 1;
    let mut lcs = vec![0usize; (left.len() + 1) * cols];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            let index = i * cols + j;
            lcs[index] = if left[i] == right[j] {
                1 + lcs[(i + 1) * cols + j + 1]
            } else {
                lcs[(i + 1) * cols + j].max(lcs[i * cols + j + 1])
            };
        }
    }

    let mut lines = Vec::new();
    let mut i = 0usize;
    let mut j = 0usize;
    while i < left.len() && j < right.len() {
        if left[i] == right[j] {
            lines.push(CompareDiffLine::Equal(left[i].clone()));
            i += 1;
            j += 1;
        } else if lcs[(i + 1) * cols + j] >= lcs[i * cols + j + 1] {
            lines.push(CompareDiffLine::Delete(left[i].clone()));
            i += 1;
        } else {
            lines.push(CompareDiffLine::Insert(right[j].clone()));
            j += 1;
        }
    }
    while i < left.len() {
        lines.push(CompareDiffLine::Delete(left[i].clone()));
        i += 1;
    }
    while j < right.len() {
        lines.push(CompareDiffLine::Insert(right[j].clone()));
        j += 1;
    }
    lines
}

fn coarse_compare_diff_lines(left: &[String], right: &[String]) -> Vec<CompareDiffLine> {
    let mut prefix = 0usize;
    while prefix < left.len() && prefix < right.len() && left[prefix] == right[prefix] {
        prefix += 1;
    }

    let mut left_end = left.len();
    let mut right_end = right.len();
    while left_end > prefix && right_end > prefix && left[left_end - 1] == right[right_end - 1] {
        left_end -= 1;
        right_end -= 1;
    }

    let mut lines = Vec::new();
    lines.extend(left[..prefix].iter().cloned().map(CompareDiffLine::Equal));
    lines.extend(
        left[prefix..left_end]
            .iter()
            .cloned()
            .map(CompareDiffLine::Delete),
    );
    lines.extend(
        right[prefix..right_end]
            .iter()
            .cloned()
            .map(CompareDiffLine::Insert),
    );
    lines.extend(left[left_end..].iter().cloned().map(CompareDiffLine::Equal));
    lines
}

fn compare_diff_tab_path(root: &Path, left: &Path, right: &Path) -> PathBuf {
    let label = format!(
        "{}__vs__{}",
        relative_path(root, left),
        relative_path(root, right)
    );
    root.join(".tscode-compare")
        .join(format!("{}.diff", sanitize_virtual_path_fragment(&label)))
}

fn git_command_output_with_path(root: &Path, args: &[&str], path: &Path) -> Result<Output> {
    let top_level = git_top_level(root).unwrap_or_else(|| root.to_path_buf());
    let path_arg = git_relative_os_arg(&top_level, path);
    Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .arg("--")
        .arg(path_arg)
        .output()
        .with_context(|| {
            format!(
                "failed to run git {} for {}",
                args.join(" "),
                path.display()
            )
        })
}

fn build_untracked_file_diff(top_level: &Path, path: &Path) -> Result<String> {
    let relative = relative_path(top_level, path);
    let bytes = fs::read(path)?;
    if bytes.contains(&0) {
        return Ok(format!(
            "diff --git a/{relative} b/{relative}\nnew file mode 100644\nBinary files /dev/null and b/{relative} differ\n"
        ));
    }

    let text = String::from_utf8_lossy(&bytes);
    let mut diff = format!(
        "diff --git a/{relative} b/{relative}\nnew file mode 100644\n--- /dev/null\n+++ b/{relative}\n"
    );
    let line_count = text.lines().count();
    diff.push_str(&format!("@@ -0,0 +1,{line_count} @@\n"));
    for line in text.lines() {
        diff.push('+');
        diff.push_str(line);
        diff.push('\n');
    }
    if !text.is_empty() && !text.ends_with('\n') {
        diff.push_str("\\ No newline at end of file\n");
    }
    Ok(diff)
}

fn source_control_diff_tab_path(top_level: &Path, path: &Path) -> PathBuf {
    let relative = relative_path(top_level, path);
    top_level.join(".tscode-diff").join(format!(
        "{}.diff",
        sanitize_virtual_path_fragment(&relative)
    ))
}

fn sanitize_virtual_path_fragment(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn first_diff_hunk_line(lines: &[String]) -> Option<usize> {
    lines.iter().position(|line| line.starts_with("@@ "))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GitDiffHunk {
    path: PathBuf,
    old_count: usize,
    new_start: usize,
    new_count: usize,
    preview: String,
}

fn load_git_diff_hunks(root: &Path, top_level: &Path) -> Vec<GitDiffHunk> {
    let with_head = [
        "diff",
        "--unified=0",
        "--no-ext-diff",
        "--no-color",
        "--find-renames",
        "HEAD",
        "--",
    ];
    let without_head = [
        "diff",
        "--unified=0",
        "--no-ext-diff",
        "--no-color",
        "--find-renames",
        "--",
    ];

    for args in [&with_head[..], &without_head[..]] {
        let Ok(output) = Command::new("git").arg("-C").arg(root).args(args).output() else {
            continue;
        };
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            return parse_git_diff_hunks(&text, top_level);
        }
    }

    Vec::new()
}

fn parse_git_diff_hunks(output: &str, top_level: &Path) -> Vec<GitDiffHunk> {
    let mut hunks = Vec::new();
    let mut current_path: Option<PathBuf> = None;
    let mut current_hunk: Option<usize> = None;

    for line in output.lines() {
        if let Some(path) = line.strip_prefix("+++ ") {
            current_path = git_diff_file_path(path, top_level);
            current_hunk = None;
            continue;
        }

        if line.starts_with("diff --git ") {
            current_path = None;
            current_hunk = None;
            continue;
        }

        if let Some(header) = line.strip_prefix("@@ ") {
            let Some(path) = current_path.clone() else {
                current_hunk = None;
                continue;
            };
            let Some((old_count, new_start, new_count)) = parse_git_hunk_header(header) else {
                current_hunk = None;
                continue;
            };
            hunks.push(GitDiffHunk {
                path,
                old_count,
                new_start,
                new_count,
                preview: String::new(),
            });
            current_hunk = Some(hunks.len() - 1);
            continue;
        }

        let Some(index) = current_hunk else {
            continue;
        };
        if hunks[index].preview.is_empty()
            && (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---")
        {
            hunks[index].preview = line.to_owned();
        }
    }

    hunks
}

fn git_diff_file_path(path: &str, top_level: &Path) -> Option<PathBuf> {
    let path = path.trim();
    if path == "/dev/null" {
        return None;
    }
    let path = path
        .strip_prefix("b/")
        .or_else(|| path.strip_prefix("a/"))
        .unwrap_or(path)
        .trim_matches('"');
    (!path.is_empty()).then(|| top_level.join(path))
}

fn parse_git_hunk_header(header: &str) -> Option<(usize, usize, usize)> {
    let mut parts = header.split_whitespace();
    let old_range = parts.next()?;
    let new_range = parts.next()?;
    let (_, old_count) = parse_git_hunk_range(old_range.strip_prefix('-')?)?;
    let (new_start, new_count) = parse_git_hunk_range(new_range.strip_prefix('+')?)?;
    Some((old_count, new_start, new_count))
}

fn parse_git_hunk_range(range: &str) -> Option<(usize, usize)> {
    let mut parts = range.splitn(2, ',');
    let start = parts.next()?.parse::<usize>().ok()?;
    let count = parts
        .next()
        .and_then(|part| part.parse::<usize>().ok())
        .unwrap_or(1);
    Some((start, count))
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeSymbol {
    kind: &'static str,
    name: String,
    line: usize,
    col: usize,
    preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceTextFile {
    path: PathBuf,
    relative: String,
    text: String,
}

type CompletionRank = (usize, usize, usize, usize, String);

#[derive(Debug, Clone, PartialEq, Eq)]
struct IdentifierToken {
    name: String,
    line: usize,
    col: usize,
    preview: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FormatterCommand {
    label: &'static str,
    program: &'static str,
    args: Vec<String>,
}

fn upsert_completion_item(
    candidates: &mut HashMap<String, (CompletionRank, QuickItem)>,
    rank: CompletionRank,
    item: QuickItem,
) {
    let key = item.label.clone();
    match candidates.get(&key) {
        Some((existing, _)) if *existing <= rank => {}
        _ => {
            candidates.insert(key, (rank, item));
        }
    }
}

fn filter_existing_quick_items(items: Vec<QuickItem>, query: &str) -> Vec<QuickItem> {
    let query = query.trim();
    if query.is_empty() {
        return items;
    }
    items
        .into_iter()
        .filter(|item| {
            fuzzy_score(
                &format!(
                    "{} {} {}",
                    item.label,
                    item.detail,
                    item.preview.as_deref().unwrap_or_default()
                ),
                query,
            )
            .is_some()
        })
        .collect()
}

fn lsp_document_symbol_to_quick_item(symbol: lsp::LspDocumentSymbol) -> QuickItem {
    let mut detail_parts = vec![
        format!("LSP {}", symbol.server),
        format!("line {}", symbol.line + 1),
        symbol.kind,
    ];
    if let Some(container) = symbol
        .container_name
        .filter(|container| !container.is_empty())
    {
        detail_parts.push(format!("in {container}"));
    }
    if let Some(detail) = symbol.detail.filter(|detail| !detail.is_empty()) {
        detail_parts.push(detail);
    }

    QuickItem {
        label: symbol.name,
        detail: detail_parts.join("  "),
        path: symbol.path,
        line: Some(symbol.line),
        col: Some(symbol.col),
        preview: symbol.preview,
        command: None,
    }
}

fn lsp_signature_help_items(
    help: &lsp::LspSignatureHelp,
    path: PathBuf,
    line: usize,
    col: usize,
) -> Vec<QuickItem> {
    let total = help.signatures.len();
    let active_signature = help
        .active_signature
        .unwrap_or(0)
        .min(total.saturating_sub(1));
    help.signatures
        .iter()
        .enumerate()
        .map(|(index, signature)| {
            let active_parameter = (index == active_signature)
                .then_some(help.active_parameter)
                .flatten()
                .filter(|parameter| *parameter < signature.parameters.len());
            let mut detail_parts = vec![
                format!("LSP {}", help.server),
                format!("signature {}/{}", index + 1, total),
            ];
            if index == active_signature {
                detail_parts.push("active".to_owned());
            }
            if let Some(parameter) = active_parameter {
                detail_parts.push(format!(
                    "parameter {}/{}: {}",
                    parameter + 1,
                    signature.parameters.len(),
                    signature.parameters[parameter].label
                ));
            }
            QuickItem {
                label: signature.label.clone(),
                detail: detail_parts.join("  "),
                path: path.clone(),
                line: Some(line),
                col: Some(col),
                preview: signature_help_preview(signature, active_parameter),
                command: None,
            }
        })
        .take(MAX_QUICK_ITEMS)
        .collect()
}

fn signature_help_preview(
    signature: &lsp::LspSignature,
    active_parameter: Option<usize>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(documentation) = signature
        .documentation
        .as_deref()
        .filter(|documentation| !documentation.trim().is_empty())
    {
        parts.push(documentation.trim().to_owned());
    }
    if let Some(parameter) = active_parameter.and_then(|index| signature.parameters.get(index)) {
        let mut parameter_text = format!("parameter: {}", parameter.label);
        if let Some(documentation) = parameter
            .documentation
            .as_deref()
            .filter(|documentation| !documentation.trim().is_empty())
        {
            parameter_text.push_str(" - ");
            parameter_text.push_str(documentation.trim());
        }
        parts.push(parameter_text);
    }
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

fn lsp_call_hierarchy_items(
    entries: Vec<lsp::LspCallHierarchyEntry>,
    root: &Path,
) -> Vec<QuickItem> {
    entries
        .into_iter()
        .filter(|entry| entry.path.is_file())
        .take(MAX_QUICK_ITEMS)
        .map(|entry| {
            let relative = relative_path(root, &entry.path);
            let mut detail_parts = vec![
                format!("LSP {}", entry.server),
                format!("{}:{}", relative, entry.line + 1),
                entry.kind,
            ];
            if entry.range_count > 1 {
                detail_parts.push(format!("{} call site(s)", entry.range_count));
            } else if entry.range_count == 1 {
                detail_parts.push("1 call site".to_owned());
            }
            if let Some(detail) = entry.detail.filter(|detail| !detail.is_empty()) {
                detail_parts.push(detail);
            }
            QuickItem {
                label: entry.name,
                detail: detail_parts.join("  "),
                path: entry.path,
                line: Some(entry.line),
                col: Some(entry.col),
                preview: entry.preview,
                command: None,
            }
        })
        .collect()
}

fn code_action_detail(action: &lsp::LspCodeAction) -> String {
    let mut parts = vec![format!("LSP {}", action.server)];
    if let Some(kind) = action.kind.as_deref().filter(|kind| !kind.is_empty()) {
        parts.push(kind.to_owned());
    }
    if action.is_preferred {
        parts.push("preferred".to_owned());
    }
    if let Some(edit) = &action.edit {
        let file_count = edit
            .edits
            .iter()
            .map(|edit| canonical_existing_path(&edit.path))
            .collect::<HashSet<_>>()
            .len();
        parts.push(format!(
            "{} edit(s) in {} file(s)",
            edit.edits.len(),
            file_count
        ));
    } else if action.command_title.is_some() {
        parts.push("command action".to_owned());
    } else {
        parts.push("no workspace edit".to_owned());
    }
    parts.join("  ")
}

fn code_action_preview(action: &lsp::LspCodeAction) -> Option<String> {
    if let Some(edit) = &action.edit {
        let mut paths = edit
            .edits
            .iter()
            .map(|edit| edit.path.display().to_string())
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        return (!paths.is_empty()).then(|| compact_preview(&paths.join(", ")));
    }
    action
        .command_title
        .as_deref()
        .map(|title| format!("Runs LSP command: {title}"))
}

fn lsp_completion_insert_text_is_plain(text: &str) -> bool {
    !text.is_empty() && !text.contains(['\r', '\n', '$'])
}

fn hover_quick_text(contents: &str) -> (String, Option<String>) {
    let summary = contents
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(compact_preview)
        .unwrap_or_else(|| "No hover text".to_owned());
    let full = compact_preview(contents);
    let preview = (!full.is_empty() && full != summary).then_some(full);
    (summary, preview)
}

fn compact_preview(text: &str) -> String {
    let mut preview = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    const MAX_PREVIEW_CHARS: usize = 160;
    if preview.chars().count() > MAX_PREVIEW_CHARS {
        preview = preview.chars().take(MAX_PREVIEW_CHARS - 1).collect();
        preview.push_str("...");
    }
    preview
}

fn completion_rank(
    name: &str,
    query: &str,
    source_rank: usize,
    kind_rank: usize,
    line_rank: usize,
) -> Option<CompletionRank> {
    if !is_completion_candidate_name(name) {
        return None;
    }
    if !query.is_empty() && name == query {
        return None;
    }

    let score = if query.is_empty() {
        0
    } else {
        fuzzy_score(name, query)?
    };

    Some((
        score,
        source_rank,
        kind_rank,
        line_rank,
        name.to_ascii_lowercase(),
    ))
}

fn keyword_completion_rank(
    name: &str,
    query: &str,
    source_rank: usize,
    kind_rank: usize,
    line_rank: usize,
) -> Option<CompletionRank> {
    if !is_identifier_token(name) || name.chars().count() < 2 {
        return None;
    }
    if !query.is_empty() && name == query {
        return None;
    }

    let score = if query.is_empty() {
        0
    } else {
        fuzzy_score(name, query)?
    };

    Some((
        score,
        source_rank,
        kind_rank,
        line_rank,
        name.to_ascii_lowercase(),
    ))
}

fn is_completion_candidate_name(name: &str) -> bool {
    is_identifier_token(name)
        && name.chars().count() >= 2
        && !is_control_symbol_name(name)
        && !matches!(
            name,
            "true"
                | "false"
                | "null"
                | "None"
                | "Some"
                | "Ok"
                | "Err"
                | "self"
                | "super"
                | "crate"
                | "this"
        )
}

fn identifier_completion_tokens(text: &str) -> Vec<IdentifierToken> {
    let mut tokens = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let chars = line.chars().collect::<Vec<_>>();
        let mut col = 0usize;
        while col < chars.len() {
            if !is_symbol_ident_start(chars[col]) {
                col += 1;
                continue;
            }

            let start = col;
            col += 1;
            while col < chars.len() && is_symbol_ident_continue(chars[col]) {
                col += 1;
            }
            let name = chars[start..col].iter().collect::<String>();
            if is_completion_candidate_name(&name) {
                tokens.push(IdentifierToken {
                    name,
                    line: line_index,
                    col: start,
                    preview: line.trim().to_owned(),
                });
            }
        }
    }
    tokens
}

fn completion_keywords_for_path(path: &Path) -> &'static [&'static str] {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("rs") => &[
            "async", "await", "const", "enum", "fn", "impl", "let", "match", "mod", "mut", "pub",
            "struct", "trait", "use", "where",
        ],
        Some("js") | Some("jsx") | Some("ts") | Some("tsx") => &[
            "async",
            "await",
            "class",
            "const",
            "export",
            "function",
            "import",
            "interface",
            "let",
            "return",
            "type",
        ],
        Some("py") => &[
            "async", "await", "class", "def", "from", "import", "lambda", "return", "self", "with",
            "yield",
        ],
        Some("go") => &[
            "chan",
            "const",
            "defer",
            "func",
            "go",
            "interface",
            "package",
            "range",
            "return",
            "struct",
            "type",
            "var",
        ],
        _ => &[
            "class", "const", "def", "fn", "function", "let", "return", "struct", "type",
        ],
    }
}

fn symbols_to_quick_items(
    path: &Path,
    text: &str,
    relative: &str,
    query: &str,
    include_path_detail: bool,
) -> Vec<QuickItem> {
    let query = query.trim();
    let mut scored = extract_code_symbols(path, text)
        .into_iter()
        .enumerate()
        .filter_map(|(index, symbol)| {
            let haystack = format!("{} {}", symbol.name, symbol.kind);
            let score = fuzzy_score(&haystack, query)?;
            let detail = if include_path_detail {
                format!("{}:{}  {}", relative, symbol.line + 1, symbol.kind)
            } else {
                format!("line {}  {}", symbol.line + 1, symbol.kind)
            };
            Some((
                if query.is_empty() { index } else { score },
                symbol.line,
                QuickItem {
                    label: symbol.name,
                    detail,
                    path: path.to_path_buf(),
                    line: Some(symbol.line),
                    col: Some(symbol.col),
                    preview: Some(symbol.preview),
                    command: None,
                },
            ))
        })
        .collect::<Vec<_>>();

    scored.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then(a.1.cmp(&b.1))
            .then(a.2.label.cmp(&b.2.label))
    });
    scored.into_iter().map(|(_, _, item)| item).collect()
}

fn symbol_to_quick_item(
    path: &Path,
    relative: &str,
    symbol: CodeSymbol,
    include_path_detail: bool,
) -> QuickItem {
    let detail = if include_path_detail {
        format!("{}:{}  {}", relative, symbol.line + 1, symbol.kind)
    } else {
        format!("line {}  {}", symbol.line + 1, symbol.kind)
    };
    QuickItem {
        label: symbol.name,
        detail,
        path: path.to_path_buf(),
        line: Some(symbol.line),
        col: Some(symbol.col),
        preview: Some(symbol.preview),
        command: None,
    }
}

fn extract_code_symbols(path: &Path, text: &str) -> Vec<CodeSymbol> {
    text.lines()
        .enumerate()
        .filter_map(|(line, text)| symbol_from_line(path, line, text))
        .collect()
}

fn symbol_from_line(path: &Path, line: usize, text: &str) -> Option<CodeSymbol> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() || is_symbol_comment(trimmed) {
        return None;
    }

    let base_col = text.chars().take_while(|c| c.is_whitespace()).count();
    let stripped = strip_symbol_modifiers(trimmed);

    let parsed = parse_impl_symbol(stripped)
        .or_else(|| parse_function_symbol(stripped))
        .or_else(|| parse_type_symbol(stripped))
        .or_else(|| parse_variable_function_symbol(stripped))
        .or_else(|| parse_shell_function_symbol(path, stripped))
        .or_else(|| parse_generic_method_symbol(stripped))?;
    let col = base_col + trimmed.find(&parsed.1).unwrap_or(0);

    Some(CodeSymbol {
        kind: parsed.0,
        name: parsed.1,
        line,
        col,
        preview: trimmed.to_owned(),
    })
}

fn is_symbol_comment(line: &str) -> bool {
    line.starts_with("//")
        || line.starts_with("/*")
        || line.starts_with('*')
        || line.starts_with('#') && !line.starts_with("#!")
}

fn strip_symbol_modifiers(mut input: &str) -> &str {
    loop {
        let before = input;
        input = input.trim_start();
        for keyword in [
            "pub(crate)",
            "pub(super)",
            "pub(self)",
            "pub",
            "export",
            "default",
            "async",
            "unsafe",
            "static",
            "open",
            "final",
            "public",
            "private",
            "protected",
            "internal",
            "abstract",
            "override",
            "virtual",
        ] {
            if let Some(rest) = strip_leading_word(input, keyword) {
                input = rest;
                break;
            }
        }

        if let Some(rest) = strip_leading_word(input, "extern") {
            input = strip_quoted_abi(rest.trim_start());
        }

        if input == before {
            return input;
        }
    }
}

fn strip_quoted_abi(input: &str) -> &str {
    let Some(rest) = input.strip_prefix('"') else {
        return input;
    };
    let Some(index) = rest.find('"') else {
        return input;
    };
    rest[index + 1..].trim_start()
}

fn strip_leading_word<'a>(input: &'a str, keyword: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(keyword)?;
    if rest.is_empty() || rest.chars().next().is_some_and(|c| c.is_whitespace()) {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn parse_impl_symbol(input: &str) -> Option<(&'static str, String)> {
    let rest = strip_leading_word(input, "impl")?;
    let name = rest
        .split('{')
        .next()
        .unwrap_or(rest)
        .split(" where ")
        .next()
        .unwrap_or(rest)
        .trim();
    if name.is_empty() {
        return None;
    }
    Some(("impl", name.to_owned()))
}

fn parse_function_symbol(input: &str) -> Option<(&'static str, String)> {
    if let Some(rest) = strip_leading_word(input, "const") {
        return parse_function_symbol(rest);
    }
    if let Some(rest) = strip_leading_word(input, "fn") {
        return read_symbol_identifier(rest).map(|(name, _)| ("fn", name));
    }
    if let Some(rest) = strip_leading_word(input, "function") {
        return read_symbol_identifier(rest).map(|(name, _)| ("function", name));
    }
    if let Some(rest) = strip_leading_word(input, "def") {
        return read_symbol_identifier(rest).map(|(name, _)| ("def", name));
    }
    if let Some(rest) = strip_leading_word(input, "func") {
        let rest = skip_receiver(rest.trim_start());
        return read_symbol_identifier(rest).map(|(name, _)| ("func", name));
    }
    None
}

fn parse_type_symbol(input: &str) -> Option<(&'static str, String)> {
    for (keyword, kind) in [
        ("struct", "struct"),
        ("enum", "enum"),
        ("trait", "trait"),
        ("class", "class"),
        ("interface", "interface"),
        ("type", "type"),
        ("mod", "module"),
        ("module", "module"),
        ("namespace", "namespace"),
    ] {
        if let Some(rest) = strip_leading_word(input, keyword)
            && let Some((name, _)) = read_symbol_identifier(rest)
        {
            return Some((kind, name));
        }
    }
    None
}

fn parse_variable_function_symbol(input: &str) -> Option<(&'static str, String)> {
    for keyword in ["const", "let", "var"] {
        let Some(rest) = strip_leading_word(input, keyword) else {
            continue;
        };
        let Some((name, rest)) = read_symbol_identifier(rest) else {
            continue;
        };
        if rest.contains("=>") || rest.contains("function") {
            return Some(("function", name));
        }
    }
    None
}

fn parse_shell_function_symbol(path: &Path, input: &str) -> Option<(&'static str, String)> {
    if !matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("sh" | "bash" | "zsh" | "fish")
    ) {
        return None;
    }

    if let Some(rest) = strip_leading_word(input, "function") {
        return read_symbol_identifier(rest).map(|(name, _)| ("function", name));
    }

    let (name, rest) = read_symbol_identifier(input)?;
    rest.trim_start()
        .strip_prefix("()")
        .map(|_| ("function", name))
}

fn parse_generic_method_symbol(input: &str) -> Option<(&'static str, String)> {
    let (name, rest) = read_symbol_identifier(input)?;
    if is_control_symbol_name(&name) {
        return None;
    }
    let rest = rest.trim_start();
    if rest.starts_with('(')
        && (input.contains('{') || input.contains("=>") || input.trim_end().ends_with(':'))
    {
        return Some(("method", name));
    }
    None
}

fn skip_receiver(input: &str) -> &str {
    let input = input.trim_start();
    let Some(rest) = input.strip_prefix('(') else {
        return input;
    };
    let Some(index) = rest.find(')') else {
        return input;
    };
    rest[index + 1..].trim_start()
}

fn read_symbol_identifier(input: &str) -> Option<(String, &str)> {
    let input = input.trim_start();
    let mut chars = input.char_indices();
    let (_, first) = chars.next()?;
    if !is_symbol_ident_start(first) {
        return None;
    }

    let mut end = first.len_utf8();
    for (index, c) in chars {
        if is_symbol_ident_continue(c) {
            end = index + c.len_utf8();
        } else {
            break;
        }
    }

    Some((input[..end].to_owned(), &input[end..]))
}

fn is_symbol_ident_start(c: char) -> bool {
    c.is_ascii_alphabetic() || c == '_' || c == '$'
}

fn is_symbol_ident_continue(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$' | '#')
}

fn is_control_symbol_name(name: &str) -> bool {
    matches!(
        name,
        "if" | "for"
            | "while"
            | "switch"
            | "catch"
            | "match"
            | "return"
            | "else"
            | "do"
            | "try"
            | "await"
    )
}

fn formatter_command_for_path(path: &Path, root: &Path) -> Option<FormatterCommand> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let path_string = path.to_string_lossy().into_owned();

    match extension.as_deref() {
        Some("rs") => Some(FormatterCommand {
            label: "rustfmt",
            program: "rustfmt",
            args: vec![
                "--edition".to_owned(),
                detect_rust_edition(path, root),
                "--emit".to_owned(),
                "stdout".to_owned(),
            ],
        }),
        Some("go") => Some(FormatterCommand {
            label: "gofmt",
            program: "gofmt",
            args: Vec::new(),
        }),
        Some("js" | "jsx" | "ts" | "tsx" | "json" | "css" | "scss" | "html" | "md")
        | Some("markdown" | "yaml" | "yml") => Some(FormatterCommand {
            label: "prettier",
            program: "prettier",
            args: vec!["--stdin-filepath".to_owned(), path_string],
        }),
        Some("py" | "pyw") => Some(FormatterCommand {
            label: "black",
            program: "black",
            args: vec![
                "-q".to_owned(),
                "--stdin-filename".to_owned(),
                path_string,
                "-".to_owned(),
            ],
        }),
        Some("sh" | "bash" | "zsh") => Some(FormatterCommand {
            label: "shfmt",
            program: "shfmt",
            args: Vec::new(),
        }),
        Some("c" | "h" | "cc" | "cpp" | "cxx" | "hpp" | "hh" | "m" | "mm" | "java")
        | Some("kt" | "kts" | "cs" | "proto") => Some(FormatterCommand {
            label: "clang-format",
            program: "clang-format",
            args: vec![format!("--assume-filename={path_string}")],
        }),
        _ if matches!(filename.as_str(), ".bashrc" | ".zshrc" | ".profile") => {
            Some(FormatterCommand {
                label: "shfmt",
                program: "shfmt",
                args: Vec::new(),
            })
        }
        _ => None,
    }
}

fn run_formatter_command(formatter: &FormatterCommand, text: &str) -> Result<String> {
    let mut child = spawn_formatter_process(formatter)?;

    {
        let mut stdin = child.stdin.take().context("formatter stdin unavailable")?;
        stdin
            .write_all(text.as_bytes())
            .context("failed to write formatter input")?;
    }

    let output = child
        .wait_with_output()
        .context("failed to wait for formatter")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "{} failed: {}",
            formatter.label,
            stderr
                .trim()
                .lines()
                .next()
                .unwrap_or("no formatter output")
        ));
    }

    String::from_utf8(output.stdout).context("formatter produced invalid UTF-8")
}

fn spawn_formatter_process(formatter: &FormatterCommand) -> Result<std::process::Child> {
    match formatter_process_command(formatter.program, &formatter.args).spawn() {
        Ok(child) => Ok(child),
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound && formatter.program == "rustfmt" =>
        {
            let Some(rustfmt) = rustup_tool_path("rustfmt") else {
                return Err(error)
                    .with_context(|| format!("formatter not found: {}", formatter.program));
            };
            formatter_process_command(&rustfmt, &formatter.args)
                .spawn()
                .with_context(|| format!("formatter not found: {rustfmt}"))
        }
        Err(error) => {
            Err(error).with_context(|| format!("formatter not found: {}", formatter.program))
        }
    }
}

fn formatter_process_command(program: &str, args: &[String]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

fn rustup_tool_path(tool: &str) -> Option<String> {
    rustup_candidates().into_iter().find_map(|rustup| {
        let output = Command::new(rustup).args(["which", tool]).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let path = String::from_utf8(output.stdout).ok()?.trim().to_owned();
        (!path.is_empty()).then_some(path)
    })
}

fn rustup_candidates() -> Vec<String> {
    let mut candidates = vec!["rustup".to_owned()];
    if let Some(home) = env::var_os("HOME") {
        candidates.push(
            PathBuf::from(home)
                .join(".cargo/bin/rustup")
                .to_string_lossy()
                .into_owned(),
        );
    }
    candidates.push("/opt/homebrew/bin/rustup".to_owned());
    candidates.push("/usr/local/bin/rustup".to_owned());
    candidates
}

fn detect_rust_edition(path: &Path, root: &Path) -> String {
    let mut current = path.parent();
    while let Some(dir) = current {
        let manifest = dir.join("Cargo.toml");
        if let Ok(text) = fs::read_to_string(&manifest)
            && let Some(edition) = parse_cargo_edition(&text)
        {
            return edition;
        }
        if dir == root {
            break;
        }
        current = dir.parent();
    }
    "2024".to_owned()
}

fn parse_cargo_edition(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        let line = line.split('#').next()?.trim();
        let (key, value) = line.split_once('=')?;
        (key.trim() == "edition").then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn identifier_at_char(line: &str, char_col: usize) -> Option<String> {
    identifier_range_at_char(line, char_col).map(|(_, _, token)| token)
}

fn completion_state_for_tab(tab: &EditorTab) -> CompletionState {
    let line_index = tab.cursor_line.min(tab.lines.len().saturating_sub(1));
    let line = tab.lines.get(line_index).map(String::as_str).unwrap_or("");
    let chars = line.chars().collect::<Vec<_>>();
    let cursor = tab.cursor_col.min(chars.len());

    let mut start = cursor;
    while start > 0 && is_symbol_ident_continue(chars[start - 1]) {
        start -= 1;
    }

    let mut end = cursor;
    while end < chars.len() && is_symbol_ident_continue(chars[end]) {
        end += 1;
    }

    let prefix = chars[start..cursor].iter().collect::<String>();
    CompletionState {
        path: tab.path.clone(),
        line: line_index,
        start_col: start,
        end_col: end,
        prefix,
    }
}

fn identifier_range_at_char(line: &str, char_col: usize) -> Option<(usize, usize, String)> {
    let chars = line.chars().collect::<Vec<_>>();
    if chars.is_empty() {
        return None;
    }

    let mut index = char_col.min(chars.len() - 1);
    if char_col > chars.len() {
        return None;
    }
    if !is_symbol_ident_continue(chars[index]) {
        if index > 0 && is_symbol_ident_continue(chars[index - 1]) {
            index -= 1;
        } else {
            return None;
        }
    }

    let mut start = index;
    while start > 0 && is_symbol_ident_continue(chars[start - 1]) {
        start -= 1;
    }
    let mut end = index + 1;
    while end < chars.len() && is_symbol_ident_continue(chars[end]) {
        end += 1;
    }
    let token = chars[start..end].iter().collect::<String>();
    is_identifier_token(&token).then_some((start, end, token))
}

fn is_identifier_token(token: &str) -> bool {
    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    is_symbol_ident_start(first) && chars.all(is_symbol_ident_continue)
}

fn identifier_occurrences(line: &str, symbol: &str) -> Vec<usize> {
    if !is_identifier_token(symbol) {
        return Vec::new();
    }

    let mut cols = Vec::new();
    let mut start_byte = 0usize;
    while start_byte <= line.len() {
        let Some(found) = line[start_byte..].find(symbol) else {
            break;
        };
        let byte = start_byte + found;
        if has_identifier_boundaries(line, byte, symbol.len()) {
            cols.push(line[..byte].chars().count());
        }
        start_byte = byte + symbol.len();
    }
    cols
}

fn replace_identifier_occurrences_in_text(
    text: &str,
    symbol: &str,
    replacement: &str,
) -> (String, usize) {
    if !is_identifier_token(symbol) || !is_identifier_token(replacement) || symbol == replacement {
        return (text.to_owned(), 0);
    }

    let mut output = String::with_capacity(text.len());
    let mut last_byte = 0usize;
    let mut search_byte = 0usize;
    let mut count = 0usize;

    while search_byte <= text.len() {
        let Some(found) = text[search_byte..].find(symbol) else {
            break;
        };
        let byte = search_byte + found;
        if has_identifier_boundaries(text, byte, symbol.len()) {
            output.push_str(&text[last_byte..byte]);
            output.push_str(replacement);
            last_byte = byte + symbol.len();
            count += 1;
        }
        search_byte = byte + symbol.len();
    }

    if count == 0 {
        return (text.to_owned(), 0);
    }

    output.push_str(&text[last_byte..]);
    (output, count)
}

fn has_identifier_boundaries(line: &str, byte: usize, len: usize) -> bool {
    let before = line[..byte]
        .chars()
        .next_back()
        .is_none_or(|c| !is_symbol_ident_continue(c));
    let after_byte = byte.saturating_add(len);
    let after = line[after_byte..]
        .chars()
        .next()
        .is_none_or(|c| !is_symbol_ident_continue(c));
    before && after
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

fn bracket_pair(ch: char) -> Option<(char, char, bool)> {
    match ch {
        '(' => Some(('(', ')', true)),
        ')' => Some(('(', ')', false)),
        '[' => Some(('[', ']', true)),
        ']' => Some(('[', ']', false)),
        '{' => Some(('{', '}', true)),
        '}' => Some(('{', '}', false)),
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

fn block_comment_tokens_for_path(path: &Path) -> Option<(&'static str, &'static str)> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase);
    match extension.as_deref() {
        Some(
            "rs" | "js" | "jsx" | "ts" | "tsx" | "go" | "java" | "kt" | "kts" | "c" | "h" | "cc"
            | "cpp" | "hpp" | "cs" | "swift" | "scala" | "php" | "dart" | "css" | "scss" | "sass"
            | "less" | "sql",
        ) => Some(("/*", "*/")),
        Some("html" | "htm" | "xml" | "svg" | "md" | "markdown") => Some(("<!--", "-->")),
        Some("lua") => Some(("--[[", "]]")),
        Some("py") => Some(("\"\"\"", "\"\"\"")),
        _ => None,
    }
}

fn comment_block_text(text: &str, open: &str, close: &str) -> String {
    if text.is_empty() {
        format!("{open}{close}")
    } else {
        format!("{open} {text} {close}")
    }
}

fn uncomment_block_text(text: &str, open: &str, close: &str) -> Option<String> {
    let start = text.find(|c: char| !c.is_whitespace())?;
    let end = text.trim_end().len();
    if start >= end {
        return None;
    }

    let core = &text[start..end];
    if !core.starts_with(open)
        || !core.ends_with(close)
        || core.len() < open.len().saturating_add(close.len())
    {
        return None;
    }

    let mut inner = core[open.len()..core.len() - close.len()].to_owned();
    if inner.starts_with(' ') {
        inner.remove(0);
    }
    if inner.ends_with(' ') {
        inner.pop();
    }

    Some(format!("{}{}{}", &text[..start], inner, &text[end..]))
}

fn comment_removal_range(line: &str, token: &str) -> Option<(usize, usize, usize)> {
    let indent_chars = line.chars().take_while(|c| c.is_whitespace()).count();
    let indent_byte = byte_index_for_char(line, indent_chars);
    let body = &line[indent_byte..];
    let token_with_space = format!("{token} ");

    if body.starts_with(&token_with_space) {
        Some((
            indent_byte,
            indent_byte + token_with_space.len(),
            token_with_space.chars().count(),
        ))
    } else if body.starts_with(token) {
        Some((
            indent_byte,
            indent_byte + token.len(),
            token.chars().count(),
        ))
    } else {
        None
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct WorkspaceCheckCommand {
    label: &'static str,
    program: &'static str,
    args: &'static [&'static str],
}

fn workspace_check_command(root: &Path) -> Option<WorkspaceCheckCommand> {
    if root.join("Cargo.toml").is_file() {
        return Some(WorkspaceCheckCommand {
            label: "cargo check",
            program: "cargo",
            args: &["check", "--message-format=short"],
        });
    }
    if root.join("go.mod").is_file() {
        return Some(WorkspaceCheckCommand {
            label: "go test ./...",
            program: "go",
            args: &["test", "./..."],
        });
    }
    if root.join("pyproject.toml").is_file() || root.join("setup.py").is_file() {
        return Some(WorkspaceCheckCommand {
            label: "python compileall",
            program: "python3",
            args: &["-m", "compileall", "-q", "."],
        });
    }
    None
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspaceTask {
    label: String,
    command: String,
    cwd: PathBuf,
    source: String,
}

fn collect_workspace_tasks(root: &Path) -> Vec<WorkspaceTask> {
    let mut tasks = Vec::new();
    tasks.extend(vscode_tasks(root));
    tasks.extend(package_json_tasks(root));
    tasks.extend(cargo_tasks(root));
    tasks.extend(makefile_tasks(root));
    tasks.extend(go_tasks(root));
    tasks.extend(python_tasks(root));

    let mut seen = HashSet::new();
    tasks.retain(|task| seen.insert((task.cwd.clone(), task.command.clone())));
    tasks
}

fn vscode_tasks(root: &Path) -> Vec<WorkspaceTask> {
    let path = root.join(".vscode/tasks.json");
    let Ok(text) = fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&strip_json_comments(&text)) else {
        return Vec::new();
    };
    let Some(tasks) = value.get("tasks").and_then(Value::as_array) else {
        return Vec::new();
    };

    tasks
        .iter()
        .filter_map(|task| {
            let command = task.get("command").and_then(Value::as_str)?.trim();
            if command.is_empty() {
                return None;
            }
            let label = task
                .get("label")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|label| !label.is_empty())
                .unwrap_or(command);
            let args = task
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(task_arg_value)
                        .map(|arg| shell_escape_task_arg(&arg))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let command = if args.is_empty() {
                command.to_owned()
            } else {
                format!("{command} {}", args.join(" "))
            };
            let cwd = task
                .get("options")
                .and_then(|options| options.get("cwd"))
                .and_then(Value::as_str)
                .map(|cwd| resolve_task_cwd(root, cwd))
                .unwrap_or_else(|| root.to_path_buf());
            Some(WorkspaceTask {
                label: format!("task: {label}"),
                command,
                cwd,
                source: ".vscode/tasks.json".to_owned(),
            })
        })
        .collect()
}

fn package_json_tasks(root: &Path) -> Vec<WorkspaceTask> {
    let path = root.join("package.json");
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    let Some(scripts) = value.get("scripts").and_then(Value::as_object) else {
        return Vec::new();
    };

    let manager = package_manager(root);
    let mut names = scripts
        .iter()
        .filter_map(|(name, command)| {
            command
                .as_str()
                .map(str::trim)
                .filter(|command| !command.is_empty())
                .map(|_| name.clone())
        })
        .collect::<Vec<_>>();
    names.sort();

    names
        .into_iter()
        .map(|name| WorkspaceTask {
            label: format!("{manager}: {name}"),
            command: format!("{manager} run {}", shell_escape_task_arg(&name)),
            cwd: root.to_path_buf(),
            source: "package.json script".to_owned(),
        })
        .collect()
}

fn cargo_tasks(root: &Path) -> Vec<WorkspaceTask> {
    if !root.join("Cargo.toml").is_file() {
        return Vec::new();
    }
    [
        ("cargo: check", "cargo check"),
        ("cargo: build", "cargo build"),
        ("cargo: test", "cargo test"),
        ("cargo: run", "cargo run"),
    ]
    .into_iter()
    .map(|(label, command)| WorkspaceTask {
        label: label.to_owned(),
        command: command.to_owned(),
        cwd: root.to_path_buf(),
        source: "Cargo.toml".to_owned(),
    })
    .collect()
}

fn makefile_tasks(root: &Path) -> Vec<WorkspaceTask> {
    let path = ["Makefile", "makefile", "GNUmakefile"]
        .into_iter()
        .map(|name| root.join(name))
        .find(|path| path.is_file());
    let Some(path) = path else {
        return Vec::new();
    };
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };

    let mut targets = text.lines().filter_map(makefile_target).collect::<Vec<_>>();
    targets.sort();
    targets.dedup();
    targets
        .into_iter()
        .take(40)
        .map(|target| WorkspaceTask {
            label: format!("make: {target}"),
            command: format!("make {}", shell_escape_task_arg(&target)),
            cwd: root.to_path_buf(),
            source: "Makefile target".to_owned(),
        })
        .collect()
}

fn go_tasks(root: &Path) -> Vec<WorkspaceTask> {
    if !root.join("go.mod").is_file() {
        return Vec::new();
    }
    [
        ("go: test", "go test ./..."),
        ("go: build", "go build ./..."),
        ("go: vet", "go vet ./..."),
    ]
    .into_iter()
    .map(|(label, command)| WorkspaceTask {
        label: label.to_owned(),
        command: command.to_owned(),
        cwd: root.to_path_buf(),
        source: "go.mod".to_owned(),
    })
    .collect()
}

fn python_tasks(root: &Path) -> Vec<WorkspaceTask> {
    if !root.join("pyproject.toml").is_file() && !root.join("setup.py").is_file() {
        return Vec::new();
    }
    let runner = if root.join("uv.lock").is_file() {
        "uv run"
    } else {
        "python3 -m"
    };
    vec![
        WorkspaceTask {
            label: "python: pytest".to_owned(),
            command: format!("{runner} pytest"),
            cwd: root.to_path_buf(),
            source: "pyproject.toml".to_owned(),
        },
        WorkspaceTask {
            label: "python: compileall".to_owned(),
            command: if runner == "uv run" {
                "uv run python -m compileall -q .".to_owned()
            } else {
                "python3 -m compileall -q .".to_owned()
            },
            cwd: root.to_path_buf(),
            source: "pyproject.toml".to_owned(),
        },
    ]
}

fn package_manager(root: &Path) -> &'static str {
    if root.join("pnpm-lock.yaml").is_file() {
        "pnpm"
    } else if root.join("yarn.lock").is_file() {
        "yarn"
    } else if root.join("bun.lock").is_file() || root.join("bun.lockb").is_file() {
        "bun"
    } else {
        "npm"
    }
}

fn makefile_target(line: &str) -> Option<String> {
    if line.starts_with(['\t', ' ', '#']) {
        return None;
    }
    let (target, after) = line.split_once(':')?;
    if after.starts_with('=') || target.is_empty() || target.starts_with('.') {
        return None;
    }
    if target.contains('%') || target.contains('$') || target.contains('/') {
        return None;
    }
    let target = target.trim();
    if target.is_empty()
        || !target
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return None;
    }
    Some(target.to_owned())
}

fn task_arg_value(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn resolve_task_cwd(root: &Path, cwd: &str) -> PathBuf {
    let root_string = root.to_string_lossy();
    let expanded = cwd.replace("${workspaceFolder}", &root_string);
    let path = PathBuf::from(expanded);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    path.canonicalize().unwrap_or(path)
}

fn shell_escape_task_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '='))
    {
        return arg.to_owned();
    }
    let escaped = arg.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn strip_json_comments(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut in_string = false;
    let mut escaped = false;

    while let Some(c) = chars.next() {
        if in_string {
            output.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }

        if c == '"' {
            in_string = true;
            output.push(c);
            continue;
        }

        if c == '/' {
            match chars.peek().copied() {
                Some('/') => {
                    let _ = chars.next();
                    for next in chars.by_ref() {
                        if next == '\n' {
                            output.push('\n');
                            break;
                        }
                    }
                    continue;
                }
                Some('*') => {
                    let _ = chars.next();
                    let mut previous = '\0';
                    for next in chars.by_ref() {
                        if next == '\n' {
                            output.push('\n');
                        }
                        if previous == '*' && next == '/' {
                            break;
                        }
                        previous = next;
                    }
                    continue;
                }
                _ => {}
            }
        }

        output.push(c);
    }

    output
}

fn parse_problem_items(output: &str, root: &Path) -> Vec<QuickItem> {
    let mut items = Vec::new();
    let lines = output.lines().collect::<Vec<_>>();
    let mut index = 0;

    while index < lines.len() {
        if let Some(item) = parse_problem_line(lines[index], root) {
            items.push(item);
            if items.len() >= MAX_QUICK_ITEMS {
                break;
            }
        } else if let Some((item, consumed)) = parse_python_problem_lines(&lines[index..], root) {
            items.push(item);
            if items.len() >= MAX_QUICK_ITEMS {
                break;
            }
            index += consumed.saturating_sub(1);
        }
        index += 1;
    }

    items
}

fn lsp_diagnostics_to_problem_items(
    root: &Path,
    diagnostics: Vec<lsp::LspDiagnostic>,
) -> Vec<QuickItem> {
    diagnostics
        .into_iter()
        .take(MAX_QUICK_ITEMS)
        .map(|diagnostic| {
            let relative = relative_path(root, &diagnostic.path);
            let line = diagnostic.line + 1;
            let col = diagnostic.col + 1;
            let mut source = format!("LSP {}", diagnostic.server);
            if let Some(name) = diagnostic.source.as_deref().filter(|name| !name.is_empty()) {
                source.push_str(" / ");
                source.push_str(name);
            }
            if let Some(code) = diagnostic.code.as_deref().filter(|code| !code.is_empty()) {
                source.push_str(" [");
                source.push_str(code);
                source.push(']');
            }
            QuickItem {
                label: format!(
                    "{} {}:{}:{}",
                    diagnostic.severity.label(),
                    relative,
                    line,
                    col
                ),
                detail: diagnostic.message,
                path: diagnostic.path,
                line: Some(diagnostic.line),
                col: Some(diagnostic.col),
                preview: Some(source),
                command: None,
            }
        })
        .collect()
}

fn parse_problem_line(line: &str, root: &Path) -> Option<QuickItem> {
    for (colon_byte, _) in line.match_indices(':') {
        let path_part = clean_problem_path_part(&line[..colon_byte]);
        let after_path = &line[colon_byte + 1..];
        let line_digits = leading_ascii_digits(after_path);
        if line_digits.is_empty() {
            continue;
        }
        let after_line = after_path.strip_prefix(line_digits)?.strip_prefix(':')?;
        let col_digits = leading_ascii_digits(after_line);
        let (col_digits, rest) = if col_digits.is_empty() {
            ("1", after_line.trim())
        } else {
            (
                col_digits,
                after_line
                    .strip_prefix(col_digits)?
                    .strip_prefix(':')?
                    .trim(),
            )
        };
        if rest.is_empty() {
            continue;
        }
        let (severity, message) = parse_problem_message(rest);
        let line_index = line_digits.parse::<usize>().ok()?.saturating_sub(1);
        let col_index = col_digits.parse::<usize>().ok()?.saturating_sub(1);
        let Some(reference) =
            file_reference_from_parts(path_part, Some(line_index), Some(col_index), root)
        else {
            continue;
        };
        let relative = relative_path(root, &reference.path);
        let label = format!(
            "{} {}:{}:{}",
            severity_label(severity),
            relative,
            line_digits,
            col_digits
        );
        return Some(QuickItem {
            label,
            detail: message.to_owned(),
            path: reference.path,
            line: reference.line,
            col: reference.col,
            preview: Some(rest.to_owned()),
            command: None,
        });
    }

    None
}

fn clean_problem_path_part(path_part: &str) -> &str {
    path_part
        .trim()
        .trim_start_matches("-->")
        .trim()
        .trim_matches('"')
}

fn parse_problem_message(rest: &str) -> (&str, &str) {
    if let Some((severity, message)) = rest.split_once(':') {
        let severity = severity.trim();
        if is_problem_severity(severity) {
            return (severity, message.trim());
        }
    }
    ("problem", rest.trim())
}

fn parse_python_problem_lines(lines: &[&str], root: &Path) -> Option<(QuickItem, usize)> {
    let location = lines.first()?.trim();
    let location = location.strip_prefix("File \"")?;
    let (path_part, after_path) = location.split_once('"')?;
    let after_path = after_path.trim_start();
    let after_line = after_path.strip_prefix(", line ")?;
    let line_digits = leading_ascii_digits(after_line);
    if line_digits.is_empty() {
        return None;
    }

    let line_index = line_digits.parse::<usize>().ok()?.saturating_sub(1);
    let reference = file_reference_from_parts(path_part, Some(line_index), Some(0), root)?;
    let mut consumed = 1;
    let mut severity = "problem";
    let mut message = "Python compile error";

    for (offset, line) in lines.iter().enumerate().skip(1).take(6) {
        consumed = offset + 1;
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some((kind, text)) = trimmed.split_once(':') {
            if is_problem_severity(kind) || kind.ends_with("Error") || kind.ends_with("Warning") {
                severity = kind.trim();
                message = text.trim();
                break;
            }
        } else if trimmed.ends_with("Error") || trimmed.ends_with("Warning") {
            severity = trimmed;
            message = trimmed;
            break;
        }
    }

    let relative = relative_path(root, &reference.path);
    Some((
        QuickItem {
            label: format!(
                "{} {}:{}:1",
                severity_label(severity),
                relative,
                line_digits
            ),
            detail: message.to_owned(),
            path: reference.path,
            line: reference.line,
            col: reference.col,
            preview: Some(format!("{severity}: {message}")),
            command: None,
        },
        consumed,
    ))
}

fn is_problem_severity(severity: &str) -> bool {
    let severity = severity.to_ascii_lowercase();
    severity.starts_with("error")
        || severity.starts_with("warning")
        || severity.starts_with("note")
        || severity.starts_with("help")
        || severity.ends_with("Error")
        || severity.ends_with("Warning")
}

fn severity_label(severity: &str) -> &'static str {
    let severity = severity.to_ascii_lowercase();
    if severity.starts_with("error") || severity.ends_with("error") {
        "error"
    } else if severity.starts_with("warning") || severity.ends_with("warning") {
        "warning"
    } else if severity.starts_with("note") {
        "note"
    } else if severity.starts_with("help") {
        "help"
    } else {
        "problem"
    }
}

fn terminal_selection_bounds(anchor: (u16, u16), head: (u16, u16)) -> ((u16, u16), (u16, u16)) {
    if anchor <= head {
        (anchor, head)
    } else {
        (head, anchor)
    }
}

fn terminal_selection_columns(
    anchor: (u16, u16),
    head: (u16, u16),
    row: u16,
    cols: u16,
) -> Option<(usize, usize)> {
    if cols == 0 {
        return None;
    }

    let ((start_row, start_col), (end_row, end_col)) = terminal_selection_bounds(anchor, head);
    if row < start_row || row > end_row {
        return None;
    }

    let start = if row == start_row { start_col } else { 0 }.min(cols);
    let end = if row == end_row {
        end_col.saturating_add(1)
    } else {
        cols
    }
    .min(cols);

    (start < end).then_some((start as usize, end as usize))
}

fn terminal_selected_text_from_screen<F>(
    anchor: (u16, u16),
    head: (u16, u16),
    cols: u16,
    mut row_text: F,
) -> Option<String>
where
    F: FnMut(u16) -> Option<String>,
{
    let ((start_row, _), (end_row, _)) = terminal_selection_bounds(anchor, head);
    let mut lines = Vec::new();

    for row in start_row..=end_row {
        let Some((start, end)) = terminal_selection_columns(anchor, head, row, cols) else {
            continue;
        };
        let line = row_text(row).unwrap_or_default();
        let char_len = line.chars().count();
        let start = start.min(char_len);
        let end = end.min(char_len);
        let mut selected = if start < end {
            slice_chars(&line, start, end)
        } else {
            String::new()
        };
        selected = selected.trim_end_matches(' ').to_owned();
        lines.push(selected);
    }

    let text = lines.join("\n");
    (!text.is_empty()).then_some(text)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileReference {
    path: PathBuf,
    line: Option<usize>,
    col: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum TerminalLink {
    File(FileReference),
    Url(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalLinkCandidate {
    start: usize,
    end: usize,
    link: TerminalLink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TerminalReferenceCandidate {
    start: usize,
    end: usize,
    reference: FileReference,
}

fn terminal_link_candidate_at(
    line: &str,
    char_col: usize,
    root: &Path,
) -> Option<TerminalLinkCandidate> {
    if let Some(candidate) = terminal_url_candidate_at(line, char_col, root) {
        return Some(candidate);
    }

    if let Some(candidate) = terminal_token_reference_candidate_at(line, char_col, root) {
        return Some(TerminalLinkCandidate {
            start: candidate.start,
            end: candidate.end,
            link: TerminalLink::File(candidate.reference),
        });
    }

    terminal_reference_candidates(line, root)
        .into_iter()
        .filter(|candidate| char_col >= candidate.start && char_col < candidate.end)
        .min_by_key(|candidate| candidate.end.saturating_sub(candidate.start))
        .map(|candidate| TerminalLinkCandidate {
            start: candidate.start,
            end: candidate.end,
            link: TerminalLink::File(candidate.reference),
        })
}

#[cfg(test)]
fn terminal_file_reference_at(line: &str, char_col: usize, root: &Path) -> Option<FileReference> {
    if let Some(TerminalLinkCandidate {
        link: TerminalLink::File(reference),
        ..
    }) = terminal_url_candidate_at(line, char_col, root)
    {
        return Some(reference);
    }

    if let Some(candidate) = terminal_token_reference_candidate_at(line, char_col, root) {
        return Some(candidate.reference);
    }

    terminal_reference_candidates(line, root)
        .into_iter()
        .filter(|candidate| char_col >= candidate.start && char_col < candidate.end)
        .min_by_key(|candidate| candidate.end.saturating_sub(candidate.start))
        .map(|candidate| candidate.reference)
}

fn terminal_token_reference_candidate_at(
    line: &str,
    char_col: usize,
    root: &Path,
) -> Option<TerminalReferenceCandidate> {
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
    parse_terminal_reference_token(&token, root).map(|reference| TerminalReferenceCandidate {
        start,
        end,
        reference,
    })
}

fn terminal_url_candidate_at(
    line: &str,
    char_col: usize,
    root: &Path,
) -> Option<TerminalLinkCandidate> {
    terminal_url_candidates(line, root)
        .into_iter()
        .filter(|candidate| char_col >= candidate.start && char_col < candidate.end)
        .min_by_key(|candidate| candidate.end.saturating_sub(candidate.start))
}

fn terminal_url_candidates(line: &str, root: &Path) -> Vec<TerminalLinkCandidate> {
    let mut candidates = Vec::new();
    for scheme in ["https://", "http://", "file://"] {
        let mut search_start = 0;
        while let Some(offset) = line[search_start..].find(scheme) {
            let start_byte = search_start + offset;
            let raw_end_byte = terminal_url_raw_end(line, start_byte);
            let trimmed_end_byte =
                start_byte + terminal_url_trimmed_len(&line[start_byte..raw_end_byte]);
            if trimmed_end_byte <= start_byte {
                search_start = raw_end_byte.max(start_byte + scheme.len());
                continue;
            }
            let url = &line[start_byte..trimmed_end_byte];
            let link = if scheme == "file://" {
                parse_file_url_reference(url, root).map(TerminalLink::File)
            } else {
                Some(TerminalLink::Url(url.to_owned()))
            };
            if let Some(link) = link {
                candidates.push(TerminalLinkCandidate {
                    start: byte_to_char_index(line, start_byte),
                    end: byte_to_char_index(line, trimmed_end_byte),
                    link,
                });
            }
            search_start = raw_end_byte.max(start_byte + scheme.len());
        }
    }
    candidates
}

fn terminal_url_raw_end(line: &str, start_byte: usize) -> usize {
    for (offset, c) in line[start_byte..].char_indices() {
        if c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>') {
            return start_byte + offset;
        }
    }
    line.len()
}

fn terminal_url_trimmed_len(url: &str) -> usize {
    let mut end = url.len();
    while let Some((index, ch)) = previous_char(url, end) {
        let trim = matches!(ch, '.' | ',' | ';' | ':' | '!' | '?')
            || (ch == ')' && unmatched_closing_suffix(&url[..end], '(', ')'))
            || (ch == ']' && unmatched_closing_suffix(&url[..end], '[', ']'))
            || (ch == '}' && unmatched_closing_suffix(&url[..end], '{', '}'));
        if !trim {
            break;
        }
        end = index;
    }
    end
}

fn previous_char(text: &str, end: usize) -> Option<(usize, char)> {
    text.get(..end)?.char_indices().last()
}

fn unmatched_closing_suffix(text: &str, opening: char, closing: char) -> bool {
    text.chars().filter(|c| *c == closing).count() > text.chars().filter(|c| *c == opening).count()
}

fn parse_file_url_reference(url: &str, root: &Path) -> Option<FileReference> {
    let rest = url.strip_prefix("file://")?;
    let path_start = rest.find('/')?;
    let decoded = percent_decode_url_path(&rest[path_start..])?;
    parse_trailing_terminal_reference(&decoded, root)
        .or_else(|| file_reference_from_parts(&decoded, None, None, root))
}

fn percent_decode_url_path(input: &str) -> Option<String> {
    let bytes = input.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hi = *bytes.get(index + 1)?;
            let lo = *bytes.get(index + 2)?;
            output.push(url_hex_value(hi)? * 16 + url_hex_value(lo)?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn url_hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn terminal_reference_candidates(line: &str, root: &Path) -> Vec<TerminalReferenceCandidate> {
    let mut candidates = Vec::new();
    collect_python_file_references(line, root, &mut candidates);
    collect_quoted_trailing_references(line, root, &mut candidates);
    collect_parenthesized_path_references(line, root, &mut candidates);
    collect_parenthesized_line_references(line, root, &mut candidates);
    candidates
}

fn collect_python_file_references(
    line: &str,
    root: &Path,
    candidates: &mut Vec<TerminalReferenceCandidate>,
) {
    for quote in ['"', '\''] {
        let needle = format!("File {quote}");
        for (prefix_start, _) in line.match_indices(&needle) {
            let path_start = prefix_start + needle.len();
            let Some(path_end_offset) = line[path_start..].find(quote) else {
                continue;
            };
            let path_end = path_start + path_end_offset;
            let path_part = &line[path_start..path_end];
            let after_path_start = path_end + quote.len_utf8();
            let after_path = &line[after_path_start..];
            let Some(line_label_offset) = after_path.find("line ") else {
                continue;
            };
            let line_digits_start = after_path_start + line_label_offset + "line ".len();
            let line_digits = leading_ascii_digits(&line[line_digits_start..]);
            if line_digits.is_empty() {
                continue;
            }
            let Ok(line_number) = line_digits.parse::<usize>() else {
                continue;
            };
            let line_index = line_number.saturating_sub(1);
            let line_digits_end = line_digits_start + line_digits.len();
            let after_line = &line[line_digits_end..];
            let (col, end_byte) = parse_python_column_suffix(after_line)
                .map(|(col, consumed)| (Some(col), line_digits_end + consumed))
                .unwrap_or((None, line_digits_end));

            if let Some(reference) =
                file_reference_from_parts(path_part, Some(line_index), col, root)
            {
                candidates.push(TerminalReferenceCandidate {
                    start: byte_to_char_index(line, path_start),
                    end: byte_to_char_index(line, end_byte),
                    reference,
                });
            }
        }
    }
}

fn parse_python_column_suffix(input: &str) -> Option<(usize, usize)> {
    let trimmed = input.strip_prefix(", column ")?;
    let digits = leading_ascii_digits(trimmed);
    if digits.is_empty() {
        return None;
    }
    let col = digits.parse::<usize>().ok()?.saturating_sub(1);
    Some((col, ", column ".len() + digits.len()))
}

fn collect_quoted_trailing_references(
    line: &str,
    root: &Path,
    candidates: &mut Vec<TerminalReferenceCandidate>,
) {
    let mut index = 0;
    while index < line.len() {
        let Some((quote_offset, quote)) = line[index..]
            .char_indices()
            .find(|(_, c)| matches!(c, '"' | '\'' | '`'))
        else {
            break;
        };
        let quote_start = index + quote_offset;
        let path_start = quote_start + quote.len_utf8();
        let Some(path_end_offset) = line[path_start..].find(quote) else {
            break;
        };
        let path_end = path_start + path_end_offset;
        let path_part = &line[path_start..path_end];
        let after_quote_start = path_end + quote.len_utf8();
        if let Some((line_index, col, consumed)) =
            parse_colon_line_suffix(&line[after_quote_start..])
            && let Some(reference) =
                file_reference_from_parts(path_part, Some(line_index), col, root)
        {
            candidates.push(TerminalReferenceCandidate {
                start: byte_to_char_index(line, path_start),
                end: byte_to_char_index(line, after_quote_start + consumed),
                reference,
            });
        }
        index = after_quote_start;
    }
}

fn collect_parenthesized_line_references(
    line: &str,
    root: &Path,
    candidates: &mut Vec<TerminalReferenceCandidate>,
) {
    for (paren_start, _) in line.match_indices('(') {
        let Some(paren_end_offset) = line[paren_start + 1..].find(')') else {
            continue;
        };
        let paren_end = paren_start + 1 + paren_end_offset;
        let inside = &line[paren_start + 1..paren_end];
        let Some((line_index, col)) = parse_parenthesized_line_column(inside) else {
            continue;
        };

        let before = &line[..paren_start];
        for path_start in terminal_reference_path_starts(before).into_iter().rev() {
            let path_part = before[path_start..].trim();
            if let Some(reference) =
                file_reference_from_parts(path_part, Some(line_index), col, root)
            {
                candidates.push(TerminalReferenceCandidate {
                    start: byte_to_char_index(line, path_start),
                    end: byte_to_char_index(line, paren_end + 1),
                    reference,
                });
                break;
            }
        }
    }
}

fn collect_parenthesized_path_references(
    line: &str,
    root: &Path,
    candidates: &mut Vec<TerminalReferenceCandidate>,
) {
    for (paren_start, _) in line.match_indices('(') {
        let Some(paren_end_offset) = line[paren_start + 1..].find(')') else {
            continue;
        };
        let inside_start = paren_start + 1;
        let paren_end = inside_start + paren_end_offset;
        let inside = &line[inside_start..paren_end];
        let (trim_start, trim_end) = trim_bounds(inside);
        if trim_start >= trim_end {
            continue;
        }
        let trimmed = &inside[trim_start..trim_end];
        let Some(reference) = parse_terminal_reference_token(trimmed, root) else {
            continue;
        };
        candidates.push(TerminalReferenceCandidate {
            start: byte_to_char_index(line, inside_start + trim_start),
            end: byte_to_char_index(line, inside_start + trim_end),
            reference,
        });
    }
}

fn terminal_reference_path_starts(input: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (index, c) in input.char_indices() {
        if is_terminal_reference_delimiter(c) {
            let next = index + c.len_utf8();
            if next < input.len() {
                starts.push(next);
            }
        }
    }
    starts
}

fn parse_colon_line_suffix(input: &str) -> Option<(usize, Option<usize>, usize)> {
    let after_colon = input.strip_prefix(':')?;
    let line_digits = leading_ascii_digits(after_colon);
    if line_digits.is_empty() {
        return None;
    }
    let line = line_digits.parse::<usize>().ok()?.saturating_sub(1);
    let mut consumed = 1 + line_digits.len();
    let mut col = None;
    if let Some(after_colon) = after_colon[line_digits.len()..].strip_prefix(':') {
        let col_digits = leading_ascii_digits(after_colon);
        if !col_digits.is_empty() {
            col = Some(col_digits.parse::<usize>().ok()?.saturating_sub(1));
            consumed += 1 + col_digits.len();
        }
    }
    Some((line, col, consumed))
}

fn parse_parenthesized_line_column(input: &str) -> Option<(usize, Option<usize>)> {
    let trimmed = input.trim();
    let separator = trimmed.find(',').or_else(|| trimmed.find(':'));
    let (line_part, col_part) = if let Some(separator) = separator {
        (&trimmed[..separator], Some(&trimmed[separator + 1..]))
    } else {
        (trimmed, None)
    };
    if line_part.is_empty() || !line_part.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let line = line_part.parse::<usize>().ok()?.saturating_sub(1);
    let col = col_part
        .map(str::trim)
        .filter(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
        .and_then(|part| part.parse::<usize>().ok())
        .map(|col| col.saturating_sub(1));
    Some((line, col))
}

fn trim_bounds(input: &str) -> (usize, usize) {
    let start = input.len().saturating_sub(input.trim_start().len());
    let end = input.trim_end().len();
    (start, end)
}

fn byte_to_char_index(text: &str, byte_index: usize) -> usize {
    text[..byte_index.min(text.len())].chars().count()
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
    use std::{thread, time::Duration};

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tscode-test-{}-{name}", std::process::id()))
    }

    fn git_available() -> bool {
        Command::new("git").arg("--version").output().is_ok()
    }

    fn assert_git(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn init_git_repo(root: &Path) {
        assert_git(root, &["init", "-q"]);
        assert_git(root, &["config", "user.email", "tscode@example.invalid"]);
        assert_git(root, &["config", "user.name", "tscode test"]);
    }

    fn cached_git_names(root: &Path) -> Vec<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["diff", "--cached", "--name-only"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git diff --cached failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut names = String::from_utf8_lossy(&output.stdout)
            .lines()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn git_head_subject(root: &Path) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["log", "-1", "--pretty=%s"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git log failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    fn git_head_changed_names(root: &Path) -> Vec<String> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["show", "--name-only", "--pretty=", "HEAD"])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git show failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mut names = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();
        names.sort();
        names
    }

    fn select_quick_item(app: &mut App, label: &str) {
        let panel = app.quick_panel.as_mut().unwrap();
        panel.selected = panel
            .items
            .iter()
            .position(|item| item.label == label)
            .unwrap_or_else(|| {
                panic!(
                    "quick item '{label}' not found in {:?}",
                    panel
                        .items
                        .iter()
                        .map(|item| item.label.as_str())
                        .collect::<Vec<_>>()
                )
            });
    }

    #[test]
    fn app_new_with_file_path_uses_parent_workspace_and_opens_file() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-file-start-{}", std::process::id()));
        let nested = root.join("src");
        let file = nested.join("main.rs");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut app = App::new(file.clone()).unwrap();
        let canonical_root = nested.canonicalize().unwrap();
        let canonical_file = file.canonicalize().unwrap();

        assert_eq!(app.root, canonical_root);
        assert_eq!(app.active_terminal().cwd, canonical_root);
        assert_eq!(
            app.active_tab().map(|tab| tab.path.clone()),
            Some(canonical_file.clone())
        );
        assert_eq!(app.focus, FocusPanel::Editor);
        assert!(
            app.visible_nodes()
                .get(app.explorer.selected)
                .is_some_and(|node| node.path == canonical_file)
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_right_click_opens_context_menu_for_row() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-context-menu-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let file = src.join("main.rs");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_file = file.canonicalize().unwrap();
        app.explorer.reveal(&canonical_file).unwrap();
        let file_index = app.explorer.selected;
        app.hit_regions
            .explorer_rows
            .push((Rect::new(0, 0, 40, 1), file_index));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 1,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.focus, FocusPanel::Explorer);
        assert_eq!(app.explorer.selected, file_index);
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::ExplorerContextMenu);
        assert!(panel.items.iter().any(|item| item.label == "Open File"
            && item.command == Some(CommandAction::OpenSelectedExplorerItem)));
        assert!(
            panel.items.iter().any(|item| item.label == "Rename"
                && item.command == Some(CommandAction::RenameSelected))
        );
        assert!(panel.items.iter().any(|item| item.label == "Sort by Size"
            && item.command == Some(CommandAction::SortExplorerBySize)));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_left_and_right_keys_behave_like_a_file_tree() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-arrow-tree-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let file = src.join("main.rs");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        let canonical_src = src.canonicalize().unwrap();
        let canonical_file = file.canonicalize().unwrap();
        app.focus = FocusPanel::Explorer;
        app.explorer.reveal(&canonical_src).unwrap();

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()))
            .unwrap();
        let nodes = app.visible_nodes();
        let selected = nodes.get(app.explorer.selected).unwrap();
        assert_eq!(selected.path, canonical_src);
        assert!(selected.expanded);

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::empty()))
            .unwrap();
        assert_eq!(
            app.visible_nodes().get(app.explorer.selected).unwrap().path,
            canonical_file
        );

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::empty()))
            .unwrap();
        assert_eq!(
            app.visible_nodes().get(app.explorer.selected).unwrap().path,
            canonical_src
        );

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::empty()))
            .unwrap();
        let nodes = app.visible_nodes();
        let selected = nodes.get(app.explorer.selected).unwrap();
        assert_eq!(selected.path, canonical_src);
        assert!(!selected.expanded);

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::empty()))
            .unwrap();
        assert_eq!(
            app.visible_nodes().get(app.explorer.selected).unwrap().path,
            canonical_root
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_context_menu_actions_run_real_file_operations() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-context-file-ops-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let file = src.join("main.rs");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_root = app.root.clone();
        let canonical_src = src.canonicalize().unwrap();
        let canonical_file = file.canonicalize().unwrap();
        app.explorer.reveal(&canonical_src).unwrap();
        assert!(app.explorer_context_menu_items("").iter().any(|item| {
            item.label == "Open Folder as Workspace"
                && item.command == Some(CommandAction::OpenSelectedFolderAsWorkspace)
        }));

        app.explorer.reveal(&canonical_file).unwrap();
        assert!(app.explorer_context_menu_items("").iter().any(|item| {
            item.label == "Run File in Terminal"
                && item.command == Some(CommandAction::RunSelectedExplorerFileInTerminal)
        }));
        app.run_command(CommandAction::CopySelectedExplorerItem)
            .unwrap();
        assert!(app.explorer_clipboard.is_some());

        app.explorer.reveal(&canonical_root).unwrap();
        app.run_command(CommandAction::PasteIntoSelectedExplorerItem)
            .unwrap();
        let pasted = canonical_root.join("main.rs");
        assert!(pasted.is_file());
        assert_eq!(fs::read_to_string(&pasted).unwrap(), "fn main() {}\n");
        assert!(
            app.visible_nodes()
                .iter()
                .any(|node| node.path == pasted && !node.is_dir)
        );

        app.explorer.reveal(&pasted).unwrap();
        app.run_command(CommandAction::DuplicateSelectedExplorerItem)
            .unwrap();
        assert!(canonical_root.join("main copy.rs").is_file());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn open_folder_switches_workspace_root_explorer_and_terminal() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-open-folder-{}", std::process::id()));
        let first = root.join("first");
        let second = root.join("second");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(second.join("src")).unwrap();
        fs::write(first.join("old.txt"), "old\n").unwrap();
        fs::write(second.join("src/new.txt"), "new\n").unwrap();

        let canonical_first = first.canonicalize().unwrap();
        let canonical_second = second.canonicalize().unwrap();
        let mut app = App::new(canonical_first.clone()).unwrap();
        app.open_file(&canonical_first.join("old.txt"));
        assert_eq!(app.tabs.len(), 1);

        app.run_command(CommandAction::OpenFolder).unwrap();
        assert_eq!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(&PromptKind::OpenFolder)
        );
        app.prompt.as_mut().unwrap().input = canonical_second.to_string_lossy().to_string();
        app.finish_prompt().unwrap();

        assert_eq!(app.root, canonical_second);
        assert!(app.tabs.is_empty());
        assert_eq!(app.focus, FocusPanel::Explorer);
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);
        assert_eq!(app.active_terminal().cwd, app.root);
        assert_eq!(app.next_terminal_id, 2);
        assert!(app.quick_panel.is_none());
        assert!(app.prompt.is_none());
        assert_eq!(
            app.visible_nodes().first().map(|node| node.path.clone()),
            Some(app.root.clone())
        );
        assert!(app.visible_nodes().iter().any(|node| node.name == "src"));
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("opened folder"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn open_selected_folder_as_workspace_uses_explorer_selection() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-open-selected-folder-{}",
            std::process::id()
        ));
        let child = root.join("child");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&child).unwrap();
        fs::write(child.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let canonical_child = child.canonicalize().unwrap();
        let mut app = App::new(canonical_root).unwrap();
        app.explorer.reveal(&canonical_child).unwrap();

        app.run_command(CommandAction::OpenSelectedFolderAsWorkspace)
            .unwrap();

        assert_eq!(app.root, canonical_child);
        assert_eq!(app.focus, FocusPanel::Explorer);
        assert_eq!(app.active_terminal().cwd, app.root);
        assert_eq!(
            app.visible_nodes().first().map(|node| node.path.clone()),
            Some(app.root.clone())
        );
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("opened folder"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn open_folder_blocks_dirty_file_backed_and_untitled_tabs() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-open-folder-dirty-{}",
            std::process::id()
        ));
        let first = root.join("first");
        let second = root.join("second");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&first).unwrap();
        fs::create_dir_all(&second).unwrap();
        fs::write(first.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_first = first.canonicalize().unwrap();
        let canonical_second = second.canonicalize().unwrap();
        let file = canonical_first.join("main.rs");
        let mut app = App::new(canonical_first.clone()).unwrap();
        app.open_file(&file);
        app.active_tab_mut().unwrap().insert_text("// dirty\n");
        app.open_workspace_root(canonical_second.clone()).unwrap();
        assert_eq!(app.root, canonical_first);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.message.as_deref().is_some_and(
            |message| message.contains("open folder blocked") && message.contains("main.rs")
        ));
        app.kill_all_terminals();

        let mut app = App::new(first.clone()).unwrap();
        app.run_command(CommandAction::NewUntitledFile).unwrap();
        app.active_tab_mut().unwrap().insert_text("scratch\n");
        app.open_workspace_root(canonical_second).unwrap();
        assert_eq!(app.root, canonical_first);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.message.as_deref().is_some_and(
            |message| message.contains("open folder blocked") && message.contains("Untitled-1")
        ));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_compare_selected_files_opens_read_only_diff_tab() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-compare-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("left.txt"), "one\ntwo\nthree\n").unwrap();
        fs::write(root.join("right.txt"), "one\nTWO\nthree\nfour\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let left = canonical_root.join("left.txt");
        let right = canonical_root.join("right.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.explorer.reveal(&left).unwrap();
        app.toggle_explorer_multi_selection();
        app.explorer.reveal(&right).unwrap();
        app.toggle_explorer_multi_selection();

        assert_eq!(
            app.selected_explorer_paths(),
            vec![left.clone(), right.clone()]
        );
        let menu = app.explorer_context_menu_items("");
        assert!(menu.iter().any(|item| {
            item.label == "Compare Selected Files"
                && item.command == Some(CommandAction::CompareSelectedFiles)
        }));

        app.run_command(CommandAction::CompareSelectedFiles)
            .unwrap();
        let tab = app.active_tab().unwrap();
        assert!(tab.read_only);
        assert_eq!(tab.title, "Compare left.txt <-> right.txt");
        let diff_text = tab.text();
        assert!(diff_text.contains("diff --tscode a/left.txt b/right.txt"));
        assert!(diff_text.contains("--- a/left.txt"));
        assert!(diff_text.contains("+++ b/right.txt"));
        assert!(diff_text.contains("-two"));
        assert!(diff_text.contains("+TWO"));
        assert!(diff_text.contains("+four"));
        assert!(
            tab.path
                .ends_with(".tscode-compare/left.txt__vs__right.txt.diff")
        );

        app.run_command(CommandAction::SaveFile).unwrap();
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("read-only"))
        );
        assert_eq!(fs::read_to_string(&left).unwrap(), "one\ntwo\nthree\n");
        assert_eq!(
            fs::read_to_string(&right).unwrap(),
            "one\nTWO\nthree\nfour\n"
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_sort_modes_reorder_real_entries_and_preserve_selection() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-explorer-sort-{}", std::process::id()));
        let dir = root.join("src");
        let alpha = root.join("alpha.rs");
        let beta = root.join("beta.txt");
        let zeta = root.join("zeta.md");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&dir).unwrap();
        fs::write(&alpha, "a").unwrap();
        fs::write(&beta, "0123456789").unwrap();
        fs::write(&zeta, "zzz").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_zeta = zeta.canonicalize().unwrap();
        let direct_child_names = |app: &App| {
            app.visible_nodes()
                .into_iter()
                .filter(|node| node.depth == 1)
                .map(|node| node.name)
                .collect::<Vec<_>>()
        };

        assert_eq!(
            direct_child_names(&app),
            vec!["src", "alpha.rs", "beta.txt", "zeta.md"]
        );

        app.explorer.reveal(&canonical_zeta).unwrap();
        app.run_command(CommandAction::SortExplorerBySize).unwrap();
        assert_eq!(app.explorer.sort_mode(), ExplorerSortMode::Size);
        assert_eq!(
            direct_child_names(&app),
            vec!["src", "beta.txt", "zeta.md", "alpha.rs"]
        );
        assert!(
            app.visible_nodes()
                .get(app.explorer.selected)
                .is_some_and(|node| node.path == canonical_zeta)
        );

        app.run_command(CommandAction::SortExplorerByType).unwrap();
        assert_eq!(app.explorer.sort_mode(), ExplorerSortMode::Type);
        assert_eq!(
            direct_child_names(&app),
            vec!["src", "zeta.md", "alpha.rs", "beta.txt"]
        );
        assert!(
            app.visible_nodes()
                .get(app.explorer.selected)
                .is_some_and(|node| node.path == canonical_zeta)
        );

        app.run_command(CommandAction::CycleExplorerSort).unwrap();
        assert_eq!(app.explorer.sort_mode(), ExplorerSortMode::Modified);

        app.run_command(CommandAction::SortExplorerByName).unwrap();
        assert_eq!(app.explorer.sort_mode(), ExplorerSortMode::Name);
        assert_eq!(
            direct_child_names(&app),
            vec!["src", "alpha.rs", "beta.txt", "zeta.md"]
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_right_click_context_menu_runs_real_editor_command() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-editor-context-menu-{}",
            std::process::id()
        ));
        let file = root.join("main.rs");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(&file, "fn main() {}\n").unwrap();

        let mut app = App::new(file.clone()).unwrap();
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 80, 12));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 80, 12));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 8,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.focus, FocusPanel::Editor);
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::EditorContextMenu);
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::RunLspDiagnostics))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::RunActiveFileInTerminal))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::CodeAction))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::SignatureHelp))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| { item.command == Some(CommandAction::GoToTypeDefinition) })
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| { item.command == Some(CommandAction::GoToImplementation) })
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::GoToMatchingBracket))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowIncomingCalls))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowOutgoingCalls))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::HighlightSymbol))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::ClearDocumentHighlights))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::ToggleBlockComment))
        );
        let toggle_index = panel
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::ToggleLineComment))
            .unwrap();

        app.activate_quick_row(toggle_index);
        assert_eq!(app.active_tab().unwrap().lines[0], "// fn main() {}");
        assert!(app.active_tab().unwrap().dirty);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_right_click_opens_context_menu_with_real_terminal_commands() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-context-menu-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.hit_regions.terminal_area = Some(Rect::new(0, 0, 80, 12));
        app.hit_regions.terminal_body = Some(Rect::new(0, 1, 80, 11));
        app.hit_regions.terminal_input = Some(Rect::new(0, 1, 80, 11));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 5,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.focus, FocusPanel::Terminal);
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::TerminalContextMenu);
        assert!(panel.items.iter().any(|item| item.label == "Paste"
            && item.command == Some(CommandAction::PasteClipboardToTerminal)));
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "Restart Terminal"
                    && item.command == Some(CommandAction::RestartTerminal))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "Rename Terminal"
                    && item.command == Some(CommandAction::RenameTerminal))
        );
        assert!(panel.items.iter().any(|item| item.label == "Clear Terminal"
            && item.command == Some(CommandAction::ClearTerminal)));
        assert!(panel.items.iter().any(|item| item.label == "Split Terminal"
            && item.command == Some(CommandAction::SplitTerminal)));
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "Copy All Output"
                    && item.command == Some(CommandAction::CopyTerminalOutput))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "Scroll to Bottom"
                    && item.command == Some(CommandAction::ScrollTerminalToBottom))
        );
        assert!(panel.items.iter().any(|item| item.label == "Run Command"
            && item.command == Some(CommandAction::RunTerminalCommand)));
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "Run Recent Command"
                    && item.command == Some(CommandAction::RunRecentTerminalCommand))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clear_document_highlights_removes_active_tab_ranges() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-clear-highlights-{}",
            std::process::id()
        ));
        let file = root.join("main.rs");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(&file, "let value = 1;\nvalue += 1;\n").unwrap();

        let mut app = App::new(file.clone()).unwrap();
        app.active_tab_mut()
            .unwrap()
            .document_highlights
            .push(lsp::LspDocumentHighlight {
                line: 0,
                start_col: 4,
                end_col: 9,
                kind: lsp::LspDocumentHighlightKind::Write,
                server: "mock".to_owned(),
            });
        assert_eq!(
            app.active_tab()
                .unwrap()
                .document_highlight_ranges_for_line(0)
                .len(),
            1
        );

        app.run_command(CommandAction::ClearDocumentHighlights)
            .unwrap();
        assert!(app.active_tab().unwrap().document_highlights.is_empty());
        assert_eq!(
            app.message.as_deref(),
            Some("cleared 1 symbol highlight(s)")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
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
    fn editor_block_comment_toggles_current_line_selection_and_undoes() {
        let path = temp_file("block-comment.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "fn main() {\n    let value = 1;\n}\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(1, 8);

        assert_eq!(tab.toggle_block_comment(), Some(true));
        assert_eq!(tab.lines[1], "    /* let value = 1; */");

        assert_eq!(tab.toggle_block_comment(), Some(false));
        assert_eq!(tab.lines[1], "    let value = 1;");

        tab.set_cursor(1, 8);
        tab.set_cursor_selecting(1, 13);
        assert_eq!(tab.selected_text().as_deref(), Some("value"));
        assert_eq!(tab.toggle_block_comment(), Some(true));
        assert_eq!(tab.lines[1], "    let /* value */ = 1;");
        assert!(tab.dirty);

        assert!(tab.undo());
        assert_eq!(tab.lines[1], "    let value = 1;");
        assert_eq!(tab.selected_text().as_deref(), Some("value"));

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_block_comment_supports_html_tokens_and_shift_alt_shortcut() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-block-comment-shortcut-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let html = root.join("index.html");
        fs::write(&html, "<main>hello</main>\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let html = canonical_root.join("index.html");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&html);
        app.focus = FocusPanel::Editor;
        app.active_tab_mut().unwrap().set_cursor(0, 0);

        app.handle_key(KeyEvent::new(
            KeyCode::Char('A'),
            KeyModifiers::ALT | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(
            app.active_tab().unwrap().lines[0],
            "<!-- <main>hello</main> -->"
        );
        assert_eq!(app.message.as_deref(), Some("added block comment"));

        app.run_command(CommandAction::ToggleBlockComment).unwrap();
        assert_eq!(app.active_tab().unwrap().lines[0], "<main>hello</main>");
        assert_eq!(app.message.as_deref(), Some("removed block comment"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_trim_trailing_whitespace_is_undoable() {
        let path = temp_file("trim-whitespace.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "fn main() {  \n\tlet x = 1;\t\n}\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, 20);
        tab.set_cursor_selecting(1, 3);

        assert_eq!(tab.trim_trailing_whitespace(), 2);
        assert_eq!(tab.lines, vec!["fn main() {", "\tlet x = 1;", "}"]);
        assert_eq!(tab.cursor_position(), (1, 3));
        assert!(tab.selection_range().is_none());
        assert!(tab.dirty);

        assert!(tab.undo());
        assert_eq!(tab.lines, vec!["fn main() {  ", "\tlet x = 1;\t", "}"]);
        assert_eq!(tab.text(), "fn main() {  \n\tlet x = 1;\t\n}\n");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_replace_entire_text_as_edit_is_undoable() {
        let path = temp_file("format-replace.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "fn main(){println!(\"hi\");}\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        assert!(tab.replace_entire_text_as_edit("fn main() {\n    println!(\"hi\");\n}\n"));
        assert_eq!(tab.lines, vec!["fn main() {", "    println!(\"hi\");", "}"]);
        assert!(tab.dirty);

        assert!(tab.undo());
        assert_eq!(tab.text(), "fn main(){println!(\"hi\");}\n");

        let _ = fs::remove_file(path);
    }

    #[test]
    fn terminal_selection_columns_follow_stream_selection() {
        assert_eq!(
            terminal_selection_columns((1, 3), (1, 7), 1, 20),
            Some((3, 8))
        );
        assert_eq!(
            terminal_selection_columns((3, 4), (1, 2), 1, 10),
            Some((2, 10))
        );
        assert_eq!(
            terminal_selection_columns((3, 4), (1, 2), 2, 10),
            Some((0, 10))
        );
        assert_eq!(
            terminal_selection_columns((3, 4), (1, 2), 3, 10),
            Some((0, 5))
        );
        assert_eq!(terminal_selection_columns((1, 2), (3, 4), 4, 10), None);
    }

    #[test]
    fn terminal_selection_text_copies_visible_rows() {
        let rows = [
            "zero".to_owned(),
            "alpha beta   ".to_owned(),
            "middle row   ".to_owned(),
            "omega tail".to_owned(),
        ];

        let text = terminal_selected_text_from_screen((1, 6), (3, 4), 12, |row| {
            rows.get(row as usize).cloned()
        })
        .unwrap();

        assert_eq!(text, "beta\nmiddle row\nomega");
    }

    #[test]
    fn terminal_wheel_scrolls_scrollback_in_terminal_direction() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-wheel-direction-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.focus = FocusPanel::Terminal;
        app.hit_regions.terminal_bodies = vec![(Rect::new(0, 0, 20, 2), 0)];
        app.hit_regions.terminal_body = Some(Rect::new(0, 0, 20, 2));
        app.hit_regions.terminal_input = Some(Rect::new(0, 0, 20, 2));
        app.active_terminal_mut().shell.clear();
        app.active_terminal_mut().shell.resize(2, 20);
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"one\r\ntwo\r\nthree\r\nfour\r\n");
        app.active_terminal_mut().shell.scroll_to_bottom();
        assert_eq!(app.active_terminal().shell.scrollback(), 0);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert!(app.active_terminal().shell.scrollback() > 0);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert_eq!(app.active_terminal().shell.scrollback(), 0);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_shift_mouse_overrides_child_mouse_mode_for_host_selection_and_scroll() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-shift-mouse-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.focus = FocusPanel::Terminal;
        app.hit_regions.terminal_bodies = vec![(Rect::new(0, 0, 20, 3), 0)];
        app.hit_regions.terminal_body = Some(Rect::new(0, 0, 20, 3));
        app.hit_regions.terminal_input = Some(Rect::new(0, 0, 20, 3));
        app.active_terminal_mut().shell.clear();
        app.active_terminal_mut().shell.resize(3, 20);
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"alpha beta\r\nsecond row\r\nthird row\r\nfourth row\r\n");
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"\x1b[?1000h");
        app.active_terminal_mut().shell.scroll_to_bottom();
        assert_ne!(
            app.active_terminal().shell.mouse_protocol_mode(),
            vt100::MouseProtocolMode::None
        );

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert!(app.terminal_selection.is_none());

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert_eq!(app.active_terminal().shell.scrollback(), 0);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::SHIFT,
        })
        .unwrap();
        assert!(app.active_terminal().shell.scrollback() > 0);

        app.active_terminal_mut().shell.scroll_to_bottom();
        app.active_terminal_mut().shell.clear();
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"alpha beta");
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"\x1b[?1000h");
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::SHIFT,
        })
        .unwrap();
        assert!(app.terminal_selection.is_some());
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 4,
            row: 0,
            modifiers: KeyModifiers::SHIFT,
        })
        .unwrap();
        assert_eq!(app.editor_clipboard.as_deref(), Some("alpha"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_output_copy_captures_scrollback_and_scroll_bottom_restores_live_view() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-copy-output-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.focus = FocusPanel::Terminal;
        app.active_terminal_mut().shell.clear();
        app.active_terminal_mut().shell.resize(2, 20);
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"alpha\r\nbeta\r\ngamma\r\n");
        app.active_terminal_mut().shell.scroll(10);
        assert!(app.active_terminal().shell.scrollback() > 0);

        app.run_command(CommandAction::CopyTerminalOutput).unwrap();
        let copied = app.editor_clipboard.clone().unwrap();
        assert!(copied.contains("alpha"));
        assert!(copied.contains("beta"));
        assert!(copied.contains("gamma"));
        assert_eq!(app.take_clipboard_export(), Some(copied));
        assert!(app.active_terminal().shell.scrollback() > 0);

        app.run_command(CommandAction::ScrollTerminalToBottom)
            .unwrap();
        assert_eq!(app.active_terminal().shell.scrollback(), 0);
        assert_eq!(app.focus, FocusPanel::Terminal);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_line_commands_apply_to_selected_line_ranges() {
        let path = temp_file("line-range-commands.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, 0);
        tab.set_cursor_selecting(2, 0);
        assert_eq!(tab.command_line_range(), (0, 1, true));

        tab.indent_line();
        assert_eq!(tab.lines, vec!["    alpha", "    beta", "gamma", "delta"]);

        assert!(tab.outdent_line());
        assert_eq!(tab.lines, vec!["alpha", "beta", "gamma", "delta"]);

        tab.set_cursor(1, 0);
        tab.set_cursor_selecting(3, 0);
        assert!(tab.toggle_line_comment());
        assert_eq!(tab.lines, vec!["alpha", "// beta", "// gamma", "delta"]);
        assert!(tab.toggle_line_comment());
        assert_eq!(tab.lines, vec!["alpha", "beta", "gamma", "delta"]);

        tab.duplicate_line();
        assert_eq!(
            tab.lines,
            vec!["alpha", "beta", "gamma", "beta", "gamma", "delta"]
        );

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_selected_line_ranges_move_and_delete_as_blocks() {
        let path = temp_file("line-range-move-delete.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(1, 0);
        tab.set_cursor_selecting(3, 0);

        assert!(tab.move_line_up());
        assert_eq!(tab.lines, vec!["beta", "gamma", "alpha", "delta"]);
        assert_eq!(tab.command_line_range(), (0, 1, true));

        assert!(tab.move_line_down());
        assert_eq!(tab.lines, vec!["alpha", "beta", "gamma", "delta"]);
        assert_eq!(tab.command_line_range(), (1, 2, true));

        tab.delete_line();
        assert_eq!(tab.lines, vec!["alpha", "delta"]);
        assert_eq!(tab.cursor_position(), (1, 0));
        assert!(tab.selection_range().is_none());

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
    fn editor_add_next_occurrence_replaces_multiple_selections_and_undoes() {
        let path = temp_file("multi-select.txt");
        let _ = fs::remove_file(&path);
        fs::write(&path, "alpha beta alpha alphabet alpha\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, 2);

        let (count, needle) = tab.add_next_occurrence_selection().unwrap();
        assert_eq!(needle, "alpha");
        assert_eq!(count, 2);
        assert_eq!(tab.selected_text().as_deref(), Some("alpha\nalpha"));
        assert_eq!(
            tab.selection_ranges(),
            vec![
                EditorSelection {
                    start: (0, 0),
                    end: (0, 5)
                },
                EditorSelection {
                    start: (0, 11),
                    end: (0, 16)
                }
            ]
        );

        tab.insert_text("gamma");
        assert_eq!(tab.lines, vec!["gamma beta gamma alphabet alpha"]);
        assert!(tab.dirty);
        assert_eq!(tab.selection_count(), 2);

        tab.insert_text("_x");
        assert_eq!(tab.lines, vec!["gamma_x beta gamma_x alphabet alpha"]);
        assert_eq!(tab.selection_count(), 2);

        assert!(tab.undo());
        assert_eq!(tab.lines, vec!["gamma beta gamma alphabet alpha"]);
        assert_eq!(tab.selection_count(), 2);

        assert!(tab.undo());
        assert_eq!(tab.lines, vec!["alpha beta alpha alphabet alpha"]);
        assert_eq!(tab.selection_count(), 2);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_select_all_occurrences_respects_identifier_boundaries() {
        let path = temp_file("multi-select-all.rs");
        let _ = fs::remove_file(&path);
        fs::write(&path, "let alpha = alpha + alphabet + alpha;\n").unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        tab.set_cursor(0, 5);

        let (count, needle) = tab.select_all_occurrences().unwrap();
        assert_eq!(needle, "alpha");
        assert_eq!(count, 3);
        assert_eq!(tab.selected_text().as_deref(), Some("alpha\nalpha\nalpha"));

        tab.insert_text("value");
        assert_eq!(tab.lines, vec!["let value = value + alphabet + value;"]);
        assert_eq!(tab.selection_count(), 3);

        tab.insert_char('!');
        assert_eq!(tab.lines, vec!["let value! = value! + alphabet + value!;"]);

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
    fn editor_horizontal_scroll_tracks_cursor_wheel_and_mouse() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-horizontal-scroll-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("long.rs");
        fs::write(&path, "abcdefghijklmnopqrstuvwxyz0123456789\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 5;
        app.editor_width = 12;

        {
            let tab = app.active_tab_mut().unwrap();
            tab.set_cursor(0, 20);
        }
        app.ensure_editor_cursor_visible();
        assert_eq!(app.active_tab().unwrap().horizontal_scroll, 15);

        app.scroll_editor_horizontal(100);
        assert_eq!(app.active_tab().unwrap().horizontal_scroll, 30);

        app.hit_regions.editor_body = Some(Rect::new(0, 0, 12, 5));
        app.hit_regions.last_mouse_x = 8;
        app.hit_regions.last_mouse_y = 0;
        app.hover = HoverTarget::Editor;
        app.set_editor_cursor_from_mouse(false);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (0, 32));

        app.hit_regions.last_mouse_x = 2;
        app.set_editor_cursor_from_mouse(false);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (0, 0));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_word_wrap_uses_visual_rows_for_scroll_cursor_and_mouse() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-word-wrap-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("long.rs");
        fs::write(&path, "abcdefghijklmnopqrstuvwxyz0123456789\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 2;
        app.editor_width = 12;

        app.active_tab_mut().unwrap().set_cursor(0, 20);
        app.run_command(CommandAction::ToggleWordWrap).unwrap();
        assert!(app.word_wrap);
        assert_eq!(app.message.as_deref(), Some("word wrap enabled"));
        assert_eq!(app.active_tab().unwrap().horizontal_scroll, 0);
        assert_eq!(app.active_tab().unwrap().scroll, 2);

        let rows = editor_visual_rows(app.active_tab().unwrap(), 6, true);
        assert_eq!(
            rows.iter()
                .map(|row| (row.line, row.start_col, row.continuation))
                .collect::<Vec<_>>(),
            vec![
                (0, 0, false),
                (0, 6, true),
                (0, 12, true),
                (0, 18, true),
                (0, 24, true),
                (0, 30, true),
            ]
        );

        app.scroll_editor_horizontal(100);
        assert_eq!(app.active_tab().unwrap().horizontal_scroll, 0);

        app.hit_regions.editor_body = Some(Rect::new(0, 0, 12, 2));
        app.hit_regions.last_mouse_x = 6;
        app.hit_regions.last_mouse_y = 1;
        app.hover = HoverTarget::Editor;
        app.set_editor_cursor_from_mouse(false);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (0, 18));

        app.scroll_editor(100);
        assert_eq!(app.active_tab().unwrap().scroll, 4);

        app.run_command(CommandAction::ToggleWordWrap).unwrap();
        assert!(!app.word_wrap);
        assert_eq!(app.message.as_deref(), Some("word wrap disabled"));
        assert!(app.active_tab().unwrap().horizontal_scroll > 0);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_code_folding_updates_visible_lines_scroll_and_cursor() {
        let path = temp_file("folding.rs");
        let _ = fs::remove_file(&path);
        fs::write(
            &path,
            "fn main() {\n    let value = 1;\n    if value > 0 {\n        println!(\"{value}\");\n    }\n}\nfn next() {}\n",
        )
        .unwrap();

        let mut tab = EditorTab::open(path.clone()).unwrap();
        assert_eq!(tab.fold_end_for_line(0), Some(5));
        assert_eq!(tab.toggle_fold_at_line(0), Some(true));
        assert_eq!(tab.visible_line_indices(), vec![0, 6]);

        tab.set_cursor(3, 8);
        assert!(tab.is_line_folded(0));
        tab.unfold_line_containing(tab.cursor_line);
        assert!(!tab.is_line_folded(0));
        assert_eq!(tab.visible_row_for_line(3), Some(3));

        assert_eq!(tab.toggle_fold_at_line(2), Some(true));
        assert_eq!(tab.visible_line_indices(), vec![0, 1, 2, 5, 6]);
        assert_eq!(tab.unfold_all(), 1);
        assert_eq!(tab.visible_line_indices(), vec![0, 1, 2, 3, 4, 5, 6]);

        let _ = fs::remove_file(path);
    }

    #[test]
    fn editor_fold_commands_and_mouse_gutter_click_use_real_view_state() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-fold-command-mouse-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(
            &path,
            "fn main() {\n    let value = 1;\n    println!(\"{value}\");\n}\nfn next() {}\n",
        )
        .unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 3;
        app.editor_width = 40;

        app.run_command(CommandAction::ToggleFold).unwrap();
        assert!(app.active_tab().unwrap().is_line_folded(0));
        assert_eq!(app.active_tab().unwrap().visible_line_indices(), vec![0, 4]);

        app.scroll_editor(10);
        assert_eq!(app.active_tab().unwrap().scroll, 0);

        app.run_command(CommandAction::UnfoldAll).unwrap();
        assert!(!app.active_tab().unwrap().is_line_folded(0));
        assert_eq!(
            app.active_tab().unwrap().visible_line_indices(),
            vec![0, 1, 2, 3, 4]
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('0'), KeyModifiers::ALT))
            .unwrap();
        assert_eq!(app.message.as_deref(), Some("folded 1 block(s)"));
        assert!(app.active_tab().unwrap().is_line_folded(0));
        assert_eq!(app.active_tab().unwrap().visible_line_indices(), vec![0, 4]);

        app.handle_key(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::ALT))
            .unwrap();
        assert_eq!(app.message.as_deref(), Some("unfolded 1 block(s)"));
        assert!(!app.active_tab().unwrap().is_line_folded(0));

        app.hit_regions.editor_area = Some(Rect::new(0, 0, 40, 5));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 40, 5));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert!(app.active_tab().unwrap().is_line_folded(0));
        assert_eq!(app.message.as_deref(), Some("folded line 1"));

        app.open_quick_panel(QuickPanelKind::EditorContextMenu)
            .unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::ToggleFold))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::FoldAll))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::UnfoldAll))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::ToggleWordWrap))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_bookmarks_toggle_list_and_jump_across_open_tabs() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-bookmarks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let alpha = root.join("alpha.rs");
        let beta = root.join("beta.rs");
        fs::write(&alpha, "fn alpha() {}\nlet a = 1;\n").unwrap();
        fs::write(&beta, "fn beta() {}\nlet b = 2;\nlet c = 3;\n").unwrap();

        let root = root.canonicalize().unwrap();
        let alpha = root.join("alpha.rs");
        let beta = root.join("beta.rs");
        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&alpha);
        app.focus = FocusPanel::Editor;
        app.active_tab_mut().unwrap().set_cursor(1, 0);
        app.run_command(CommandAction::ToggleBookmark).unwrap();
        assert!(app.active_tab().unwrap().has_bookmark(1));

        app.open_file(&beta);
        app.active_tab_mut().unwrap().set_cursor(2, 0);
        app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::ALT))
            .unwrap();
        assert!(app.active_tab().unwrap().has_bookmark(2));

        app.run_command(CommandAction::PreviousBookmark).unwrap();
        assert_eq!(app.active_tab().unwrap().path, alpha);
        assert_eq!(app.active_tab().unwrap().cursor_line, 1);

        app.run_command(CommandAction::NextBookmark).unwrap();
        assert_eq!(app.active_tab().unwrap().path, beta);
        assert_eq!(app.active_tab().unwrap().cursor_line, 2);

        app.run_command(CommandAction::ShowBookmarks).unwrap();
        assert_eq!(
            app.quick_panel.as_ref().map(|panel| &panel.kind),
            Some(&QuickPanelKind::Bookmarks)
        );
        let panel = app.quick_panel.as_ref().unwrap();
        assert!(panel.items.iter().any(|item| item.label == "alpha.rs:2"));
        assert!(panel.items.iter().any(|item| item.label == "beta.rs:3"));

        select_quick_item(&mut app, "alpha.rs:2");
        app.activate_selected_quick_item();
        assert_eq!(app.active_tab().unwrap().path, alpha);
        assert_eq!(app.active_tab().unwrap().cursor_line, 1);

        let commands = app.command_palette_items("bookmark");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ToggleBookmark))
        );
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowBookmarks))
        );

        app.open_quick_panel(QuickPanelKind::EditorContextMenu)
            .unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.command == Some(CommandAction::NextBookmark))
        );

        app.run_command(CommandAction::ClearBookmarks).unwrap();
        assert!(app.active_tab().unwrap().bookmarks.is_empty());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_bookmark_gutter_click_and_line_edits_keep_real_positions() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-bookmark-gutter-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "alpha\nbeta\ngamma\ndelta\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 4;
        app.editor_width = 40;
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 40, 4));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 40, 4));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert!(app.active_tab().unwrap().has_bookmark(1));
        assert_eq!(app.editor_gutter_dragging, None);
        assert_eq!(app.message.as_deref(), Some("bookmarked line 2"));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert!(!app.active_tab().unwrap().has_bookmark(1));
        assert_eq!(app.message.as_deref(), Some("removed bookmark line 2"));

        let tab = app.active_tab_mut().unwrap();
        assert_eq!(tab.toggle_bookmark_at_line(2), Some(true));
        tab.set_cursor(0, 0);
        tab.newline();
        assert!(tab.has_bookmark(3));
        assert!(!tab.has_bookmark(2));

        tab.set_cursor(1, 0);
        tab.delete_line();
        assert!(tab.has_bookmark(2));

        tab.set_cursor(0, 0);
        tab.delete_line();
        assert!(tab.has_bookmark(1));

        tab.set_cursor(1, 0);
        tab.delete_line();
        assert!(tab.bookmarks.is_empty());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_alt_click_toggles_mouse_cursors_and_typing_uses_all() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-alt-click-cursors-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "alpha\nbeta\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 5;
        app.editor_width = 24;
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 24, 5));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 24, 5));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 1,
            modifiers: KeyModifiers::ALT,
        })
        .unwrap();
        assert_eq!(app.focus, FocusPanel::Editor);
        assert_eq!(
            app.active_tab().unwrap().cursor_positions(),
            vec![(0, 0), (1, 0)]
        );
        assert_eq!(app.active_tab().unwrap().selection_count(), 2);

        app.handle_key(KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.active_tab().unwrap().lines, vec!["!alpha", "!beta"]);
        assert_eq!(
            app.active_tab().unwrap().cursor_positions(),
            vec![(0, 1), (1, 1)]
        );

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 7,
            row: 1,
            modifiers: KeyModifiers::ALT,
        })
        .unwrap();
        assert_eq!(app.active_tab().unwrap().cursor_positions(), vec![(0, 1)]);
        assert_eq!(app.active_tab().unwrap().selection_count(), 0);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_mouse_drag_selects_text_and_copy_uses_selection() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-editor-drag-select-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 5;
        app.editor_width = 32;
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 32, 5));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 32, 5));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 6,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert!(app.editor_selection_dragging);
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 10,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 10,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();

        assert!(!app.editor_selection_dragging);
        assert_eq!(
            app.active_tab().unwrap().selected_text().as_deref(),
            Some("alpha\nbeta")
        );
        app.copy_editor_selection();
        assert_eq!(app.editor_clipboard.as_deref(), Some("alpha\nbeta"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_gutter_drag_selects_whole_lines_for_real_line_commands() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-editor-gutter-lines-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "zero\none\ntwo\nthree\nfour\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 5;
        app.editor_width = 32;
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 32, 5));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 32, 5));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: 3,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert_eq!(app.editor_gutter_dragging, Some(3));
        assert_eq!(
            app.active_tab().unwrap().selected_text().as_deref(),
            Some("three")
        );

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 1,
            row: 1,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();

        assert_eq!(app.editor_gutter_dragging, None);
        assert_eq!(
            app.active_tab().unwrap().selected_text().as_deref(),
            Some("one\ntwo\nthree")
        );
        assert_eq!(app.active_tab().unwrap().cursor_position(), (1, 0));
        assert_eq!(app.message.as_deref(), Some("3 editor line(s) selected"));

        app.run_command(CommandAction::DeleteLine).unwrap();
        assert_eq!(app.active_tab().unwrap().lines, vec!["zero", "four"]);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_mouse_drag_beyond_body_scrolls_and_extends_selection() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-editor-drag-scroll-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "zero\none\ntwo\nthree\nfour\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.editor_height = 2;
        app.editor_width = 20;
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 20, 2));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 20, 2));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 0,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 100,
            row: 4,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 100,
            row: 4,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();

        assert_eq!(app.active_tab().unwrap().scroll, 2);
        assert_eq!(
            app.active_tab().unwrap().selected_text().as_deref(),
            Some("zero\none\ntwo\nthree")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
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
    fn trim_trailing_whitespace_command_saves_real_file() {
        let root = std::env::temp_dir().join(format!("tscode-test-trim-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "fn main() {  \n    println!(\"hi\"); \n}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.run_command(CommandAction::TrimTrailingWhitespace)
            .unwrap();

        assert_eq!(
            app.active_tab().unwrap().lines,
            vec!["fn main() {", "    println!(\"hi\");", "}"]
        );
        assert_eq!(
            app.message.as_deref(),
            Some("trimmed trailing whitespace on 2 line(s)")
        );

        app.run_command(CommandAction::SaveFile).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "fn main() {\n    println!(\"hi\");\n}\n"
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_copy_and_cut_without_selection_use_current_line() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-line-copy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.active_tab_mut().unwrap().set_cursor(1, 2);

        app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.editor_clipboard.as_deref(), Some("beta\n"));
        assert_eq!(app.take_clipboard_export().as_deref(), Some("beta\n"));
        assert_eq!(
            app.message.as_deref(),
            Some("copied current line to clipboard")
        );
        assert_eq!(
            app.active_tab().unwrap().lines,
            vec!["alpha", "beta", "gamma"]
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.editor_clipboard.as_deref(), Some("beta\n"));
        assert_eq!(app.take_clipboard_export().as_deref(), Some("beta\n"));
        assert_eq!(
            app.message.as_deref(),
            Some("cut current line to clipboard")
        );
        assert_eq!(app.active_tab().unwrap().lines, vec!["alpha", "gamma"]);
        assert!(app.active_tab().unwrap().dirty);

        app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(
            app.active_tab().unwrap().lines,
            vec!["alpha", "beta", "gamma"]
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_as_writes_new_file_and_retargets_active_tab() {
        let root = std::env::temp_dir().join(format!("tscode-test-save-as-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("main.rs");
        fs::write(&source, "fn main() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&source);
        app.active_tab_mut().unwrap().insert_text("// saved as\n");
        app.save_as_from_prompt("nested/copy.rs".to_owned())
            .unwrap();

        let target = root.join("nested/copy.rs").canonicalize().unwrap();
        assert_eq!(
            fs::read_to_string(&target).unwrap(),
            "// saved as\nfn main() {}\n"
        );
        assert_eq!(fs::read_to_string(&source).unwrap(), "fn main() {}\n");
        assert_eq!(app.active_tab().unwrap().path, target);
        assert_eq!(app.active_tab().unwrap().title, "copy.rs");
        assert!(!app.active_tab().unwrap().dirty);
        assert!(app.visible_nodes().iter().any(|node| node.path == target));
        assert_eq!(app.message, Some(format!("saved as {}", target.display())));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn save_as_refuses_dirty_open_target() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-save-as-dirty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source = root.join("source.rs");
        let target = root.join("target.rs");
        fs::write(&source, "source\n").unwrap();
        fs::write(&target, "target\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&source);
        app.open_file(&target);
        app.active_tab_mut().unwrap().insert_text("dirty ");
        app.active_tab = Some(0);
        app.active_tab_mut().unwrap().insert_text("copy ");

        app.save_as_from_prompt("target.rs".to_owned()).unwrap();

        assert_eq!(fs::read_to_string(&target).unwrap(), "target\n");
        assert_eq!(
            app.active_tab().unwrap().path,
            source.canonicalize().unwrap()
        );
        assert!(app.active_tab().unwrap().dirty);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("already open with unsaved edits"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn untitled_editor_tab_saves_as_real_file_without_placeholder() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-untitled-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL))
            .unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let placeholder = canonical_root.join("Untitled-1");
        let tab = app.active_tab().unwrap();
        assert!(tab.untitled);
        assert_eq!(tab.title, "Untitled-1");
        assert_eq!(tab.path, placeholder);
        assert!(!placeholder.exists());
        assert_eq!(app.focus, FocusPanel::Editor);

        app.active_tab_mut().unwrap().insert_text("fn main() {}\n");
        app.run_command(CommandAction::SaveFile).unwrap();
        assert!(matches!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::SaveAs)
        ));
        assert_eq!(app.message.as_deref(), Some("Untitled-1 needs Save As"));
        assert!(!placeholder.exists());
        app.prompt = None;

        app.save_as_from_prompt("src/new.rs".to_owned()).unwrap();
        let target = canonical_root.join("src/new.rs").canonicalize().unwrap();
        let tab = app.active_tab().unwrap();
        assert!(!tab.untitled);
        assert_eq!(tab.path, target);
        assert_eq!(tab.title, "new.rs");
        assert!(!tab.dirty);
        assert_eq!(fs::read_to_string(&target).unwrap(), "fn main() {}\n");
        assert!(!placeholder.exists());

        app.run_command(CommandAction::NewUntitledFile).unwrap();
        app.active_tab_mut().unwrap().insert_text("scratch");
        app.run_command(CommandAction::SaveAll).unwrap();
        assert!(app.active_tab().unwrap().untitled);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("skipped 1 untitled tab"))
        );

        let commands = app.command_palette_items("new untitled");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::NewUntitledFile))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(canonical_root);
    }

    #[test]
    fn ctrl_k_s_chord_saves_all_dirty_file_backed_tabs() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-save-all-chord-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let first = root.join("first.rs");
        let second = root.join("second.rs");
        fs::write(&first, "fn first() {}\n").unwrap();
        fs::write(&second, "fn second() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        let first = canonical_root.join("first.rs");
        let second = canonical_root.join("second.rs");

        app.open_file(&first);
        app.active_tab_mut().unwrap().insert_text("// saved one\n");
        app.open_file(&second);
        app.active_tab_mut().unwrap().insert_text("// saved two\n");
        app.run_command(CommandAction::NewUntitledFile).unwrap();
        app.active_tab_mut().unwrap().insert_text("scratch");
        assert_eq!(app.tabs.iter().filter(|tab| tab.dirty).count(), 3);

        app.focus = FocusPanel::Editor;
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.pending_key_chord, Some(PendingKeyChord::CtrlK));
        assert_eq!(
            app.message.as_deref(),
            Some("Ctrl-K chord: press S to Save All")
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE))
            .unwrap();

        assert_eq!(
            fs::read_to_string(&first).unwrap(),
            "// saved one\nfn first() {}\n"
        );
        assert_eq!(
            fs::read_to_string(&second).unwrap(),
            "// saved two\nfn second() {}\n"
        );
        assert!(
            app.tabs
                .iter()
                .filter(|tab| !tab.untitled)
                .all(|tab| !tab.dirty)
        );
        assert!(app.tabs.iter().any(|tab| tab.untitled && tab.dirty));
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("saved 2 dirty tab(s)")
                    && message.contains("skipped 1 untitled tab"))
        );

        app.focus = FocusPanel::Terminal;
        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.pending_key_chord, None);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(canonical_root);
    }

    #[test]
    fn dirty_tab_close_prompt_can_save_or_discard() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-dirty-close-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let save_file = root.join("save.rs");
        let discard_file = root.join("discard.rs");
        fs::write(&save_file, "fn save() {}\n").unwrap();
        fs::write(&discard_file, "fn discard() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&save_file);
        app.active_tab_mut().unwrap().insert_text("// saved\n");
        app.run_command(CommandAction::CloseActiveTab).unwrap();

        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::DirtyClose { index: 0 });
        assert!(panel.items.iter().any(|item| {
            item.command == Some(CommandAction::SaveAndCloseTab(0))
                && item.label == "Save and Close"
        }));
        let cancel_index = panel
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::CancelCloseTab))
            .unwrap();
        app.activate_quick_row(cancel_index);
        assert_eq!(app.tabs.len(), 1);
        assert!(app.active_tab().unwrap().dirty);
        assert_eq!(app.message.as_deref(), Some("close cancelled"));

        app.run_command(CommandAction::CloseActiveTab).unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        let save_index = panel
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::SaveAndCloseTab(0)))
            .unwrap();
        app.activate_quick_row(save_index);
        assert!(app.tabs.is_empty());
        assert_eq!(
            fs::read_to_string(&save_file).unwrap(),
            "// saved\nfn save() {}\n"
        );

        app.open_file(&discard_file);
        app.active_tab_mut().unwrap().insert_text("// discarded\n");
        app.run_command(CommandAction::CloseActiveTab).unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        let discard_index = panel
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::DiscardAndCloseTab(0)))
            .unwrap();
        app.activate_quick_row(discard_index);

        assert!(app.tabs.is_empty());
        assert_eq!(
            fs::read_to_string(&discard_file).unwrap(),
            "fn discard() {}\n"
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn open_editors_panel_lists_dirty_untitled_and_switches_tabs() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-open-editors-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let first = root.join("first.rs");
        let second = root.join("second.rs");
        fs::write(&first, "fn first() {}\n").unwrap();
        fs::write(&second, "fn second() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        let first = canonical_root.join("first.rs");
        let second = canonical_root.join("second.rs");

        app.open_file(&first);
        app.open_file(&second);
        app.active_tab_mut().unwrap().insert_text("// dirty\n");
        app.run_command(CommandAction::NewUntitledFile).unwrap();
        app.active_tab_mut().unwrap().insert_text("scratch");

        app.run_command(CommandAction::ShowOpenEditors).unwrap();
        let panel = app.quick_panel.as_ref().expect("open editors panel");
        assert_eq!(panel.kind, QuickPanelKind::OpenEditors);
        assert_eq!(panel.items.len(), 3);
        assert!(panel.items.iter().any(|item| {
            item.path == first
                && item.command == Some(CommandAction::SelectEditorTab(0))
                && item.detail.contains("first.rs")
        }));
        assert!(panel.items.iter().any(|item| {
            item.path == second
                && item.command == Some(CommandAction::SelectEditorTab(1))
                && item.label.contains("* second.rs")
                && item.detail.contains("dirty")
        }));
        assert!(panel.items.iter().any(|item| {
            item.command == Some(CommandAction::SelectEditorTab(2))
                && item.detail.contains("Untitled editor")
                && item.detail.contains("dirty")
        }));

        app.quick_panel.as_mut().unwrap().query = "first".to_owned();
        app.quick_panel.as_mut().unwrap().query_cursor = "first".len();
        app.refresh_quick_panel().unwrap();
        assert_eq!(
            app.quick_panel.as_ref().unwrap().items[0].command,
            Some(CommandAction::SelectEditorTab(0))
        );
        app.activate_selected_quick_item();
        assert_eq!(app.active_tab().unwrap().path, first);
        assert_eq!(app.focus, FocusPanel::Editor);
        assert_eq!(app.message.as_deref(), Some("editor: first.rs"));

        app.run_command(CommandAction::ShowOpenEditors).unwrap();
        let untitled_index = app
            .quick_panel
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::SelectEditorTab(2)))
            .expect("untitled item");
        app.quick_panel.as_mut().unwrap().selected = untitled_index;
        app.activate_selected_quick_item();
        assert!(app.active_tab().unwrap().untitled);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(canonical_root);
    }

    #[test]
    fn repair_runtime_state_recovers_invalid_terminal_prompt_and_panel_indices() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-repair-runtime-state-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        app.kill_all_terminals();
        app.terminals.clear();
        app.active_terminal = 42;
        app.split_terminal = Some(77);
        app.active_tab = Some(99);
        app.explorer.selected = usize::MAX;
        app.explorer.scroll = usize::MAX;
        app.prompt = Some(PromptState {
            kind: PromptKind::NewFile,
            input: "src/main.rs".to_owned(),
            cursor: 99,
        });
        app.quick_panel = Some(QuickPanel {
            kind: QuickPanelKind::CommandPalette,
            query: "open".to_owned(),
            query_cursor: 99,
            items: Vec::new(),
            selected: 99,
            scroll: 99,
        });

        app.repair_runtime_state().unwrap();

        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.active_terminal, 0);
        assert_eq!(app.split_terminal, None);
        assert_eq!(app.active_tab, None);
        assert_eq!(app.prompt.as_ref().unwrap().cursor, "src/main.rs".len());
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.selected, 0);
        assert_eq!(panel.scroll, 0);
        assert_eq!(panel.query_cursor, "open".len());
        assert!(app.last_error.as_deref().unwrap().contains("recovered"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(canonical_root);
    }

    #[test]
    fn dirty_untitled_close_can_save_as_then_close_without_placeholder() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-untitled-close-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.run_command(CommandAction::NewUntitledFile).unwrap();
        let placeholder = app.root.join("Untitled-1");
        app.active_tab_mut().unwrap().insert_text("fn main() {}\n");
        app.run_command(CommandAction::CloseActiveTab).unwrap();

        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::DirtyClose { index: 0 });
        let save_index = panel
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::SaveAndCloseTab(0)))
            .unwrap();
        app.activate_quick_row(save_index);

        assert!(matches!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::SaveAsClose { index: 0 })
        ));
        app.prompt.as_mut().unwrap().input = "src/scratch.rs".to_owned();
        app.finish_prompt().unwrap();

        let target = app.root.join("src/scratch.rs");
        assert!(app.tabs.is_empty());
        assert_eq!(fs::read_to_string(&target).unwrap(), "fn main() {}\n");
        assert!(!placeholder.exists());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reopen_closed_editor_restores_clean_file_with_view_state() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-reopen-clean-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("main.rs");
        fs::write(&file, "fn main() {\n    let beta = 1;\n}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&file);
        {
            let tab = app.active_tab_mut().unwrap();
            tab.set_cursor(0, 3);
            tab.scroll = 0;
            tab.horizontal_scroll = 3;
            tab.folded_lines.insert(0);
        }
        app.run_command(CommandAction::CloseActiveTab).unwrap();
        assert!(app.tabs.is_empty());
        assert_eq!(app.closed_tabs.len(), 1);

        fs::write(&file, "fn main() {\n    let changed = 1;\n}\n").unwrap();
        app.handle_key(KeyEvent::new(
            KeyCode::Char('T'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();

        let tab = app.active_tab().unwrap();
        assert_eq!(tab.path, file.canonicalize().unwrap());
        assert_eq!(tab.lines, vec!["fn main() {", "    let changed = 1;", "}"]);
        assert!(!tab.dirty);
        assert_eq!(tab.cursor_position(), (0, 3));
        assert_eq!(tab.scroll, 0);
        assert_eq!(tab.horizontal_scroll, 3);
        assert!(tab.is_line_folded(0));
        assert_eq!(app.focus, FocusPanel::Editor);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("reopened main.rs"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn reopen_closed_editor_restores_dirty_and_untitled_buffers() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-reopen-dirty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("dirty.rs");
        fs::write(&file, "fn dirty() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&file);
        app.active_tab_mut().unwrap().insert_text("// unsaved\n");
        app.run_command(CommandAction::CloseActiveTab).unwrap();
        let discard_index = app
            .quick_panel
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::DiscardAndCloseTab(0)))
            .unwrap();
        app.activate_quick_row(discard_index);
        assert!(app.tabs.is_empty());
        assert_eq!(fs::read_to_string(&file).unwrap(), "fn dirty() {}\n");

        app.run_command(CommandAction::ReopenClosedEditor).unwrap();
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.path, file.canonicalize().unwrap());
        assert!(tab.dirty);
        assert_eq!(tab.text(), "// unsaved\nfn dirty() {}\n");

        app.run_command(CommandAction::NewUntitledFile).unwrap();
        app.active_tab_mut().unwrap().insert_text("scratch\n");
        let placeholder = app.active_tab().unwrap().path.clone();
        app.run_command(CommandAction::CloseActiveTab).unwrap();
        let discard_index = app
            .quick_panel
            .as_ref()
            .unwrap()
            .items
            .iter()
            .position(|item| item.command == Some(CommandAction::DiscardAndCloseTab(1)))
            .unwrap();
        app.activate_quick_row(discard_index);
        assert_eq!(app.tabs.len(), 1);
        assert!(!placeholder.exists());

        app.run_command(CommandAction::ReopenClosedEditor).unwrap();
        let tab = app.active_tab().unwrap();
        assert!(tab.untitled);
        assert!(tab.dirty);
        assert_eq!(tab.title, "Untitled-1");
        assert_eq!(tab.text(), "scratch\n");
        assert!(!placeholder.exists());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn close_other_editors_closes_clean_targets_and_keeps_dirty_tabs() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-close-other-editors-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let files = ["a.rs", "b.rs", "c.rs", "d.rs"].map(|name| {
            let path = root.join(name);
            fs::write(
                &path,
                format!("fn {}() {{}}\n", name.trim_end_matches(".rs")),
            )
            .unwrap();
            path
        });

        let mut app = App::new(root.clone()).unwrap();
        for file in &files {
            app.open_file(file);
        }
        app.active_tab = Some(1);
        app.editor_split = Some(3);
        app.tabs[2].insert_text("// dirty\n");

        app.run_command(CommandAction::CloseOtherTabs).unwrap();

        let titles = app
            .tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, vec!["b.rs", "c.rs"]);
        assert_eq!(app.active_tab().unwrap().title, "b.rs");
        assert!(app.tabs[1].dirty);
        assert_eq!(app.editor_split, None);
        let closed_titles = app
            .closed_tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(closed_titles, vec!["a.rs", "d.rs"]);
        assert_eq!(
            app.message.as_deref(),
            Some("closed 2 other clean editor tab(s); kept 1 dirty tab(s) open")
        );

        app.run_command(CommandAction::ReopenClosedEditor).unwrap();
        assert_eq!(app.active_tab().unwrap().title, "d.rs");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn close_tabs_to_right_keeps_dirty_right_tabs_and_reopens_rightmost_clean_tab() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-close-right-editors-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let files = ["a.rs", "b.rs", "c.rs", "d.rs"].map(|name| {
            let path = root.join(name);
            fs::write(
                &path,
                format!("fn {}() {{}}\n", name.trim_end_matches(".rs")),
            )
            .unwrap();
            path
        });

        let mut app = App::new(root.clone()).unwrap();
        for file in &files {
            app.open_file(file);
        }
        app.active_tab = Some(1);
        app.editor_split = Some(3);
        app.tabs[2].insert_text("// dirty\n");

        app.run_command(CommandAction::CloseTabsToRight).unwrap();

        let titles = app
            .tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(titles, vec!["a.rs", "b.rs", "c.rs"]);
        assert_eq!(app.active_tab().unwrap().title, "b.rs");
        assert!(app.tabs[2].dirty);
        assert_eq!(app.editor_split, None);
        assert_eq!(app.closed_tabs.last().unwrap().title, "d.rs");
        assert_eq!(
            app.message.as_deref(),
            Some("closed 1 clean editor tab(s) to the right; kept 1 dirty tab(s) open")
        );

        app.run_command(CommandAction::ReopenClosedEditor).unwrap();
        assert_eq!(app.active_tab().unwrap().title, "d.rs");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn close_all_editors_closes_clean_tabs_and_keeps_dirty_tabs() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-close-all-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let files = ["a.rs", "b.rs", "c.rs"].map(|name| {
            let path = root.join(name);
            fs::write(
                &path,
                format!("fn {}() {{}}\n", name.trim_end_matches(".rs")),
            )
            .unwrap();
            path
        });

        let mut app = App::new(root.clone()).unwrap();
        for file in &files {
            app.open_file(file);
        }
        app.active_tab = Some(0);
        app.editor_split = Some(2);
        app.tabs[1].insert_text("// dirty\n");

        app.run_command(CommandAction::CloseAllTabs).unwrap();

        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab().unwrap().title, "b.rs");
        assert!(app.active_tab().unwrap().dirty);
        assert_eq!(app.editor_split, None);
        let closed_titles = app
            .closed_tabs
            .iter()
            .map(|tab| tab.title.as_str())
            .collect::<Vec<_>>();
        assert_eq!(closed_titles, vec!["a.rs", "c.rs"]);
        assert_eq!(
            app.message.as_deref(),
            Some("closed 2 clean editor tab(s); kept 1 dirty tab(s) open")
        );

        app.run_command(CommandAction::ReopenClosedEditor).unwrap();
        assert_eq!(app.active_tab().unwrap().title, "c.rs");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn format_document_runs_rustfmt_and_marks_buffer_dirty() {
        if Command::new("rustfmt").arg("--version").output().is_err() {
            return;
        }

        let root = std::env::temp_dir().join(format!("tscode-test-format-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(root.join("Cargo.toml"), "[package]\nedition = \"2024\"\n").unwrap();
        let path = src.join("main.rs");
        fs::write(&path, "fn main(){println!(\"hi\");}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.run_command(CommandAction::FormatDocument).unwrap();

        let tab = app.active_tab().unwrap();
        assert_eq!(tab.lines, vec!["fn main() {", "    println!(\"hi\");", "}"]);
        assert!(tab.dirty);
        assert_eq!(
            app.message.as_deref(),
            Some("formatted main.rs with rustfmt")
        );

        app.undo_active_tab();
        assert_eq!(
            app.active_tab().unwrap().text(),
            "fn main(){println!(\"hi\");}\n"
        );
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "fn main(){println!(\"hi\");}\n"
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn revert_file_discards_dirty_buffer_and_reloads_disk() {
        let root = std::env::temp_dir().join(format!("tscode-test-revert-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "disk one\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.active_tab_mut().unwrap().insert_text("dirty ");
        assert_eq!(app.active_tab().unwrap().lines, vec!["dirty disk one"]);
        assert!(app.active_tab().unwrap().dirty);

        fs::write(&path, "disk two\n").unwrap();
        app.run_command(CommandAction::RevertFile).unwrap();

        let tab = app.active_tab().unwrap();
        assert_eq!(tab.lines, vec!["disk two"]);
        assert!(!tab.dirty);
        assert_eq!(tab.text(), "disk two\n");
        assert!(tab.selection_range().is_none());
        assert_eq!(app.message.as_deref(), Some("reverted main.rs from disk"));
        assert!(!app.active_tab_mut().unwrap().undo());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn clean_open_tab_reloads_external_file_change() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-external-reload-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "disk one\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        thread::sleep(Duration::from_millis(10));
        fs::write(&path, "external disk two\n").unwrap();

        assert!(app.check_external_file_changes());
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.lines, vec!["external disk two"]);
        assert!(!tab.dirty);
        assert_eq!(tab.external_state, ExternalFileState::Clean);
        assert_eq!(
            app.message.as_deref(),
            Some("reloaded 1 clean tab(s) from disk")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn dirty_open_tab_marks_external_change_and_refuses_save_overwrite() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-external-conflict-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "disk one\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.active_tab_mut().unwrap().insert_text("dirty ");
        thread::sleep(Duration::from_millis(10));
        fs::write(&path, "external changed on disk\n").unwrap();

        assert!(app.check_external_file_changes());
        let tab = app.active_tab().unwrap();
        assert!(tab.dirty);
        assert_eq!(tab.external_state, ExternalFileState::Modified);

        app.run_command(CommandAction::SaveFile).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "external changed on disk\n"
        );
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("modified on disk"))
        );

        app.run_command(CommandAction::RevertFile).unwrap();
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.text(), "external changed on disk\n");
        assert!(!tab.dirty);
        assert_eq!(tab.external_state, ExternalFileState::Clean);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deleted_open_file_marks_disk_deleted_and_save_refuses_recreate() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-external-delete-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "disk one\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        fs::remove_file(&path).unwrap();

        assert!(app.check_external_file_changes());
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.external_state, ExternalFileState::Deleted);
        assert!(!tab.dirty);
        assert!(!path.exists());

        app.run_command(CommandAction::SaveFile).unwrap();
        assert!(!path.exists());
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("deleted on disk"))
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
    fn prompt_input_supports_cursor_editing_and_paste() {
        let root = std::env::temp_dir().join(format!("tscode-test-prompt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.start_prompt(PromptKind::Search, "alpha");
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE))
            .unwrap();

        let prompt = app.prompt.as_ref().unwrap();
        assert_eq!(prompt.input, "alpXha");
        assert_eq!(prompt.cursor, 4);
        assert_eq!(
            editable_text_with_cursor(&prompt.input, prompt.cursor),
            "alpX|ha"
        );

        app.handle_key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::NONE))
            .unwrap();
        let prompt = app.prompt.as_ref().unwrap();
        assert_eq!(prompt.input, "alpa");
        assert_eq!(prompt.cursor, 3);

        app.handle_paste(" β\nz".to_owned()).unwrap();
        let prompt = app.prompt.as_ref().unwrap();
        assert_eq!(prompt.input, "alp β za");
        assert_eq!(prompt.cursor, 7);

        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL))
            .unwrap();
        let prompt = app.prompt.as_ref().unwrap();
        assert_eq!(prompt.input, "a");
        assert_eq!(prompt.cursor, 0);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn quick_panel_query_supports_cursor_editing() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-quick-query-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_quick_panel(QuickPanelKind::CommandPalette)
            .unwrap();
        for c in "save".chars() {
            app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE))
                .unwrap();
        }
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE))
            .unwrap();
        app.handle_key(KeyEvent::new(KeyCode::Char('X'), KeyModifiers::NONE))
            .unwrap();

        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.query, "saXve");
        assert_eq!(panel.query_cursor, 3);
        assert_eq!(
            editable_text_with_cursor(&panel.query, panel.query_cursor),
            "saX|ve"
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::CONTROL))
            .unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.query, "saX");
        assert_eq!(panel.query_cursor, 3);

        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))
            .unwrap();
        app.handle_paste("run ".to_owned()).unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.query, "run saX");
        assert_eq!(panel.query_cursor, 4);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
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

        let commands = app.command_palette_items("open folder");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::OpenFolder))
        );
        let commands = app.command_palette_items("reopen closed");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ReopenClosedEditor))
        );
        let commands = app.command_palette_items("open editors");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowOpenEditors))
        );
        let commands = app.command_palette_items("close other editors");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CloseOtherTabs))
        );
        let commands = app.command_palette_items("close editors right");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CloseTabsToRight))
        );
        let commands = app.command_palette_items("close all editors");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CloseAllTabs))
        );

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
        let commands = app.command_palette_items("next match");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::AddSelectionToNextMatch))
        );
        let commands = app.command_palette_items("all occurrences");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::SelectAllOccurrences))
        );
        let commands = app.command_palette_items("word wrap");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ToggleWordWrap))
        );
        let commands = app.command_palette_items("trim whitespace");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::TrimTrailingWhitespace))
        );
        let commands = app.command_palette_items("revert file");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RevertFile))
        );
        let commands = app.command_palette_items("reload disk");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RevertFile))
        );
        let commands = app.command_palette_items("save as");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::SaveAs))
        );
        let commands = app.command_palette_items("format document");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::FormatDocument))
        );
        let commands = app.command_palette_items("incoming calls");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowIncomingCalls))
        );
        let commands = app.command_palette_items("outgoing calls");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowOutgoingCalls))
        );
        let commands = app.command_palette_items("highlight symbol");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::HighlightSymbol))
        );
        let commands = app.command_palette_items("clear symbol highlights");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ClearDocumentHighlights))
        );
        let commands = app.command_palette_items("replace files");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::WorkspaceReplace))
        );
        let commands = app.command_palette_items("workspace check");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RunWorkspaceCheck))
        );
        let commands = app.command_palette_items("lsp diagnostics");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RunLspDiagnostics))
        );
        let commands = app.command_palette_items("show problems");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowProblems))
        );
        let commands = app.command_palette_items("source control");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ShowSourceControl))
        );
        let commands = app.command_palette_items("stage all");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::StageAllChanges))
        );
        let commands = app.command_palette_items("unstage all");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::UnstageAllChanges))
        );
        let commands = app.command_palette_items("sort explorer size");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::SortExplorerBySize))
        );
        let commands = app.command_palette_items("compare selected files");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CompareSelectedFiles))
        );
        let commands = app.command_palette_items("run task");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RunTask))
        );
        let commands = app.command_palette_items("symbol file");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::DocumentSymbols))
        );
        let commands = app.command_palette_items("workspace symbol");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::WorkspaceSymbols))
        );
        let commands = app.command_palette_items("go definition");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::GoToDefinition))
        );
        let commands = app.command_palette_items("type definition");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::GoToTypeDefinition))
        );
        let commands = app.command_palette_items("implementation");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::GoToImplementation))
        );
        let commands = app.command_palette_items("matching bracket");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::GoToMatchingBracket))
        );
        let commands = app.command_palette_items("find references");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::FindReferences))
        );
        let commands = app.command_palette_items("code action");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CodeAction))
        );
        let commands = app.command_palette_items("signature help");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::SignatureHelp))
        );
        let commands = app.command_palette_items("rename symbol");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RenameSymbol))
        );
        let commands = app.command_palette_items("trigger suggest");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::TriggerSuggest))
        );
        let commands = app.command_palette_items("block comment");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ToggleBlockComment))
        );
        let commands = app.command_palette_items("copy relative path");
        assert!(commands.iter().any(|item| {
            item.command == Some(CommandAction::CopyActiveFileRelativePath)
                || item.command == Some(CommandAction::CopySelectedExplorerRelativePath)
        }));
        let commands = app.command_palette_items("run selection terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RunSelectionInTerminal))
        );
        let commands = app.command_palette_items("run active file terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RunActiveFileInTerminal))
        );
        let commands = app.command_palette_items("copy terminal selection");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CopyTerminalSelection))
        );
        let commands = app.command_palette_items("copy terminal output");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CopyTerminalOutput))
        );
        let commands = app.command_palette_items("paste clipboard terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::PasteClipboardToTerminal))
        );
        let commands = app.command_palette_items("find terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::FindInTerminal))
        );
        let commands = app.command_palette_items("run terminal command");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RunTerminalCommand))
        );
        let commands = app.command_palette_items("recent terminal command");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RunRecentTerminalCommand))
        );
        let commands = app.command_palette_items("next terminal search");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::TerminalSearchNext))
        );
        let commands = app.command_palette_items("new terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::NewTerminal))
        );
        let commands = app.command_palette_items("new terminal here");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::NewTerminalHere))
        );
        let commands = app.command_palette_items("split terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::SplitTerminal))
        );
        let commands = app.command_palette_items("rename terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::RenameTerminal))
        );
        let commands = app.command_palette_items("next terminal");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::NextTerminal))
        );
        let commands = app.command_palette_items("scroll terminal bottom");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::ScrollTerminalToBottom))
        );
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_submission_text_uses_carriage_return_enters() {
        assert_eq!(terminal_submission_text("echo ok"), "echo ok\r");
        assert_eq!(
            terminal_submission_text("echo one\necho two\n"),
            "echo one\recho two\r"
        );
        assert_eq!(
            terminal_submission_text("echo one\r\necho two"),
            "echo one\recho two\r"
        );
        assert_eq!(
            normalize_terminal_history_command(" echo one\r\necho two\r "),
            "echo one\necho two"
        );
    }

    #[test]
    fn run_command_for_file_detects_supported_source_files() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-run-command-for-file-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let script = root.join("script.sh");
        let readme = root.join("README.md");
        fs::write(&script, "printf ok\n").unwrap();
        fs::write(&readme, "# docs\n").unwrap();

        assert_eq!(
            run_command_for_file(&root, &script),
            Some(FileRunCommand {
                command: "sh script.sh".to_owned(),
                cwd: root.clone(),
            })
        );
        assert_eq!(run_command_for_file(&root, &readme), None);

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn task_panel_detects_vscode_package_cargo_and_make_tasks() {
        let root = std::env::temp_dir().join(format!("tscode-test-tasks-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join(".vscode")).unwrap();
        fs::write(
            root.join(".vscode/tasks.json"),
            r#"
            {
              // JSONC comments are valid in VS Code task files.
              "version": "2.0.0",
              "tasks": [
                {
                  "label": "smoke",
                  "type": "shell",
                  "command": "printf",
                  "args": ["hello world"],
                  "options": { "cwd": "${workspaceFolder}" }
                }
              ]
            }
            "#,
        )
        .unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"scripts":{"test":"vitest","build":"vite build"}}"#,
        )
        .unwrap();
        fs::write(root.join("pnpm-lock.yaml"), "").unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"task_test\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(
            root.join("Makefile"),
            "install-deps:\n\t@true\n.PHONY: skip\n",
        )
        .unwrap();

        let tasks = collect_workspace_tasks(&root);
        assert!(tasks.iter().any(|task| {
            task.label == "task: smoke" && task.command == "printf \"hello world\""
        }));
        assert!(
            tasks
                .iter()
                .any(|task| { task.label == "pnpm: build" && task.command == "pnpm run build" })
        );
        assert!(tasks.iter().any(|task| task.label == "cargo: test"));
        assert!(tasks.iter().any(|task| {
            task.label == "make: install-deps" && task.command == "make install-deps"
        }));

        let mut app = App::new(root.clone()).unwrap();
        app.run_command(CommandAction::RunTask).unwrap();
        assert!(
            app.quick_panel
                .as_ref()
                .is_some_and(|panel| panel.kind == QuickPanelKind::Tasks)
        );
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn run_task_item_executes_in_new_real_pty_terminal() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-run-task-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let out = root.join("task.out");
        let command = format!(
            "printf task-ok > {}",
            shell_escape_task_arg(&out.to_string_lossy())
        );

        let mut app = App::new(root.clone()).unwrap();
        let initial_terminals = app.terminals.len();
        app.run_task_item(QuickItem {
            label: "shell smoke".to_owned(),
            detail: command.clone(),
            path: root.clone(),
            line: None,
            col: None,
            preview: Some("test".to_owned()),
            command: None,
        })
        .unwrap();

        assert_eq!(app.focus, FocusPanel::Terminal);
        assert_eq!(app.terminals.len(), initial_terminals + 1);
        assert!(app.active_terminal().title.starts_with("task: shell smoke"));
        assert_eq!(app.terminal_command_history.first(), Some(&command));
        for _ in 0..50 {
            app.drain_terminal();
            if out.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(fs::read_to_string(&out).unwrap(), "task-ok");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn run_selection_in_terminal_executes_current_line_in_real_pty() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-run-terminal-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let out = root.join("out.txt");
        let path = root.join("cmd.txt");
        fs::write(&path, format!("printf run-ok > {}\n", out.display())).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;
        app.run_selection_in_terminal().unwrap();
        assert_eq!(app.focus, FocusPanel::Terminal);
        assert_eq!(
            app.terminal_command_history.first(),
            Some(&format!("printf run-ok > {}", out.display()))
        );

        for _ in 0..50 {
            app.drain_terminal();
            if out.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        assert_eq!(fs::read_to_string(&out).unwrap(), "run-ok");
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn terminal_command_prompt_and_recent_picker_send_real_pty_input() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-command-history-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let out = root.join("history.out");
        let command = format!(
            "printf x >> {}",
            shell_escape_task_arg(&out.to_string_lossy())
        );

        let mut app = App::new(root.clone()).unwrap();
        app.run_command(CommandAction::RunTerminalCommand).unwrap();
        assert_eq!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(&PromptKind::RunTerminalCommand)
        );
        app.prompt.as_mut().unwrap().input = command.clone();
        app.finish_prompt().unwrap();
        assert_eq!(app.focus, FocusPanel::Terminal);
        assert_eq!(app.terminal_command_history, vec![command.clone()]);

        for _ in 0..50 {
            app.drain_terminal();
            if fs::read_to_string(&out).unwrap_or_default() == "x" {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(fs::read_to_string(&out).unwrap(), "x");

        app.run_command(CommandAction::RunRecentTerminalCommand)
            .unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::TerminalCommandHistory);
        assert_eq!(panel.items[0].detail, command);
        app.activate_selected_quick_item();
        assert_eq!(app.terminal_command_history, vec![command]);

        for _ in 0..50 {
            app.drain_terminal();
            if fs::read_to_string(&out).unwrap_or_default() == "xx" {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(fs::read_to_string(&out).unwrap(), "xx");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn run_file_actions_execute_saved_files_in_new_real_pty_terminals() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-run-file-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested")).unwrap();
        let editor_script = root.join("editor.sh");
        let explorer_script = root.join("nested").join("explorer.sh");
        let editor_out = root.join("editor.out");
        let explorer_out = root.join("nested").join("explorer.out");
        fs::write(&editor_script, "printf editor-run > editor.out\n").unwrap();
        fs::write(&explorer_script, "printf explorer-run > explorer.out\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let editor_script = editor_script.canonicalize().unwrap();
        let explorer_script = explorer_script.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        let initial_terminals = app.terminals.len();

        app.open_file(&editor_script);
        app.run_active_file_in_terminal().unwrap();
        assert_eq!(app.focus, FocusPanel::Terminal);
        assert_eq!(app.terminals.len(), initial_terminals + 1);
        assert!(app.active_terminal().title.starts_with("run: editor.sh"));
        assert_eq!(app.active_terminal().cwd, canonical_root);

        for _ in 0..50 {
            app.drain_terminal();
            if editor_out.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(fs::read_to_string(&editor_out).unwrap(), "editor-run");

        app.reveal_path(&explorer_script).unwrap();
        app.run_selected_explorer_file_in_terminal().unwrap();
        assert_eq!(app.terminals.len(), initial_terminals + 2);
        assert!(app.active_terminal().title.starts_with("run: explorer.sh"));
        assert_eq!(
            app.active_terminal().cwd,
            canonical_root.join("nested").canonicalize().unwrap()
        );

        for _ in 0..50 {
            app.drain_terminal();
            if explorer_out.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(fs::read_to_string(&explorer_out).unwrap(), "explorer-run");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_active_file_blocks_unsaved_or_untitled_buffers() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-run-file-dirty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let script = root.join("script.sh");
        fs::write(&script, "printf disk\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let script = script.canonicalize().unwrap();
        let mut app = App::new(canonical_root).unwrap();
        let initial_terminals = app.terminals.len();
        app.open_file(&script);
        app.active_tab_mut().unwrap().insert_text("printf dirty\n");
        app.run_active_file_in_terminal().unwrap();
        assert_eq!(app.terminals.len(), initial_terminals);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("save script.sh before running it"))
        );

        app.run_command(CommandAction::NewUntitledFile).unwrap();
        app.run_active_file_in_terminal().unwrap();
        assert_eq!(app.terminals.len(), initial_terminals);
        assert_eq!(
            app.message.as_deref(),
            Some("save the Untitled file before running it")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn terminal_search_finds_scrollback_and_moves_between_matches() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-search-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.focus = FocusPanel::Terminal;
        app.active_terminal_mut()
            .shell
            .send_text("printf 'alpha-one\\nbeta\\nalpha-two\\n'\r")
            .unwrap();

        for _ in 0..50 {
            app.drain_terminal();
            if app
                .active_terminal_mut()
                .shell
                .search_matches("alpha")
                .len()
                >= 2
            {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        app.terminal_search_from_prompt("alpha".to_owned());
        let Some((selected, count)) = app.active_terminal_search_summary() else {
            panic!("terminal search summary missing");
        };
        assert!(count >= 2);
        assert_eq!(selected, count);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("terminal find"))
        );

        let active_row = app.terminal_search.as_ref().unwrap().matches
            [app.terminal_search.as_ref().unwrap().selected]
            .row;
        let top = app.active_terminal_mut().shell.visible_top_row();
        let (height, _) = app.active_terminal().shell.size();
        assert!(active_row >= top);
        assert!(active_row < top + height as usize);
        let local_row = (active_row - top) as u16;
        let active_terminal = app.active_terminal;
        assert!(
            app.terminal_search_ranges_for_terminal_row(active_terminal, local_row)
                .iter()
                .any(|(_, _, active)| *active)
        );

        let before = app.terminal_search.as_ref().unwrap().selected;
        app.next_terminal_search_match();
        assert_ne!(app.terminal_search.as_ref().unwrap().selected, before);
        app.previous_terminal_search_match();
        assert_eq!(app.terminal_search.as_ref().unwrap().selected, before);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_search_uses_dirty_open_buffer_text() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-workspace-search-dirty-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("dirty.txt"), "disk-only needle\n").unwrap();
        fs::write(root.join("clean.txt"), "clean needle\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let dirty = canonical_root.join("dirty.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&dirty);
        {
            let tab = app.active_tab_mut().unwrap();
            assert!(tab.replace_entire_text_as_edit("memory-only unsaved-needle\n"));
        }

        let dirty_items = app.workspace_search_items("unsaved-needle").unwrap();
        assert_eq!(dirty_items.len(), 1);
        assert_eq!(dirty_items[0].path, dirty);
        assert_eq!(dirty_items[0].line, Some(0));
        assert_eq!(dirty_items[0].col, Some("memory-only ".chars().count()));
        assert!(dirty_items[0].detail.contains("(unsaved)"));
        assert!(
            dirty_items[0]
                .preview
                .as_deref()
                .is_some_and(|preview| preview.contains("unsaved-needle"))
        );

        let stale_items = app.workspace_search_items("disk-only needle").unwrap();
        assert!(stale_items.is_empty());

        let clean_items = app.workspace_search_items("clean needle").unwrap();
        assert_eq!(clean_items.len(), 1);
        assert!(!clean_items[0].detail.contains("(unsaved)"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn trigger_suggest_completes_workspace_symbol_at_cursor() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-completion-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {\n    make_cl\n}\n").unwrap();
        fs::write(root.join("src/client.rs"), "fn make_client() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let main = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root).unwrap();
        app.open_file(&main);
        app.active_tab_mut()
            .unwrap()
            .set_cursor(1, "    make_cl".chars().count());

        app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::CONTROL))
            .unwrap();
        let panel = app.quick_panel.as_ref().expect("completion panel");
        assert_eq!(panel.kind, QuickPanelKind::Completions);
        assert_eq!(panel.query, "make_cl");
        let index = panel
            .items
            .iter()
            .position(|item| item.label == "make_client")
            .expect("make_client completion");
        app.set_quick_selection(index);
        app.activate_selected_quick_item();

        let tab = app.active_tab_mut().unwrap();
        assert_eq!(tab.lines[1], "    make_client");
        assert_eq!(
            tab.cursor_position(),
            (1, "    make_client".chars().count())
        );
        assert!(tab.dirty);
        assert!(tab.undo());
        assert_eq!(tab.lines[1], "    make_cl");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completion_items_include_dirty_open_buffer_identifiers() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-completion-dirty-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.rs"), "fn main() {\n    memory_h\n}\n").unwrap();
        fs::write(root.join("helper.rs"), "fn disk_helper() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let main = canonical_root.join("main.rs");
        let helper = canonical_root.join("helper.rs");
        let mut app = App::new(canonical_root).unwrap();
        app.open_file(&helper);
        {
            let tab = app.active_tab_mut().unwrap();
            assert!(tab.replace_entire_text_as_edit("fn memory_helper() {}\n"));
        }
        app.open_file(&main);

        let items = app.completion_items("memory_h").unwrap();
        let item = items
            .iter()
            .find(|item| item.label == "memory_helper")
            .expect("dirty helper identifier completion");
        assert!(item.detail.contains("helper.rs"));
        assert!(
            item.preview
                .as_deref()
                .is_some_and(|preview| preview.contains("memory_helper"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn completion_keywords_include_control_words() {
        assert!(keyword_completion_rank("return", "ret", 0, 2, usize::MAX).is_some());
        assert!(keyword_completion_rank("await", "awa", 0, 2, usize::MAX).is_some());
        assert!(completion_rank("return", "ret", 0, 1, 0).is_none());
    }

    #[test]
    fn workspace_replace_writes_files_updates_tabs_and_skips_dirty_buffers() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-workspace-replace-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let target = root.join("target");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(src.join("main.rs"), "needle one\nneedle two\n").unwrap();
        fs::write(root.join("README.md"), "docs needle\n").unwrap();
        fs::write(root.join("dirty.txt"), "dirty needle\n").unwrap();
        fs::write(target.join("generated.txt"), "needle generated\n").unwrap();
        fs::write(root.join("binary.bin"), b"needle\0skip").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let main = canonical_root.join("src/main.rs");
        let readme = canonical_root.join("README.md");
        let dirty = canonical_root.join("dirty.txt");
        let generated = canonical_root.join("target/generated.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&main);
        app.open_file(&dirty);
        {
            let tab = app.active_tab_mut().unwrap();
            tab.set_cursor(0, tab.lines[0].chars().count());
            tab.insert_text(" unsaved needle");
        }

        app.replace_workspace_matches("needle".to_owned(), "value".to_owned())
            .unwrap();

        assert_eq!(fs::read_to_string(&main).unwrap(), "value one\nvalue two\n");
        assert_eq!(fs::read_to_string(&readme).unwrap(), "docs value\n");
        assert_eq!(fs::read_to_string(&dirty).unwrap(), "dirty needle\n");
        assert_eq!(
            fs::read_to_string(&generated).unwrap(),
            "needle generated\n"
        );
        let main_tab = app.tabs.iter().find(|tab| tab.path == main).unwrap();
        assert_eq!(main_tab.lines, vec!["value one", "value two"]);
        assert!(!main_tab.dirty);
        let dirty_tab = app.tabs.iter().find(|tab| tab.path == dirty).unwrap();
        assert!(dirty_tab.dirty);
        assert!(dirty_tab.text().contains("unsaved needle"));
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("skipped 1 dirty open file"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn identifier_text_replacement_respects_symbol_boundaries() {
        let (text, count) = replace_identifier_occurrences_in_text(
            "make_client make_client_extra object.make_client\n",
            "make_client",
            "build_client",
        );
        assert_eq!(count, 2);
        assert_eq!(text, "build_client make_client_extra object.build_client\n");
    }

    #[test]
    fn identifier_range_at_char_ignores_columns_far_past_line_end() {
        assert_eq!(identifier_at_char("x", 1), Some("x".to_owned()));
        assert_eq!(identifier_at_char("foo", 3), Some("foo".to_owned()));
        assert_eq!(identifier_at_char("foo;", 3), Some("foo".to_owned()));
        assert_eq!(identifier_at_char("foo", 22), None);
    }

    #[test]
    fn rename_symbol_updates_open_buffers_and_closed_workspace_files() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-rename-symbol-{}", std::process::id()));
        let src = root.join("src");
        let target = root.join("target");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(src.join("lib.rs"), "pub fn make_client() {}\n").unwrap();
        fs::write(
            src.join("main.rs"),
            "fn main() {\n    make_client();\n    make_client_extra();\n}\n",
        )
        .unwrap();
        fs::write(root.join("dirty.rs"), "let cached = make_client();\n").unwrap();
        fs::write(
            root.join("README.md"),
            "docs make_client make_client_extra\n",
        )
        .unwrap();
        fs::write(target.join("generated.rs"), "make_client generated\n").unwrap();
        fs::write(root.join("binary.bin"), b"make_client\0skip").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let main = canonical_root.join("src/main.rs");
        let dirty = canonical_root.join("dirty.rs");
        let readme = canonical_root.join("README.md");
        let generated = canonical_root.join("target/generated.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&dirty);
        {
            let tab = app.active_tab_mut().unwrap();
            tab.set_cursor(0, tab.lines[0].chars().count());
            tab.insert_text("\nlet unsaved = make_client();");
        }
        app.open_file(&main);
        app.active_tab_mut().unwrap().set_cursor(1, 6);

        app.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(
            app.prompt,
            Some(PromptState {
                kind: PromptKind::RenameSymbol {
                    old: "make_client".to_owned()
                },
                input: "make_client".to_owned(),
                cursor: "make_client".chars().count(),
            })
        );
        app.prompt = None;

        app.rename_symbol_occurrences("make_client".to_owned(), "build_client".to_owned())
            .unwrap();

        let main_tab = app.tabs.iter().find(|tab| tab.path == main).unwrap();
        assert!(main_tab.dirty);
        assert!(main_tab.text().contains("build_client();"));
        assert!(main_tab.text().contains("make_client_extra();"));
        assert!(!main_tab.text().contains("make_client();"));

        let dirty_tab = app.tabs.iter().find(|tab| tab.path == dirty).unwrap();
        assert!(dirty_tab.dirty);
        assert_eq!(dirty_tab.text().matches("build_client").count(), 2);

        assert_eq!(
            fs::read_to_string(&main).unwrap(),
            "fn main() {\n    make_client();\n    make_client_extra();\n}\n"
        );
        assert_eq!(
            fs::read_to_string(&dirty).unwrap(),
            "let cached = make_client();\n"
        );
        assert_eq!(
            fs::read_to_string(&lib).unwrap(),
            "pub fn build_client() {}\n"
        );
        assert_eq!(
            fs::read_to_string(&readme).unwrap(),
            "docs build_client make_client_extra\n"
        );
        assert_eq!(
            fs::read_to_string(&generated).unwrap(),
            "make_client generated\n"
        );
        assert_eq!(app.search_needle.as_deref(), Some("build_client"));
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("5 occurrence"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lsp_workspace_edit_updates_open_buffers_and_closed_files() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-lsp-workspace-edit-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.rs"), "fn main() {\n    old_name();\n}\n").unwrap();
        fs::write(root.join("lib.rs"), "pub fn old_name() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let main = canonical_root.join("main.rs");
        let lib = canonical_root.join("lib.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&main);

        let edit = lsp::LspWorkspaceEdit {
            server: "mock-rename-lsp".to_owned(),
            edits: vec![
                lsp::LspTextEdit {
                    path: main.clone(),
                    start_line: 1,
                    start_utf16_col: "    ".chars().count(),
                    end_line: 1,
                    end_utf16_col: "    old_name".chars().count(),
                    new_text: "new_name".to_owned(),
                },
                lsp::LspTextEdit {
                    path: lib.clone(),
                    start_line: 0,
                    start_utf16_col: "pub fn ".chars().count(),
                    end_line: 0,
                    end_utf16_col: "pub fn old_name".chars().count(),
                    new_text: "new_name".to_owned(),
                },
            ],
        };

        let summary = app
            .apply_lsp_workspace_edit(edit)
            .unwrap()
            .expect("applied LSP edit");
        assert_eq!(
            summary,
            LspRenameSummary {
                server: "mock-rename-lsp".to_owned(),
                edit_count: 2,
                open_count: 1,
                file_count: 1,
            }
        );

        let main_tab = app.tabs.iter().find(|tab| tab.path == main).unwrap();
        assert!(main_tab.dirty);
        assert_eq!(main_tab.text(), "fn main() {\n    new_name();\n}\n");
        assert_eq!(
            fs::read_to_string(&main).unwrap(),
            "fn main() {\n    old_name();\n}\n"
        );
        assert_eq!(fs::read_to_string(&lib).unwrap(), "pub fn new_name() {}\n");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(canonical_root);
    }

    #[test]
    fn code_action_panel_applies_lsp_workspace_edit_to_open_buffer() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-code-action-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.rs"), "fn main() {\n    old_name();\n}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let main = canonical_root.join("main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&main);
        app.lsp_code_actions = vec![lsp::LspCodeAction {
            title: "Replace old_name with new_name".to_owned(),
            kind: Some("quickfix".to_owned()),
            is_preferred: true,
            edit: Some(lsp::LspWorkspaceEdit {
                server: "mock-code-action".to_owned(),
                edits: vec![lsp::LspTextEdit {
                    path: main.clone(),
                    start_line: 1,
                    start_utf16_col: "    ".chars().count(),
                    end_line: 1,
                    end_utf16_col: "    old_name".chars().count(),
                    new_text: "new_name".to_owned(),
                }],
            }),
            command_title: None,
            command: None,
            server: "mock-code-action".to_owned(),
        }];

        app.open_quick_panel(QuickPanelKind::CodeActions).unwrap();
        let panel = app.quick_panel.as_ref().expect("code action panel");
        assert_eq!(panel.kind, QuickPanelKind::CodeActions);
        assert_eq!(panel.items.len(), 1);
        assert_eq!(panel.items[0].label, "Replace old_name with new_name");
        assert!(panel.items[0].detail.contains("1 edit"));

        app.activate_selected_quick_item();

        let tab = app.active_tab().unwrap();
        assert_eq!(tab.text(), "fn main() {\n    new_name();\n}\n");
        assert!(tab.dirty);
        assert_eq!(
            fs::read_to_string(&main).unwrap(),
            "fn main() {\n    old_name();\n}\n"
        );
        assert!(app.lsp_code_actions.is_empty());
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("applied code action"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(canonical_root);
    }

    #[test]
    fn lsp_text_edit_application_respects_utf16_columns() {
        let text = "let crab = \"🦀\";\ncrab();\n";
        let edits = vec![
            lsp::LspTextEdit {
                path: PathBuf::from("main.rs"),
                start_line: 0,
                start_utf16_col: "let crab = \"🦀\";".encode_utf16().count(),
                end_line: 0,
                end_utf16_col: "let crab = \"🦀\";".encode_utf16().count(),
                new_text: " // ok".to_owned(),
            },
            lsp::LspTextEdit {
                path: PathBuf::from("main.rs"),
                start_line: 1,
                start_utf16_col: 0,
                end_line: 1,
                end_utf16_col: "crab".encode_utf16().count(),
                new_text: "ferris".to_owned(),
            },
        ];

        let (updated, count) = apply_lsp_text_edits_to_text(text, &edits).unwrap();
        assert_eq!(count, 2);
        assert_eq!(updated, "let crab = \"🦀\"; // ok\nferris();\n");
    }

    #[test]
    fn terminal_focus_shortcuts_work_from_inside_terminal_focus() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-focus-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.focus = FocusPanel::Terminal;

        app.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.focus, FocusPanel::Explorer);

        app.handle_key(KeyEvent::new(KeyCode::Char('`'), KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.focus, FocusPanel::Terminal);

        let terminal_count = app.terminals.len();
        app.handle_key(KeyEvent::new(
            KeyCode::Char('`'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.terminals.len(), terminal_count + 1);
        assert_eq!(app.focus, FocusPanel::Terminal);

        app.handle_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE))
            .unwrap();
        assert!(app.terminal_maximized);

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(!app.terminal_maximized);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_full_screen_child_receives_app_keys_before_tscode_shortcuts() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-passthrough-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.focus = FocusPanel::Terminal;
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"\x1b[?1049h");
        assert!(app.terminal_child_owns_keyboard());

        let terminal_count = app.terminals.len();

        app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(app.prompt.is_none());
        assert!(app.terminal_search.is_none());

        app.handle_key(KeyEvent::new(KeyCode::F(7), KeyModifiers::NONE))
            .unwrap();
        assert_eq!(app.terminals.len(), terminal_count);

        app.handle_key(KeyEvent::new(
            KeyCode::Char('`'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.terminals.len(), terminal_count);
        assert_eq!(app.focus, FocusPanel::Terminal);

        app.handle_key(KeyEvent::new(
            KeyCode::Char('~'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(app.terminals.len(), terminal_count);
        assert_eq!(app.focus, FocusPanel::Terminal);

        app.handle_key(KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE))
            .unwrap();
        assert!(!app.terminal_maximized);

        app.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::CONTROL))
            .unwrap();
        assert!(!app.terminal_maximized);

        app.handle_key(KeyEvent::new(KeyCode::F(6), KeyModifiers::NONE))
            .unwrap();
        assert_ne!(app.focus, FocusPanel::Terminal);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ctrl_page_keys_switch_editor_tabs_and_terminal_sessions() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-ctrl-page-navigation-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("first.rs"), "fn first() {}\n").unwrap();
        fs::write(root.join("second.rs"), "fn second() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let canonical_root = root.canonicalize().unwrap();
        let first = canonical_root.join("first.rs");
        let second = canonical_root.join("second.rs");
        app.open_file(&first);
        app.open_file(&second);
        assert_eq!(app.active_tab().unwrap().path, second);

        app.focus = FocusPanel::Editor;
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_tab().unwrap().path, first);
        assert_eq!(app.focus, FocusPanel::Editor);

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_tab().unwrap().path, second);

        app.new_terminal().unwrap();
        assert_eq!(app.active_terminal, 1);
        app.focus = FocusPanel::Terminal;
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_terminal, 0);
        assert_eq!(app.focus, FocusPanel::Terminal);

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_terminal, 1);

        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"\x1b[?1049h");
        assert!(app.terminal_child_owns_keyboard());
        app.handle_key(KeyEvent::new(KeyCode::PageUp, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_terminal, 1);
        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL))
            .unwrap();
        assert_eq!(app.active_terminal, 1);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(canonical_root);
    }

    #[test]
    fn git_status_parser_marks_files_and_dirty_parent_dirs() {
        let root = std::env::temp_dir().join(format!("tscode-test-git-{}", std::process::id()));
        let src = root.join("src");
        let entries = parse_git_status_entries_z(
            b" M src/main.rs\0?? src/new.rs\0R  src/lib.rs\0src/old.rs\0UU src/conflict.rs\0M  src/staged.rs\0MM src/both.rs\0",
            &root,
        );
        let statuses = parse_git_status_z(
            b" M src/main.rs\0?? src/new.rs\0R  src/lib.rs\0src/old.rs\0UU src/conflict.rs\0M  src/staged.rs\0MM src/both.rs\0",
            &root,
        );

        assert_eq!(
            statuses.get(&src.join("main.rs")),
            Some(&GitStatusKind::Modified)
        );
        assert_eq!(
            statuses.get(&src.join("new.rs")),
            Some(&GitStatusKind::Untracked)
        );
        assert_eq!(
            statuses.get(&src.join("lib.rs")),
            Some(&GitStatusKind::Renamed)
        );
        assert_eq!(
            statuses.get(&src.join("conflict.rs")),
            Some(&GitStatusKind::Conflicted)
        );
        let main_entry = entries
            .iter()
            .find(|entry| entry.path == src.join("main.rs"))
            .unwrap();
        assert!(main_entry.can_stage());
        assert!(!main_entry.can_unstage());
        let new_entry = entries
            .iter()
            .find(|entry| entry.path == src.join("new.rs"))
            .unwrap();
        assert!(new_entry.can_stage());
        assert!(!new_entry.can_unstage());
        let staged_entry = entries
            .iter()
            .find(|entry| entry.path == src.join("staged.rs"))
            .unwrap();
        assert!(!staged_entry.can_stage());
        assert!(staged_entry.can_unstage());
        let both_entry = entries
            .iter()
            .find(|entry| entry.path == src.join("both.rs"))
            .unwrap();
        assert!(both_entry.can_stage());
        assert!(both_entry.can_unstage());

        let dirty_dirs = git_dirty_directories(&statuses, &root);
        assert!(dirty_dirs.contains(&root));
        assert!(dirty_dirs.contains(&src));
    }

    #[test]
    fn git_diff_parser_reads_unified_zero_hunks() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-git-diff-parser-{}",
            std::process::id()
        ));
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1111111..2222222 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -2 +2 @@ pub fn value() -> i32 {
-    1
+    2
@@ -5,0 +6,2 @@
+pub fn added() {}
+pub fn added_two() {}
";

        let hunks = parse_git_diff_hunks(diff, &root);
        assert_eq!(hunks.len(), 2);
        assert_eq!(hunks[0].path, root.join("src/lib.rs"));
        assert_eq!(hunks[0].new_start, 2);
        assert_eq!(hunks[0].new_count, 1);
        assert_eq!(hunks[0].old_count, 1);
        assert_eq!(hunks[0].preview, "-    1");
        assert_eq!(hunks[1].new_start, 6);
        assert_eq!(hunks[1].new_count, 2);
        assert_eq!(hunks[1].old_count, 0);
        assert_eq!(hunks[1].preview, "+pub fn added() {}");
    }

    #[test]
    #[cfg(not(windows))]
    fn source_control_panel_lists_git_changes_and_diff_hunks() {
        if !git_available() {
            return;
        }

        let root =
            std::env::temp_dir().join(format!("tscode-test-source-control-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["init", "-q"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["config", "user.email", "tscode@example.invalid"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["config", "user.name", "tscode test"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(["add", "src/lib.rs"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&root)
                .args([
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "-q",
                    "-m",
                    "initial"
                ])
                .status()
                .unwrap()
                .success()
        );

        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        fs::write(root.join("new.txt"), "new file\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.run_command(CommandAction::ShowSourceControl).unwrap();

        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::SourceControl);
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "+ Stage All Changes"
                    && item.command == Some(CommandAction::StageAllChanges))
        );
        assert!(panel.items.iter().any(|item| {
            item.label.starts_with("B Checkout Branch")
                && item.command == Some(CommandAction::ShowGitBranches)
        }));
        assert!(panel.items.iter().any(|item| {
            item.label == "+ Create Branch" && item.command == Some(CommandAction::CreateGitBranch)
        }));
        assert!(panel.items.iter().any(|item| item.label == "M src/lib.rs"));
        assert!(panel.items.iter().any(|item| item.label == "M src/lib.rs"
            && item.command == Some(CommandAction::OpenSourceControlDiff)));
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "+ Stage src/lib.rs"
                    && item.command == Some(CommandAction::StageSourceControlItem))
        );
        assert!(panel.items.iter().any(|item| item.label == "? new.txt"));
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "+ Stage new.txt"
                    && item.command == Some(CommandAction::StageSourceControlItem))
        );
        let hunk_index = panel
            .items
            .iter()
            .position(|item| item.label == "~ src/lib.rs:2")
            .unwrap();
        assert_eq!(panel.items[hunk_index].preview.as_deref(), Some("-    1"));

        app.quick_panel.as_mut().unwrap().selected = hunk_index;
        app.activate_selected_quick_item();
        assert_eq!(app.focus, FocusPanel::Editor);
        assert_eq!(app.active_tab().unwrap().path, lib);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (1, 0));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn git_branch_panel_checks_out_and_creates_branches_with_dirty_buffer_protection() {
        if !git_available() {
            return;
        }

        let root =
            std::env::temp_dir().join(format!("tscode-test-git-branches-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        init_git_repo(&root);
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );
        let initial_branch = git_current_branch(&root).unwrap();
        assert_git(&root, &["checkout", "-q", "-b", "feature"]);
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "feature edit",
            ],
        );
        assert_git(&root, &["checkout", "-q", &initial_branch]);

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        assert_eq!(app.git_branch.as_deref(), Some(initial_branch.as_str()));
        app.open_file(&lib);
        assert!(app.active_tab().unwrap().text().contains("1"));

        app.run_command(CommandAction::ShowGitBranches).unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::Branches);
        assert!(panel.items.iter().any(|item| {
            item.label == format!("* {initial_branch}")
                && item.command == Some(CommandAction::CheckoutGitBranch)
        }));
        assert!(panel.items.iter().any(|item| {
            item.label == "  feature" && item.command == Some(CommandAction::CheckoutGitBranch)
        }));

        select_quick_item(&mut app, "  feature");
        app.activate_selected_quick_item();
        assert_eq!(
            git_current_branch(&canonical_root).as_deref(),
            Some("feature")
        );
        assert_eq!(app.git_branch.as_deref(), Some("feature"));
        assert!(app.active_tab().unwrap().text().contains("2"));

        app.active_tab_mut().unwrap().insert_text("// unsaved\n");
        app.run_command(CommandAction::ShowGitBranches).unwrap();
        select_quick_item(&mut app, &format!("  {initial_branch}"));
        app.activate_selected_quick_item();
        assert_eq!(
            git_current_branch(&canonical_root).as_deref(),
            Some("feature")
        );
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("blocked by unsaved editor tab"))
        );

        app.revert_active_tab().unwrap();
        app.run_command(CommandAction::ShowGitBranches).unwrap();
        select_quick_item(&mut app, &format!("  {initial_branch}"));
        app.activate_selected_quick_item();
        assert_eq!(
            git_current_branch(&canonical_root).as_deref(),
            Some(initial_branch.as_str())
        );
        assert_eq!(app.git_branch.as_deref(), Some(initial_branch.as_str()));
        assert!(app.active_tab().unwrap().text().contains("1"));

        app.run_command(CommandAction::CreateGitBranch).unwrap();
        assert_eq!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(&PromptKind::CreateGitBranch)
        );
        app.prompt.as_mut().unwrap().input = "topic".to_owned();
        app.finish_prompt().unwrap();
        assert_eq!(
            git_current_branch(&canonical_root).as_deref(),
            Some("topic")
        );
        assert_eq!(app.git_branch.as_deref(), Some("topic"));
        assert!(
            git_local_branches(&canonical_root)
                .unwrap()
                .iter()
                .any(|branch| branch == "topic")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn source_control_panel_opens_read_only_diff_tabs() {
        if !git_available() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "tscode-test-source-control-diff-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        init_git_repo(&root);
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );

        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        fs::write(root.join("new.txt"), "new file\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.run_command(CommandAction::ShowSourceControl).unwrap();

        select_quick_item(&mut app, "M src/lib.rs");
        app.activate_selected_quick_item();
        let tab = app.active_tab().unwrap();
        assert!(tab.read_only);
        assert_eq!(tab.title, "Diff src/lib.rs");
        let diff_text = tab.text();
        assert!(diff_text.contains("diff --git a/src/lib.rs b/src/lib.rs"));
        assert!(diff_text.contains("-    1"));
        assert!(diff_text.contains("+    2"));
        assert!(tab.path.ends_with(".tscode-diff/src_lib.rs.diff"));

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .unwrap();
        let tab = app.active_tab().unwrap();
        assert!(tab.read_only);
        assert!(!tab.dirty);
        assert_eq!(tab.text(), diff_text);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("read-only"))
        );

        app.run_command(CommandAction::ShowSourceControl).unwrap();
        select_quick_item(&mut app, "? new.txt");
        app.activate_selected_quick_item();
        let tab = app.active_tab().unwrap();
        assert!(tab.read_only);
        assert_eq!(tab.title, "Diff new.txt");
        let diff_text = tab.text();
        assert!(diff_text.contains("new file mode"));
        assert!(diff_text.contains("--- /dev/null"));
        assert!(diff_text.contains("+new file"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn source_control_panel_stages_and_unstages_changes() {
        if !git_available() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "tscode-test-source-control-stage-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        init_git_repo(&root);
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );

        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        fs::write(root.join("new.txt"), "new file\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.run_command(CommandAction::ShowSourceControl).unwrap();

        select_quick_item(&mut app, "+ Stage src/lib.rs");
        app.activate_selected_quick_item();
        assert_eq!(cached_git_names(&canonical_root), vec!["src/lib.rs"]);
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::SourceControl);
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "- Unstage src/lib.rs"
                    && item.command == Some(CommandAction::UnstageSourceControlItem))
        );

        select_quick_item(&mut app, "- Unstage src/lib.rs");
        app.activate_selected_quick_item();
        assert!(cached_git_names(&canonical_root).is_empty());

        select_quick_item(&mut app, "+ Stage All Changes");
        app.activate_selected_quick_item();
        assert_eq!(
            cached_git_names(&canonical_root),
            vec!["new.txt", "src/lib.rs"]
        );

        select_quick_item(&mut app, "- Unstage All Changes");
        app.activate_selected_quick_item();
        assert!(cached_git_names(&canonical_root).is_empty());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn source_control_panel_commits_staged_changes_with_message() {
        if !git_available() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "tscode-test-source-control-commit-staged-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        init_git_repo(&root);
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );

        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        fs::write(root.join("new.txt"), "new file\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.run_command(CommandAction::ShowSourceControl).unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert!(panel.items.iter().any(|item| {
            item.label == "C Commit All Changes"
                && item.command == Some(CommandAction::CommitAllChanges)
        }));
        assert!(
            !panel
                .items
                .iter()
                .any(|item| item.label == "C Commit Staged Changes")
        );

        select_quick_item(&mut app, "+ Stage src/lib.rs");
        app.activate_selected_quick_item();
        assert_eq!(cached_git_names(&canonical_root), vec!["src/lib.rs"]);
        let panel = app.quick_panel.as_ref().unwrap();
        assert!(panel.items.iter().any(|item| {
            item.label == "C Commit Staged Changes"
                && item.command == Some(CommandAction::CommitStagedChanges)
        }));

        select_quick_item(&mut app, "C Commit Staged Changes");
        app.activate_selected_quick_item();
        assert_eq!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(&PromptKind::CommitStagedSourceControlChanges)
        );
        app.prompt.as_mut().unwrap().input = "   ".to_owned();
        app.finish_prompt().unwrap();
        assert_eq!(git_head_subject(&canonical_root), "initial");
        assert_eq!(cached_git_names(&canonical_root), vec!["src/lib.rs"]);

        app.run_command(CommandAction::ShowSourceControl).unwrap();
        select_quick_item(&mut app, "C Commit Staged Changes");
        app.activate_selected_quick_item();
        app.prompt.as_mut().unwrap().input = "update lib".to_owned();
        app.finish_prompt().unwrap();

        assert_eq!(git_head_subject(&canonical_root), "update lib");
        assert_eq!(git_head_changed_names(&canonical_root), vec!["src/lib.rs"]);
        assert!(cached_git_names(&canonical_root).is_empty());
        let entries = load_git_status_entries(&canonical_root);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            relative_path(&canonical_root, &entries[0].path),
            "new.txt".to_owned()
        );
        assert_eq!(entries[0].kind, GitStatusKind::Untracked);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("committed staged changes"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn source_control_commit_all_blocks_dirty_buffers_then_commits_everything() {
        if !git_available() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "tscode-test-source-control-commit-all-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        init_git_repo(&root);
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );

        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        fs::write(root.join("scratch.txt"), "scratch\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let scratch = canonical_root.join("scratch.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&lib);
        app.active_tab_mut().unwrap().insert_text("// unsaved\n");

        app.run_command(CommandAction::CommitAllChanges).unwrap();
        assert!(app.prompt.is_none());
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("blocked by unsaved editor tab"))
        );
        assert_eq!(git_head_subject(&canonical_root), "initial");
        assert!(scratch.exists());
        assert!(!load_git_status_entries(&canonical_root).is_empty());

        app.revert_active_tab().unwrap();
        app.run_command(CommandAction::CommitAllChanges).unwrap();
        let PromptKind::CommitAllSourceControlChanges(paths) = &app.prompt.as_ref().unwrap().kind
        else {
            panic!("expected commit-all prompt");
        };
        let mut paths = paths.clone();
        paths.sort();
        let mut expected = vec![lib.clone(), scratch.clone()];
        expected.sort();
        assert_eq!(paths, expected);
        app.prompt.as_mut().unwrap().input = "commit all changes".to_owned();
        app.finish_prompt().unwrap();

        assert_eq!(git_head_subject(&canonical_root), "commit all changes");
        assert_eq!(
            git_head_changed_names(&canonical_root),
            vec!["scratch.txt", "src/lib.rs"]
        );
        assert!(
            load_git_status_entries(&canonical_root).is_empty(),
            "commit all should leave a clean working tree"
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn source_control_panel_discards_changes_with_confirmation() {
        if !git_available() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "tscode-test-source-control-discard-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        init_git_repo(&root);
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );

        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        fs::write(root.join("new.txt"), "new file\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let new_file = canonical_root.join("new.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&lib);
        app.open_file(&new_file);
        assert_eq!(app.tabs.len(), 2);

        app.run_command(CommandAction::ShowSourceControl).unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "! Discard All Changes"
                    && item.command == Some(CommandAction::DiscardAllChanges))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "! Discard src/lib.rs"
                    && item.command == Some(CommandAction::DiscardSourceControlItem))
        );
        assert!(
            panel
                .items
                .iter()
                .any(|item| item.label == "! Discard new.txt"
                    && item.command == Some(CommandAction::DiscardSourceControlItem))
        );

        select_quick_item(&mut app, "! Discard src/lib.rs");
        app.activate_selected_quick_item();
        assert_eq!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(&PromptKind::DiscardSourceControlPath(lib.clone()))
        );
        app.prompt.as_mut().unwrap().input = "no".to_owned();
        app.finish_prompt().unwrap();
        assert!(fs::read_to_string(&lib).unwrap().contains("2"));

        app.run_command(CommandAction::ShowSourceControl).unwrap();
        select_quick_item(&mut app, "! Discard src/lib.rs");
        app.activate_selected_quick_item();
        app.prompt.as_mut().unwrap().input = "discard".to_owned();
        app.finish_prompt().unwrap();
        assert!(fs::read_to_string(&lib).unwrap().contains("1"));
        let lib_tab = app.tabs.iter().find(|tab| tab.path == lib).unwrap();
        assert!(lib_tab.text().contains("1"));

        select_quick_item(&mut app, "! Discard new.txt");
        app.activate_selected_quick_item();
        app.prompt.as_mut().unwrap().input = "discard".to_owned();
        app.finish_prompt().unwrap();
        assert!(!new_file.exists());
        assert!(!app.tabs.iter().any(|tab| tab.path == new_file));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn source_control_discard_all_blocks_dirty_buffers_then_discards_clean_changes() {
        if !git_available() {
            return;
        }

        let root = std::env::temp_dir().join(format!(
            "tscode-test-source-control-discard-all-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    1\n}\n").unwrap();

        init_git_repo(&root);
        assert_git(&root, &["add", "src/lib.rs"]);
        assert_git(
            &root,
            &[
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        );

        fs::write(src.join("lib.rs"), "pub fn value() -> i32 {\n    2\n}\n").unwrap();
        fs::write(root.join("scratch.txt"), "scratch\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let scratch = canonical_root.join("scratch.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&lib);
        app.active_tab_mut().unwrap().insert_text("// unsaved\n");

        app.run_command(CommandAction::DiscardAllChanges).unwrap();
        assert!(app.prompt.is_none());
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("blocked by unsaved editor tab"))
        );
        assert!(fs::read_to_string(&lib).unwrap().contains("2"));
        assert!(scratch.exists());

        app.revert_active_tab().unwrap();
        app.run_command(CommandAction::DiscardAllChanges).unwrap();
        let PromptKind::DiscardAllSourceControlChanges(paths) = &app.prompt.as_ref().unwrap().kind
        else {
            panic!("expected discard-all prompt");
        };
        let mut paths = paths.clone();
        paths.sort();
        let mut expected = vec![lib.clone(), scratch.clone()];
        expected.sort();
        assert_eq!(paths, expected);
        app.prompt.as_mut().unwrap().input = "discard".to_owned();
        app.finish_prompt().unwrap();

        assert!(fs::read_to_string(&lib).unwrap().contains("1"));
        assert!(!scratch.exists());
        assert!(
            load_git_status_entries(&canonical_root).is_empty(),
            "discard all should leave a clean working tree"
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
    fn terminal_records_exit_status_and_restart_clears_it() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-exit-status-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.active_terminal_mut()
            .shell
            .send_text("exit 7\r")
            .unwrap();

        for _ in 0..60 {
            app.drain_terminal();
            if app.active_terminal().exited {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        assert!(app.active_terminal().exited);
        let status = app.active_terminal().exit_status.as_ref().unwrap();
        assert_eq!(status.code, 7);
        assert_eq!(status.label(), "exit:7");
        assert!(!status.success);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("exit:7"))
        );

        app.restart_terminal().unwrap();
        assert!(!app.active_terminal().exited);
        assert_eq!(app.active_terminal().exit_status, None);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn split_terminal_creates_side_by_side_pty_panes_and_mouse_focuses_each_pane() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-split-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let first_id = app.active_terminal().id;
        let first_cwd = app.active_terminal().cwd.clone();

        app.split_terminal().unwrap();

        assert_eq!(app.terminals.len(), 2);
        assert_eq!(app.split_terminal, Some(0));
        assert_eq!(app.active_terminal, 1);
        assert_eq!(app.active_terminal().cwd, first_cwd);
        assert_eq!(app.visible_terminal_indices(), vec![0, 1]);
        assert!(app.terminal_split_active());
        assert_eq!(app.focus, FocusPanel::Terminal);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("split terminal"))
        );

        app.hit_regions.terminal_area = Some(Rect::new(0, 0, 81, 12));
        app.hit_regions
            .terminal_bodies
            .push((Rect::new(0, 1, 40, 11), 0));
        app.hit_regions
            .terminal_bodies
            .push((Rect::new(41, 1, 40, 11), 1));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 2,
            row: 2,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.active_terminal, 0);
        assert_eq!(app.active_terminal().id, first_id);
        assert_eq!(app.split_terminal, Some(1));
        assert_eq!(app.visible_terminal_indices(), vec![1, 0]);

        app.close_terminal(1).unwrap();
        assert_eq!(app.terminals.len(), 1);
        assert_eq!(app.split_terminal, None);
        assert!(!app.terminal_split_active());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_panel_can_be_resized_by_dragging_top_border() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-resize-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.hit_regions.editor_area = Some(Rect::new(32, 1, 88, 28));
        app.hit_regions.terminal_area = Some(Rect::new(32, 29, 88, 11));
        app.hit_regions.terminal_resize = Some(Rect::new(32, 29, 88, 1));
        app.terminal_rows = 11;

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 29,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert!(app.terminal_resize_dragging);
        assert_eq!(app.focus, FocusPanel::Terminal);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 60,
            row: 24,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert_eq!(app.terminal_rows, 16);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 60,
            row: 9,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert!(!app.terminal_resize_dragging);
        assert_eq!(app.terminal_rows, 31);
        assert_eq!(
            app.message.as_deref(),
            Some("terminal height set to 31 rows")
        );

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 60,
            row: 29,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 60,
            row: 200,
            modifiers: KeyModifiers::NONE,
        })
        .unwrap();
        assert_eq!(app.terminal_rows, 4);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_can_be_renamed_without_restarting_session() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-rename-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        let terminal_id = app.active_terminal().id;
        let terminal_count = app.terminals.len();
        let cwd = app.active_terminal().cwd.clone();

        app.run_command(CommandAction::RenameTerminal).unwrap();
        assert_eq!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(&PromptKind::RenameTerminal)
        );
        assert_eq!(
            app.prompt.as_ref().map(|prompt| prompt.input.as_str()),
            Some("term 1")
        );

        app.prompt.as_mut().unwrap().input = "  server logs  ".to_owned();
        app.finish_prompt().unwrap();
        assert_eq!(app.active_terminal().title, "server logs");
        assert_eq!(app.active_terminal().id, terminal_id);
        assert_eq!(app.terminals.len(), terminal_count);
        assert_eq!(app.active_terminal().cwd, cwd);
        assert_eq!(app.focus, FocusPanel::Terminal);

        app.run_command(CommandAction::RenameTerminal).unwrap();
        app.prompt.as_mut().unwrap().input = "   ".to_owned();
        app.finish_prompt().unwrap();
        assert_eq!(app.active_terminal().title, "server logs");
        assert_eq!(
            app.message.as_deref(),
            Some("terminal rename requires a title")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    #[cfg(not(windows))]
    fn new_terminal_here_starts_real_pty_in_selected_explorer_directory() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-here-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        let canonical_src = src.canonicalize().unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.reveal_path(&canonical_src).unwrap();
        app.new_terminal_here().unwrap();
        assert_eq!(app.focus, FocusPanel::Terminal);
        assert_eq!(app.active_terminal().cwd, canonical_src);
        assert!(app.active_terminal().title.contains("src"));

        app.active_terminal_mut()
            .shell
            .send_text("pwd > ../cwd.txt\r")
            .unwrap();

        let out = root.join("cwd.txt");
        for _ in 0..50 {
            app.drain_terminal();
            if out.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(100));
        }

        assert_eq!(
            fs::read_to_string(&out).unwrap().trim(),
            src.canonicalize().unwrap().to_string_lossy()
        );

        app.restart_terminal().unwrap();
        assert_eq!(app.active_terminal().cwd, src.canonicalize().unwrap());
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_osc7_updates_session_cwd() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-osc7-{}", std::process::id()));
        let nested = root.join("nested");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        let canonical_nested = nested.canonicalize().unwrap();

        let mut app = App::new(root.clone()).unwrap();
        for _ in 0..10 {
            app.drain_terminal();
            thread::sleep(Duration::from_millis(20));
        }

        let sequence = format!("\x1b]7;file://localhost{}\x07", canonical_nested.display());
        app.active_terminal_mut()
            .shell
            .process_output_for_test(sequence.as_bytes());

        assert!(app.drain_terminal());
        assert_eq!(app.active_terminal().cwd, canonical_nested);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_osc_title_updates_unlocked_session_title_only() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-title-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        assert!(!app.active_terminal().title_locked);
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"\x1b]2;server logs\x07");

        assert!(app.drain_terminal());
        assert_eq!(app.active_terminal().title, "server logs");

        app.rename_terminal_from_prompt("manual name".to_owned());
        assert!(app.active_terminal().title_locked);
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"\x1b]0;ignored title\x07");

        assert!(!app.drain_terminal());
        assert_eq!(app.active_terminal().title, "manual name");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_osc52_updates_internal_and_host_clipboards() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-osc52-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.active_terminal_mut()
            .shell
            .process_output_for_test(b"\x1b]52;c;Y29weSBmcm9tIGNoaWxkIHR1aQ==\x07");

        assert!(app.drain_terminal());
        assert_eq!(app.editor_clipboard.as_deref(), Some("copy from child tui"));
        assert_eq!(
            app.take_clipboard_export(),
            Some("copy from child tui".to_owned())
        );
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("copied 19 char(s) through OSC52"))
        );

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
    fn problem_parser_reads_cargo_and_go_style_diagnostics() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-problem-parser-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn ok() {}\n").unwrap();
        fs::write(src.join("main.go"), "package main\n").unwrap();
        fs::write(root.join("bad.py"), "def bad(:\n").unwrap();
        let root = root.canonicalize().unwrap();

        let output = "\
src/lib.rs:2:5: error[E0425]: cannot find value `missing` in this scope
./src/main.go:7:13: undefined: missing
  File \"./bad.py\", line 1
    def bad(:
            ^
SyntaxError: invalid syntax
";
        let items = parse_problem_items(output, &root);

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].label, "error src/lib.rs:2:5");
        assert_eq!(items[0].detail, "cannot find value `missing` in this scope");
        assert_eq!(items[0].line, Some(1));
        assert_eq!(items[0].col, Some(4));
        assert_eq!(items[1].label, "problem src/main.go:7:13");
        assert_eq!(items[1].detail, "undefined: missing");
        assert_eq!(items[2].label, "error bad.py:1:1");
        assert_eq!(items[2].detail, "invalid syntax");

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn lsp_diagnostics_map_to_problem_items_for_existing_gutter_ui() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-lsp-problems-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn broken() {\n    missing();\n}\n").unwrap();
        let root = root.canonicalize().unwrap();
        let lib = root.join("src/lib.rs");

        let items = lsp_diagnostics_to_problem_items(
            &root,
            vec![lsp::LspDiagnostic {
                path: lib.clone(),
                line: 1,
                col: 4,
                severity: lsp::LspDiagnosticSeverity::Error,
                message: "cannot find function `missing`".to_owned(),
                source: Some("mock-checker".to_owned()),
                code: Some("E0425".to_owned()),
                server: "mock-lsp".to_owned(),
            }],
        );

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "error src/lib.rs:2:5");
        assert_eq!(items[0].detail, "cannot find function `missing`");
        assert_eq!(items[0].path, lib);
        assert_eq!(items[0].line, Some(1));
        assert_eq!(items[0].col, Some(4));
        assert_eq!(
            items[0].preview.as_deref(),
            Some("LSP mock-lsp / mock-checker [E0425]")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn problem_summaries_group_active_file_lines_by_severity() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-problem-summary-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn broken() {\n    missing();\n}\n").unwrap();
        let root = root.canonicalize().unwrap();
        let lib = root.join("src/lib.rs");

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&lib);
        app.problems = parse_problem_items(
            "\
src/lib.rs:2:9: warning: first warning
src/lib.rs:2:5: error[E0425]: cannot find value `missing` in this scope
src/lib.rs:3:1: note: trailing note
",
            &root,
        );

        let summaries = app.problem_summaries_for_path(&lib);
        let line_two = summaries.get(&1).expect("line 2 summary");
        assert_eq!(line_two.severity, ProblemSeverity::Error);
        assert_eq!(line_two.count, 2);
        assert_eq!(line_two.col, 4);
        assert!(line_two.message.contains("cannot find value"));
        assert_eq!(summaries.get(&2).unwrap().severity, ProblemSeverity::Note);
        assert_eq!(app.active_file_problem_count(), 3);

        app.active_tab_mut().unwrap().set_cursor(1, 4);
        assert_eq!(
            app.active_line_problem_summary().unwrap().severity,
            ProblemSeverity::Error
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn run_workspace_check_collects_cargo_problems_and_jumps_to_file() {
        if Command::new("cargo").arg("--version").output().is_err() {
            return;
        }

        let root =
            std::env::temp_dir().join(format!("tscode-test-cargo-check-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"tscode_problem_test\"\nversion = \"0.0.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::write(src.join("lib.rs"), "pub fn broken() {\n    missing();\n}\n").unwrap();
        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");

        let mut app = App::new(canonical_root.clone()).unwrap();
        app.run_workspace_check().unwrap();

        assert!(!app.problems.is_empty());
        assert!(
            app.problems
                .iter()
                .any(|item| item.path == lib && item.label.starts_with("error src/lib.rs:2:"))
        );
        assert!(
            app.quick_panel
                .as_ref()
                .is_some_and(|panel| panel.kind == QuickPanelKind::Problems)
        );

        app.activate_selected_quick_item();
        assert_eq!(app.focus, FocusPanel::Editor);
        assert_eq!(app.active_tab().unwrap().path, lib);
        assert_eq!(app.active_tab().unwrap().cursor_position().0, 1);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
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
    fn terminal_file_reference_handles_tracebacks_quotes_and_parentheses() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-rich-termref-{}", std::process::id()));
        let src = root.join("space dir");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.py"), "print('hi')\n").unwrap();
        fs::write(src.join("app.ts"), "console.log('hi')\n").unwrap();
        let root = root.canonicalize().unwrap();
        let python = root.join("space dir/main.py");
        let ts = root.join("space dir/app.ts");

        let traceback = format!("  File \"{}\", line 3, in <module>", python.display());
        let from_path =
            terminal_file_reference_at(&traceback, traceback.find("main.py").unwrap(), &root)
                .unwrap();
        assert_eq!(from_path.path, python);
        assert_eq!(from_path.line, Some(2));
        assert_eq!(from_path.col, None);
        let from_line = terminal_file_reference_at(
            &traceback,
            traceback.find("line 3").unwrap() + "line ".len(),
            &root,
        )
        .unwrap();
        assert_eq!(from_line.line, Some(2));

        let quoted = "\"space dir/main.py\":4:2: error from quoted relative path";
        let quoted_ref =
            terminal_file_reference_at(quoted, quoted.find("dir").unwrap(), &root).unwrap();
        assert_eq!(quoted_ref.path, root.join("space dir/main.py"));
        assert_eq!(quoted_ref.line, Some(3));
        assert_eq!(quoted_ref.col, Some(1));

        let ts_line = "TypeError at space dir/app.ts(9,13): failed";
        let ts_ref =
            terminal_file_reference_at(ts_line, ts_line.find("9,13").unwrap(), &root).unwrap();
        assert_eq!(ts_ref.path, ts);
        assert_eq!(ts_ref.line, Some(8));
        assert_eq!(ts_ref.col, Some(12));

        let node_line = "    at render (space dir/app.ts:10:14)";
        let node_ref =
            terminal_file_reference_at(node_line, node_line.find("10:14").unwrap(), &root).unwrap();
        assert_eq!(node_ref.path, ts);
        assert_eq!(node_ref.line, Some(9));
        assert_eq!(node_ref.col, Some(13));

        let absolute_node_line = format!("    at Object.<anonymous> ({}:11:15)", ts.display());
        let absolute_node_ref = terminal_file_reference_at(
            &absolute_node_line,
            absolute_node_line.find("app.ts").unwrap(),
            &root,
        )
        .unwrap();
        assert_eq!(absolute_node_ref.path, ts);
        assert_eq!(absolute_node_ref.line, Some(10));
        assert_eq!(absolute_node_ref.col, Some(14));

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_link_parser_handles_web_and_file_urls() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-links-{}", std::process::id()));
        let src = root.join("space dir");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("app.rs"), "fn main() {}\n").unwrap();
        let root = root.canonicalize().unwrap();
        let app_file = root.join("space dir/app.rs");

        let web_line = "open https://example.com/a_(b)?q=1).";
        let web =
            terminal_link_candidate_at(web_line, web_line.find("example").unwrap(), &root).unwrap();
        assert_eq!(
            web.link,
            TerminalLink::Url("https://example.com/a_(b)?q=1".to_owned())
        );

        let encoded_path = app_file.to_string_lossy().replace(' ', "%20");
        let file_line = format!("trace file://localhost{encoded_path}:4:2");
        let file = terminal_link_candidate_at(&file_line, file_line.find("app.rs").unwrap(), &root)
            .unwrap();
        assert_eq!(
            file.link,
            TerminalLink::File(FileReference {
                path: app_file,
                line: Some(3),
                col: Some(1),
            })
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_url_click_copies_url_and_hover_highlights_range() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-terminal-url-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let mut app = App::new(root.clone()).unwrap();
        let line = "visit https://example.com/docs";

        app.active_terminal_mut().shell.clear();
        app.active_terminal_mut().shell.resize(5, 240);
        app.active_terminal_mut()
            .shell
            .process_output_for_test(line.as_bytes());

        let url_start = line.find("https://").unwrap();
        app.hit_regions.terminal_bodies = vec![(Rect::new(0, 0, 80, 5), 0)];
        app.hit_regions.last_mouse_x = (url_start + 2) as u16;
        app.hit_regions.last_mouse_y = 0;
        assert_eq!(
            app.terminal_link_ranges_for_terminal_row(0, 0),
            vec![(url_start, line.len())]
        );

        assert!(app.open_terminal_reference(0, (url_start + 3) as u16));
        assert_eq!(
            app.editor_clipboard.as_deref(),
            Some("https://example.com/docs")
        );
        assert_eq!(
            app.take_clipboard_export().as_deref(),
            Some("https://example.com/docs")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn terminal_file_url_click_opens_file_location() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-terminal-file-url-{}",
            std::process::id()
        ));
        let src = root.join("space dir");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("app.rs"), "fn main() {}\nfn next() {}\n").unwrap();
        let root = root.canonicalize().unwrap();
        let app_file = root.join("space dir/app.rs");
        let encoded_path = app_file.to_string_lossy().replace(' ', "%20");
        let line = format!("open file://localhost{encoded_path}:2:4");
        let mut app = App::new(root.clone()).unwrap();

        app.active_terminal_mut().shell.clear();
        app.active_terminal_mut().shell.resize(5, 240);
        app.active_terminal_mut()
            .shell
            .process_output_for_test(line.as_bytes());

        let click_col = line.find("app.rs").unwrap();
        let rendered = app.active_terminal().shell.row_text(0).unwrap_or_default();
        assert!(
            app.open_terminal_reference(0, click_col as u16),
            "rendered row={rendered:?}, line={line:?}, click_col={click_col}"
        );
        assert_eq!(app.active_tab().unwrap().path, app_file);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (1, 3));

        app.kill_all_terminals();
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
    fn document_symbol_panel_jumps_inside_active_buffer() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-document-symbols-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(
            &path,
            "pub struct App {}\nimpl App {\n    pub fn run(&self) {}\n}\n",
        )
        .unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        let items = app.document_symbol_items("run");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "run");
        assert_eq!(items[0].line, Some(2));

        app.quick_panel = Some(QuickPanel {
            kind: QuickPanelKind::DocumentSymbols,
            query_cursor: "run".chars().count(),
            query: "run".to_owned(),
            items,
            selected: 0,
            scroll: 0,
        });
        app.activate_selected_quick_item();

        assert_eq!(app.focus, FocusPanel::Editor);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (2, 11));
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sidebar_outline_lists_symbols_and_jumps_with_keyboard() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-sidebar-outline-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("main.rs"),
            "pub struct App {}\nimpl App {\n    pub fn run(&self) {}\n}\nfn helper() {}\n",
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("main.rs");

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.run_command(CommandAction::ShowOutline).unwrap();

        assert_eq!(app.sidebar_mode, SidebarMode::Outline);
        assert_eq!(app.focus, FocusPanel::Explorer);
        let items = app.visible_outline_items();
        let run_index = items
            .iter()
            .position(|item| item.label == "run")
            .expect("run symbol");
        app.set_outline_selection(run_index);
        app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
            .unwrap();

        assert_eq!(app.sidebar_mode, SidebarMode::Outline);
        assert_eq!(app.focus, FocusPanel::Editor);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (2, 11));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn sidebar_outline_mouse_click_jumps_and_hover_targets_rows() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-sidebar-outline-mouse-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("main.rs"),
            "fn first() {}\nfn second() {}\nfn third() {}\n",
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("main.rs");

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.run_command(CommandAction::ShowOutline).unwrap();
        let second = app
            .visible_outline_items()
            .iter()
            .position(|item| item.label == "second")
            .expect("second symbol");
        app.hit_regions.outline_rows = vec![(Rect::new(0, 1, 40, 1), second)];
        app.hit_regions.outline_area = Some(Rect::new(0, 0, 40, 8));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 3,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.hover, HoverTarget::OutlineRow(second));
        assert_eq!(app.sidebar_mode, SidebarMode::Outline);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (1, 3));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn document_symbol_panel_prefers_cached_lsp_symbols() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-lsp-document-symbols-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(
            root.join("main.rs"),
            "fn heuristic_only() {}\nfn semantic_run() {}\n",
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        let path = root.join("main.rs");

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.lsp_document_symbol_path = Some(path.clone());
        app.lsp_document_symbol_items = vec![QuickItem {
            label: "semantic_run".to_owned(),
            detail: "LSP mock-symbols  line 2  function".to_owned(),
            path: path.clone(),
            line: Some(1),
            col: Some(3),
            preview: Some("fn semantic_run() {}".to_owned()),
            command: None,
        }];

        let items = app.document_symbol_items("semantic");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "semantic_run");
        assert!(items[0].detail.contains("LSP mock-symbols"));

        let empty = app.document_symbol_items("heuristic");
        assert!(empty.is_empty());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn document_symbol_cache_is_ignored_for_other_active_files() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-lsp-document-symbol-cache-path-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.rs"), "fn one() {}\n").unwrap();
        fs::write(root.join("two.rs"), "fn two() {}\n").unwrap();
        let root = root.canonicalize().unwrap();
        let one = root.join("one.rs");
        let two = root.join("two.rs");

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&one);
        app.lsp_document_symbol_path = Some(one.clone());
        app.lsp_document_symbol_items = vec![QuickItem {
            label: "semantic_one".to_owned(),
            detail: "LSP mock-symbols  line 1  function".to_owned(),
            path: one.clone(),
            line: Some(0),
            col: Some(3),
            preview: Some("fn one() {}".to_owned()),
            command: None,
        }];

        app.open_file(&two);
        let items = app.visible_outline_items();
        assert!(items.iter().any(|item| item.label == "two"));
        assert!(!items.iter().any(|item| item.label == "semantic_one"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_symbol_panel_prefers_cached_lsp_symbols() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-lsp-workspace-symbols-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "fn heuristic_only() {}\nfn semantic_run() {}\n").unwrap();

        let mut app = App::new(root.clone()).unwrap();
        app.open_file(&path);
        app.lsp_workspace_symbol_query = Some("semantic".to_owned());
        app.lsp_workspace_symbol_items = vec![QuickItem {
            label: "semantic_run".to_owned(),
            detail: "LSP mock-workspace  line 2  function".to_owned(),
            path: path.clone(),
            line: Some(1),
            col: Some(3),
            preview: Some("fn semantic_run() {}".to_owned()),
            command: None,
        }];

        let items = app.workspace_symbol_items("semantic").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "semantic_run");
        assert!(items[0].detail.contains("LSP mock-workspace"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_symbols_scan_real_files_and_open_dirty_buffer_symbols() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-workspace-symbols-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let target = root.join("target");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&target).unwrap();
        fs::write(src.join("lib.rs"), "pub fn server_start() {}\n").unwrap();
        fs::write(
            src.join("web.ts"),
            "export const makeClient = () => fetch('/');\n",
        )
        .unwrap();
        fs::write(target.join("generated.rs"), "fn generated_symbol() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let web = canonical_root.join("src/web.ts");
        let mut app = App::new(canonical_root.clone()).unwrap();
        let items = app.workspace_symbol_items("server").unwrap();
        assert!(items.iter().any(|item| item.label == "server_start"));
        assert!(
            !app.workspace_symbol_items("generated")
                .unwrap()
                .iter()
                .any(|item| item.label == "generated_symbol")
        );

        app.open_file(&web);
        {
            let tab = app.active_tab_mut().unwrap();
            tab.set_cursor(0, tab.lines[0].chars().count());
            tab.insert_text("\nfunction dirtyOnly() {}\n");
        }
        let items = app.workspace_symbol_items("dirty").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].label, "dirtyOnly");
        assert_eq!(items[0].path, web);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn go_to_definition_jumps_from_symbol_under_cursor() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-definition-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        let lib = src.join("lib.rs");
        let main = src.join("main.rs");
        fs::write(&lib, "pub fn make_client() {}\n").unwrap();
        fs::write(
            &main,
            "fn main() {\n    make_client();\n    make_client_extra();\n}\n",
        )
        .unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let main = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&main);
        app.active_tab_mut().unwrap().set_cursor(1, 6);

        app.run_command(CommandAction::GoToDefinition).unwrap();

        let tab = app.active_tab().unwrap();
        assert_eq!(tab.path, lib);
        assert_eq!(tab.cursor_position(), (0, 7));
        assert_eq!(
            app.message.as_deref(),
            Some("jumped to definition: make_client")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn go_to_matching_bracket_jumps_pairs_and_tracks_navigation_history() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-matching-bracket-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(
            &path,
            "fn main() {\n    let total = ((1 + 2) * 3);\n    let values = [alpha(1), beta(2)];\n}\n",
        )
        .unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let path = canonical_root.join("main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&path);
        app.focus = FocusPanel::Editor;

        app.active_tab_mut().unwrap().set_cursor(0, 10);
        app.run_command(CommandAction::GoToMatchingBracket).unwrap();
        assert_eq!(app.active_tab().unwrap().cursor_position(), (3, 0));
        assert_eq!(
            app.navigation_back,
            vec![EditorLocation {
                path: path.clone(),
                line: 0,
                col: 10,
            }]
        );

        app.run_command(CommandAction::GoBack).unwrap();
        assert_eq!(app.active_tab().unwrap().cursor_position(), (0, 10));

        let nested_line = app.active_tab().unwrap().lines[1].clone();
        let outer_open = nested_line.find('(').unwrap();
        let outer_close = nested_line.rfind(')').unwrap();
        app.active_tab_mut().unwrap().set_cursor(1, outer_open);
        app.handle_key(KeyEvent::new(
            KeyCode::Char('\\'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ))
        .unwrap();
        assert_eq!(
            app.active_tab().unwrap().cursor_position(),
            (1, outer_close)
        );

        let square_line = app.active_tab().unwrap().lines[2].clone();
        let square_open = square_line.find('[').unwrap();
        let square_close = square_line.find(']').unwrap();
        app.active_tab_mut()
            .unwrap()
            .set_cursor(2, square_close + 1);
        app.run_command(CommandAction::GoToMatchingBracket).unwrap();
        assert_eq!(
            app.active_tab().unwrap().cursor_position(),
            (2, square_open)
        );

        app.active_tab_mut().unwrap().set_cursor(0, 0);
        app.run_command(CommandAction::GoToMatchingBracket).unwrap();
        assert_eq!(
            app.message.as_deref(),
            Some("no matching bracket at cursor")
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_hover_reports_symbol_definition_and_references() {
        let root = std::env::temp_dir().join(format!("tscode-test-hover-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn make_client() {}\n").unwrap();
        fs::write(
            src.join("main.rs"),
            "fn main() {\n    make_client();\n    make_client_extra();\n}\n",
        )
        .unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let main = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&main);
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 80, 8));
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 80, 8));

        let gutter = editor_gutter_width(app.active_tab().unwrap().lines.len()) as u16;
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: gutter + 6,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        let hover = app.editor_hover.as_ref().expect("editor hover");
        assert_eq!(hover.symbol, "make_client");
        assert_eq!(hover.path, main);
        assert_eq!(hover.line, 1);
        assert_eq!(hover.definition_count, 1);
        assert_eq!(hover.reference_count, 2);
        assert_eq!(
            hover.definition,
            Some(EditorLocation {
                path: lib,
                line: 0,
                col: 7,
            })
        );
        assert!(
            hover
                .definition_preview
                .as_deref()
                .is_some_and(|preview| preview.contains("pub fn make_client"))
        );

        app.hit_regions.explorer_area = Some(Rect::new(0, 10, 20, 4));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Moved,
            column: 1,
            row: 11,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert!(app.editor_hover.is_none());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn navigation_history_moves_back_and_forward_after_definition_jump() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-navigation-definition-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn make_client() {}\n").unwrap();
        fs::write(src.join("main.rs"), "fn main() {\n    make_client();\n}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let lib = canonical_root.join("src/lib.rs");
        let main = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&main);
        app.active_tab_mut().unwrap().set_cursor(1, 6);

        app.run_command(CommandAction::GoToDefinition).unwrap();

        assert_eq!(app.active_tab().unwrap().path, lib);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (0, 7));
        assert_eq!(
            app.navigation_back,
            vec![EditorLocation {
                path: main.clone(),
                line: 1,
                col: 6,
            }]
        );

        app.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::ALT))
            .unwrap();
        assert_eq!(app.active_tab().unwrap().path, main);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (1, 6));
        assert_eq!(
            app.navigation_forward,
            vec![EditorLocation {
                path: lib.clone(),
                line: 0,
                col: 7,
            }]
        );

        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::ALT))
            .unwrap();
        assert_eq!(app.active_tab().unwrap().path, lib);
        assert_eq!(app.active_tab().unwrap().cursor_position(), (0, 7));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn navigation_history_tracks_goto_line_and_command_actions() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-navigation-line-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(&path, "alpha\nbeta\ngamma\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let path = canonical_root.join("main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&path);
        app.active_tab_mut().unwrap().set_cursor(0, 2);

        app.goto_line_from_prompt("3:3".to_owned());

        assert_eq!(app.active_tab().unwrap().cursor_position(), (2, 2));
        assert_eq!(
            app.navigation_back,
            vec![EditorLocation {
                path: path.clone(),
                line: 0,
                col: 2,
            }]
        );

        app.run_command(CommandAction::GoBack).unwrap();
        assert_eq!(app.active_tab().unwrap().cursor_position(), (0, 2));

        app.run_command(CommandAction::GoForward).unwrap();
        assert_eq!(app.active_tab().unwrap().cursor_position(), (2, 2));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn find_references_lists_whole_word_matches_and_opens_selected_reference() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-references-{}", std::process::id()));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("lib.rs"), "pub fn make_client() {}\n").unwrap();
        fs::write(
            src.join("main.rs"),
            "fn main() {\n    make_client();\n    make_client_extra();\n}\n",
        )
        .unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let main = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&main);
        app.active_tab_mut().unwrap().set_cursor(1, 6);

        app.run_command(CommandAction::FindReferences).unwrap();

        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.kind, QuickPanelKind::References);
        assert_eq!(panel.query, "make_client");
        assert_eq!(panel.items.len(), 2);
        assert!(panel.items.iter().any(|item| item.label == "src/lib.rs:1"));
        let main_ref = panel
            .items
            .iter()
            .position(|item| item.label == "src/main.rs:2")
            .unwrap();
        assert!(!panel.items.iter().any(|item| {
            item.preview
                .as_deref()
                .is_some_and(|preview| preview.contains("make_client_extra"))
        }));

        app.quick_panel.as_mut().unwrap().selected = main_ref;
        app.activate_selected_quick_item();
        let tab = app.active_tab().unwrap();
        assert_eq!(tab.path, main);
        assert_eq!(tab.cursor_position(), (1, 4));

        app.kill_all_terminals();
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
    fn explorer_visibility_respects_gitignore_and_toggle() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-gitignore-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::write(root.join(".gitignore"), "ignored.log\nlogs/\n*.tmp\n").unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("ignored.log"), "ignored\n").unwrap();
        fs::write(root.join("scratch.tmp"), "ignored tmp\n").unwrap();
        fs::write(root.join("logs/output.txt"), "ignored dir\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        let names = app
            .visible_nodes()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"main.rs".to_owned()));
        assert!(!names.contains(&"ignored.log".to_owned()));
        assert!(!names.contains(&"scratch.tmp".to_owned()));
        assert!(!names.contains(&"logs".to_owned()));

        app.toggle_ignored_files();
        let names = app
            .visible_nodes()
            .into_iter()
            .map(|node| node.name)
            .collect::<Vec<_>>();
        assert!(names.contains(&"ignored.log".to_owned()));
        assert!(names.contains(&"scratch.tmp".to_owned()));
        assert!(names.contains(&"logs".to_owned()));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_search_respects_gitignore_and_toggle() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-search-gitignore-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(root.join("visible.txt"), "needle visible\n").unwrap();
        fs::write(root.join("ignored.txt"), "needle ignored\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        let items = app.workspace_search_items("needle").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, canonical_root.join("visible.txt"));

        app.toggle_ignored_files();
        let items = app.workspace_search_items("needle").unwrap();
        let paths = items.into_iter().map(|item| item.path).collect::<Vec<_>>();
        assert!(paths.contains(&canonical_root.join("visible.txt")));
        assert!(paths.contains(&canonical_root.join("ignored.txt")));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_filter_expands_collapsed_nested_matches_from_filesystem() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-filter-expand-{}",
            std::process::id()
        ));
        let nested = root.join("src/deep/module");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("needle.rs"), "fn needle() {}\n").unwrap();
        fs::write(root.join("README.md"), "hello\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        assert!(
            app.visible_nodes()
                .iter()
                .all(|node| node.name != "needle.rs")
        );

        app.set_explorer_filter("needle".to_owned());
        let visible = app.visible_nodes();
        let names = visible
            .iter()
            .map(|node| node.name.as_str())
            .collect::<Vec<_>>();
        assert!(names.contains(&"src"));
        assert!(names.contains(&"deep"));
        assert!(names.contains(&"module"));
        assert!(names.contains(&"needle.rs"));
        assert!(
            visible
                .iter()
                .any(|node| node.path == canonical_root.join("src/deep/module/needle.rs"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_auto_refreshes_when_external_file_is_created() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-auto-create-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        let src_path = canonical_root.join("src");
        app.explorer.selected = app
            .visible_nodes()
            .iter()
            .position(|node| node.path == src_path)
            .unwrap();
        app.open_or_toggle_selected().unwrap();
        assert!(
            app.visible_nodes()
                .iter()
                .all(|node| node.name != "created.rs")
        );

        fs::write(src.join("created.rs"), "fn created() {}\n").unwrap();
        assert!(app.force_check_workspace_tree_changes());

        let visible = app.visible_nodes();
        assert!(
            visible
                .iter()
                .any(|node| node.path == canonical_root.join("src/created.rs"))
        );
        assert_eq!(
            visible
                .get(app.explorer.selected)
                .map(|node| node.path.clone()),
            Some(src_path)
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_auto_refreshes_when_external_file_is_deleted() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-auto-delete-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("delete_me.rs");
        fs::write(&file, "fn doomed() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        let canonical_file = canonical_root.join("delete_me.rs");
        assert!(
            app.visible_nodes()
                .iter()
                .any(|node| node.path == canonical_file)
        );

        fs::remove_file(&file).unwrap();
        assert!(app.force_check_workspace_tree_changes());
        assert!(
            app.visible_nodes()
                .iter()
                .all(|node| node.path != canonical_file)
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_auto_refresh_expands_new_filtered_matches() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-auto-filter-{}",
            std::process::id()
        ));
        let nested = root.join("src/deep/module");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&nested).unwrap();
        fs::write(root.join("README.md"), "hello\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.set_explorer_filter("needle".to_owned());
        assert!(
            app.visible_nodes()
                .iter()
                .all(|node| node.name != "needle.rs")
        );

        fs::write(nested.join("needle.rs"), "fn needle() {}\n").unwrap();
        assert!(app.force_check_workspace_tree_changes());

        let visible = app.visible_nodes();
        assert!(visible.iter().any(|node| node.name == "needle.rs"));
        assert!(
            visible
                .iter()
                .any(|node| node.path == canonical_root.join("src/deep/module/needle.rs"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn workspace_path_scan_includes_directories_and_respects_visibility() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-workspace-path-scan-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("src/.secret")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        fs::write(root.join("src/.secret/token.txt"), "hidden\n").unwrap();
        fs::write(root.join("target/generated.rs"), "generated\n").unwrap();

        let visible = collect_workspace_paths(&root, false, false).unwrap();
        assert!(visible.contains(&root.join("src")));
        assert!(visible.contains(&root.join("src/lib.rs")));
        assert!(!visible.contains(&root.join("src/.secret")));
        assert!(!visible.contains(&root.join("target")));

        let all = collect_workspace_paths(&root, true, true).unwrap();
        assert!(all.contains(&root.join("src/.secret/token.txt")));
        assert!(all.contains(&root.join("target/generated.rs")));

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
    fn deleting_explorer_folder_refuses_to_discard_dirty_open_tabs() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-delete-dirty-folder-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let canonical_src = canonical_root.join("src");
        let file = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&file);
        app.active_tab_mut().unwrap().insert_text("// unsaved\n");

        app.delete_paths(vec![canonical_src.clone()]).unwrap();

        assert!(canonical_src.exists());
        assert!(file.exists());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab().unwrap().path, file);
        assert!(app.active_tab().unwrap().dirty);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("delete blocked by unsaved tab"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn deleting_explorer_folder_closes_clean_tabs_and_preserves_other_active_tab() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-delete-clean-folder-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("README.md"), "# docs\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let canonical_src = canonical_root.join("src");
        let deleted_file = canonical_root.join("src/main.rs");
        let kept_file = canonical_root.join("README.md");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&deleted_file);
        app.open_file(&kept_file);
        assert_eq!(app.active_tab().unwrap().path, kept_file);

        app.delete_paths(vec![canonical_src.clone()]).unwrap();

        assert!(!canonical_src.exists());
        assert_eq!(app.tabs.len(), 1);
        assert_eq!(app.active_tab().unwrap().path, kept_file);
        assert!(
            app.tabs
                .iter()
                .all(|tab| tab.untitled || !tab.path.starts_with(&canonical_src))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_new_file_and_folder_prompts_use_selected_context() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-new-item-context-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let src_path = canonical_root.join("src");
        let main_path = canonical_root.join("src/main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();

        app.explorer.reveal(&src_path).unwrap();
        app.start_new_file_prompt();
        assert!(matches!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::NewFile)
        ));
        assert_eq!(
            app.prompt.as_ref().map(|prompt| prompt.input.as_str()),
            Some("src/")
        );
        app.prompt.as_mut().unwrap().input = "src/mod.rs".to_owned();
        app.finish_prompt().unwrap();

        let mod_path = canonical_root.join("src/mod.rs");
        assert!(mod_path.is_file());
        assert_eq!(app.active_tab().map(|tab| tab.path.clone()), Some(mod_path));

        app.explorer.reveal(&main_path).unwrap();
        app.start_new_dir_prompt();
        assert!(matches!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::NewDir)
        ));
        assert_eq!(
            app.prompt.as_ref().map(|prompt| prompt.input.as_str()),
            Some("src/")
        );
        app.prompt.as_mut().unwrap().input = "src/components".to_owned();
        app.finish_prompt().unwrap();

        assert!(canonical_root.join("src/components").is_dir());
        assert_eq!(
            app.visible_nodes()
                .get(app.explorer.selected)
                .map(|node| node.path.clone()),
            Some(canonical_root.join("src/components"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_rename_refuses_root_and_existing_targets() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-rename-safety-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), "alpha").unwrap();
        fs::write(root.join("b.txt"), "beta").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let a = canonical_root.join("a.txt");
        let b = canonical_root.join("b.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();

        app.rename_from_prompt(a.clone(), "b.txt".to_owned())
            .unwrap();
        assert_eq!(fs::read_to_string(&a).unwrap(), "alpha");
        assert_eq!(fs::read_to_string(&b).unwrap(), "beta");
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("rename target already exists"))
        );

        app.rename_from_prompt(canonical_root.clone(), "workspace".to_owned())
            .unwrap();
        assert!(canonical_root.is_dir());
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("workspace root"))
        );

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_multi_select_batches_copy_paste_duplicate_and_delete() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-multi-file-ops-{}", std::process::id()));
        let src = root.join("src");
        let dst = root.join("dst");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::write(src.join("a.txt"), "alpha").unwrap();
        fs::write(src.join("b.txt"), "beta").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let a = canonical_root.join("src/a.txt");
        let b = canonical_root.join("src/b.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.explorer.reveal(&a).unwrap();
        app.toggle_explorer_multi_selection();
        app.explorer.reveal(&b).unwrap();
        app.toggle_explorer_multi_selection();

        assert_eq!(app.selected_explorer_paths(), vec![a.clone(), b.clone()]);

        app.copy_selected_path();
        assert_eq!(
            app.explorer_clipboard
                .as_ref()
                .map(|clipboard| clipboard.paths.clone()),
            Some(vec![a.clone(), b.clone()])
        );

        app.explorer.reveal(&canonical_root.join("dst")).unwrap();
        app.paste_into_selected().unwrap();
        assert_eq!(
            fs::read_to_string(canonical_root.join("dst/a.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(canonical_root.join("dst/b.txt")).unwrap(),
            "beta"
        );

        app.duplicate_selected().unwrap();
        assert_eq!(
            fs::read_to_string(canonical_root.join("src/a copy.txt")).unwrap(),
            "alpha"
        );
        assert_eq!(
            fs::read_to_string(canonical_root.join("src/b copy.txt")).unwrap(),
            "beta"
        );

        app.prompt_delete();
        assert!(matches!(
            app.prompt.as_ref().map(|prompt| &prompt.kind),
            Some(PromptKind::DeletePaths(paths)) if paths == &vec![a.clone(), b.clone()]
        ));
        app.prompt.as_mut().unwrap().input = "yes".to_owned();
        app.finish_prompt().unwrap();

        assert!(!a.exists());
        assert!(!b.exists());
        assert!(canonical_root.join("src/a copy.txt").is_file());
        assert!(canonical_root.join("dst/a.txt").is_file());
        assert!(app.explorer_multi_selection.is_empty());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_mouse_shift_and_control_click_build_multi_selection() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-multi-click-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), "alpha").unwrap();
        fs::write(root.join("b.txt"), "beta").unwrap();
        fs::write(root.join("c.txt"), "gamma").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let a = canonical_root.join("a.txt");
        let b = canonical_root.join("b.txt");
        let c = canonical_root.join("c.txt");
        let mut app = App::new(canonical_root.clone()).unwrap();
        let visible = app.visible_nodes();
        app.hit_regions.explorer_rows = visible
            .iter()
            .enumerate()
            .map(|(row, _)| (Rect::new(0, row as u16, 40, 1), row))
            .collect();
        let a_index = visible.iter().position(|node| node.path == a).unwrap();
        let c_index = visible.iter().position(|node| node.path == c).unwrap();

        app.explorer.selected = a_index;
        app.set_explorer_anchor_to_current();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: c_index as u16,
            modifiers: KeyModifiers::SHIFT,
        })
        .unwrap();

        assert_eq!(
            app.selected_explorer_paths(),
            vec![a.clone(), b.clone(), c.clone()]
        );

        let b_index = visible.iter().position(|node| node.path == b).unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: b_index as u16,
            modifiers: KeyModifiers::CONTROL,
        })
        .unwrap();

        assert_eq!(app.selected_explorer_paths(), vec![a.clone(), c.clone()]);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Right),
            column: 1,
            row: b_index as u16,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert!(app.explorer_multi_selection.is_empty());
        assert_eq!(app.selected_explorer_paths(), vec![b.clone()]);
        assert!(matches!(
            app.quick_panel.as_ref().map(|panel| &panel.kind),
            Some(QuickPanelKind::ExplorerContextMenu)
        ));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_mouse_click_opens_file_on_release() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-click-open-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("main.rs"), "fn main() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let file = canonical_root.join("main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        let visible = app.visible_nodes();
        app.hit_regions.explorer_rows = visible
            .iter()
            .enumerate()
            .map(|(row, _)| (Rect::new(0, row as u16, 40, 1), row))
            .collect();
        let file_index = visible.iter().position(|node| node.path == file).unwrap();

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: file_index as u16,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert!(app.active_tab().is_none());
        assert!(app.explorer_drag.is_some());

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 1,
            row: file_index as u16,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.focus, FocusPanel::Editor);
        assert_eq!(app.active_tab().map(|tab| tab.path.clone()), Some(file));
        assert!(app.explorer_drag.is_none());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_mouse_drag_moves_and_alt_drag_copies_selected_files() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-drag-drop-{}",
            std::process::id()
        ));
        let src = root.join("src");
        let dst = root.join("dst");
        let alt_dst = root.join("copy-dst");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&src).unwrap();
        fs::create_dir_all(&dst).unwrap();
        fs::create_dir_all(&alt_dst).unwrap();
        fs::write(src.join("a.txt"), "alpha").unwrap();
        fs::write(src.join("b.txt"), "beta").unwrap();
        fs::write(src.join("c.txt"), "gamma").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let a = canonical_root.join("src/a.txt");
        let b = canonical_root.join("src/b.txt");
        let c = canonical_root.join("src/c.txt");
        let dst = canonical_root.join("dst");
        let alt_dst = canonical_root.join("copy-dst");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&a);
        app.explorer.reveal(&a).unwrap();
        app.toggle_explorer_multi_selection();
        app.explorer.reveal(&b).unwrap();
        app.toggle_explorer_multi_selection();

        let visible = app.visible_nodes();
        app.hit_regions.explorer_rows = visible
            .iter()
            .enumerate()
            .map(|(row, _)| (Rect::new(0, row as u16, 40, 1), row))
            .collect();
        let a_index = visible.iter().position(|node| node.path == a).unwrap();
        let dst_index = visible.iter().position(|node| node.path == dst).unwrap();

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: a_index as u16,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 1,
            row: dst_index as u16,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert_eq!(app.explorer_drag_target_index(), Some(dst_index));
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 1,
            row: dst_index as u16,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert!(!a.exists());
        assert!(!b.exists());
        assert_eq!(fs::read_to_string(dst.join("a.txt")).unwrap(), "alpha");
        assert_eq!(fs::read_to_string(dst.join("b.txt")).unwrap(), "beta");
        assert_eq!(
            app.tabs
                .iter()
                .find(|tab| tab.title == "a.txt")
                .map(|tab| tab.path.clone()),
            Some(dst.join("a.txt"))
        );
        assert!(app.explorer_multi_selection.is_empty());
        assert!(app.explorer_drag.is_none());

        app.explorer.reveal(&c).unwrap();
        let visible = app.visible_nodes();
        app.hit_regions.explorer_rows = visible
            .iter()
            .enumerate()
            .map(|(row, _)| (Rect::new(0, row as u16, 40, 1), row))
            .collect();
        let c_index = visible.iter().position(|node| node.path == c).unwrap();
        let alt_dst_index = visible
            .iter()
            .position(|node| node.path == alt_dst)
            .unwrap();

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 1,
            row: c_index as u16,
            modifiers: KeyModifiers::ALT,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Drag(MouseButton::Left),
            column: 1,
            row: alt_dst_index as u16,
            modifiers: KeyModifiers::ALT,
        })
        .unwrap();
        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::Up(MouseButton::Left),
            column: 1,
            row: alt_dst_index as u16,
            modifiers: KeyModifiers::ALT,
        })
        .unwrap();

        assert_eq!(fs::read_to_string(&c).unwrap(), "gamma");
        assert_eq!(fs::read_to_string(alt_dst.join("c.txt")).unwrap(), "gamma");

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn binary_files_open_as_read_only_preview_and_do_not_rewrite_bytes() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-binary-open-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("image.bin");
        let original = b"needle\0\xffPNG\r\n".to_vec();
        fs::write(&binary, &original).unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let canonical_binary = canonical_root.join("image.bin");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&canonical_binary);

        let tab = app.active_tab().unwrap();
        assert_eq!(tab.path, canonical_binary);
        assert!(tab.read_only);
        assert!(tab.text().contains("Read-only file preview"));
        assert!(tab.text().contains("binary data was detected"));
        assert!(tab.text().contains("00000000"));
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("read-only preview"))
        );

        app.handle_key(KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE))
            .unwrap();
        app.save_active_tab();

        assert_eq!(fs::read(&canonical_binary).unwrap(), original);
        assert_eq!(app.message.as_deref(), Some("image.bin is read-only"));

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn app_new_with_binary_file_path_preserves_read_only_preview_status() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-binary-file-arg-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("image.bin");
        fs::write(&binary, b"not text\0").unwrap();

        let app = App::new(binary.clone()).unwrap();

        assert_eq!(app.root, root.canonicalize().unwrap());
        assert!(app.active_tab().unwrap().read_only);
        assert!(
            app.message
                .as_deref()
                .is_some_and(|message| message.contains("read-only preview"))
        );

        let mut app = app;
        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn read_only_previews_are_not_treated_as_workspace_text_files() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-binary-search-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("binary.bin"), b"needle\0skip").unwrap();
        fs::write(root.join("notes.txt"), "plain needle\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let binary = canonical_root.join("binary.bin");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&binary);

        let items = app.workspace_search_items("needle").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].detail, "notes.txt");
        assert!(!items.iter().any(|item| item.path == binary));

        app.kill_all_terminals();
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
            paths: vec![source.clone()],
        });
        app.explorer.reveal(&canonical_root.join("dst")).unwrap();
        app.paste_into_selected().unwrap();

        assert!(!source.exists());
        assert!(destination.exists());
        assert_eq!(app.active_tab().unwrap().path, destination);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_open_to_side_creates_real_editor_split() {
        let root =
            std::env::temp_dir().join(format!("tscode-test-editor-split-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("left.rs"), "fn left() {}\n").unwrap();
        fs::write(root.join("right.rs"), "fn right() {}\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let left = canonical_root.join("left.rs");
        let right = canonical_root.join("right.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&left);
        app.explorer.reveal(&right).unwrap();

        let commands = app.command_palette_items("open side");
        assert!(commands.iter().any(|item| {
            item.command == Some(CommandAction::OpenSelectedExplorerItemToSide)
                || item.command == Some(CommandAction::OpenActiveTabToSide)
        }));
        assert!(
            app.explorer_context_menu_items("")
                .iter()
                .any(|item| item.label == "Open to Side"
                    && item.command == Some(CommandAction::OpenSelectedExplorerItemToSide))
        );

        app.run_command(CommandAction::OpenSelectedExplorerItemToSide)
            .unwrap();

        assert_eq!(app.active_tab().unwrap().path, right);
        assert!(app.editor_split_active());
        assert_eq!(app.active_editor_pane, 1);
        assert_eq!(
            app.editor_visible_panes()
                .into_iter()
                .map(|(_, index)| app.tabs[index].path.clone())
                .collect::<Vec<_>>(),
            vec![left.clone(), right.clone()]
        );

        app.run_command(CommandAction::CloseEditorSplit).unwrap();
        assert!(!app.editor_split_active());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_split_mouse_wheel_activates_and_scrolls_hovered_pane() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-editor-split-mouse-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let left_text = (0..30)
            .map(|index| format!("left {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let right_text = (0..30)
            .map(|index| format!("right {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.join("left.rs"), format!("{left_text}\n")).unwrap();
        fs::write(root.join("right.rs"), format!("{right_text}\n")).unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let left = canonical_root.join("left.rs");
        let right = canonical_root.join("right.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&left);
        app.open_file_to_side(&right);

        app.hit_regions.editor_area = Some(Rect::new(0, 0, 81, 8));
        app.hit_regions.editor_panes = vec![
            (Rect::new(0, 0, 40, 4), 0, 0),
            (Rect::new(41, 0, 40, 4), 1, 1),
        ];
        app.hit_regions.editor_body = Some(Rect::new(41, 0, 40, 4));
        app.editor_height = 4;
        app.editor_width = 40;

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 2,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.active_tab().unwrap().path, left);
        assert_eq!(app.active_editor_pane, 0);
        assert!(app.editor_split_active());
        assert_eq!(
            app.editor_split.map(|index| app.tabs[index].path.clone()),
            Some(right)
        );
        assert!(app.tabs[0].scroll > 0);
        assert_eq!(app.tabs[1].scroll, 0);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn explorer_wheel_scrolls_tree_in_terminal_direction() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-explorer-wheel-direction-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for index in 0..12 {
            fs::write(root.join(format!("file-{index}.rs")), "fn main() {}\n").unwrap();
        }

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.focus = FocusPanel::Explorer;
        app.explorer_height = 4;
        app.hit_regions.explorer_area = Some(Rect::new(0, 0, 30, 4));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert!(app.explorer.scroll > 0);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        assert_eq!(app.explorer.scroll, 0);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn quick_panel_wheel_scrolls_results_and_keeps_selection_visible() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-quick-panel-wheel-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.quick_panel_height = 3;
        app.quick_panel = Some(QuickPanel {
            kind: QuickPanelKind::CommandPalette,
            query: String::new(),
            query_cursor: 0,
            items: (0..8)
                .map(|index| QuickItem {
                    label: format!("item {index}"),
                    detail: String::new(),
                    path: PathBuf::new(),
                    line: None,
                    col: None,
                    preview: None,
                    command: None,
                })
                .collect(),
            selected: 0,
            scroll: 0,
        });
        app.hit_regions.quick_rows = vec![(Rect::new(0, 0, 40, 3), 0)];

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.scroll, 3);
        assert_eq!(panel.selected, 3);

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 1,
            row: 1,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();
        let panel = app.quick_panel.as_ref().unwrap();
        assert_eq!(panel.scroll, 0);
        assert!(panel.selected < app.quick_panel_height);

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_wheel_far_past_short_line_does_not_panic_hover_lookup() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-editor-wheel-hover-bounds-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("short.rs");
        fs::write(&path, "fn\nlet value = 1\nlet next = 2\nlet done = 3\n").unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let file = canonical_root.join("short.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&file);
        app.focus = FocusPanel::Editor;
        app.editor_height = 2;
        app.editor_width = 80;
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 80, 2));
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 80, 2));

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 30,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert!(app.active_tab().unwrap().scroll > 0);
        assert!(app.editor_hover.is_none());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn editor_wheel_over_symbol_scrolls_without_expensive_hover_lookup() {
        let root = std::env::temp_dir().join(format!(
            "tscode-test-editor-wheel-skip-hover-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("main.rs");
        fs::write(
            &path,
            "let value = 1\nlet next = value + 1\nlet done = next + 1\n",
        )
        .unwrap();

        let canonical_root = root.canonicalize().unwrap();
        let file = canonical_root.join("main.rs");
        let mut app = App::new(canonical_root.clone()).unwrap();
        app.open_file(&file);
        app.focus = FocusPanel::Editor;
        app.editor_height = 1;
        app.editor_width = 80;
        app.hit_regions.editor_body = Some(Rect::new(0, 0, 80, 1));
        app.hit_regions.editor_area = Some(Rect::new(0, 0, 80, 1));
        let gutter = editor_gutter_width(app.active_tab().unwrap().lines.len()) as u16;

        app.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: gutter + 5,
            row: 0,
            modifiers: KeyModifiers::empty(),
        })
        .unwrap();

        assert_eq!(app.active_tab().unwrap().scroll, 2);
        assert!(app.editor_hover.is_none());

        app.kill_all_terminals();
        let _ = fs::remove_dir_all(root);
    }
}
