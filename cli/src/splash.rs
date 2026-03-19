use ratatui::{
    layout::Rect,
    style::{Color, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

/// A mapping from a single character in ASCII art to a display style.
/// Each entry defines which character it matches and what color/style to apply.
pub struct ArtColorRule {
    pub ch: char,
    pub style: Style,
    /// When set, render this character instead of `ch`.
    pub display: Option<char>,
}

/// A colored ASCII art image defined as lines of text plus a set of color rules.
/// Characters not matching any rule are rendered with the default style.
pub struct ColoredArt {
    pub lines: Vec<&'static str>,
    pub rules: Vec<ArtColorRule>,
    pub default_style: Style,
}

impl ColoredArt {
    /// Convert the art into styled ratatui Lines for rendering.
    pub fn to_lines(&self) -> Vec<Line<'static>> {
        self.lines
            .iter()
            .map(|line| {
                let spans: Vec<Span<'static>> = line
                    .chars()
                    .map(|ch| {
                        let (display_ch, style) = self
                            .rules
                            .iter()
                            .find(|r| r.ch == ch)
                            .map(|r| (r.display.unwrap_or(ch), r.style))
                            .unwrap_or((ch, self.default_style));
                        Span::styled(display_ch.to_string(), style)
                    })
                    .collect();
                Line::from(spans)
            })
            .collect()
    }
}

// ─── Big block letters for "WRECK-IT" ────────────────────────────────

/// Big block letter title "WRECK-IT" using █ block characters.
pub const TITLE_ART: &[&str] = &[
    "██       ██ ███████  █████ ██████  ██   ██    ██ ██████",
    "██       ██ ██    ██ ██    ██   ██ ██  ██     ██   ██  ",
    "██  ██   ██ ███████  ████  ██      █████  ███ ██   ██  ",
    "██ ██ ██ ██ ██   ██  ██    ██   ██ ██  ██     ██   ██  ",
    " ██     ██  ██    ██ █████ ██████  ██   ██    ██   ██  ",
];

// ─── Ralph character art ─────────────────────────────────────────────

/// The character ASCII art provided in the issue, rendered with monospace font.
/// Each symbol can be independently colored via `ralph_art()` rules.
pub const RALPH_ART: &[&str] = &[
    "████████████████████████████████",
    "██████▓▓▓▓█████▓▓▓▓████▓▓▓▓█████",
    "████████▓▓▓▓▓▓▓▓▓▓▓██▓▓▓▓███████",
    "███▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓███",
    "█████▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓█████",
    "█▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓█",
    "███▓▓▓▓▓▓▓▓░░░░░░░░░░▓▓▓▓▓▓▓▓███",
    "█████▓▓▓▓▒▒██░░░░░░██▒▒▓▓▓▓█████",
    "███▓▓▓▓▓▓▒▒▒▒██▓▓██▒▒▒▒▓▓▓▓▓▓███",
    "█▓▓▒▒▒▒▓▓▒▒────▒▒────▒▒▓▓▒▒▒▒▓▓█",
    "███░░▒▒▓▓░░──██░▒██──░░▓▓▒▒░░███",
    "███░░░░▓▓░░░░▓▓▓▓▓▓░░░░▓▓░░░░███",
    "███████░░░░░░▓▓▓▓▓▓░░░░░░███████",
    "███████░░░░░░░░░░░░░░░░░░███████",
    "█████░░░░██▀▀▀▀▀▀▀▀▀▀██░░░░█████",
    "█████░░██████████████████░░█████",
    "█████░░██▒▒██▓▓▓▓▓▓██▒▒██░░█████",
    "█████░░██▓█▀▀▀▀▀▀▀▀▀▀█▓██░░█████",
    "███████░░░░░░░░░░░░░░░░░░███████",
    "█████████▒▒▒▒▒▒▒▒▒▒▒▒▒▒█████████",
    "████████████████████████████████",
];

/// Build the colored art for the Ralph character.
pub fn ralph_art() -> ColoredArt {
    ColoredArt {
        lines: RALPH_ART.to_vec(),
        rules: vec![
            ArtColorRule {
                ch: '▓',
                style: Style::default().fg(Color::Red),
                display: Some('█'),
            },
            ArtColorRule {
                ch: '░',
                style: Style::default().fg(Color::Rgb(242, 192, 156)), // skin
                display: Some('█'),
            },
            ArtColorRule {
                ch: '█',
                style: Style::default().fg(Color::Black),
                display: Some('█'),
            },
            ArtColorRule {
                ch: '▒',
                style: Style::default().fg(Color::Rgb(180, 100, 80)), // light brown
                display: Some('█'),
            },
            ArtColorRule {
                ch: '─',
                style: Style::default().fg(Color::White),
                display: Some('█'),
            },
            ArtColorRule {
                ch: '▀',
                style: Style::default().fg(Color::White),
                display: Some('█'),
            },
            ArtColorRule {
                ch: '▄',
                style: Style::default().fg(Color::White),
                display: Some('█'),
            },
        ],
        default_style: Style::default().fg(Color::White),
    }
}

/// Build the colored art for the big "WRECK-IT" title.
pub fn title_art() -> ColoredArt {
    ColoredArt {
        lines: TITLE_ART.to_vec(),
        rules: vec![ArtColorRule {
            ch: '█',
            style: Style::default().fg(Color::Cyan),
            display: None,
        }],
        default_style: Style::default().fg(Color::Cyan),
    }
}

/// Pre-computed character counts for each TITLE_ART line.
fn title_line_char_counts() -> Vec<usize> {
    TITLE_ART.iter().map(|l| l.chars().count()).collect()
}

/// Render the splash screen: title on the left, Ralph art on the right.
pub fn render_splash(f: &mut Frame, area: Rect) {
    let title = title_art();
    let ralph = ralph_art();

    let title_lines = title.to_lines();
    let ralph_lines = ralph.to_lines();

    // Combine title + art side-by-side.
    // Title goes on the left, Ralph art on the right, separated by a gap.
    let char_counts = title_line_char_counts();
    let title_width = char_counts.iter().copied().max().unwrap_or(0);
    let gap = 4;

    // Vertically center the title next to the art.
    let ralph_height = ralph_lines.len();
    let title_height = title_lines.len();
    let title_v_offset = if ralph_height > title_height {
        (ralph_height - title_height) / 2
    } else {
        0
    };

    let mut combined: Vec<Line<'static>> = Vec::new();

    // Blank line at top
    combined.push(Line::from(""));

    for (row, ralph_line) in ralph_lines.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();

        // Title portion (or padding)
        if row >= title_v_offset && row < title_v_offset + title_height {
            let title_row = row - title_v_offset;
            let line_spans = title_lines[title_row].spans.clone();
            spans.extend(line_spans);
            // Pad to title_width
            let line_char_count = char_counts[title_row];
            if line_char_count < title_width {
                spans.push(Span::raw(" ".repeat(title_width - line_char_count)));
            }
        } else {
            spans.push(Span::raw(" ".repeat(title_width)));
        }

        // Gap
        spans.push(Span::raw(" ".repeat(gap)));

        // Ralph art portion
        spans.extend(ralph_line.spans.clone());

        combined.push(Line::from(spans));
    }

    // Subtitle line
    combined.push(Line::from(""));
    combined.push(Line::from(vec![Span::styled(
        "                     Press any key to continue...",
        Style::default().fg(Color::DarkGray),
    )]));

    let splash = Paragraph::new(combined);
    f.render_widget(splash, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ralph_art_lines_count() {
        let art = ralph_art();
        let lines = art.to_lines();
        assert_eq!(lines.len(), RALPH_ART.len());
    }

    #[test]
    fn test_title_art_lines_count() {
        let art = title_art();
        let lines = art.to_lines();
        assert_eq!(lines.len(), TITLE_ART.len());
    }

    #[test]
    fn test_color_rule_matching() {
        let art = ralph_art();
        // Verify that all expected character rules are present
        let rule_chars: Vec<char> = art.rules.iter().map(|r| r.ch).collect();
        assert!(rule_chars.contains(&'▓'));
        assert!(rule_chars.contains(&'░'));
        assert!(rule_chars.contains(&'█'));
        assert!(rule_chars.contains(&'▒'));
    }

    #[test]
    fn test_colored_art_to_lines_applies_styles() {
        let art = ColoredArt {
            lines: vec!["█▓"],
            rules: vec![
                ArtColorRule {
                    ch: '█',
                    style: Style::default().fg(Color::Blue),
                    display: None,
                },
                ArtColorRule {
                    ch: '▓',
                    style: Style::default().fg(Color::Red),
                    display: None,
                },
            ],
            default_style: Style::default(),
        };
        let lines = art.to_lines();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].spans.len(), 2);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Blue));
        assert_eq!(lines[0].spans[1].style.fg, Some(Color::Red));
    }

    #[test]
    fn test_default_style_for_unmatched_chars() {
        let art = ColoredArt {
            lines: vec!["ab"],
            rules: vec![],
            default_style: Style::default().fg(Color::Green),
        };
        let lines = art.to_lines();
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Green));
        assert_eq!(lines[0].spans[1].style.fg, Some(Color::Green));
    }
}
