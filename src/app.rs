use std::{
    collections::{HashMap, HashSet},
    env, fs,
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use anyhow::{Context, Result, anyhow};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::Rect;
use serde_json::Value;

use crate::{
    fs_tree::{FsTree, VisibleNode},
    shell::{ShellPanel, TerminalSearchMatch},
    syntax::SyntaxHighlighter,
};

const MAX_QUICK_ITEMS: usize = 200;
const MAX_FILE_SCAN_BYTES: u64 = 1_000_000;
const MAX_OSC52_CLIPBOARD_BYTES: usize = 512 * 1024;
const MAX_NAVIGATION_HISTORY: usize = 200;

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
    pub dirty: bool,
    trailing_newline: bool,
    undo_stack: Vec<EditorSnapshot>,
    redo_stack: Vec<EditorSnapshot>,
}

impl EditorTab {
    fn open(path: PathBuf) -> Result<Self> {
        let bytes = std::fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes);
        let (lines, trailing_newline) = split_editor_text(&text);

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
            horizontal_scroll: 0,
            cursor_line: 0,
            cursor_col: 0,
            selection_anchor: None,
            extra_selections: Vec::new(),
            extra_cursors: Vec::new(),
            dirty: false,
            trailing_newline,
            undo_stack: Vec::new(),
            redo_stack: Vec::new(),
        })
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
        self.dirty = false;
        self.undo_stack.clear();
        self.redo_stack.clear();
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
        self.dirty = true;
        true
    }

    fn save(&mut self) -> Result<()> {
        fs::write(&self.path, self.text())?;
        self.dirty = false;
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
            self.cursor_line = 0;
            self.cursor_col = 0;
        } else {
            self.lines.drain(start..=end);
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
        self.clamp_cursor_col();
        self.clamp_selections();
    }

    fn push_undo(&mut self) {
        self.undo_stack.push(self.snapshot());
        if self.undo_stack.len() > 200 {
            self.undo_stack.remove(0);
        }
        self.redo_stack.clear();
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
    Delete(PathBuf),
    ExplorerFilter,
    Search,
    ReplaceFind { all: bool },
    ReplaceWith { needle: String, all: bool },
    WorkspaceReplaceFind,
    WorkspaceReplaceWith { needle: String },
    RenameSymbol { old: String },
    SaveAs,
    TerminalSearch,
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
    Completions,
    WorkspaceSearch,
    DocumentSymbols,
    WorkspaceSymbols,
    Definitions,
    References,
    Problems,
    SourceControl,
    Tasks,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditorLocation {
    pub path: PathBuf,
    pub line: usize,
    pub col: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandAction {
    QuickOpen,
    TriggerSuggest,
    WorkspaceSearch,
    DocumentSymbols,
    WorkspaceSymbols,
    GoToDefinition,
    FindReferences,
    GoBack,
    GoForward,
    RenameSymbol,
    WorkspaceReplace,
    RunWorkspaceCheck,
    ShowProblems,
    ShowSourceControl,
    RunTask,
    SaveFile,
    SaveAs,
    SaveAll,
    RevertFile,
    FormatDocument,
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
    AddSelectionToNextMatch,
    SelectAllOccurrences,
    DuplicateLine,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    ToggleLineComment,
    TrimTrailingWhitespace,
    IndentLine,
    OutdentLine,
    SelectAll,
    CopySelection,
    CutSelection,
    PasteClipboard,
    RunSelectionInTerminal,
    CopyTerminalSelection,
    PasteClipboardToTerminal,
    FindInTerminal,
    TerminalSearchNext,
    TerminalSearchPrevious,
    FocusExplorer,
    FocusEditor,
    FocusTerminal,
    ClearTerminal,
    RestartTerminal,
    NewTerminal,
    NewTerminalHere,
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
pub struct CompletionState {
    pub path: PathBuf,
    pub line: usize,
    pub start_col: usize,
    pub end_col: usize,
    pub prefix: String,
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
    pub cwd: PathBuf,
    pub shell: ShellPanel,
    pub exited: bool,
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
        Ok(Self {
            id,
            title,
            shell: ShellPanel::new(cwd.clone())?,
            cwd,
            exited: false,
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
    pub editor_width: usize,
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
    pub completion_state: Option<CompletionState>,
    pub quick_panel_height: usize,
    pub explorer_clipboard: Option<ExplorerClipboard>,
    pub editor_clipboard: Option<String>,
    pub git_statuses: HashMap<PathBuf, GitStatusKind>,
    pub git_dirty_dirs: HashSet<PathBuf>,
    pub navigation_back: Vec<EditorLocation>,
    pub navigation_forward: Vec<EditorLocation>,
    pub terminal_selection: Option<TerminalSelection>,
    pub terminal_search: Option<TerminalSearchState>,
    pub problems: Vec<QuickItem>,
    pending_clipboard_export: Option<String>,
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
        let mut app = Self {
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
            editor_width: 0,
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
            completion_state: None,
            quick_panel_height: 0,
            explorer_clipboard: None,
            editor_clipboard: None,
            git_statuses,
            git_dirty_dirs,
            navigation_back: Vec::new(),
            navigation_forward: Vec::new(),
            terminal_selection: None,
            terminal_search: None,
            problems: Vec::new(),
            pending_clipboard_export: None,
        };

        if let Some(file) = initial_file {
            app.open_file(&file);
            let _ = app.explorer.reveal(&file);
            app.message = Some(format!("opened {}", file.display()));
        }

        Ok(app)
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
                KeyCode::Char('`') => {
                    self.toggle_terminal_focus();
                    return Ok(());
                }
                KeyCode::Char('j') if !terminal_child_owns_keyboard => {
                    self.toggle_terminal_maximized();
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
                KeyCode::Char('p') => {
                    self.open_quick_panel(QuickPanelKind::OpenFile)?;
                    return Ok(());
                }
                KeyCode::Char(' ') | KeyCode::Null if self.focus == FocusPanel::Editor => {
                    self.trigger_suggest()?;
                    return Ok(());
                }
                KeyCode::Char('t') | KeyCode::Char('T') => {
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

        if self.terminal_selection.is_some()
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
            && self.handle_terminal_selection_mouse(mouse)?
        {
            return Ok(());
        }

        if matches!(target, HoverTarget::Terminal | HoverTarget::TerminalInput)
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
            MouseEventKind::Down(MouseButton::Left)
                if target == HoverTarget::Editor && mouse.modifiers.contains(KeyModifiers::ALT) =>
            {
                self.focus = FocusPanel::Editor;
                self.toggle_editor_cursor_from_mouse();
            }
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
            MouseEventKind::ScrollLeft => self.scroll_target_horizontal(target, -8),
            MouseEventKind::ScrollRight => self.scroll_target_horizontal(target, 8),
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
            KeyCode::Char('t') => self.new_terminal_here()?,
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
            KeyCode::Esc => {
                if self
                    .quick_panel
                    .as_ref()
                    .is_some_and(|panel| panel.kind == QuickPanelKind::Completions)
                {
                    self.completion_state = None;
                }
                self.quick_panel = None;
            }
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
        let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        if let Some(index) = self.tabs.iter().position(|tab| tab.path == path) {
            self.active_tab = Some(index);
            self.focus = FocusPanel::Editor;
            return;
        }

        match EditorTab::open(path) {
            Ok(tab) => {
                self.tabs.push(tab);
                self.active_tab = Some(self.tabs.len() - 1);
                self.focus = FocusPanel::Editor;
            }
            Err(error) => self.last_error = Some(error.to_string()),
        }
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

    fn scroll_target_horizontal(&mut self, target: HoverTarget, amount: isize) {
        match target {
            HoverTarget::Editor | HoverTarget::Tab(_) | HoverTarget::TabClose(_) => {
                self.scroll_editor_horizontal(amount)
            }
            HoverTarget::None if self.focus == FocusPanel::Editor => {
                self.scroll_editor_horizontal(amount)
            }
            _ => {}
        }
    }

    fn scroll_editor_horizontal(&mut self, amount: isize) {
        let editor_width = self.editor_width;
        if let Some(tab) = self.active_tab_mut() {
            let code_width = editor_code_width(tab, editor_width);
            let max_scroll = max_editor_horizontal_scroll(tab, code_width);
            tab.horizontal_scroll = add_signed(tab.horizontal_scroll, amount).min(max_scroll);
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
    line_count.max(1).to_string().len().max(3) + 2
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

fn command_catalog() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            label: "Quick Open",
            detail: "Open a workspace file by fuzzy path",
            shortcut: "Ctrl-P",
            action: CommandAction::QuickOpen,
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
            label: "Go to Definition",
            detail: "Jump from the symbol under the editor cursor to its workspace definition",
            shortcut: "Ctrl-]",
            action: CommandAction::GoToDefinition,
        },
        CommandSpec {
            label: "Find References",
            detail: "List whole-word workspace references for the symbol under the editor cursor",
            shortcut: "Ctrl-R",
            action: CommandAction::FindReferences,
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
            label: "Show Problems",
            detail: "Show the last collected workspace diagnostics",
            shortcut: "",
            action: CommandAction::ShowProblems,
        },
        CommandSpec {
            label: "Source Control",
            detail: "Show Git changed files and diff hunks",
            shortcut: "",
            action: CommandAction::ShowSourceControl,
        },
        CommandSpec {
            label: "Run Task",
            detail: "Detect workspace tasks and run one in a real integrated PTY terminal",
            shortcut: "Ctrl-Shift-B",
            action: CommandAction::RunTask,
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
            shortcut: "",
            action: CommandAction::SaveAll,
        },
        CommandSpec {
            label: "Revert File",
            detail: "Discard editor changes and reload the active file from disk",
            shortcut: "",
            action: CommandAction::RevertFile,
        },
        CommandSpec {
            label: "Format Document",
            detail: "Format the active editor buffer with an installed language formatter",
            shortcut: "Shift-Alt-F",
            action: CommandAction::FormatDocument,
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
            label: "New Terminal",
            detail: "Create a new integrated PTY terminal session",
            shortcut: "F7",
            action: CommandAction::NewTerminal,
        },
        CommandSpec {
            label: "New Terminal Here",
            detail: "Create a new PTY terminal in the selected explorer folder",
            shortcut: "Explorer t",
            action: CommandAction::NewTerminalHere,
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

    fn start_workspace_replace_prompt(&mut self) {
        let initial = self.search_needle.clone().unwrap_or_default();
        self.start_prompt(PromptKind::WorkspaceReplaceFind, &initial);
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
        let Some(tab) = self.active_tab() else {
            self.message = Some("no active editor for suggestions".to_owned());
            return Ok(());
        };
        let state = completion_state_for_tab(tab);
        let query = state.prefix.clone();
        self.completion_state = Some(state);
        self.open_quick_panel_with_query(QuickPanelKind::Completions, query)
    }

    fn open_quick_panel(&mut self, kind: QuickPanelKind) -> Result<()> {
        self.open_quick_panel_with_query(kind, String::new())
    }

    fn open_quick_panel_with_query(&mut self, kind: QuickPanelKind, query: String) -> Result<()> {
        if kind != QuickPanelKind::Completions {
            self.completion_state = None;
        }
        self.quick_panel = Some(QuickPanel {
            kind,
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
        let items = match kind {
            QuickPanelKind::OpenFile => self.quick_open_items(&query)?,
            QuickPanelKind::Completions => self.completion_items(&query)?,
            QuickPanelKind::WorkspaceSearch => self.workspace_search_items(&query)?,
            QuickPanelKind::DocumentSymbols => self.document_symbol_items(&query),
            QuickPanelKind::WorkspaceSymbols => self.workspace_symbol_items(&query)?,
            QuickPanelKind::Definitions => self.definition_items(&query)?,
            QuickPanelKind::References => self.reference_items(&query)?,
            QuickPanelKind::Problems => self.problem_items(&query),
            QuickPanelKind::SourceControl => self.source_control_items(&query)?,
            QuickPanelKind::Tasks => self.task_items(&query),
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

    fn completion_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let Some(active_tab) = self.active_tab() else {
            return Ok(Vec::new());
        };
        let active_path = active_tab.path.clone();
        let query = query.trim();
        let mut candidates = HashMap::<String, (CompletionRank, QuickItem)>::new();

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
        let Some(tab) = self.active_tab() else {
            return Vec::new();
        };
        let relative = relative_path(&self.root, &tab.path);
        let items = symbols_to_quick_items(&tab.path, &tab.text(), &relative, query, false);
        items.into_iter().take(MAX_QUICK_ITEMS).collect()
    }

    fn workspace_symbol_items(&self, query: &str) -> Result<Vec<QuickItem>> {
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

    fn source_control_items(&self, query: &str) -> Result<Vec<QuickItem>> {
        let Some(top_level) = git_top_level(&self.root) else {
            return Ok(Vec::new());
        };

        let (statuses, _) = load_git_status(&self.root);
        let mut items = Vec::new();
        let mut changed_files = statuses.into_iter().collect::<Vec<_>>();
        changed_files.sort_by(|(left, _), (right, _)| {
            relative_path(&top_level, left).cmp(&relative_path(&top_level, right))
        });

        for (path, status) in changed_files {
            let relative = relative_path(&top_level, &path);
            let preview = (!path.is_file()).then(|| "not available in working tree".to_owned());
            items.push(QuickItem {
                label: format!("{} {relative}", status.short_label()),
                detail: status.description().to_owned(),
                path,
                line: None,
                col: None,
                preview,
                command: None,
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

    fn workspace_text_files(&self) -> Result<Vec<WorkspaceTextFile>> {
        let open_texts = self
            .tabs
            .iter()
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
                let destination = destination
                    .canonicalize()
                    .unwrap_or_else(|_| destination.clone());
                self.update_open_tabs_for_move(&source, &destination);
                self.update_navigation_for_move(&source, &destination);
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
            PromptKind::TerminalSearch => self.terminal_search_from_prompt(prompt.input),
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
        self.expand_active_explorer_filter_matches();
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
        self.expand_active_explorer_filter_matches();
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
        let new_path = new_path.canonicalize().unwrap_or(new_path);
        self.update_open_tabs_for_move(&path, &new_path);
        self.update_navigation_for_move(&path, &new_path);
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
        self.prune_navigation_for_deleted_path(&path);
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
        let selected = self
            .visible_nodes()
            .get(self.explorer.selected)
            .map(|node| node.path.clone());
        self.explorer.refresh()?;
        self.refresh_git_status();
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

    fn refresh_git_status(&mut self) {
        let (statuses, dirty_dirs) = load_git_status(&self.root);
        self.git_statuses = statuses;
        self.git_dirty_dirs = dirty_dirs;
    }

    fn collapse_explorer(&mut self) {
        self.explorer.collapse_all();
        self.explorer.selected = 0;
        self.explorer.scroll = 0;
        self.message = Some("explorer collapsed".to_owned());
    }

    fn save_active_tab(&mut self) {
        let Some(tab) = self.active_tab_mut() else {
            return;
        };
        let path = tab.path.clone();
        match tab.save() {
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
        let Some(target) = resolve_prompt_path(&self.root, &input) else {
            self.message = Some("save as cancelled".to_owned());
            return Ok(());
        };
        if target.is_dir() {
            self.message = Some(format!(
                "save as target is a directory: {}",
                target.display()
            ));
            return Ok(());
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
            return Ok(());
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
                self.active_tab = Some(index - 1);
            }
        }

        let Some(active_index) = self.active_tab else {
            return Ok(());
        };
        let title = saved_path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .unwrap_or("[file]")
            .to_owned();
        self.tabs[active_index].path = saved_path.clone();
        self.tabs[active_index].title = title;
        self.tabs[active_index].dirty = false;

        self.refresh_explorer()?;
        if saved_path.starts_with(&self.root) {
            self.reveal_path(&saved_path)?;
        }
        self.focus = FocusPanel::Editor;
        self.refresh_git_status();
        self.message = Some(format!("saved as {}", saved_path.display()));
        Ok(())
    }

    fn revert_active_tab(&mut self) -> Result<()> {
        let Some(index) = self.active_tab else {
            self.message = Some("no active file to revert".to_owned());
            return Ok(());
        };

        let path = self.tabs[index].path.clone();
        let title = self.tabs[index].title.clone();
        let previous_text = self.tabs[index].text();
        let bytes = fs::read(&path)?;
        let text = String::from_utf8_lossy(&bytes);
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
        if saved > 0 {
            self.refresh_git_status();
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
        self.rename_symbol_occurrences(old, new_name)
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

    fn trim_active_trailing_whitespace(&mut self) {
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

        let path = self.tabs[index].path.clone();
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
        let title = self.tabs[index].title.clone();
        if self.tabs[index].replace_entire_text_as_edit(&formatted) {
            self.ensure_editor_cursor_visible();
            self.message = Some(format!("formatted {title} with {}", formatter.label));
        } else {
            self.message = Some(format!("already formatted with {}", formatter.label));
        }
        Ok(())
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

    fn run_selection_in_terminal(&mut self) -> Result<()> {
        let Some(text) = self.editor_text_for_terminal_submission() else {
            self.message = Some("no editor text to run in terminal".to_owned());
            return Ok(());
        };

        let submitted = terminal_submission_text(&text);
        self.active_terminal_mut().shell.send_text(&submitted)?;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!(
            "sent {} line(s) to terminal",
            text.lines().count().max(1)
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
        let id = self.next_terminal_id;
        self.next_terminal_id += 1;
        let title = format!("task: {}", truncate_chars(&item.label, 28));
        let terminal = TerminalSession::with_title(id, title.clone(), cwd)?;
        self.terminals.push(terminal);
        self.active_terminal = self.terminals.len() - 1;
        self.focus = FocusPanel::Terminal;
        self.terminal_selection = None;

        let submitted = terminal_submission_text(&command);
        self.active_terminal_mut().shell.send_text(&submitted)?;
        self.message = Some(format!("started {title}: {command}"));
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
        let definitions = self.definition_items(&symbol)?;

        match definitions.len() {
            0 => {
                self.message = Some(format!("definition not found: {symbol}"));
            }
            1 => {
                let item = definitions.into_iter().next().unwrap();
                self.open_quick_item(item, None);
                self.focus = FocusPanel::Editor;
                self.message = Some(format!("jumped to definition: {symbol}"));
            }
            _ => {
                self.quick_panel = Some(QuickPanel {
                    kind: QuickPanelKind::Definitions,
                    query: symbol.clone(),
                    items: definitions,
                    selected: 0,
                    scroll: 0,
                });
                self.message = Some(format!("multiple definitions: {symbol}"));
            }
        }

        Ok(())
    }

    fn find_references_under_cursor(&mut self) -> Result<()> {
        let Some(symbol) = self.active_identifier_under_cursor() else {
            self.message = Some("no symbol under cursor".to_owned());
            return Ok(());
        };
        self.open_quick_panel_with_query(QuickPanelKind::References, symbol)
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
        if let Some(tab) = self.active_tab_mut() {
            if tab.cursor_line < tab.scroll {
                tab.scroll = tab.cursor_line;
            } else if tab.cursor_line >= tab.scroll + height {
                tab.scroll = tab.cursor_line.saturating_sub(height - 1);
            }

            let code_width = editor_code_width(tab, editor_width);
            if code_width > 0 {
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
        let HoverTarget::Editor = self.hover else {
            return;
        };
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

    fn editor_mouse_position(&self) -> Option<(usize, usize)> {
        let body = self.hit_regions.editor_body?;
        let tab = self.active_tab()?;
        let line_number_width = tab.lines.len().max(1).to_string().len().max(3) + 2;
        let local_x = self.hit_regions.last_mouse_x.saturating_sub(body.x) as usize;
        let col = if local_x < line_number_width {
            0
        } else {
            local_x.saturating_sub(line_number_width) + tab.horizontal_scroll
        };
        let line = tab.scroll + self.hit_regions.last_mouse_y.saturating_sub(body.y) as usize;
        Some((line, col))
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

        if kind == QuickPanelKind::Completions {
            self.quick_panel = None;
            self.apply_completion_item(item);
            return;
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

    fn apply_completion_item(&mut self, item: QuickItem) {
        let Some(state) = self.completion_state.take() else {
            self.message = Some("no active completion request".to_owned());
            return;
        };

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

    fn run_command(&mut self, command: CommandAction) -> Result<()> {
        match command {
            CommandAction::QuickOpen => self.open_quick_panel(QuickPanelKind::OpenFile)?,
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
            CommandAction::GoToDefinition => self.go_to_definition_under_cursor()?,
            CommandAction::FindReferences => self.find_references_under_cursor()?,
            CommandAction::GoBack => self.go_back(),
            CommandAction::GoForward => self.go_forward(),
            CommandAction::RenameSymbol => self.start_rename_symbol_prompt(),
            CommandAction::WorkspaceReplace => self.start_workspace_replace_prompt(),
            CommandAction::RunWorkspaceCheck => self.run_workspace_check()?,
            CommandAction::ShowProblems => self.open_quick_panel(QuickPanelKind::Problems)?,
            CommandAction::ShowSourceControl => {
                self.refresh_git_status();
                if git_top_level(&self.root).is_none() {
                    self.message = Some("not a git repository".to_owned());
                }
                self.open_quick_panel(QuickPanelKind::SourceControl)?;
            }
            CommandAction::RunTask => self.open_quick_panel(QuickPanelKind::Tasks)?,
            CommandAction::SaveFile => self.save_active_tab(),
            CommandAction::SaveAs => self.start_save_as_prompt(),
            CommandAction::SaveAll => self.save_all_tabs(),
            CommandAction::RevertFile => self.revert_active_tab()?,
            CommandAction::FormatDocument => self.format_active_document()?,
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
            CommandAction::AddSelectionToNextMatch => self.add_selection_to_next_match(),
            CommandAction::SelectAllOccurrences => self.select_all_occurrences_in_active_tab(),
            CommandAction::DuplicateLine => self.duplicate_active_line(),
            CommandAction::DeleteLine => self.delete_active_line(),
            CommandAction::MoveLineUp => self.move_active_line_up(),
            CommandAction::MoveLineDown => self.move_active_line_down(),
            CommandAction::ToggleLineComment => self.toggle_active_line_comment(),
            CommandAction::TrimTrailingWhitespace => self.trim_active_trailing_whitespace(),
            CommandAction::IndentLine => self.indent_active_line(),
            CommandAction::OutdentLine => self.outdent_active_line(),
            CommandAction::SelectAll => self.select_all_active_tab(),
            CommandAction::CopySelection => self.copy_editor_selection(),
            CommandAction::CutSelection => self.cut_editor_selection(),
            CommandAction::PasteClipboard => self.paste_editor_clipboard(),
            CommandAction::RunSelectionInTerminal => self.run_selection_in_terminal()?,
            CommandAction::CopyTerminalSelection => self.copy_terminal_selection(),
            CommandAction::PasteClipboardToTerminal => self.paste_clipboard_to_terminal()?,
            CommandAction::FindInTerminal => self.start_terminal_search_prompt(),
            CommandAction::TerminalSearchNext => self.next_terminal_search_match(),
            CommandAction::TerminalSearchPrevious => self.previous_terminal_search_match(),
            CommandAction::FocusExplorer => self.focus = FocusPanel::Explorer,
            CommandAction::FocusEditor => self.focus = FocusPanel::Editor,
            CommandAction::FocusTerminal => self.focus = FocusPanel::Terminal,
            CommandAction::ClearTerminal => {
                self.active_terminal_mut().shell.clear();
                self.message = Some("terminal cleared".to_owned());
            }
            CommandAction::RestartTerminal => self.restart_terminal()?,
            CommandAction::NewTerminal => self.new_terminal()?,
            CommandAction::NewTerminalHere => self.new_terminal_here()?,
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

    fn forward_terminal_mouse_event(
        &mut self,
        kind: MouseEventKind,
        modifiers: KeyModifiers,
    ) -> Result<bool> {
        let Some(body) = self.hit_regions.terminal_body else {
            return Ok(false);
        };
        let row = self.hit_regions.last_mouse_y.saturating_sub(body.y);
        let col = self.hit_regions.last_mouse_x.saturating_sub(body.x);
        self.active_terminal_mut()
            .shell
            .send_mouse_event(kind, row, col, modifiers)
    }

    fn start_terminal_selection_from_mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(cell) = self.terminal_mouse_cell(mouse) else {
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
        let body = self.hit_regions.terminal_body?;
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

    pub fn terminal_selection_for_active(&self) -> Option<&TerminalSelection> {
        let selection = self.terminal_selection.as_ref()?;
        (selection.terminal_id == self.active_terminal().id).then_some(selection)
    }

    pub fn terminal_selection_columns_for_row(&self, row: u16) -> Option<(usize, usize)> {
        let selection = self.terminal_selection_for_active()?;
        let (_, cols) = self.active_terminal().shell.size();
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

    pub fn terminal_search_ranges_for_row(&mut self, row: u16) -> Vec<(usize, usize, bool)> {
        let active_id = self.active_terminal().id;
        let visible_top = self.active_terminal_mut().shell.visible_top_row();
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
        let Some(reference) = terminal_file_reference_at(&line, col as usize, &self.root) else {
            return false;
        };

        self.push_navigation_location_for_jump(&reference.path, reference.line, reference.col);
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
        let id = self.active_terminal().id;
        let cwd = self.active_terminal().cwd.clone();
        let _ = self.active_terminal_mut().shell.kill();
        self.terminal_selection = None;
        self.terminals[self.active_terminal] = TerminalSession {
            id,
            title: title.clone(),
            shell: ShellPanel::new(cwd.clone())?,
            cwd,
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
        self.active_terminal = self.terminals.len() - 1;
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("new terminal in {}: {title}", cwd.display()));
        Ok(())
    }

    fn select_terminal(&mut self, index: usize) {
        if index >= self.terminals.len() {
            return;
        }
        self.active_terminal = index;
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
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("closed terminal: {title}"));
        Ok(())
    }

    fn next_terminal(&mut self) {
        if self.terminals.is_empty() {
            return;
        }
        self.active_terminal = (self.active_terminal + 1) % self.terminals.len();
        self.terminal_selection = None;
        self.focus = FocusPanel::Terminal;
        self.message = Some(format!("terminal: {}", self.active_terminal().title));
    }

    fn previous_terminal(&mut self) {
        if self.terminals.is_empty() {
            return;
        }
        self.active_terminal =
            (self.active_terminal + self.terminals.len() - 1) % self.terminals.len();
        self.terminal_selection = None;
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

fn split_editor_text(text: &str) -> (Vec<String>, bool) {
    let trailing_newline = text.ends_with('\n');
    let mut lines = text.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    if lines.is_empty() {
        lines.push(String::new());
    }
    (lines, trailing_newline)
}

fn terminal_submission_text(text: &str) -> String {
    let mut normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if !normalized.ends_with('\n') {
        normalized.push('\n');
    }
    normalized.replace('\n', "\r")
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
    let mut files = Vec::new();
    collect_workspace_files_into(root, &mut files, show_hidden, show_ignored)?;
    files.sort();
    Ok(files)
}

fn collect_workspace_paths(
    root: &Path,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    collect_workspace_paths_into(root, &mut paths, show_hidden, show_ignored)?;
    paths.sort();
    Ok(paths)
}

fn collect_workspace_paths_into(
    dir: &Path,
    paths: &mut Vec<PathBuf>,
    show_hidden: bool,
    show_ignored: bool,
) -> Result<()> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(dir)? {
        entries.push(entry?);
    }
    entries.sort_by_key(|entry| entry.file_name());

    for entry in entries {
        let path = entry.path();
        let file_type = entry.file_type()?;
        let name = entry.file_name();
        let hidden = name.to_str().is_some_and(is_hidden_file_name);
        if file_type.is_dir() {
            if (!show_hidden && hidden) || (!show_ignored && should_skip_dir(&path)) {
                continue;
            }
            paths.push(path.clone());
            let _ = collect_workspace_paths_into(&path, paths, show_hidden, show_ignored);
        } else if file_type.is_file() && (show_hidden || !hidden) {
            paths.push(path);
        }
    }
    Ok(())
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

fn load_git_status(root: &Path) -> (HashMap<PathBuf, GitStatusKind>, HashSet<PathBuf>) {
    let Some(top_level) = git_top_level(root) else {
        return (HashMap::new(), HashSet::new());
    };

    let Ok(output) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["status", "--porcelain=v1", "-z", "--untracked-files=all"])
        .output()
    else {
        return (HashMap::new(), HashSet::new());
    };
    if !output.status.success() {
        return (HashMap::new(), HashSet::new());
    }

    let statuses = parse_git_status_z(&output.stdout, &top_level);
    let dirty_dirs = git_dirty_directories(&statuses, root);
    (statuses, dirty_dirs)
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

fn parse_git_status_z(output: &[u8], top_level: &Path) -> HashMap<PathBuf, GitStatusKind> {
    let mut statuses = HashMap::new();
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
        statuses.insert(path, status);

        if matches!(x, b'R' | b'C') || matches!(y, b'R' | b'C') {
            let _ = records.next();
        }
    }

    statuses
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

    let mut index = char_col.min(chars.len().saturating_sub(1));
    if char_col >= chars.len() || !is_symbol_ident_continue(chars[index]) {
        if char_col > 0 && is_symbol_ident_continue(chars[char_col - 1]) {
            index = char_col - 1;
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
    use std::{thread, time::Duration};

    fn temp_file(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("tscode-test-{}-{name}", std::process::id()))
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
        assert_eq!(app.active_tab().unwrap().horizontal_scroll, 14);

        app.scroll_editor_horizontal(100);
        assert_eq!(app.active_tab().unwrap().horizontal_scroll, 29);

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
            column: 5,
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
            column: 6,
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
        let commands = app.command_palette_items("find references");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::FindReferences))
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
        let commands = app.command_palette_items("copy terminal selection");
        assert!(
            commands
                .iter()
                .any(|item| item.command == Some(CommandAction::CopyTerminalSelection))
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
            detail: command,
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
        assert!(
            app.terminal_search_ranges_for_row(local_row)
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
                input: "make_client".to_owned()
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
    fn git_status_parser_marks_files_and_dirty_parent_dirs() {
        let root = std::env::temp_dir().join(format!("tscode-test-git-{}", std::process::id()));
        let src = root.join("src");
        let statuses = parse_git_status_z(
            b" M src/main.rs\0?? src/new.rs\0R  src/lib.rs\0src/old.rs\0UU src/conflict.rs\0",
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
        if Command::new("git").arg("--version").output().is_err() {
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
        assert!(panel.items.iter().any(|item| item.label == "M src/lib.rs"));
        assert!(panel.items.iter().any(|item| item.label == "? new.txt"));
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
