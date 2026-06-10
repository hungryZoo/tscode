use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

use crate::{
    app::{
        App, ClipboardAction, ExternalFileState, FocusPanel, HoverTarget, ProblemSeverity,
        SidebarMode, editor_visual_rows,
    },
    fs_tree::VisibleNode,
    lsp::{LspDocumentHighlight, LspDocumentHighlightKind},
    shell::{TerminalSpan, TerminalStyle},
};

const TITLE_BG: Color = Color::Rgb(32, 40, 54);
const PANEL_BG: Color = Color::Rgb(13, 17, 23);
const HOVER_BG: Color = Color::Rgb(42, 54, 71);
const ACTIVE_BG: Color = Color::Rgb(24, 64, 92);
const SEARCH_BG: Color = Color::Rgb(104, 76, 28);
const SEARCH_ACTIVE_BG: Color = Color::Rgb(222, 184, 75);
const HIGHLIGHT_TEXT_BG: Color = Color::Rgb(43, 54, 79);
const HIGHLIGHT_READ_BG: Color = Color::Rgb(28, 71, 58);
const HIGHLIGHT_WRITE_BG: Color = Color::Rgb(91, 56, 38);
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

    match app.sidebar_mode {
        SidebarMode::Files => draw_explorer(frame, app, body[0]),
        SidebarMode::Outline => draw_outline(frame, app, body[0]),
    }

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
    draw_editor_hover(frame, app, root_chunks[1]);
    draw_quick_panel(frame, app, root_chunks[1]);
    draw_status(frame, app, root_chunks[2]);
}

fn draw_title(frame: &mut Frame, app: &App, area: Rect) {
    let text = format!(
        " tscode  {}  sidebar:{}  focus:{} ",
        app.root.display(),
        app.sidebar_mode.label(),
        focus_name(app.focus)
    );
    frame.render_widget(
        Paragraph::new(text).style(Style::default().fg(TEXT).bg(TITLE_BG)),
        area,
    );
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    if let Some(prompt) = &app.prompt {
        let input = crate::app::editable_text_with_cursor(&prompt.input, prompt.cursor);
        let text = format!(" {}: {} ", prompt_title(&prompt.kind), input);
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
            let read_only = if tab.read_only { "  ro" } else { "" };
            let disk = match tab.external_state {
                ExternalFileState::Clean => String::new(),
                state => format!("  Disk {}", state.label()),
            };
            let path_label = if tab.untitled {
                tab.title.clone()
            } else {
                tab.path.display().to_string()
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
            let highlights = if tab.document_highlights.is_empty() {
                String::new()
            } else {
                format!("  Highlights {}", tab.document_highlights.len())
            };
            let bookmarks = if tab.bookmarks.is_empty() {
                String::new()
            } else {
                format!("  Bookmarks {}", tab.bookmarks.len())
            };
            let wrap = if app.word_wrap {
                "  Wrap".to_owned()
            } else {
                String::new()
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
                "{}{}{}  Ln {}, Col {}{}{}{}{}{}{}{}{}",
                path_label,
                dirty,
                read_only,
                tab.cursor_line + 1,
                tab.cursor_col + 1,
                wrap,
                disk,
                selection,
                search,
                problems,
                highlights,
                bookmarks,
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
            let target = if clipboard.paths.len() == 1 {
                display_name(&clipboard.paths[0])
            } else {
                format!("{} items", clipboard.paths.len())
            };
            format!("  clipboard:{action} {target}")
        })
        .unwrap_or_default();
    let editor_clipboard = app
        .editor_clipboard
        .as_ref()
        .map(|text| format!("  editor-clip:{} chars", text.chars().count()))
        .unwrap_or_default();
    let hover_detail = app
        .editor_hover
        .as_ref()
        .map(|hover| {
            format!(
                "  symbol:{} defs:{} refs:{}",
                hover.symbol, hover.definition_count, hover.reference_count
            )
        })
        .unwrap_or_default();
    let branch = app
        .git_branch
        .as_ref()
        .map(|branch| format!("  branch:{branch}"))
        .unwrap_or_default();
    let text = format!(
        " {} tabs:{}  hover:{}{}{}{}{}{}{} ",
        active,
        app.tabs.len(),
        hover_name(&app.hover),
        branch,
        hover_detail,
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
    let sort = app.explorer.sort_mode().label();
    let selection = if app.explorer_multi_selection.is_empty() {
        String::new()
    } else {
        format!(
            " sel:{}  Space toggle  Shift/Ctrl-click  drag move  Alt-drag copy",
            app.explorer_multi_selection.len()
        )
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Explorer  / filter  s sort:{sort}  . hidden  i generated  n/N new  e rename  D delete  c copy  x cut  p paste  y dup  o reveal  O open  r refresh  {hidden} {ignored}{filter}{selection} "
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
        let multi_selected = app.explorer_multi_selection.contains(&node.path);
        let hovered = app.hover == HoverTarget::ExplorerRow(row);
        let drop_target = app.explorer_drag_target_index() == Some(row);
        let style = explorer_row_style(selected, hovered, multi_selected, drop_target);
        let marker = if node.is_dir {
            if node.expanded { "-" } else { "+" }
        } else {
            " "
        };
        let indent = "  ".repeat(node.depth);
        let selection_marker = if multi_selected { "*" } else { " " };
        let prefix = format!("{selection_marker}{indent}{marker} {}", node.name);
        let suffix = explorer_node_suffix(node, app.git_status_marker(&node.path, node.is_dir));
        let text = fit_with_suffix(&prefix, &suffix, row_area.width as usize);
        frame.render_widget(Paragraph::new(text).style(style), row_area);
    }
}

fn draw_outline(frame: &mut Frame, app: &mut App, area: Rect) {
    app.hit_regions.outline_area = Some(area);
    let focused = app.focus == FocusPanel::Explorer;
    let active = app
        .active_tab()
        .map(|tab| tab.title.clone())
        .unwrap_or_else(|| "no file".to_owned());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Outline  m files  r refresh  O quick  {} ",
            truncate_width(&active, 24)
        ))
        .border_style(border_style(focused));
    let inner = block.inner(area);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), area);

    app.explorer_height = inner.height as usize;
    let items = app.visible_outline_items();
    let max_scroll = items.len().saturating_sub(app.explorer_height.max(1));
    app.outline_scroll = app.outline_scroll.min(max_scroll);
    app.outline_selected = app.outline_selected.min(items.len().saturating_sub(1));

    if items.is_empty() {
        frame.render_widget(
            Paragraph::new("Open a source file to see symbols.")
                .style(Style::default().fg(MUTED).bg(PANEL_BG)),
            inner,
        );
        return;
    }

    for (row, item) in items
        .iter()
        .enumerate()
        .skip(app.outline_scroll)
        .take(inner.height as usize)
    {
        let y = inner.y + (row - app.outline_scroll) as u16;
        let row_area = Rect::new(inner.x, y, inner.width, 1);
        app.hit_regions.outline_rows.push((row_area, row));
        let selected = focused && app.outline_selected == row;
        let hovered = app.hover == HoverTarget::OutlineRow(row);
        let style = row_style(selected, hovered);
        let line = item
            .line
            .map(|line| format!(" {}", line + 1))
            .unwrap_or_default();
        let prefix = format!("  {}  {}", item.label, item.detail);
        let text = fit_with_suffix(&prefix, &line, row_area.width as usize);
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

    let panes = app.editor_visible_panes();
    if panes.is_empty() {
        let focused = app.focus == FocusPanel::Editor;
        let block = Block::default()
            .borders(Borders::ALL)
            .title(" Editor ")
            .border_style(border_style(focused));
        let inner = block.inner(chunks[1]);
        app.hit_regions.editor_body = Some(inner);
        frame.render_widget(block.style(Style::default().bg(PANEL_BG)), chunks[1]);
        frame.render_widget(
            Paragraph::new("Click a file in Explorer to open it.")
                .style(Style::default().fg(MUTED).bg(PANEL_BG)),
            inner,
        );
        return;
    }

    if panes.len() == 2 && chunks[1].width >= 54 {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(50),
                Constraint::Length(1),
                Constraint::Percentage(50),
            ])
            .split(chunks[1]);
        let separator = (0..split[1].height)
            .map(|_| Line::from("│"))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(separator).style(Style::default().fg(BORDER).bg(PANEL_BG)),
            split[1],
        );
        draw_editor_pane(frame, app, panes[0].1, split[0], panes[0].0);
        draw_editor_pane(frame, app, panes[1].1, split[2], panes[1].0);
    } else {
        draw_editor_pane(frame, app, panes[0].1, chunks[1], panes[0].0);
    }
}

fn draw_editor_pane(
    frame: &mut Frame,
    app: &mut App,
    active_index: usize,
    area: Rect,
    pane: usize,
) {
    let active_pane = app.active_tab == Some(active_index)
        && (!app.editor_split_active() || app.active_editor_pane == pane);
    let focused = app.focus == FocusPanel::Editor && active_pane;
    let title = if app.editor_split_active() {
        format!(
            " Editor {}  Ctrl-S save  Ctrl-F find  Ctrl-H replace  Ctrl-\\ split ",
            pane + 1
        )
    } else {
        " Editor  Ctrl-S save  Ctrl-F find  Ctrl-H replace  Alt-[ fold  Alt-0 fold all  Alt-] unfold ".to_owned()
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border_style(focused));
    let inner = block.inner(area);
    app.hit_regions
        .editor_panes
        .push((inner, pane, active_index));
    if active_pane {
        app.hit_regions.editor_body = Some(inner);
        app.editor_height = inner.height as usize;
        app.editor_width = inner.width as usize;
    }
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), area);

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

    let gutter_width = editor_gutter_width(tab.lines.len());
    let code_width = inner.width as usize;
    let code_width = code_width.saturating_sub(gutter_width);
    let word_wrap = app.word_wrap;
    let height = inner.height as usize;
    let visual_rows = editor_visual_rows(tab, code_width, word_wrap);
    let max_scroll = visual_rows.len().saturating_sub(height.max(1));
    tab.scroll = tab.scroll.min(max_scroll);
    let start = tab.scroll;
    let end = (start + height).min(visual_rows.len());
    if word_wrap {
        tab.horizontal_scroll = 0;
    } else {
        let max_horizontal_scroll = max_horizontal_scroll(tab, code_width);
        tab.horizontal_scroll = tab.horizontal_scroll.min(max_horizontal_scroll);
    }
    let raw_start = visual_rows
        .iter()
        .take(end)
        .skip(start)
        .map(|row| row.line)
        .min()
        .unwrap_or(0);
    let raw_end = visual_rows
        .iter()
        .take(end)
        .skip(start)
        .map(|row| row.line.saturating_add(1))
        .max()
        .unwrap_or(raw_start);
    let path = tab.path.clone();
    let lines = tab.lines.clone();

    let highlighted = app
        .syntax
        .highlight_visible(&path, &lines, raw_start, raw_end);

    let Some(tab) = app.tabs.get(active_index) else {
        return;
    };

    if inner.width == 0 || inner.height == 0 {
        return;
    };

    let number_width = tab.lines.len().max(1).to_string().len().max(3);
    let mut rendered = Vec::new();
    for visual_row in visual_rows.iter().take(end).skip(start).copied() {
        let line_index = visual_row.line;
        let problem = problem_summaries.get(&line_index);
        let gutter_style = problem
            .map(|problem| problem_gutter_style(problem.severity))
            .unwrap_or_else(|| Style::default().fg(MUTED));
        let fold_marker = if tab.is_line_folded(line_index) {
            "+"
        } else if tab.fold_end_for_line(line_index).is_some() {
            "-"
        } else {
            " "
        };
        let mut spans = vec![
            Span::styled(
                if !visual_row.continuation && tab.has_bookmark(line_index) {
                    "B"
                } else {
                    " "
                },
                if !visual_row.continuation && tab.has_bookmark(line_index) {
                    Style::default().fg(ACCENT)
                } else {
                    Style::default().fg(MUTED)
                },
            ),
            Span::styled(
                if visual_row.continuation {
                    format!("{:>width$} ", "", width = number_width)
                } else {
                    format!("{:>width$} ", line_index + 1, width = number_width)
                },
                gutter_style,
            ),
            Span::styled(
                if visual_row.continuation {
                    ">"
                } else {
                    problem
                        .map(|problem| problem_marker(problem.severity))
                        .unwrap_or(fold_marker)
                },
                problem
                    .map(|problem| problem_gutter_style(problem.severity))
                    .unwrap_or_else(|| {
                        if tab.is_line_folded(line_index) {
                            Style::default().fg(ACCENT)
                        } else {
                            Style::default().fg(MUTED)
                        }
                    }),
            ),
        ];
        let mut code_spans = Vec::new();
        let syntax_spans = highlighted
            .get(line_index.saturating_sub(raw_start))
            .cloned()
            .unwrap_or_else(|| vec![Span::raw(tab.lines[line_index].clone())]);
        let document_highlights = tab.document_highlight_ranges_for_line(line_index);
        let selection_ranges = line_selection_ranges(tab, line_index);
        if !selection_ranges.is_empty() {
            code_spans.extend(selection_line_spans(
                &tab.lines[line_index],
                &selection_ranges,
            ));
        } else if focused {
            let cursor_cols = line_cursor_cols(tab, line_index);
            if !cursor_cols.is_empty() {
                let base_spans = if document_highlights.is_empty() {
                    syntax_spans
                } else {
                    document_highlight_line_spans(syntax_spans, &document_highlights)
                };
                code_spans.extend(cursor_overlay_line_spans(base_spans, &cursor_cols));
            } else if let Some(needle) = &search_needle
                && let Some(search_spans) = search_line_spans(tab, line_index, needle, focused)
            {
                code_spans.extend(search_spans);
            } else if !document_highlights.is_empty() {
                code_spans.extend(document_highlight_line_spans(
                    syntax_spans,
                    &document_highlights,
                ));
            } else {
                code_spans.extend(syntax_spans);
            }
        } else if let Some(needle) = &search_needle
            && let Some(search_spans) = search_line_spans(tab, line_index, needle, focused)
        {
            code_spans.extend(search_spans);
        } else if !document_highlights.is_empty() {
            code_spans.extend(document_highlight_line_spans(
                syntax_spans,
                &document_highlights,
            ));
        } else {
            code_spans.extend(syntax_spans);
        }
        if tab.is_line_folded(line_index)
            && let Some(end) = tab.fold_end_for_line(line_index)
        {
            code_spans.push(Span::styled(
                format!("  ... {} folded line(s)", end.saturating_sub(line_index)),
                Style::default().fg(MUTED),
            ));
        }
        let crop_start = if word_wrap {
            visual_row.start_col
        } else {
            tab.horizontal_scroll
        };
        spans.extend(crop_spans_by_chars(code_spans, crop_start, code_width));
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

fn draw_editor_hover(frame: &mut Frame, app: &App, area: Rect) {
    if app.quick_panel.is_some() || app.prompt.is_some() {
        return;
    }
    let Some(hover) = &app.editor_hover else {
        return;
    };
    if app.hit_regions.editor_body.is_none() || area.width < 8 || area.height < 4 {
        return;
    }

    let definition = hover
        .definition
        .as_ref()
        .map(|location| {
            let path = location
                .path
                .strip_prefix(&app.root)
                .map(|path| path.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| location.path.display().to_string());
            format!("def {path}:{}:{}", location.line + 1, location.col + 1)
        })
        .unwrap_or_else(|| "def not found".to_owned());
    let detail = hover
        .definition_detail
        .as_deref()
        .map(|detail| truncate_width(detail, 72))
        .unwrap_or_default();
    let preview = hover
        .definition_preview
        .as_deref()
        .map(|preview| truncate_width(preview, 72))
        .unwrap_or_default();

    let mut rows = vec![
        format!(
            "{}  defs:{} refs:{}",
            hover.symbol, hover.definition_count, hover.reference_count
        ),
        if detail.is_empty() {
            definition
        } else {
            format!("{definition}  {detail}")
        },
    ];
    if !preview.is_empty() {
        rows.push(preview);
    }

    let text_width = rows
        .iter()
        .map(|row| row.width())
        .max()
        .unwrap_or(24)
        .clamp(24, 76);
    let width = (text_width + 4).min(area.width as usize) as u16;
    let height = (rows.len() + 2).min(area.height as usize) as u16;
    if width == 0 || height == 0 {
        return;
    }

    let mut x = app.hit_regions.last_mouse_x.saturating_add(1);
    if x.saturating_add(width) > area.right() {
        x = area.right().saturating_sub(width);
    }
    let mut y = app.hit_regions.last_mouse_y.saturating_add(1);
    if y.saturating_add(height) > area.bottom() {
        y = app.hit_regions.last_mouse_y.saturating_sub(height);
    }
    x = x.max(area.x);
    y = y.max(area.y);

    let popup = Rect::new(x, y, width, height);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Hover ")
        .border_style(Style::default().fg(ACCENT));
    let inner = block.inner(popup);
    let inner_width = inner.width as usize;
    let lines = rows
        .into_iter()
        .map(|row| Line::from(truncate_width(&row, inner_width)))
        .collect::<Vec<_>>();

    frame.render_widget(Clear, popup);
    frame.render_widget(block.style(Style::default().bg(PANEL_BG)), popup);
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(TEXT).bg(PANEL_BG)),
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
        let read_only = if tab.read_only { " ro" } else { "" };
        let label = format!(" {}{}{}{} x ", tab.title, dirty, read_only, external);
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
        let split_peer = app.editor_split_active() && app.editor_split == Some(index) && !active;
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
        } else if split_peer {
            Style::default().fg(Color::White).bg(Color::Rgb(38, 50, 62))
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
    if !app.terminal_maximized && area.height > 0 {
        app.hit_regions.terminal_resize = Some(Rect::new(area.x, area.y, area.width, 1));
    }
    let focused = app.focus == FocusPanel::Terminal;
    let title = terminal_panel_title(app);
    let resizing = app.hover == HoverTarget::TerminalResize || app.terminal_resize_dragging;
    let border = if resizing {
        Style::default()
            .fg(ACCENT)
            .bg(HOVER_BG)
            .add_modifier(Modifier::BOLD)
    } else {
        border_style(focused)
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(border);
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

    let panes = app.visible_terminal_indices();
    if panes.len() > 1 && body.width >= 40 {
        let split = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(50),
                Constraint::Length(1),
                Constraint::Percentage(50),
            ])
            .split(body);
        let separator = (0..split[1].height)
            .map(|_| Line::from("│"))
            .collect::<Vec<_>>();
        frame.render_widget(
            Paragraph::new(separator).style(Style::default().fg(BORDER).bg(PANEL_BG)),
            split[1],
        );
        draw_terminal_pane(frame, app, panes[0], split[0], focused);
        draw_terminal_pane(frame, app, panes[1], split[2], focused);
    } else {
        draw_terminal_pane(frame, app, app.active_terminal, body, focused);
    }
}

fn draw_terminal_pane(
    frame: &mut Frame,
    app: &mut App,
    terminal_index: usize,
    body: Rect,
    focused: bool,
) {
    if terminal_index >= app.terminals.len() || body.width == 0 || body.height == 0 {
        return;
    }

    app.hit_regions.terminal_bodies.push((body, terminal_index));
    if terminal_index == app.active_terminal {
        app.hit_regions.terminal_body = Some(body);
        app.hit_regions.terminal_input = Some(body);
        app.terminal_height = body.height as usize;
    }

    let terminal_rows = {
        let terminal = &mut app.terminals[terminal_index];
        terminal.shell.resize(body.height, body.width);
        terminal.shell.styled_rows()
    };
    let lines = terminal_rows
        .into_iter()
        .enumerate()
        .map(|(row_index, row)| {
            let selection =
                app.terminal_selection_columns_for_terminal_row(terminal_index, row_index as u16);
            let search_ranges =
                app.terminal_search_ranges_for_terminal_row(terminal_index, row_index as u16);
            let link_ranges =
                app.terminal_link_ranges_for_terminal_row(terminal_index, row_index as u16);
            Line::from(terminal_row_spans(
                row,
                selection,
                &search_ranges,
                &link_ranges,
            ))
        })
        .collect::<Vec<_>>();

    let active = terminal_index == app.active_terminal;
    let pane_bg = if active && app.terminal_split_active() {
        Color::Rgb(15, 23, 32)
    } else {
        PANEL_BG
    };
    frame.render_widget(
        Paragraph::new(lines).style(Style::default().fg(TEXT).bg(pane_bg)),
        body,
    );

    if focused
        && active
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
    let state = if terminal.exited {
        terminal
            .exit_status
            .as_ref()
            .map(|status| status.label())
            .unwrap_or_else(|| "exited".to_owned())
    } else {
        "live".to_owned()
    };
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
    let split = if app.terminal_split_active() {
        "  split"
    } else {
        ""
    };
    format!(
        " Terminal  {}  cwd:{}  {}{}{}{}{}  F6 focus  F7 new  F8 next  F9 close  F12 max ",
        terminal.title, cwd, state, scroll, modes, search, split
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
        .map(|terminal| {
            let exit_label = terminal.exit_status.as_ref().map(|status| status.label());
            (terminal.title.clone(), terminal.exited, exit_label)
        })
        .collect::<Vec<_>>();
    for (index, (title, exited, exit_label)) in terminals.into_iter().enumerate() {
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
        let style = if exited {
            style.fg(Color::Rgb(248, 113, 113))
        } else {
            style
        };
        frame.render_widget(Paragraph::new(label).style(style), rect);
        if let Some(exit_label) = exit_label
            && active
            && rect.width > 12
        {
            let hint_width = exit_label
                .width()
                .min(rect.width.saturating_sub(4) as usize) as u16;
            if hint_width > 0 {
                let hint_rect = Rect::new(
                    rect.right().saturating_sub(hint_width + 3),
                    rect.y,
                    hint_width,
                    1,
                );
                frame.render_widget(
                    Paragraph::new(exit_label).style(style.add_modifier(Modifier::BOLD)),
                    hint_rect,
                );
            }
        }
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
        crate::app::QuickPanelKind::CodeActions => " Code Actions ",
        crate::app::QuickPanelKind::DirtyClose { .. } => " Unsaved Changes  Ctrl-W ",
        crate::app::QuickPanelKind::ExplorerContextMenu => " Explorer Context Menu  Right Click ",
        crate::app::QuickPanelKind::EditorContextMenu => " Editor Context Menu  Right Click ",
        crate::app::QuickPanelKind::TerminalContextMenu => " Terminal Context Menu  Right Click ",
        crate::app::QuickPanelKind::WorkspaceSearch => " Search Workspace  Ctrl-Shift-F ",
        crate::app::QuickPanelKind::DocumentSymbols => " Go to Symbol in File  Ctrl-Shift-O ",
        crate::app::QuickPanelKind::WorkspaceSymbols => " Go to Symbol in Workspace  Ctrl-T ",
        crate::app::QuickPanelKind::LspHover => " LSP Hover ",
        crate::app::QuickPanelKind::SignatureHelp => " Signature Help  Ctrl-Shift-Space ",
        crate::app::QuickPanelKind::Definitions => " Go to Definition  Ctrl-] ",
        crate::app::QuickPanelKind::TypeDefinitions => " Go to Type Definition ",
        crate::app::QuickPanelKind::Implementations => " Go to Implementation ",
        crate::app::QuickPanelKind::IncomingCalls => " Incoming Calls ",
        crate::app::QuickPanelKind::OutgoingCalls => " Outgoing Calls ",
        crate::app::QuickPanelKind::References => " Find References  Ctrl-R ",
        crate::app::QuickPanelKind::LspReferences => " LSP References  Ctrl-R ",
        crate::app::QuickPanelKind::Problems => " Problems ",
        crate::app::QuickPanelKind::Bookmarks => " Bookmarks ",
        crate::app::QuickPanelKind::SourceControl => " Source Control ",
        crate::app::QuickPanelKind::Branches => " Git Branches ",
        crate::app::QuickPanelKind::Tasks => " Run Task  Ctrl-Shift-B ",
        crate::app::QuickPanelKind::TerminalCommandHistory => " Recent Terminal Commands ",
        crate::app::QuickPanelKind::CommandPalette => " Command Palette  F1 / Ctrl-Shift-P ",
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            "{title}  {} ",
            crate::app::editable_text_with_cursor(&panel.query, panel.query_cursor)
        ))
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
            crate::app::QuickPanelKind::CodeActions => {
                "No code actions returned for the current editor cursor."
            }
            crate::app::QuickPanelKind::DirtyClose { .. } => {
                "The tab is no longer open or no close action matches the query."
            }
            crate::app::QuickPanelKind::ExplorerContextMenu => {
                "No explorer actions match the current query."
            }
            crate::app::QuickPanelKind::EditorContextMenu => {
                "No editor actions match the current query."
            }
            crate::app::QuickPanelKind::TerminalContextMenu => {
                "No terminal actions match the current query."
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
            crate::app::QuickPanelKind::LspHover => {
                "No language-server hover content matches the current query."
            }
            crate::app::QuickPanelKind::SignatureHelp => {
                "No language-server signature help matches the current query."
            }
            crate::app::QuickPanelKind::Definitions => "No definition found for the current query.",
            crate::app::QuickPanelKind::TypeDefinitions => {
                "No type definitions match the current query."
            }
            crate::app::QuickPanelKind::Implementations => {
                "No implementations match the current query."
            }
            crate::app::QuickPanelKind::IncomingCalls => {
                "No incoming calls match the current query."
            }
            crate::app::QuickPanelKind::OutgoingCalls => {
                "No outgoing calls match the current query."
            }
            crate::app::QuickPanelKind::References => {
                "No whole-word references found for the current query."
            }
            crate::app::QuickPanelKind::LspReferences => {
                "No language-server references match the current query."
            }
            crate::app::QuickPanelKind::Problems => {
                "No problems collected. Run Workspace Check or Run LSP Diagnostics from the command palette."
            }
            crate::app::QuickPanelKind::Bookmarks => "No editor bookmarks yet.",
            crate::app::QuickPanelKind::SourceControl => {
                "No Git changes found, or this workspace is not inside a Git repository."
            }
            crate::app::QuickPanelKind::Branches => {
                "No local Git branches found, or this workspace is not inside a Git repository."
            }
            crate::app::QuickPanelKind::Tasks => {
                "No tasks detected from .vscode/tasks.json, package.json, Cargo.toml, Makefile, go.mod, or pyproject.toml."
            }
            crate::app::QuickPanelKind::TerminalCommandHistory => {
                "No recent tscode-submitted terminal commands yet."
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

fn explorer_row_style(
    selected: bool,
    hovered: bool,
    multi_selected: bool,
    drop_target: bool,
) -> Style {
    if drop_target {
        Style::default()
            .fg(Color::White)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD)
    } else if selected {
        Style::default().fg(Color::White).bg(ACTIVE_BG)
    } else if hovered && multi_selected {
        Style::default().fg(Color::White).bg(Color::Rgb(42, 64, 92))
    } else if multi_selected {
        Style::default().fg(Color::White).bg(Color::Rgb(25, 44, 66))
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
    link_ranges: &[(usize, usize)],
) -> Vec<Span<'static>> {
    if selection.is_none() && search_ranges.is_empty() && link_ranges.is_empty() {
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
            let style =
                terminal_cell_overlay_style(base_style, col, selection, search_ranges, link_ranges);
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
    link_ranges: &[(usize, usize)],
) -> Style {
    let mut style = if link_ranges
        .iter()
        .any(|(start, end)| col >= *start && col < *end)
    {
        terminal_link_style(base_style)
    } else {
        base_style
    };

    if selection.is_some_and(|(start, end)| col >= start && col < end) {
        style = terminal_selection_style(base_style);
    }

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

fn terminal_link_style(style: Style) -> Style {
    style.fg(ACCENT).add_modifier(Modifier::UNDERLINED)
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
        HoverTarget::Outline => "outline".to_owned(),
        HoverTarget::OutlineRow(index) => format!("outline row {index}"),
        HoverTarget::Editor => "editor".to_owned(),
        HoverTarget::EditorPane(index) => format!("editor pane {index}"),
        HoverTarget::Tab(index) => format!("tab {index}"),
        HoverTarget::TabClose(index) => format!("tab close {index}"),
        HoverTarget::TerminalTab(index) => format!("terminal tab {index}"),
        HoverTarget::TerminalTabClose(index) => format!("terminal close {index}"),
        HoverTarget::TerminalNew => "terminal new".to_owned(),
        HoverTarget::TerminalResize => "terminal resize".to_owned(),
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
        crate::app::PromptKind::DeletePaths(_) => "delete: type yes",
        crate::app::PromptKind::ExplorerFilter => "explorer filter",
        crate::app::PromptKind::OpenFolder => "open folder",
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
        crate::app::PromptKind::SaveAsClose { .. } => "save as then close",
        crate::app::PromptKind::CreateGitBranch => "create branch",
        crate::app::PromptKind::CommitStagedSourceControlChanges => "commit staged: message",
        crate::app::PromptKind::CommitAllSourceControlChanges(_) => "commit all: message",
        crate::app::PromptKind::DiscardSourceControlPath(_) => "discard: type discard",
        crate::app::PromptKind::DiscardAllSourceControlChanges(_) => "discard all: type discard",
        crate::app::PromptKind::TerminalSearch => "find terminal",
        crate::app::PromptKind::RunTerminalCommand => "run terminal command",
        crate::app::PromptKind::RenameTerminal => "rename terminal",
        crate::app::PromptKind::GotoLine => "go to line",
        crate::app::PromptKind::QuitDirty => "unsaved: type quit",
    }
}

fn editor_gutter_width(line_count: usize) -> usize {
    line_count.max(1).to_string().len().max(3) + 3
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

fn document_highlight_line_spans(
    base_spans: Vec<Span<'static>>,
    highlights: &[LspDocumentHighlight],
) -> Vec<Span<'static>> {
    if highlights.is_empty() {
        return base_spans;
    }

    let mut spans = Vec::new();
    let mut col = 0usize;
    for span in base_spans {
        let mut buffer = String::new();
        let mut current_style = None::<Style>;
        for ch in span.content.chars() {
            let style = document_highlight_cell_style(span.style, col, highlights);
            if current_style == Some(style) {
                buffer.push(ch);
            } else {
                flush_owned_span(&mut spans, &mut buffer, current_style);
                current_style = Some(style);
                buffer.push(ch);
            }
            col += 1;
        }
        flush_owned_span(&mut spans, &mut buffer, current_style);
    }
    spans
}

fn document_highlight_cell_style(
    base_style: Style,
    col: usize,
    highlights: &[LspDocumentHighlight],
) -> Style {
    match document_highlight_kind_at_col(col, highlights) {
        Some(LspDocumentHighlightKind::Write) => base_style
            .fg(Color::White)
            .bg(HIGHLIGHT_WRITE_BG)
            .add_modifier(Modifier::BOLD),
        Some(LspDocumentHighlightKind::Read) => base_style.bg(HIGHLIGHT_READ_BG),
        Some(LspDocumentHighlightKind::Text) => base_style.bg(HIGHLIGHT_TEXT_BG),
        None => base_style,
    }
}

fn document_highlight_kind_at_col(
    col: usize,
    highlights: &[LspDocumentHighlight],
) -> Option<LspDocumentHighlightKind> {
    let mut matched = None;
    for highlight in highlights {
        if col < highlight.start_col || col >= highlight.end_col {
            continue;
        }
        match highlight.kind {
            LspDocumentHighlightKind::Write => return Some(LspDocumentHighlightKind::Write),
            LspDocumentHighlightKind::Read => matched = Some(LspDocumentHighlightKind::Read),
            LspDocumentHighlightKind::Text => {
                matched.get_or_insert(LspDocumentHighlightKind::Text);
            }
        }
    }
    matched
}

fn line_cursor_cols(tab: &crate::app::EditorTab, line_index: usize) -> Vec<usize> {
    let line_len = tab.lines[line_index].chars().count();
    tab.cursor_positions()
        .into_iter()
        .filter_map(|(line, col)| (line == line_index).then_some(col.min(line_len)))
        .collect()
}

fn cursor_overlay_line_spans(
    base_spans: Vec<Span<'static>>,
    cursor_cols: &[usize],
) -> Vec<Span<'static>> {
    if cursor_cols.is_empty() {
        return base_spans;
    }

    let mut cursor_cols = cursor_cols.to_vec();
    cursor_cols.sort();
    cursor_cols.dedup();
    let mut cursor_index = 0usize;
    let mut col = 0usize;
    let mut spans = Vec::new();
    let mut buffer = String::new();
    let mut current_style = None::<Style>;

    for span in base_spans {
        for ch in span.content.chars() {
            let style = if cursor_index < cursor_cols.len() && cursor_cols[cursor_index] == col {
                while cursor_index < cursor_cols.len() && cursor_cols[cursor_index] == col {
                    cursor_index += 1;
                }
                Style::default().fg(Color::Black).bg(ACCENT)
            } else {
                span.style
            };
            if current_style == Some(style) {
                buffer.push(ch);
            } else {
                flush_owned_span(&mut spans, &mut buffer, current_style);
                current_style = Some(style);
                buffer.push(ch);
            }
            col += 1;
        }
    }
    flush_owned_span(&mut spans, &mut buffer, current_style);

    while cursor_index < cursor_cols.len() {
        if cursor_cols[cursor_index] >= col {
            spans.push(Span::styled(
                " ",
                Style::default().fg(Color::Black).bg(ACCENT),
            ));
        }
        cursor_index += 1;
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
