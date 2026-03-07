use anyhow::{Result, anyhow};
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
use serde::{Deserialize, Serialize};
use std::io;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

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
    host: String,
    model: String,
    composer_input: String,
    command_input: String,
    messages: Vec<ChatMessage>,
    in_flight: Option<InFlightRequest>,
    status_message: String,
}

impl AppState {
    fn new(host: &str, model: &str) -> Self {
        Self {
            mode: InputMode::Normal,
            host: host.to_string(),
            model: model.to_string(),
            composer_input: String::new(),
            command_input: String::new(),
            messages: Vec::new(),
            in_flight: None,
            status_message: "Normal mode. Press i to enter Insert mode.".to_string(),
        }
    }

    fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Normal,
    Insert,
    CommandPalette,
}

#[derive(Clone, Serialize, Deserialize)]
struct ChatMessage {
    role: String,
    content: String,
}

enum StreamEvent {
    Token(String),
    Done,
    Error(String),
}

struct InFlightRequest {
    receiver: mpsc::UnboundedReceiver<StreamEvent>,
    handle: JoinHandle<()>,
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    host: &str,
    model: &str,
) -> Result<()> {
    let mut app = AppState::new(host, model);

    loop {
        process_stream_events(&mut app);

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
            let header = Paragraph::new(format!(
                "Rosie TUI | Mode: {mode_label} | Host: {} | Model: {}{}",
                app.host,
                app.model,
                if app.is_busy() { " | Streaming..." } else { "" }
            ))
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

            let transcript_lines: Vec<Line<'_>> = if app.messages.is_empty() {
                vec![Line::from("No messages yet. Press i, type, then Enter.")]
            } else {
                app.messages
                    .iter()
                    .map(|entry| {
                        let label = match entry.role.as_str() {
                            "user" => "You",
                            "assistant" => "Assistant",
                            _ => "System",
                        };
                        Line::from(format!("{label}: {}", entry.content))
                    })
                    .collect()
            };
            let transcript = Paragraph::new(transcript_lines)
                .block(Block::default().borders(Borders::ALL).title("Transcript"));
            frame.render_widget(transcript, body[1]);

            let composer = Paragraph::new(app.composer_input.as_str())
                .block(Block::default().borders(Borders::ALL).title("Composer"));
            frame.render_widget(composer, root[2]);

            let footer_help = match app.mode {
                InputMode::Normal => {
                    if app.is_busy() {
                        "i: insert (disabled) | : command palette | Esc: cancel stream | Ctrl+C: quit"
                    } else {
                        "i: insert | : command palette | Ctrl+C: quit"
                    }
                }
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
                cancel_request(&mut app, true);
                break;
            }

            match app.mode {
                InputMode::Normal => match key.code {
                    KeyCode::Char('i') => {
                        if app.is_busy() {
                            app.status_message =
                                "Wait for streaming to finish or press Esc to cancel.".to_string();
                        } else {
                            app.mode = InputMode::Insert;
                            app.status_message = "Insert mode.".to_string();
                        }
                    }
                    KeyCode::Char(':') => {
                        app.mode = InputMode::CommandPalette;
                        app.command_input.clear();
                        app.status_message = "Command palette open.".to_string();
                    }
                    KeyCode::Esc => {
                        if app.is_busy() {
                            cancel_request(&mut app, false);
                        }
                    }
                    _ => {}
                },
                InputMode::Insert => match key.code {
                    KeyCode::Esc => {
                        app.mode = InputMode::Normal;
                        app.status_message = "Normal mode.".to_string();
                    }
                    KeyCode::Enter => {
                        submit_composer_message(&mut app);
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
                            cancel_request(&mut app, true);
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

fn submit_composer_message(app: &mut AppState) {
    if app.is_busy() {
        app.status_message = "A response is already streaming.".to_string();
        return;
    }

    let trimmed = app.composer_input.trim();
    if trimmed.is_empty() {
        return;
    }

    let user_content = trimmed.to_string();
    app.messages.push(ChatMessage {
        role: "user".to_string(),
        content: user_content,
    });
    app.composer_input.clear();

    let request_messages = app.messages.clone();
    app.messages.push(ChatMessage {
        role: "assistant".to_string(),
        content: String::new(),
    });

    let (tx, rx) = mpsc::unbounded_channel();
    let host = app.host.clone();
    let model = app.model.clone();

    let handle = tokio::spawn(async move {
        if let Err(err) = stream_ollama_chat(&host, &model, request_messages, tx.clone()).await {
            let _ = tx.send(StreamEvent::Error(err.to_string()));
        }
    });

    app.in_flight = Some(InFlightRequest {
        receiver: rx,
        handle,
    });
    app.mode = InputMode::Normal;
    app.status_message = "Sending request to Ollama...".to_string();
}

fn process_stream_events(app: &mut AppState) {
    let Some(in_flight) = app.in_flight.as_mut() else {
        return;
    };

    let mut done = false;
    loop {
        match in_flight.receiver.try_recv() {
            Ok(StreamEvent::Token(content)) => {
                if let Some(last) = app.messages.last_mut() {
                    if last.role == "assistant" {
                        last.content.push_str(&content);
                    }
                }
                app.status_message = "Streaming response...".to_string();
            }
            Ok(StreamEvent::Done) => {
                app.status_message = "Response complete.".to_string();
                done = true;
                break;
            }
            Ok(StreamEvent::Error(message)) => {
                if let Some(last) = app.messages.last_mut() {
                    if last.role == "assistant" && last.content.trim().is_empty() {
                        last.content = format!("[error] {message}");
                    }
                }
                app.status_message = format!("Request error: {message}");
                done = true;
                break;
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                done = true;
                break;
            }
        }
    }

    if done {
        app.in_flight = None;
    }
}

fn cancel_request(app: &mut AppState, silent: bool) {
    if let Some(in_flight) = app.in_flight.take() {
        in_flight.handle.abort();
        if let Some(last) = app.messages.last_mut() {
            if last.role == "assistant" && last.content.trim().is_empty() {
                last.content = "[cancelled]".to_string();
            }
        }
        if !silent {
            app.status_message = "Streaming cancelled.".to_string();
        }
    }
}

#[derive(Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Deserialize)]
struct OllamaChatChunk {
    message: Option<OllamaChunkMessage>,
    done: Option<bool>,
    error: Option<String>,
}

#[derive(Deserialize)]
struct OllamaChunkMessage {
    content: Option<String>,
}

async fn stream_ollama_chat(
    host: &str,
    model: &str,
    messages: Vec<ChatMessage>,
    tx: mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let url = format!("{}/api/chat", host.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let request = OllamaChatRequest {
        model: model.to_string(),
        messages,
        stream: true,
    };

    let mut resp = client
        .post(url)
        .json(&request)
        .send()
        .await
        .map_err(|e| anyhow!("HTTP send error: {e}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "Ollama returned {}: {}",
            resp.status(),
            resp.text().await.unwrap_or_else(|_| "<no body>".to_string())
        ));
    }

    let mut buffer = String::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow!("Stream read error: {e}"))?
    {
        buffer.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(newline_pos) = buffer.find('\n') {
            let line = buffer[..newline_pos].trim().to_string();
            buffer = buffer[newline_pos + 1..].to_string();
            if line.is_empty() {
                continue;
            }
            parse_and_emit_line(&line, &tx)?;
        }
    }

    let remainder = buffer.trim();
    if !remainder.is_empty() {
        parse_and_emit_line(remainder, &tx)?;
    }

    let _ = tx.send(StreamEvent::Done);
    Ok(())
}

fn parse_and_emit_line(line: &str, tx: &mpsc::UnboundedSender<StreamEvent>) -> Result<()> {
    let parsed: OllamaChatChunk =
        serde_json::from_str(line).map_err(|e| anyhow!("Failed to parse stream JSON: {e}"))?;

    if let Some(error) = parsed.error {
        let _ = tx.send(StreamEvent::Error(error));
        return Ok(());
    }

    if let Some(message) = parsed.message
        && let Some(content) = message.content
        && !content.is_empty()
    {
        let _ = tx.send(StreamEvent::Token(content));
    }

    if parsed.done.unwrap_or(false) {
        let _ = tx.send(StreamEvent::Done);
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
            if app.is_busy() {
                app.status_message =
                    "Cannot reset while request is in progress. Press Esc to cancel first."
                        .to_string();
            } else {
                app.messages.clear();
                app.composer_input.clear();
                app.status_message = "Started new local conversation.".to_string();
            }
            false
        }
        "model" => {
            app.status_message = format!("Active model: {}", app.model);
            false
        }
        "help" => {
            app.status_message = "Commands: :help :new :model :quit".to_string();
            false
        }
        _ => {
            app.status_message = format!("Unknown command: :{command}");
            false
        }
    }
}
