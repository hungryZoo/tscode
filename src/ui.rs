use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, ClipboardAction, FocusPanel, HoverTarget},
    fs_tree::VisibleNode,
    shell::TerminalStyle,
};

const TITLE_BG: Color = Color::Rgb(32, 40, 54);
const PANEL_BG: Color = Color::Rgb(13, 17, 23);
const HOVER_BG: Color = Color::Rgb(42, 54, 71);
const ACTIVE_BG: Color = Color::Rgb(24, 64, 92);
const SEARCH_BG: Color = Color::Rgb(104, 76, 28);
const SEARCH_ACTIVE_BG: Color = Color::Rgb(222, 184, 75);
const BORDER: Color = Color::Rgb(75, 89, 110);
const ACCENT: Color = Color::Rgb(89, 169, 255);
const TEXT: Color = Color::Rgb(205, 213, 224);
const MUTED: Color = Color::Rgb(117, 132, 154);

pub fn draw(frame: &mut Frame, app: &mut App) {
    app.hit_regions.clear();

    let area = frame.area();
    let root_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(8),
            Constraint::Length(1),
        ])
        .split(area);

    draw_title(frame, app, root_chunks[0]);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(32), Constraint::Min(30)])
        .split(root_chunks[1]);

    draw_explorer(frame, app, body[0]);

    if app.terminal_maximized {
        draw_terminal(frame, app, body[1]);
    } else {
        let main = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(8),
                Constraint::Length(app.terminal_rows.max(4)),
            ])
            .split(body[1]);

        draw_editor(frame, app, main[0]);
        draw_terminal(frame, app, main[1]);
    }
    draw_quick_panel(frame, app, root_chunks[1]);
    draw_status(frame, app, root_chunks[2]);
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let text = format!(
        " tscode  {}  focus:{} ",
        app.root.display(),
        focus_name(app.focus)
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(TEXT).bg(TITLE_BG)),
        area,
    );
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(prompt) = &app.prompt {
        let text = format!(" {}: {} ", prompt_title(&prompt.kind), prompt.input);
        frame.render_widget(
            Paragraph::new(text).style(Style::default().fg(Color::White).bg(ACTIVE_BG)),
            area,
        );
        return;
    }

    let active = app
        .active_tab()
        .map(|tab| {
            let dirty = if tab.dirty { " *" } else { "" };
            let selection = tab
                .selected_text()
                .map(|text| format!("  Sel {}", text.chars().count()))
                .unwrap_or_default();
            let search = app
                .active_search_match_count()
                .map(|count| format!("  Find {count}"))
                .unwrap_or_default();
            format!(
                "{}{}  Ln {}, Col {}{}{}",
                tab.path.display(),
                dirty,
                tab.cursor_line + 1,
                tab.cursor_col + 1,
                selection,
                search
            )
        })
        .unwrap_or_else(|| "no file open".to_owned());
    let error = app
        .last_error
        .as_ref()
        .map(|error| format!("  error: {error}"))
        .unwrap_or_default();
    let message = app
        .message
        .as_ref()
        .map(|message| format!("  {message}"))
        .unwrap_or_default();
    let clipboard = app
        .explorer_clipboard
        .as_ref()
        .map(|clipboard| {
            let action = match clipboard.action {
                ClipboardAction::Copy => "copy",
                ClipboardAction::Cut => "cut",
            };
            format!("  clipboard:{action} {}", display_name(&clipboard.path))
        })
        .unwrap_or_default();
    let editor_clipboard = app
        .editor_clipboard
        .as_ref()
        .map(|text| format!("  editor-clip:{} chars", text.chars().count()))
        .unwrap_or_default();
    let text = format!(
        " {} tabs:{}  hover:{}{}{}{}{} ",
        active,
        app.tabs.len(),
        hover_name(&app.hover),
        clipboard,
        editor_clipboard,
        message,
        error
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(TEXT).bg(TITLE_BG)),
        area,
    );
}

fn draw_explorer(frame: &mut Frame, app: &mut App, area: Rect) {
    app.hit_regions.explorer_area = Some(area);
    let focused = app.focus == FocusPanel::Explorer;
    let filter = app
        .explorer_filter
        .as_ref()
        .map(|filter| format!(" filter:{filter}"))
        .unwrap_or_default();
    let hidden = if app.show_hidden {
        "hidden:on"
    } else {
        "hidden:off"
    };
    let ignored = if app.show_ignored {
        "generated:on"
    } else {
        "generated:off"
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Explorer  / filter  . hidden  i generated  n/N new  e rename  D delete  c copy  x cut  p paste  y dup  o reveal  r refresh  {hidden} {ignored}{filter} "
        ))
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), area);

    app.explorer_height = inner.height as usize;
    let nodes = app.visible_nodes();
    let max_scroll = nodes.len().saturating_sub(app.explorer_height.max(1));
    app.explorer.scroll = app.explorer.scroll.min(max_scroll);

    for (row, node) in nodes
        .iter()
        .enumerate()
        .skip(app.explorer.scroll)
        .take(inner.height as usize)
    {
        let y = inner.y + (row - app.explorer.scroll) as u16;
        let row_area = Rect::new(inner.x, y, inner.width, 1);
        app.hit_regions.explorer_rows.push((row_area, row));

        let selected = focused && app.explorer.selected == row;
        let hovered = app.hover == HoverTarget::ExplorerRow(row);
        let style = row_style(selected, hovered);
        let marker = if node.is_dir {
            if node.expanded { "-" } else { "+" }
        } else {
            " "
        };
        let indent = "  ".repeat(node.depth);
        let prefix = format!("{indent}{marker} {}", node.name);
        let suffix = explorer_node_suffix(node);
        let text = fit_with_suffix(&prefix, &suffix, row_area.width as usize);
        frame.render_widget(Paragraph::new(text).style(style), row_area);
    }
}

fn draw_editor(frame: &mut Frame, app: &mut App, area: Rect) {
    app.hit_regions.editor_area = Some(area);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(3)])
        .split(area);

    draw_tabs(frame, app, chunks[0]);

    let focused = app.focus == FocusPanel::Editor;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Editor  Ctrl-S save  Ctrl-F find  Ctrl-H replace  Ctrl-A/C/X/V clipboard ")
        .border_style(border_style(focused));
    let inner = block.inner(chunks[1]);
    app.hit_regions.editor_body = Some(inner);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), chunks[1]);

    app.editor_height = inner.height as usize;
    let Some(active_index) = app.active_tab else {
        frame.render_widget(
            Paragraph::new("Click a file in Explorer to open it.")
                .style(Style::default().fg(MUTED).bg(PANEL_BG)),
            inner,
        );
        return;
    };

    let search_needle = app
        .search_needle
        .clone()
        .filter(|needle| !needle.is_empty());
    let Some(tab) = app.tabs.get_mut(active_index) else {
        return;
    };

    let height = inner.height as usize;
    let max_scroll = tab.lines.len().saturating_sub(height.max(1));
    tab.scroll = tab.scroll.min(max_scroll);
    let start = tab.scroll;
    let end = (start + height).min(tab.lines.len());
    let number_width = tab.lines.len().max(1).to_string().len().max(3);
    let highlighted = app
        .syntax
        .highlight_visible(&tab.path, &tab.lines, start, end);

    let mut rendered = Vec::new();
    for (offset, line_index) in (start..end).enumerate() {
        let mut spans = vec![
            Span::styled(
                format!("{:>width$} ", line_index + 1, width = number_width),
                Style::default().fg(MUTED),
            ),
            Span::raw(" "),
        ];
        if let Some((selection_start, selection_end)) = line_selection_range(tab, line_index) {
            let source = &tab.lines[line_index];
            spans.push(Span::raw(take_chars(source, selection_start)));
            spans.push(Span::styled(
                slice_chars(source, selection_start, selection_end),
                Style::default().fg(Color::White).bg(ACTIVE_BG),
            ));
            spans.push(Span::raw(skip_chars(source, selection_end)));
        } else if let Some(needle) = &search_needle
            && let Some(search_spans) = search_line_spans(tab, line_index, needle, focused)
        {
            spans.extend(search_spans);
        } else if focused && line_index == tab.cursor_line {
            let cursor_col = tab.cursor_col;
            let source = &tab.lines[line_index];
            let before = take_chars(source, cursor_col);
            let cursor = source.chars().nth(cursor_col).unwrap_or(' ');
            let after = skip_chars(source, cursor_col.saturating_add(1));
            spans.push(Span::raw(before));
            spans.push(Span::styled(
                cursor.to_string(),
                Style::default().fg(Color::Black).bg(ACCENT),
            ));
            spans.push(Span::raw(after));
        } else if let Some(parts) = highlighted.get(offset) {
            spans.extend(parts.clone());
        }
        rendered.push(Line::from(spans));
    }

    frame.render_widget(
        Paragraph::new(rendered).style(Style::default().fg(TEXT).bg(PANEL_BG)),
        inner,
    );
}

fn draw_tabs(frame: &mut Frame, app: &mut App, area: Rect) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().fg(TEXT).bg(Color::Rgb(18, 24, 33))),
        area,
    );

    let mut x = area.x;
    for (index, tab) in app.tabs.iter().enumerate() {
        if x >= area.x.saturating_add(area.width) {
            break;
        }

        let dirty = if tab.dirty { "*" } else { "" };
        let label = format!(" {}{} x ", tab.title, dirty);
        let width = label.width().clamp(8, 24) as u16;
        let remaining = area.x.saturating_add(area.width).saturating_sub(x);
        let width = width.min(remaining);
        let rect = Rect::new(x, area.y, width, 1);
        app.hit_regions.tabs.push((rect, index));
        if width >= 3 {
            app.hit_regions.tab_closes.push((
                Rect::new(rect.right().saturating_sub(3), rect.y, 3, 1),
                index,
            ));
        }

        let active = app.active_tab == Some(index);
        let hovered =
            app.hover == HoverTarget::Tab(index) || app.hover == HoverTarget::TabClose(index);
        let style = if active {
            Style::default()
                .fg(Color::White)
                .bg(ACTIVE_BG)
                .add_modifier(Modifier::BOLD)
        } else if hovered {
            Style::default().fg(Color::White).bg(HOVER_BG)
        } else {
            Style::default().fg(TEXT).bg(Color::Rgb(18, 24, 33))
        };
        frame.render_widget(Paragraph::new(label).style(style), rect);
        x = x.saturating_add(width);
    }
}

fn draw_terminal(frame: &mut Frame, app: &mut App, area: Rect) {
    app.hit_regions.terminal_area = Some(area);
    let focused = app.focus == FocusPanel::Terminal;
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Terminal  real pty shell  F6 focus  F12 max  Shift-Page scroll  Ctrl-Q quit app ")
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), area);

    if inner.height == 0 {
        return;
    }

    app.hit_regions.terminal_body = Some(inner);
    app.hit_regions.terminal_input = Some(inner);
    app.terminal_height = inner.height as usize;
    app.terminal.resize(inner.height, inner.width);

    let lines = app
        .terminal
        .styled_rows()
        .into_iter()
        .map(|row| {
            Line::from(
                row.into_iter()
                    .map(|span| Span::styled(span.text, terminal_style(span.style)))
                    .collect::<Vec<_>>(),
            )
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(TEXT).bg(PANEL_BG)),
        inner,
    );

    if focused {
        let (row, col) = app.terminal.cursor();
        let x = inner
            .x
            .saturating_add(col)
            .min(inner.right().saturating_sub(1));
        let y = inner
            .y
            .saturating_add(row)
            .min(inner.bottom().saturating_sub(1));
        frame.set_cursor_position((x, y));
    }
}

fn draw_quick_panel(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(panel) = &mut app.quick_panel else {
        return;
    };
    let width = area.width.saturating_sub(4).clamp(24, 82).min(area.width);
    let height = area.height.saturating_sub(2).clamp(6, 18).min(area.height);
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + 1;
    let panel_area = Rect::new(x, y, width, height);
    let title = match panel.kind {
        crate::app::QuickPanelKind::OpenFile => " Quick Open  Ctrl-P ",
        crate::app::QuickPanelKind::WorkspaceSearch => " Search Workspace  Ctrl-Shift-F ",
        crate::app::QuickPanelKind::CommandPalette => " Command Palette  F1 / Ctrl-Shift-P ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!("{title}  {} ", panel.query))
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(panel_area);
    app.quick_panel_height = inner.height as usize;
    let max_scroll = panel
        .items
        .len()
        .saturating_sub(app.quick_panel_height.max(1));
    panel.scroll = panel.scroll.min(max_scroll);

    frame.render_widget(Clear, panel_area);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), panel_area);

    if panel.items.is_empty() {
        let empty = match panel.kind {
            crate::app::QuickPanelKind::OpenFile => "Type a file name or path fragment.",
            crate::app::QuickPanelKind::WorkspaceSearch => {
                "Type text to search across workspace files."
            }
            crate::app::QuickPanelKind::CommandPalette => "Type a command name.",
        };
        frame.render_widget(
            Paragraph::new(empty).style(Style::default().fg(MUTED).bg(PANEL_BG)),
            inner,
        );
        return;
    }

    for (offset, item_index) in
        (panel.scroll..(panel.scroll + inner.height as usize).min(panel.items.len())).enumerate()
    {
        let y = inner.y + offset as u16;
        let row_area = Rect::new(inner.x, y, inner.width, 1);
        app.hit_regions.quick_rows.push((row_area, item_index));
        let selected = panel.selected == item_index;
        let hovered = app.hover == HoverTarget::QuickRow(item_index);
        let style = row_style(selected, hovered);
        let item = &panel.items[item_index];
        let preview = item
            .preview
            .as_ref()
            .map(|preview| format!("  {preview}"))
            .unwrap_or_default();
        let text = format!("{}  {}{}", item.label, item.detail, preview);
        frame.render_widget(Paragraph::new(text).style(style), row_area);
    }
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(BORDER)
    }
}

fn explorer_node_suffix(node: &VisibleNode) -> String {
    if node.is_dir {
        if node.readonly {
            " ro".to_owned()
        } else {
            String::new()
        }
    } else {
        let size = node.size.map(format_size).unwrap_or_else(|| "?".to_owned());
        if node.readonly {
            format!(" {size} ro")
        } else {
            format!(" {size}")
        }
    }
}

fn fit_with_suffix(prefix: &str, suffix: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }
    if suffix.is_empty() {
        return truncate_width(prefix, width);
    }

    let suffix_width = suffix.width();
    if suffix_width + 1 >= width {
        return truncate_width(prefix, width);
    }

    let prefix_width = prefix.width();
    if prefix_width + suffix_width >= width {
        let prefix_width = width.saturating_sub(suffix_width + 1);
        return format!(
            "{} {}",
            truncate_width(prefix, prefix_width),
            suffix.trim_start()
        );
    }

    format!(
        "{}{}{}",
        prefix,
        " ".repeat(width.saturating_sub(prefix_width + suffix_width)),
        suffix
    )
}

fn truncate_width(text: &str, width: usize) -> String {
    let mut rendered = String::new();
    let mut used = 0usize;
    for c in text.chars() {
        let char_width = c.to_string().width().max(1);
        if used + char_width > width {
            break;
        }
        rendered.push(c);
        used += char_width;
    }
    rendered
}

fn format_size(size: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    if size >= MIB {
        format!("{}M", size / MIB)
    } else if size >= KIB {
        format!("{}K", size / KIB)
    } else {
        format!("{size}B")
    }
}

fn row_style(selected: bool, hovered: bool) -> Style {
    if selected {
        Style::default().fg(Color::White).bg(ACTIVE_BG)
    } else if hovered {
        Style::default().fg(Color::White).bg(HOVER_BG)
    } else {
        Style::default().fg(TEXT).bg(PANEL_BG)
    }
}

fn terminal_style(style: TerminalStyle) -> Style {
    let mut fg = terminal_color(style.fg).unwrap_or(TEXT);
    let mut bg = terminal_color(style.bg).unwrap_or(PANEL_BG);
    if style.inverse {
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut rendered = Style::default().fg(fg).bg(bg);
    if style.bold {
        rendered = rendered.add_modifier(Modifier::BOLD);
    }
    if style.dim {
        rendered = rendered.add_modifier(Modifier::DIM);
    }
    if style.italic {
        rendered = rendered.add_modifier(Modifier::ITALIC);
    }
    if style.underline {
        rendered = rendered.add_modifier(Modifier::UNDERLINED);
    }
    rendered
}

fn terminal_color(color: vt100::Color) -> Option<Color> {
    match color {
        vt100::Color::Default => None,
        vt100::Color::Rgb(red, green, blue) => Some(Color::Rgb(red, green, blue)),
        vt100::Color::Idx(index) => Some(match index {
            0 => Color::Black,
            1 => Color::Red,
            2 => Color::Green,
            3 => Color::Yellow,
            4 => Color::Blue,
            5 => Color::Magenta,
            6 => Color::Cyan,
            7 => Color::Gray,
            8 => Color::DarkGray,
            9 => Color::LightRed,
            10 => Color::LightGreen,
            11 => Color::LightYellow,
            12 => Color::LightBlue,
            13 => Color::LightMagenta,
            14 => Color::LightCyan,
            15 => Color::White,
            index => Color::Indexed(index),
        }),
    }
}

fn focus_name(focus: FocusPanel) -> &'static str {
    match focus {
        FocusPanel::Explorer => "explorer",
        FocusPanel::Editor => "editor",
        FocusPanel::Terminal => "terminal",
    }
}

fn hover_name(hover: &HoverTarget) -> String {
    match hover {
        HoverTarget::None => "none".to_owned(),
        HoverTarget::Explorer => "explorer".to_owned(),
        HoverTarget::ExplorerRow(index) => format!("explorer row {index}"),
        HoverTarget::Editor => "editor".to_owned(),
        HoverTarget::Tab(index) => format!("tab {index}"),
        HoverTarget::TabClose(index) => format!("tab close {index}"),
        HoverTarget::QuickRow(index) => format!("quick row {index}"),
        HoverTarget::Terminal => "terminal".to_owned(),
        HoverTarget::TerminalInput => "terminal input".to_owned(),
    }
}

fn prompt_title(kind: &crate::app::PromptKind) -> &'static str {
    match kind {
        crate::app::PromptKind::NewFile => "new file",
        crate::app::PromptKind::NewDir => "new folder",
        crate::app::PromptKind::Rename(_) => "rename",
        crate::app::PromptKind::Delete(_) => "delete: type yes",
        crate::app::PromptKind::ExplorerFilter => "explorer filter",
        crate::app::PromptKind::Search => "find",
        crate::app::PromptKind::ReplaceFind { all } => {
            if *all {
                "replace all: find"
            } else {
                "replace: find"
            }
        }
        crate::app::PromptKind::ReplaceWith { all, .. } => {
            if *all {
                "replace all: with"
            } else {
                "replace: with"
            }
        }
        crate::app::PromptKind::GotoLine => "go to line",
        crate::app::PromptKind::QuitDirty => "unsaved: type quit",
    }
}

fn take_chars(s: &str, count: usize) -> String {
    s.chars().take(count).collect()
}

fn skip_chars(s: &str, count: usize) -> String {
    s.chars().skip(count).collect()
}

fn slice_chars(s: &str, start: usize, end: usize) -> String {
    s.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

fn line_selection_range(tab: &crate::app::EditorTab, line_index: usize) -> Option<(usize, usize)> {
    let (start, end) = tab.selection_range()?;
    let (start_line, start_col) = start;
    let (end_line, end_col) = end;
    if line_index < start_line || line_index > end_line {
        return None;
    }

    let line_len = tab.lines[line_index].chars().count();
    let selection_start = if line_index == start_line {
        start_col.min(line_len)
    } else {
        0
    };
    let selection_end = if line_index == end_line {
        end_col.min(line_len)
    } else {
        line_len
    };

    (selection_start != selection_end).then_some((selection_start, selection_end))
}

fn search_line_spans(
    tab: &crate::app::EditorTab,
    line_index: usize,
    needle: &str,
    focused: bool,
) -> Option<Vec<Span<'static>>> {
    if needle.is_empty() {
        return None;
    }

    let source = &tab.lines[line_index];
    let ranges = line_match_ranges(source, needle);
    if ranges.is_empty() {
        return None;
    }

    let cursor_col = (focused && line_index == tab.cursor_line).then_some(tab.cursor_col);
    let chars = source.chars().collect::<Vec<_>>();
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut current_style = None::<Style>;

    for (index, c) in chars.iter().enumerate() {
        let style = search_char_style(index, &ranges, cursor_col);
        if current_style == style {
            buffer.push(*c);
            continue;
        }

        flush_owned_span(&mut spans, &mut buffer, current_style);
        current_style = style;
        buffer.push(*c);
    }
    flush_owned_span(&mut spans, &mut buffer, current_style);

    if cursor_col == Some(chars.len()) {
        spans.push(Span::styled(
            " ",
            Style::default().fg(Color::Black).bg(ACCENT),
        ));
    }

    Some(spans)
}

fn search_char_style(
    char_index: usize,
    ranges: &[(usize, usize)],
    cursor_col: Option<usize>,
) -> Option<Style> {
    for (start, end) in ranges {
        if char_index >= *start && char_index < *end {
            let active = cursor_col == Some(*start);
            return Some(if active {
                Style::default().fg(Color::Black).bg(SEARCH_ACTIVE_BG)
            } else {
                Style::default().fg(Color::White).bg(SEARCH_BG)
            });
        }
    }

    if cursor_col == Some(char_index) {
        Some(Style::default().fg(Color::Black).bg(ACCENT))
    } else {
        None
    }
}

fn flush_owned_span(spans: &mut Vec<Span<'static>>, buffer: &mut String, style: Option<Style>) {
    if buffer.is_empty() {
        return;
    }

    let text = std::mem::take(buffer);
    if let Some(style) = style {
        spans.push(Span::styled(text, style));
    } else {
        spans.push(Span::raw(text));
    }
}

fn line_match_ranges(line: &str, needle: &str) -> Vec<(usize, usize)> {
    if needle.is_empty() {
        return Vec::new();
    }

    let mut ranges = Vec::new();
    let mut byte_start = 0usize;
    while byte_start <= line.len() {
        let Some(found) = line[byte_start..].find(needle) else {
            break;
        };
        let start_byte = byte_start + found;
        let end_byte = start_byte + needle.len();
        let start_col = line[..start_byte].chars().count();
        let end_col = line[..end_byte].chars().count();
        ranges.push((start_col, end_col));
        byte_start = end_byte;
    }
    ranges
}

fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_else(|| path.to_str().unwrap_or("[path]"))
        .to_owned()
}
