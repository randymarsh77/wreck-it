use rand::Rng;
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
    "                                ",
    "      ▓▓▓▓     ▓▓▓▓    ▓▓▓▓     ",
    "        ▓▓▓▓▓▓▓▓▓▓▓  ▓▓▓▓       ",
    "   ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓   ",
    "     ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓     ",
    " ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓ ",
    "   ▓▓▓▓▓▓▓▓░░░░░░░░░░▓▓▓▓▓▓▓▓   ",
    "     ▓▓▓▓▒▒██░░░░░░██▒▒▓▓▓▓     ",
    "   ▓▓▓▓▓▓▒▒▒▒██▓▓██▒▒▒▒▓▓▓▓▓▓   ",
    " ▓▓▒▒▒▒▓▓▒▒────▒▒────▒▒▓▓▒▒▒▒▓▓ ",
    "   ░░▒▒▓▓░░──██░▒██──░░▓▓▒▒░░   ",
    "   ░░░░▓▓░░░░▓▓▓▓▓▓░░░░▓▓░░░░   ",
    "       ░░░░░░▓▓▓▓▓▓░░░░░░       ",
    "       ░░░░░░░░░░░░░░░░░░       ",
    "     ░░░░██▀▀▀▀▀▀▀▀▀▀██░░░░     ",
    "     ░░██████████████████░░     ",
    "     ░░██▒▒██▓▓▓▓▓▓██▒▒██░░     ",
    "     ░░██▓█▀▀▀▀▀▀▀▀▀▀█▓██░░     ",
    "       ░░░░░░░░░░░░░░░░░░       ",
    "         ▒▒▒▒▒▒▒▒▒▒▒▒▒▒         ",
    "                                ",
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

// ─── Generic falling-character animator ──────────────────────────────

/// A positioned, styled character cell tracked by the animator.
struct AnimCell {
    ch: char,
    style: Style,
    col: u16,
    final_row: f32,
    current_y: f32,
    speed_in: f32,
    delay_in: u16,
    speed_out: f32,
    delay_out: u16,
}

#[derive(Clone, Copy, PartialEq)]
enum AnimPhase {
    FallingIn,
    HoldPreShimmer,
    Shimmer,
    HoldPostShimmer,
    FallingOut,
    Done,
}

/// A generic falling-character animator.
///
/// Takes arbitrary styled `Line`s, centers them in the terminal area,
/// and animates each non-space character:
///   1. **Fall in** — characters drop from random positions above the screen
///      at randomized speeds and staggered delays.
///   2. **Hold (pre-shimmer)** — brief pause.
///   3. **Shimmer** — a fast diagonal band of white sweeps across.
///   4. **Hold (post-shimmer)** — brief pause.
///   5. **Fall out** — characters drop off the bottom in a randomized order.
pub struct FallingCharAnimator {
    cells: Vec<AnimCell>,
    phase: AnimPhase,
    tick: u64,
    out_tick: u64,
    hold_remaining: u16,
    area: Rect,
    /// Shimmer: the current x-position of the leading edge of the diagonal band.
    shimmer_x: f32,
    /// How many columns wide the diagonal shimmer band is.
    shimmer_width: f32,
    /// How fast the shimmer sweeps (columns per tick).
    shimmer_speed: f32,
    /// The rightmost column the shimmer needs to pass before finishing.
    shimmer_end: f32,
}

impl FallingCharAnimator {
    /// Create a new animator from styled lines, centered in the given area.
    pub fn new(lines: Vec<Line<'static>>, area: Rect) -> Self {
        let content_height = lines.len();
        let content_width = lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|s| s.content.chars().count())
                    .sum::<usize>()
            })
            .max()
            .unwrap_or(0);

        let v_offset = (area.height as usize).saturating_sub(content_height) / 2;
        let h_offset = (area.width as usize).saturating_sub(content_width) / 2;

        let mut rng = rand::thread_rng();
        let mut cells = Vec::new();

        for (row_idx, line) in lines.iter().enumerate() {
            let mut col = h_offset as u16;
            for span in &line.spans {
                for ch in span.content.chars() {
                    if ch != ' ' {
                        let final_row = (v_offset + row_idx) as f32;
                        cells.push(AnimCell {
                            ch,
                            style: span.style,
                            col,
                            final_row,
                            current_y: rng.gen_range(-(area.height as f32)..0.0),
                            speed_in: rng.gen_range(1.6..5.0),
                            delay_in: rng.gen_range(0..4),
                            speed_out: rng.gen_range(1.6..6.0),
                            delay_out: rng.gen_range(0..10),
                        });
                    }
                    col += 1;
                }
            }
        }

        let max_col = cells.iter().map(|c| c.col).max().unwrap_or(0) as f32;
        let shimmer_width = 6.0;
        let shimmer_speed = 12.0;
        // The diagonal offsets by row, so we need extra travel to clear all rows.
        let shimmer_end = max_col + (area.height as f32) + shimmer_width;

        Self {
            cells,
            phase: AnimPhase::FallingIn,
            tick: 0,
            out_tick: 0,
            hold_remaining: 10, // brief pause (~0.3 s at 30 fps)
            area,
            shimmer_x: -(area.height as f32) - shimmer_width,
            shimmer_width,
            shimmer_speed,
            shimmer_end,
        }
    }

    /// Advance the animation by one frame.
    pub fn tick(&mut self) {
        self.tick += 1;
        match self.phase {
            AnimPhase::FallingIn => {
                let tick = self.tick;
                let mut all_landed = true;
                for cell in &mut self.cells {
                    if tick <= cell.delay_in as u64 {
                        all_landed = false;
                        continue;
                    }
                    if cell.current_y < cell.final_row {
                        cell.current_y += cell.speed_in;
                        if cell.current_y >= cell.final_row {
                            cell.current_y = cell.final_row;
                        } else {
                            all_landed = false;
                        }
                    }
                }
                if all_landed {
                    self.phase = AnimPhase::HoldPreShimmer;
                }
            }
            AnimPhase::HoldPreShimmer => {
                self.hold_remaining = self.hold_remaining.saturating_sub(1);
                if self.hold_remaining == 0 {
                    self.phase = AnimPhase::Shimmer;
                }
            }
            AnimPhase::Shimmer => {
                self.shimmer_x += self.shimmer_speed;
                if self.shimmer_x > self.shimmer_end {
                    self.hold_remaining = 10; // brief pause after shimmer
                    self.phase = AnimPhase::HoldPostShimmer;
                }
            }
            AnimPhase::HoldPostShimmer => {
                self.hold_remaining = self.hold_remaining.saturating_sub(1);
                if self.hold_remaining == 0 {
                    self.phase = AnimPhase::FallingOut;
                }
            }
            AnimPhase::FallingOut => {
                self.out_tick += 1;
                let out_tick = self.out_tick;
                let area_h = self.area.height as f32;
                let mut all_gone = true;
                for cell in &mut self.cells {
                    if out_tick <= cell.delay_out as u64 {
                        all_gone = false;
                        continue;
                    }
                    if cell.current_y <= area_h {
                        cell.current_y += cell.speed_out;
                        if cell.current_y <= area_h {
                            all_gone = false;
                        }
                    }
                }
                if all_gone {
                    self.phase = AnimPhase::Done;
                }
            }
            AnimPhase::Done => {}
        }
    }

    /// Returns true when the full animation cycle has completed.
    pub fn is_done(&self) -> bool {
        matches!(self.phase, AnimPhase::Done)
    }

    /// Render the current animation frame.
    pub fn render(&self, f: &mut Frame, area: Rect) {
        let w = area.width as usize;
        let h = area.height as usize;

        // During shimmer phase, compute which cells are hit by the diagonal band.
        let shimmer_style = Style::default().fg(Color::White);
        let in_shimmer = matches!(self.phase, AnimPhase::Shimmer);

        // Collect visible cells grouped by row (sparse).
        let mut row_cells: Vec<Vec<(usize, char, Style)>> = vec![Vec::new(); h];
        for cell in &self.cells {
            let y = cell.current_y.round() as i32;
            let x = cell.col as usize;
            if y >= 0 && (y as usize) < h && x < w {
                // Shimmer compositing: diagonal band position = shimmer_x - row
                let style = if in_shimmer {
                    let band_x = self.shimmer_x - (y as f32);
                    if (x as f32) >= band_x && (x as f32) < band_x + self.shimmer_width {
                        shimmer_style
                    } else {
                        cell.style
                    }
                } else {
                    cell.style
                };
                row_cells[y as usize].push((x, cell.ch, style));
            }
        }

        let lines: Vec<Line<'static>> = row_cells
            .into_iter()
            .map(|mut cells_in_row| {
                if cells_in_row.is_empty() {
                    return Line::from("");
                }
                cells_in_row.sort_by_key(|&(x, _, _)| x);
                let mut spans = Vec::new();
                let mut cursor = 0usize;
                for (x, ch, style) in cells_in_row {
                    if x > cursor {
                        spans.push(Span::raw(" ".repeat(x - cursor)));
                    }
                    spans.push(Span::styled(ch.to_string(), style));
                    cursor = x + 1;
                }
                Line::from(spans)
            })
            .collect();

        f.render_widget(Paragraph::new(lines), area);

        // Fixed "Press any key to continue..." at the bottom
        let hint = "Press any key to continue...";
        let hint_x = (area.width as usize).saturating_sub(hint.len()) / 2;
        let hint_y = area.height.saturating_sub(2);
        let hint_area = Rect::new(hint_x as u16, hint_y, hint.len() as u16, 1);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hint,
                Style::default().fg(Color::DarkGray),
            ))),
            hint_area,
        );
    }
}

// ─── Splash-specific content builder ─────────────────────────────────

/// Build the splash content as styled lines (title beside Ralph art)
/// without any centering — the animator handles positioning.
pub fn build_splash_content() -> Vec<Line<'static>> {
    let title = title_art();
    let ralph = ralph_art();
    let title_lines = title.to_lines();
    let ralph_lines = ralph.to_lines();

    let char_counts = title_line_char_counts();
    let title_width = char_counts.iter().copied().max().unwrap_or(0);
    let gap = 4;

    let ralph_height = ralph_lines.len();
    let title_height = title_lines.len();
    let title_v_offset = if ralph_height > title_height {
        (ralph_height - title_height) / 2
    } else {
        0
    };

    let mut lines = Vec::new();

    for (row, ralph_line) in ralph_lines.iter().enumerate() {
        let mut spans: Vec<Span<'static>> = Vec::new();

        if row >= title_v_offset && row < title_v_offset + title_height {
            let title_row = row - title_v_offset;
            spans.extend(title_lines[title_row].spans.clone());
            let line_char_count = char_counts[title_row];
            if line_char_count < title_width {
                spans.push(Span::raw(" ".repeat(title_width - line_char_count)));
            }
        } else {
            spans.push(Span::raw(" ".repeat(title_width)));
        }

        spans.push(Span::raw(" ".repeat(gap)));
        spans.extend(ralph_line.spans.clone());

        lines.push(Line::from(spans));
    }

    lines
}

/// Create a splash screen animator for the given terminal area.
pub fn new_splash_animator(area: Rect) -> FallingCharAnimator {
    FallingCharAnimator::new(build_splash_content(), area)
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
