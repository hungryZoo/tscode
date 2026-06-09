use std::path::Path;

use ratatui::{
    style::{Color as TuiColor, Style as TuiStyle},
    text::Span,
};
use syntect::{
    easy::HighlightLines,
    highlighting::{Color as SynColor, Theme, ThemeSet},
    parsing::SyntaxSet,
};

pub struct SyntaxHighlighter {
    syntaxes: SyntaxSet,
    theme: Theme,
}

impl SyntaxHighlighter {
    pub fn new() -> Self {
        let syntaxes = SyntaxSet::load_defaults_newlines();
        let themes = ThemeSet::load_defaults();
        let theme = themes
            .themes
            .get("base16-ocean.dark")
            .or_else(|| themes.themes.values().next())
            .cloned()
            .unwrap_or_default();

        Self { syntaxes, theme }
    }

    pub fn highlight_visible<'a>(
        &'a self,
        path: &Path,
        lines: &'a [String],
        start: usize,
        end: usize,
    ) -> Vec<Vec<Span<'static>>> {
        let syntax = self
            .syntaxes
            .find_syntax_for_file(path)
            .ok()
            .flatten()
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text());

        let mut highlighter = HighlightLines::new(syntax, &self.theme);
        let mut highlighted = Vec::new();

        for (index, line) in lines.iter().enumerate().take(end) {
            let ranges = highlighter.highlight_line(line, &self.syntaxes);
            if index < start {
                continue;
            }

            match ranges {
                Ok(ranges) => highlighted.push(
                    ranges
                        .into_iter()
                        .map(|(style, text)| {
                            Span::styled(
                                text.to_owned(),
                                TuiStyle::default().fg(to_tui(style.foreground)),
                            )
                        })
                        .collect(),
                ),
                Err(_) => highlighted.push(vec![Span::raw(line.clone())]),
            }
        }

        highlighted
    }
}

fn to_tui(color: SynColor) -> TuiColor {
    TuiColor::Rgb(color.r, color.g, color.b)
}
