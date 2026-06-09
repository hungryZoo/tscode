use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::app::{App, FocusPanel, HoverTarget};

const TITLE_BG: Color = Color::Rgb(32, 40, 54);
const PANEL_BG: Color = Color::Rgb(13, 17, 23);
const HOVER_BG: Color = Color::Rgb(42, 54, 71);
const ACTIVE_BG: Color = Color::Rgb(24, 64, 92);
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

    let main = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(8), Constraint::Length(10)])
        .split(body[1]);

    draw_editor(frame, app, main[0]);
    draw_terminal(frame, app, main[1]);
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
    let active = app
        .active_tab()
        .map(|tab| tab.path.display().to_string())
        .unwrap_or_else(|| "no file open".to_owned());
    let error = app
        .last_error
        .as_ref()
        .map(|error| format!("  error: {error}"))
        .unwrap_or_default();
    let text = format!(
        " {} tabs:{}  hover:{}{} ",
        active,
        app.tabs.len(),
        hover_name(&app.hover),
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Explorer ")
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
        let text = format!("{indent}{marker} {}", node.name);
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
        .title(" Editor ")
        .border_style(border_style(focused));
    let inner = block.inner(chunks[1]);
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
        if let Some(parts) = highlighted.get(offset) {
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

        let label = format!(" {} x ", tab.title);
        let width = label.width().clamp(8, 24) as u16;
        let remaining = area.x.saturating_add(area.width).saturating_sub(x);
        let width = width.min(remaining);
        let rect = Rect::new(x, area.y, width, 1);
        app.hit_regions.tabs.push((rect, index));

        let active = app.active_tab == Some(index);
        let hovered = app.hover == HoverTarget::Tab(index);
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
        .title(" Terminal ")
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), area);

    if inner.height == 0 {
        return;
    }

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    let output_area = chunks[0];
    let input_area = chunks[1];
    app.hit_regions.terminal_input = Some(input_area);
    app.terminal_height = output_area.height as usize;

    let max_scroll = app.terminal.max_scroll(app.terminal_height.max(1));
    app.terminal.scroll = app.terminal.scroll.min(max_scroll);
    let start = app.terminal.scroll;
    let end = (start + output_area.height as usize).min(app.terminal.lines.len());
    let lines = app.terminal.lines[start..end]
        .iter()
        .map(|line| Line::from(Span::raw(line.clone())))
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(TEXT).bg(PANEL_BG)),
        output_area,
    );

    let hovered = matches!(
        app.hover,
        HoverTarget::TerminalInput | HoverTarget::Terminal
    );
    let input_style = if focused {
        Style::default().fg(Color::White).bg(ACTIVE_BG)
    } else if hovered {
        Style::default().fg(Color::White).bg(HOVER_BG)
    } else {
        Style::default().fg(TEXT).bg(Color::Rgb(18, 24, 33))
    };
    let prompt = format!("> {}", app.terminal.input);
    frame.render_widget(Paragraph::new(prompt).style(input_style), input_area);
}

fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(BORDER)
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
        HoverTarget::Terminal => "terminal".to_owned(),
        HoverTarget::TerminalInput => "terminal input".to_owned(),
    }
}
