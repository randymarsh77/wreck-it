use crate::ralph_loop::RalphLoop;
use crate::types::TaskStatus;
use anyhow::Result;
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, Paragraph, Wrap},
    Frame, Terminal,
};
use std::io;

pub struct TuiApp {
    ralph_loop: RalphLoop,
    should_quit: bool,
    paused: bool,
}

impl TuiApp {
    pub fn new(ralph_loop: RalphLoop) -> Self {
        Self {
            ralph_loop,
            should_quit: false,
            paused: false,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        // Setup terminal
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let backend = CrosstermBackend::new(stdout);
        let mut terminal = Terminal::new(backend)?;

        // Initialize the loop
        self.ralph_loop.initialize()?;

        // Run the main loop
        let result = self.run_loop(&mut terminal).await;

        // Restore terminal
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        result
    }

    async fn run_loop(
        &mut self,
        terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    ) -> Result<()> {
        loop {
            terminal.draw(|f| self.ui(f))?;

            // Handle input events
            if event::poll(std::time::Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Char('q') => {
                            self.should_quit = true;
                            self.ralph_loop.stop();
                        }
                        KeyCode::Char(' ') => {
                            self.paused = !self.paused;
                        }
                        _ => {}
                    }
                }
            }

            if self.should_quit {
                break;
            }

            // Run iteration if not paused
            if !self.paused && self.ralph_loop.state().running {
                match self.ralph_loop.run_iteration().await {
                    Ok(should_continue) => {
                        if !should_continue {
                            // Loop finished naturally
                            self.paused = true;
                        }
                    }
                    Err(e) => {
                        self.ralph_loop.state_mut().add_log(format!("Error: {}", e));
                        self.paused = true;
                    }
                }
            }
        }

        Ok(())
    }

    fn ui(&self, f: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(10),
                Constraint::Length(10),
                Constraint::Length(3),
            ])
            .split(f.size());

        self.render_header(f, chunks[0]);
        self.render_tasks(f, chunks[1]);
        self.render_logs(f, chunks[2]);
        self.render_footer(f, chunks[3]);
    }

    fn render_header(&self, f: &mut Frame, area: Rect) {
        let state = self.ralph_loop.state();
        let title = vec![Line::from(vec![
            Span::styled(
                "Ralph Wiggum Loop",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" - "),
            Span::styled(
                format!("Iteration {}/{}", state.iteration, state.max_iterations),
                Style::default().fg(Color::Yellow),
            ),
        ])];

        let status = if self.paused {
            Span::styled("PAUSED", Style::default().fg(Color::Yellow))
        } else if state.running {
            Span::styled("RUNNING", Style::default().fg(Color::Green))
        } else {
            Span::styled("STOPPED", Style::default().fg(Color::Red))
        };

        let header = Paragraph::new(title)
            .block(Block::default().borders(Borders::ALL).title(vec![status]));
        f.render_widget(header, area);
    }

    fn render_tasks(&self, f: &mut Frame, area: Rect) {
        let state = self.ralph_loop.state();
        let tasks: Vec<ListItem> = state
            .tasks
            .iter()
            .enumerate()
            .map(|(idx, task)| {
                let (symbol, color) = match task.status {
                    TaskStatus::Pending => ("○", Color::Gray),
                    TaskStatus::InProgress => ("◐", Color::Yellow),
                    TaskStatus::Completed => ("●", Color::Green),
                    TaskStatus::Failed => ("✗", Color::Red),
                };

                let style = if Some(idx) == state.current_task {
                    Style::default().fg(color).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(color)
                };

                let content = format!("{} [{}] {}", symbol, task.id, task.description);
                ListItem::new(content).style(style)
            })
            .collect();

        let tasks_list =
            List::new(tasks).block(Block::default().borders(Borders::ALL).title("Tasks"));
        f.render_widget(tasks_list, area);
    }

    fn render_logs(&self, f: &mut Frame, area: Rect) {
        let state = self.ralph_loop.state();
        let logs: Vec<Line> = state
            .logs
            .iter()
            .rev()
            .take(8)
            .rev()
            .map(|log| Line::from(log.as_str()))
            .collect();

        let logs_paragraph = Paragraph::new(logs)
            .block(Block::default().borders(Borders::ALL).title("Logs"))
            .wrap(Wrap { trim: true });
        f.render_widget(logs_paragraph, area);
    }

    fn render_footer(&self, f: &mut Frame, area: Rect) {
        let help_text = vec![Line::from(vec![
            Span::styled("[Space]", Style::default().fg(Color::Green)),
            Span::raw(" Pause/Resume  "),
            Span::styled("[Q]", Style::default().fg(Color::Red)),
            Span::raw(" Quit"),
        ])];

        let footer = Paragraph::new(help_text).block(Block::default().borders(Borders::ALL));
        f.render_widget(footer, area);
    }
}
