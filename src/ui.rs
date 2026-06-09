use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{App, ClipboardAction, ExternalFileState, FocusPanel, HoverTarget, ProblemSeverity},
    fs_tree::VisibleNode,
    shell::{TerminalSpan, TerminalStyle},
};

const TITLE_BG: Color = Color::Rgb(32, 40, 54);
const PANEL_BG: Color = Color::Rgb(13, 17, 23);
const HOVER_BG: Color = Color::Rgb(42, 54, 71);
const ACTIVE_BG: Color = Color::Rgb(24, 64, 92);
const SEARCH_BG: Color = Color::Rgb(104, 76, 28);
const SEARCH_ACTIVE_BG: Color = Color::Rgb(222, 184, 75);
const ERROR_BG: Color = Color::Rgb(61, 27, 34);
const WARNING_BG: Color = Color::Rgb(58, 48, 24);
const INFO_BG: Color = Color::Rgb(29, 44, 61);
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
            let disk = match tab.external_state {
                ExternalFileState::Clean => String::new(),
                state => format!("  Disk {}", state.label()),
            };
            let selection = if let Some(text) = tab.selected_text() {
                let count = tab.selection_count();
                if count > 1 {
                    format!("  Sel {}x/{} chars", count, text.chars().count())
                } else {
                    format!("  Sel {}", text.chars().count())
                }
            } else if tab.selection_count() > 1 {
                format!("  Cursors {}", tab.selection_count())
            } else {
                String::new()
            };
            let search = app
                .active_search_match_count()
                .map(|count| format!("  Find {count}"))
                .unwrap_or_default();
            let problem_count = app.active_file_problem_count();
            let problems = if problem_count == 0 {
                String::new()
            } else {
                format!("  Problems {problem_count}")
            };
            let line_problem = app
                .active_line_problem_summary()
                .map(|problem| {
                    format!(
                        "  {}: {}",
                        problem.severity.label(),
                        truncate_width(&problem.message, 72)
                    )
                })
                .unwrap_or_default();
            format!(
                "{}{}  Ln {}, Col {}{}{}{}{}{}",
                tab.path.display(),
                dirty,
                tab.cursor_line + 1,
                tab.cursor_col + 1,
                disk,
                selection,
                search,
                problems,
                line_problem
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
        let suffix = explorer_node_suffix(node, app.git_status_marker(&node.path, node.is_dir));
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
    app.editor_width = inner.width as usize;
    let Some(active_index) = app.active_tab else {
        frame.render_widget(
            Paragraph::new("Click a file in Explorer to open it.")
                .style(Style::default().fg(MUTED).bg(PANEL_BG)),
            inner,
        );
        return;
    };

    let problem_summaries = app
        .tabs
        .get(active_index)
        .map(|tab| app.problem_summaries_for_path(&tab.path))
        .unwrap_or_default();
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
    let gutter_width = editor_gutter_width(tab.lines.len());
    let code_width = inner.width as usize;
    let code_width = code_width.saturating_sub(gutter_width);
    let max_horizontal_scroll = max_horizontal_scroll(tab, code_width);
    tab.horizontal_scroll = tab.horizontal_scroll.min(max_horizontal_scroll);
    let highlighted = app
        .syntax
        .highlight_visible(&tab.path, &tab.lines, start, end);

    let mut rendered = Vec::new();
    for (offset, line_index) in (start..end).enumerate() {
        let problem = problem_summaries.get(&line_index);
        let gutter_style = problem
            .map(|problem| problem_gutter_style(problem.severity))
            .unwrap_or_else(|| Style::default().fg(MUTED));
        let mut spans = vec![
            Span::styled(
                format!("{:>width$} ", line_index + 1, width = number_width),
                gutter_style,
            ),
            Span::styled(
                problem
                    .map(|problem| problem_marker(problem.severity))
                    .unwrap_or(" "),
                problem
                    .map(|problem| problem_gutter_style(problem.severity))
                    .unwrap_or_else(|| Style::default().fg(MUTED)),
            ),
        ];
        let mut code_spans = Vec::new();
        let selection_ranges = line_selection_ranges(tab, line_index);
        if !selection_ranges.is_empty() {
            code_spans.extend(selection_line_spans(
                &tab.lines[line_index],
                &selection_ranges,
            ));
        } else if focused {
            let cursor_cols = line_cursor_cols(tab, line_index);
            if !cursor_cols.is_empty() {
                code_spans.extend(cursor_line_spans(&tab.lines[line_index], &cursor_cols));
            } else if let Some(needle) = &search_needle
                && let Some(search_spans) = search_line_spans(tab, line_index, needle, focused)
            {
                code_spans.extend(search_spans);
            } else if let Some(parts) = highlighted.get(offset) {
                code_spans.extend(parts.clone());
            }
        } else if let Some(needle) = &search_needle
            && let Some(search_spans) = search_line_spans(tab, line_index, needle, focused)
        {
            code_spans.extend(search_spans);
        } else if let Some(parts) = highlighted.get(offset) {
            code_spans.extend(parts.clone());
        }
        spans.extend(crop_spans_by_chars(
            code_spans,
            tab.horizontal_scroll,
            code_width,
        ));
        let line = if let Some(problem) = problem {
            Line::from(spans).style(problem_line_style(problem.severity))
        } else {
            Line::from(spans)
        };
        rendered.push(line);
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
        let external = match tab.external_state {
            ExternalFileState::Clean => "",
            ExternalFileState::Modified => "!",
            ExternalFileState::Deleted => "?",
        };
        let label = format!(" {}{}{} x ", tab.title, dirty, external);
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
        let fg = if tab.external_state == ExternalFileState::Deleted {
            Color::LightRed
        } else if tab.external_state == ExternalFileState::Modified {
            Color::Yellow
        } else {
            TEXT
        };
        let style = if active {
            Style::default()
                .fg(Color::White)
                .bg(ACTIVE_BG)
                .add_modifier(Modifier::BOLD)
        } else if hovered {
            Style::default().fg(Color::White).bg(HOVER_BG)
        } else {
            Style::default().fg(fg).bg(Color::Rgb(18, 24, 33))
        };
        frame.render_widget(Paragraph::new(label).style(style), rect);
        x = x.saturating_add(width);
    }
}

fn draw_terminal(frame: &mut Frame, app: &mut App, area: Rect) {
    app.hit_regions.terminal_area = Some(area);
    let focused = app.focus == FocusPanel::Terminal;
    let title = terminal_panel_title(app);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), area);

    if inner.height == 0 {
        return;
    }

    let chunks = if inner.height > 1 {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner)
    } else {
        Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1)])
            .split(inner)
    };
    let body = if chunks.len() > 1 {
        draw_terminal_tabs(frame, app, chunks[0]);
        chunks[1]
    } else {
        inner
    };

    app.hit_regions.terminal_body = Some(body);
    app.hit_regions.terminal_input = Some(body);
    app.terminal_height = body.height as usize;
    app.active_terminal_mut()
        .shell
        .resize(body.height, body.width);

    let terminal_rows = app.active_terminal().shell.styled_rows();
    let lines = terminal_rows
        .into_iter()
        .enumerate()
        .map(|(row_index, row)| {
            let selection = app.terminal_selection_columns_for_row(row_index as u16);
            let search_ranges = app.terminal_search_ranges_for_row(row_index as u16);
            Line::from(terminal_row_spans(row, selection, &search_ranges))
        })
        .collect::<Vec<_>>();

    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(TEXT).bg(PANEL_BG)),
        body,
    );

    if focused
        && !app.active_terminal().shell.hide_cursor()
        && app.active_terminal().shell.scrollback() == 0
    {
        let (row, col) = app.active_terminal().shell.cursor();
        let x = body
            .x
            .saturating_add(col)
            .min(body.right().saturating_sub(1));
        let y = body
            .y
            .saturating_add(row)
            .min(body.bottom().saturating_sub(1));
        frame.set_cursor_position((x, y));
    }
}

fn terminal_panel_title(app: &App) -> String {
    let terminal = app.active_terminal();
    let state = if terminal.exited { "exited" } else { "live" };
    let cwd = terminal_cwd_label(&terminal.cwd, &app.root);
    let scroll = terminal.shell.scrollback();
    let scroll = if scroll == 0 {
        String::new()
    } else {
        format!("  scroll:{scroll}")
    };
    let alt = if terminal.shell.alternate_screen() {
        " alt"
    } else {
        ""
    };
    let paste = if terminal.shell.bracketed_paste() {
        " paste"
    } else {
        ""
    };
    let mouse = terminal_mouse_label(terminal.shell.mouse_protocol_mode());
    let modes = format!("{alt}{paste}{mouse}");
    let modes = if modes.is_empty() {
        String::new()
    } else {
        format!("  mode:{}", modes.trim())
    };
    let search = app
        .active_terminal_search_summary()
        .map(|(selected, count)| format!("  find:{selected}/{count}"))
        .unwrap_or_default();
    format!(
        " Terminal  {}  cwd:{}  {}{}{}{}  F6 focus  F7 new  F8 next  F9 close  F12 max ",
        terminal.title, cwd, state, scroll, modes, search
    )
}

fn terminal_cwd_label(cwd: &std::path::Path, root: &std::path::Path) -> String {
    if cwd == root {
        return ".".to_owned();
    }
    cwd.strip_prefix(root)
        .map(|path| path.to_string_lossy().replace('\\', "/"))
        .unwrap_or_else(|_| {
            cwd.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("/")
                .to_owned()
        })
}

fn terminal_mouse_label(mode: vt100::MouseProtocolMode) -> &'static str {
    match mode {
        vt100::MouseProtocolMode::None => "",
        vt100::MouseProtocolMode::Press => " mouse:press",
        vt100::MouseProtocolMode::PressRelease => " mouse:press",
        vt100::MouseProtocolMode::ButtonMotion => " mouse:drag",
        vt100::MouseProtocolMode::AnyMotion => " mouse:any",
    }
}

fn draw_terminal_tabs(frame: &mut Frame, app: &mut App, area: Rect) {
    frame.render_widget(
        Paragraph::new("").style(Style::default().fg(TEXT).bg(Color::Rgb(18, 24, 33))),
        area,
    );

    let mut x = area.x;
    let terminals = app
        .terminals
        .iter()
        .map(|terminal| (terminal.title.clone(), terminal.exited))
        .collect::<Vec<_>>();
    for (index, (title, exited)) in terminals.into_iter().enumerate() {
        if x >= area.right() {
            break;
        }
        let exit_marker = if exited { "!" } else { "" };
        let label = format!(" {}{} x ", title, exit_marker);
        let width = label.width().clamp(9, 20) as u16;
        let width = width.min(area.right().saturating_sub(x));
        let rect = Rect::new(x, area.y, width, 1);
        app.hit_regions.terminal_tabs.push((rect, index));
        if width >= 3 {
            app.hit_regions.terminal_tab_closes.push((
                Rect::new(rect.right().saturating_sub(3), rect.y, 3, 1),
                index,
            ));
        }

        let active = app.active_terminal == index;
        let hovered = app.hover == HoverTarget::TerminalTab(index)
            || app.hover == HoverTarget::TerminalTabClose(index);
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

    if x < area.right() {
        let width = 5_u16.min(area.right().saturating_sub(x));
        let rect = Rect::new(x, area.y, width, 1);
        app.hit_regions.terminal_new = Some(rect);
        let hovered = app.hover == HoverTarget::TerminalNew;
        let style = if hovered {
            Style::default().fg(Color::White).bg(HOVER_BG)
        } else {
            Style::default().fg(ACCENT).bg(Color::Rgb(18, 24, 33))
        };
        frame.render_widget(Paragraph::new(" + ").style(style), rect);
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
        crate::app::QuickPanelKind::Completions => " Suggestions  Ctrl-Space ",
        crate::app::QuickPanelKind::ExplorerContextMenu => " Explorer Context Menu  Right Click ",
        crate::app::QuickPanelKind::WorkspaceSearch => " Search Workspace  Ctrl-Shift-F ",
        crate::app::QuickPanelKind::DocumentSymbols => " Go to Symbol in File  Ctrl-Shift-O ",
        crate::app::QuickPanelKind::WorkspaceSymbols => " Go to Symbol in Workspace  Ctrl-T ",
        crate::app::QuickPanelKind::Definitions => " Go to Definition  Ctrl-] ",
        crate::app::QuickPanelKind::References => " Find References  Ctrl-R ",
        crate::app::QuickPanelKind::Problems => " Problems ",
        crate::app::QuickPanelKind::SourceControl => " Source Control ",
        crate::app::QuickPanelKind::Tasks => " Run Task  Ctrl-Shift-B ",
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
            crate::app::QuickPanelKind::Completions => {
                "No suggestions found for the current editor cursor."
            }
            crate::app::QuickPanelKind::ExplorerContextMenu => {
                "No explorer actions match the current query."
            }
            crate::app::QuickPanelKind::WorkspaceSearch => {
                "Type text to search across workspace files."
            }
            crate::app::QuickPanelKind::DocumentSymbols => {
                "No symbols found in the active editor buffer."
            }
            crate::app::QuickPanelKind::WorkspaceSymbols => {
                "Type a symbol name, or clear the query to list workspace symbols."
            }
            crate::app::QuickPanelKind::Definitions => "No definition found for the current query.",
            crate::app::QuickPanelKind::References => {
                "No whole-word references found for the current query."
            }
            crate::app::QuickPanelKind::Problems => {
                "No problems collected. Run Workspace Check from the command palette."
            }
            crate::app::QuickPanelKind::SourceControl => {
                "No Git changes found, or this workspace is not inside a Git repository."
            }
            crate::app::QuickPanelKind::Tasks => {
                "No tasks detected from .vscode/tasks.json, package.json, Cargo.toml, Makefile, go.mod, or pyproject.toml."
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

fn explorer_node_suffix(node: &VisibleNode, git_marker: Option<&str>) -> String {
    let git = git_marker
        .map(|marker| format!(" {marker}"))
        .unwrap_or_default();
    if node.is_dir {
        if node.readonly {
            format!("{git} ro")
        } else {
            git
        }
    } else {
        let size = node.size.map(format_size).unwrap_or_else(|| "?".to_owned());
        if node.readonly {
            format!(" {size}{git} ro")
        } else {
            format!(" {size}{git}")
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

fn problem_marker(severity: ProblemSeverity) -> &'static str {
    match severity {
        ProblemSeverity::Error => "E",
        ProblemSeverity::Warning => "W",
        ProblemSeverity::Note | ProblemSeverity::Help => "i",
        ProblemSeverity::Problem => "!",
    }
}

fn problem_gutter_style(severity: ProblemSeverity) -> Style {
    match severity {
        ProblemSeverity::Error => Style::default().fg(Color::LightRed).bg(ERROR_BG),
        ProblemSeverity::Warning => Style::default().fg(Color::LightYellow).bg(WARNING_BG),
        ProblemSeverity::Note | ProblemSeverity::Help => {
            Style::default().fg(Color::LightCyan).bg(INFO_BG)
        }
        ProblemSeverity::Problem => Style::default().fg(Color::White).bg(INFO_BG),
    }
    .add_modifier(Modifier::BOLD)
}

fn problem_line_style(severity: ProblemSeverity) -> Style {
    match severity {
        ProblemSeverity::Error => Style::default().bg(ERROR_BG),
        ProblemSeverity::Warning => Style::default().bg(WARNING_BG),
        ProblemSeverity::Note | ProblemSeverity::Help | ProblemSeverity::Problem => {
            Style::default().bg(INFO_BG)
        }
    }
}

fn terminal_row_spans(
    row: Vec<TerminalSpan>,
    selection: Option<(usize, usize)>,
    search_ranges: &[(usize, usize, bool)],
) -> Vec<Span<'static>> {
    if selection.is_none() && search_ranges.is_empty() {
        return row
            .into_iter()
            .map(|span| Span::styled(span.text, terminal_style(span.style)))
            .collect();
    };

    let selection = selection.filter(|(start, end)| end > start);
    let mut spans = Vec::new();
    let mut col = 0usize;
    for span in row {
        let chars = span.text.chars().collect::<Vec<_>>();
        let base_style = terminal_style(span.style);
        let mut current_text = String::new();
        let mut current_style = None::<Style>;

        for ch in chars {
            let style = terminal_cell_overlay_style(base_style, col, selection, search_ranges);
            if current_style == Some(style) {
                current_text.push(ch);
            } else {
                if let Some(style) = current_style {
                    spans.push(Span::styled(std::mem::take(&mut current_text), style));
                }
                current_style = Some(style);
                current_text.push(ch);
            }
            col += 1;
        }

        if let Some(style) = current_style {
            spans.push(Span::styled(current_text, style));
        }
    }

    if let Some((selection_start, selection_end)) = selection
        && selection_end > col
    {
        let start = selection_start.max(col);
        if selection_end > start {
            spans.push(Span::styled(
                " ".repeat(selection_end - start),
                terminal_selection_style(Style::default().fg(TEXT).bg(PANEL_BG)),
            ));
        }
    }

    spans
}

fn terminal_cell_overlay_style(
    base_style: Style,
    col: usize,
    selection: Option<(usize, usize)>,
    search_ranges: &[(usize, usize, bool)],
) -> Style {
    let mut style = if selection.is_some_and(|(start, end)| col >= start && col < end) {
        terminal_selection_style(base_style)
    } else {
        base_style
    };

    for (start, end, active) in search_ranges {
        if col >= *start && col < *end {
            style = terminal_search_style(style, *active);
        }
    }

    style
}

fn terminal_selection_style(style: Style) -> Style {
    style.fg(Color::Black).bg(ACCENT)
}

fn terminal_search_style(style: Style, active: bool) -> Style {
    if active {
        style.fg(Color::Black).bg(SEARCH_ACTIVE_BG)
    } else {
        style.fg(Color::White).bg(SEARCH_BG)
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
        HoverTarget::TerminalTab(index) => format!("terminal tab {index}"),
        HoverTarget::TerminalTabClose(index) => format!("terminal close {index}"),
        HoverTarget::TerminalNew => "terminal new".to_owned(),
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
        crate::app::PromptKind::WorkspaceReplaceFind => "replace files: find",
        crate::app::PromptKind::WorkspaceReplaceWith { .. } => "replace files: with",
        crate::app::PromptKind::RenameSymbol { .. } => "rename symbol",
        crate::app::PromptKind::SaveAs => "save as",
        crate::app::PromptKind::TerminalSearch => "find terminal",
        crate::app::PromptKind::GotoLine => "go to line",
        crate::app::PromptKind::QuitDirty => "unsaved: type quit",
    }
}

fn editor_gutter_width(line_count: usize) -> usize {
    line_count.max(1).to_string().len().max(3) + 2
}

fn max_horizontal_scroll(tab: &crate::app::EditorTab, code_width: usize) -> usize {
    if code_width == 0 {
        return 0;
    }
    tab.lines
        .iter()
        .map(|line| line.chars().count().saturating_sub(code_width))
        .max()
        .unwrap_or(0)
}

fn crop_spans_by_chars(
    spans: Vec<Span<'static>>,
    start: usize,
    max_chars: usize,
) -> Vec<Span<'static>> {
    if max_chars == 0 {
        return Vec::new();
    }

    let mut skipped = 0usize;
    let mut taken = 0usize;
    let mut cropped = Vec::new();

    for span in spans {
        if taken >= max_chars {
            break;
        }

        let span_len = span.content.chars().count();
        if skipped + span_len <= start {
            skipped += span_len;
            continue;
        }

        let local_start = start.saturating_sub(skipped);
        let remaining = max_chars - taken;
        let text = span
            .content
            .chars()
            .skip(local_start)
            .take(remaining)
            .collect::<String>();
        if !text.is_empty() {
            taken += text.chars().count();
            cropped.push(Span::styled(text, span.style));
        }
        skipped += span_len;
    }

    cropped
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

fn line_selection_ranges(tab: &crate::app::EditorTab, line_index: usize) -> Vec<(usize, usize)> {
    let line_len = tab.lines[line_index].chars().count();
    tab.selection_ranges()
        .into_iter()
        .filter_map(|selection| {
            let (start_line, start_col) = selection.start;
            let (end_line, end_col) = selection.end;
            if line_index < start_line || line_index > end_line {
                return None;
            }

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
        })
        .collect()
}

fn selection_line_spans(source: &str, ranges: &[(usize, usize)]) -> Vec<Span<'static>> {
    let mut ranges = ranges.to_vec();
    ranges.sort();
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    for (start, end) in ranges {
        if start > cursor {
            spans.push(Span::raw(slice_chars(source, cursor, start)));
        }
        spans.push(Span::styled(
            slice_chars(source, start, end),
            Style::default().fg(Color::White).bg(ACTIVE_BG),
        ));
        cursor = end;
    }
    let line_len = source.chars().count();
    if cursor < line_len {
        spans.push(Span::raw(skip_chars(source, cursor)));
    }
    spans
}

fn line_cursor_cols(tab: &crate::app::EditorTab, line_index: usize) -> Vec<usize> {
    let line_len = tab.lines[line_index].chars().count();
    tab.cursor_positions()
        .into_iter()
        .filter_map(|(line, col)| (line == line_index).then_some(col.min(line_len)))
        .collect()
}

fn cursor_line_spans(source: &str, cursor_cols: &[usize]) -> Vec<Span<'static>> {
    let mut cursor_cols = cursor_cols.to_vec();
    cursor_cols.sort();
    cursor_cols.dedup();
    let chars = source.chars().collect::<Vec<_>>();
    let mut spans = Vec::new();
    let mut cursor_index = 0usize;
    for (index, c) in chars.iter().enumerate() {
        if cursor_index < cursor_cols.len() && cursor_cols[cursor_index] == index {
            spans.push(Span::styled(
                c.to_string(),
                Style::default().fg(Color::Black).bg(ACCENT),
            ));
            while cursor_index < cursor_cols.len() && cursor_cols[cursor_index] == index {
                cursor_index += 1;
            }
        } else {
            spans.push(Span::raw(c.to_string()));
        }
    }
    if cursor_cols.iter().any(|col| *col >= chars.len()) {
        spans.push(Span::styled(
            " ",
            Style::default().fg(Color::Black).bg(ACCENT),
        ));
    }
    spans
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
