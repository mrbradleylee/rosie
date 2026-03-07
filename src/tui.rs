use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use std::io;
use std::time::Duration;

pub fn run(host: &str, model: &str) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, host, model);
    let cleanup_result = cleanup_terminal(&mut terminal);

    result.and(cleanup_result)
}

struct AppState {
    mode: InputMode,
    composer_input: String,
    command_input: String,
    transcript: Vec<String>,
    status_message: String,
}

impl AppState {
    fn new() -> Self {
        Self {
            mode: InputMode::Normal,
            composer_input: String::new(),
            command_input: String::new(),
            transcript: Vec::new(),
            status_message: "Normal mode. Press i to enter Insert mode.".to_string(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Insert,
    CommandPalette,
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    host: &str,
    model: &str,
) -> Result<()> {
    let mut app = AppState::new();

    loop {
        terminal.draw(|frame| {
            let root = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(1),
                    Constraint::Length(3),
                    Constraint::Length(1),
                ])
                .split(frame.area());

            let mode_label = match app.mode {
                InputMode::Normal => "NORMAL",
                InputMode::Insert => "INSERT",
                InputMode::CommandPalette => "COMMAND",
            };
            let header =
                Paragraph::new(format!("Rosie TUI | Mode: {mode_label} | Host: {host} | Model: {model}"))
                .block(Block::default().borders(Borders::ALL).title("Status"))
                .style(Style::default().add_modifier(Modifier::BOLD));
            frame.render_widget(header, root[0]);

            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(30), Constraint::Percentage(70)])
                .split(root[1]);

            let sessions = Paragraph::new("Session list (coming soon)")
                .block(Block::default().borders(Borders::ALL).title("Sessions"));
            frame.render_widget(sessions, body[0]);

            let transcript_lines: Vec<Line<'_>> = if app.transcript.is_empty() {
                vec![Line::from("No messages yet. Type in Composer and press Enter.")]
            } else {
                app.transcript
                    .iter()
                    .map(|entry| Line::from(entry.as_str()))
                    .collect()
            };
            let transcript = Paragraph::new(transcript_lines)
                .block(Block::default().borders(Borders::ALL).title("Transcript"));
            frame.render_widget(transcript, body[1]);

            let composer = Paragraph::new(app.composer_input.as_str())
                .block(Block::default().borders(Borders::ALL).title("Composer"));
            frame.render_widget(composer, root[2]);

            let footer_help = match app.mode {
                InputMode::Normal => "i: insert | : command palette | Ctrl+C: quit",
                InputMode::Insert => "Enter: send | Backspace: edit | Esc: normal",
                InputMode::CommandPalette => "Type command | Enter: run | Esc: cancel",
            };
            let footer = Paragraph::new(format!("{} | {}", footer_help, app.status_message))
                .style(Style::default().add_modifier(Modifier::DIM));
            frame.render_widget(footer, root[3]);

            if app.mode == InputMode::CommandPalette {
                let popup = centered_rect(60, 3, frame.area());
                let command = Paragraph::new(format!(":{}", app.command_input))
                    .block(Block::default().borders(Borders::ALL).title("Command"))
                    .alignment(Alignment::Left);
                frame.render_widget(Clear, popup);
                frame.render_widget(command, popup);
            }
        })?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                break;
            }

            match app.mode {
                InputMode::Normal => match key.code {
                    KeyCode::Char('i') => {
                        app.mode = InputMode::Insert;
                        app.status_message = "Insert mode.".to_string();
                    }
                    KeyCode::Char(':') => {
                        app.mode = InputMode::CommandPalette;
                        app.command_input.clear();
                        app.status_message = "Command palette open.".to_string();
                    }
                    _ => {}
                },
                InputMode::Insert => match key.code {
                    KeyCode::Esc => {
                        app.mode = InputMode::Normal;
                        app.status_message = "Normal mode.".to_string();
                    }
                    KeyCode::Enter => {
                        let trimmed = app.composer_input.trim();
                        if !trimmed.is_empty() {
                            app.transcript.push(format!("You: {trimmed}"));
                            app.composer_input.clear();
                            app.status_message = "Message added locally.".to_string();
                        }
                    }
                    KeyCode::Backspace => {
                        app.composer_input.pop();
                    }
                    KeyCode::Char(ch) => {
                        app.composer_input.push(ch);
                    }
                    _ => {}
                },
                InputMode::CommandPalette => match key.code {
                    KeyCode::Esc => {
                        app.mode = InputMode::Normal;
                        app.command_input.clear();
                        app.status_message = "Command cancelled.".to_string();
                    }
                    KeyCode::Enter => {
                        if run_palette_command(&mut app) {
                            break;
                        }
                    }
                    KeyCode::Backspace => {
                        app.command_input.pop();
                    }
                    KeyCode::Char(ch) => {
                        app.command_input.push(ch);
                    }
                    _ => {}
                },
            }
        }
    }

    Ok(())
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(height),
            Constraint::Min(1),
        ])
        .split(area);
    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Percentage((100 - width_percent) / 2),
        ])
        .split(vertical[1]);
    horizontal[1]
}

fn run_palette_command(app: &mut AppState) -> bool {
    let command = app
        .command_input
        .trim()
        .trim_start_matches(':')
        .trim()
        .to_ascii_lowercase();

    app.command_input.clear();
    app.mode = InputMode::Normal;

    match command.as_str() {
        "" => {
            app.status_message = "No command entered.".to_string();
            false
        }
        "quit" | "q" => true,
        "new" => {
            app.status_message = "Command :new not implemented yet.".to_string();
            false
        }
        "model" => {
            app.status_message = "Command :model not implemented yet.".to_string();
            false
        }
        _ => {
            app.status_message = format!("Unknown command: :{command}");
            false
        }
    }
}
