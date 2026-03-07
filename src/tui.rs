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
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::io;
use std::path::Path;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub fn run(host: &str, model: &str, db_path: &Path) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, host, model, db_path);
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
    active_session_id: i64,
    sessions: Vec<SessionSummary>,
    selected_session_index: usize,
    normal_focus: NormalFocus,
    store: SessionStore,
    transcript_scroll: u16,
    transcript_follow: bool,
    transcript_view_width: usize,
    transcript_view_height: usize,
    pending_g: bool,
    in_flight: Option<InFlightRequest>,
    status_message: String,
}

impl AppState {
    fn new(
        host: &str,
        model: &str,
        store: SessionStore,
        active_session_id: i64,
        messages: Vec<ChatMessage>,
        sessions: Vec<SessionSummary>,
        selected_session_index: usize,
    ) -> Self {
        Self {
            mode: InputMode::Normal,
            host: host.to_string(),
            model: model.to_string(),
            composer_input: String::new(),
            command_input: String::new(),
            messages,
            active_session_id,
            sessions,
            selected_session_index,
            normal_focus: NormalFocus::Transcript,
            store,
            transcript_scroll: 0,
            transcript_follow: true,
            transcript_view_width: 1,
            transcript_view_height: 1,
            pending_g: false,
            in_flight: None,
            status_message: format!(
                "Loaded session #{}. Press i to enter Insert mode.",
                active_session_id
            ),
        }
    }

    fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NormalFocus {
    Sessions,
    Transcript,
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

struct SessionStore {
    conn: Connection,
}

struct LoadedSession {
    session_id: i64,
    messages: Vec<ChatMessage>,
}

#[derive(Clone)]
struct SessionSummary {
    id: i64,
    title: Option<String>,
    message_count: i64,
}

impl SessionStore {
    fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS sessions (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                title TEXT,
                is_archived INTEGER NOT NULL DEFAULT 0
            );

            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_id INTEGER NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
                role TEXT NOT NULL,
                content TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_session_id_id
                ON messages(session_id, id);

            CREATE INDEX IF NOT EXISTS idx_sessions_updated_at
                ON sessions(updated_at DESC);
            ",
        )?;

        Ok(Self { conn })
    }

    fn load_or_create_active_session(&self) -> Result<LoadedSession> {
        let session_id = self
            .conn
            .query_row(
                "SELECT id FROM sessions WHERE is_archived = 0 ORDER BY updated_at DESC, id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(self.create_session()?);

        Ok(LoadedSession {
            session_id,
            messages: self.load_messages(session_id)?,
        })
    }

    fn create_session(&self) -> Result<i64> {
        let now = unix_timestamp();
        self.conn.execute(
            "INSERT INTO sessions (created_at, updated_at, title, is_archived) VALUES (?1, ?2, NULL, 0)",
            params![now, now],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    fn load_messages(&self, session_id: i64) -> Result<Vec<ChatMessage>> {
        let mut stmt = self
            .conn
            .prepare("SELECT role, content FROM messages WHERE session_id = ?1 ORDER BY id ASC")?;
        let rows = stmt.query_map(params![session_id], |row| {
            Ok(ChatMessage {
                role: row.get(0)?,
                content: row.get(1)?,
            })
        })?;

        let messages = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(messages)
    }

    fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT s.id, s.title, COUNT(m.id) AS message_count
            FROM sessions s
            LEFT JOIN messages m ON m.session_id = s.id
            WHERE s.is_archived = 0
            GROUP BY s.id
            ORDER BY s.updated_at DESC, s.id DESC
            ",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                message_count: row.get(2)?,
            })
        })?;

        let sessions = rows.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(sessions)
    }

    fn insert_message(&self, session_id: i64, role: &str, content: &str) -> Result<i64> {
        let now = unix_timestamp();
        self.conn.execute(
            "INSERT INTO messages (session_id, role, content, created_at) VALUES (?1, ?2, ?3, ?4)",
            params![session_id, role, content, now],
        )?;
        self.touch_session(session_id)?;
        Ok(self.conn.last_insert_rowid())
    }

    fn update_message_content(&self, message_id: i64, content: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE messages SET content = ?1 WHERE id = ?2",
            params![content, message_id],
        )?;
        self.conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = (
                SELECT session_id FROM messages WHERE id = ?2
            )",
            params![unix_timestamp(), message_id],
        )?;
        Ok(())
    }

    fn touch_session(&self, session_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET updated_at = ?1 WHERE id = ?2",
            params![unix_timestamp(), session_id],
        )?;
        Ok(())
    }

    fn rename_session(&self, session_id: i64, title: Option<&str>) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?1, updated_at = ?2 WHERE id = ?3",
            params![title, unix_timestamp(), session_id],
        )?;
        Ok(())
    }

    fn archive_session(&self, session_id: i64) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET is_archived = 1, updated_at = ?1 WHERE id = ?2",
            params![unix_timestamp(), session_id],
        )?;
        Ok(())
    }

    fn delete_session(&self, session_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

enum StreamEvent {
    Token(String),
    Done,
    Error(String),
}

struct InFlightRequest {
    receiver: mpsc::UnboundedReceiver<StreamEvent>,
    handle: JoinHandle<()>,
    assistant_message_id: i64,
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    host: &str,
    model: &str,
    db_path: &Path,
) -> Result<()> {
    let store = SessionStore::open(db_path)?;
    let session = store.load_or_create_active_session()?;
    let sessions = store.list_sessions()?;
    let selected_session_index = sessions
        .iter()
        .position(|item| item.id == session.session_id)
        .unwrap_or(0);
    let mut app = AppState::new(
        host,
        model,
        store,
        session.session_id,
        session.messages,
        sessions,
        selected_session_index,
    );

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
            let transcript_inner = body[1].inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 1,
            });
            app.transcript_view_width = transcript_inner.width.max(1) as usize;
            app.transcript_view_height = transcript_inner.height.max(1) as usize;

            let session_rows = session_rows(&app);
            let session_lines: Vec<Line<'_>> =
                session_rows.iter().map(|row| Line::from(row.as_str())).collect();
            let sessions = Paragraph::new(session_lines).block(
                Block::default().borders(Borders::ALL).title(format!(
                    "Sessions{}",
                    if app.normal_focus == NormalFocus::Sessions {
                        " [focus]"
                    } else {
                        ""
                    }
                )),
            );
            frame.render_widget(sessions, body[0]);

            let transcript_rows = transcript_rows(&app.messages);
            let total_lines = total_wrapped_lines(&transcript_rows, app.transcript_view_width);
            let max_scroll = max_scroll_for_view(total_lines, app.transcript_view_height);
            if app.transcript_follow {
                app.transcript_scroll = max_scroll;
            } else if app.transcript_scroll > max_scroll {
                app.transcript_scroll = max_scroll;
            }
            let transcript_lines: Vec<Line<'_>> = transcript_rows
                .iter()
                .map(|row| Line::from(row.as_str()))
                .collect();
            let transcript = Paragraph::new(transcript_lines)
                .block(Block::default().borders(Borders::ALL).title(format!(
                    "Transcript{}",
                    if app.normal_focus == NormalFocus::Transcript {
                        " [focus]"
                    } else {
                        ""
                    }
                )))
                .wrap(Wrap { trim: false })
                .scroll((app.transcript_scroll, 0));
            frame.render_widget(transcript, body[1]);

            let composer = Paragraph::new(app.composer_input.as_str())
                .block(Block::default().borders(Borders::ALL).title("Composer"))
                .wrap(Wrap { trim: false });
            frame.render_widget(composer, root[2]);

            let footer_help = match app.mode {
                InputMode::Normal => {
                    if app.is_busy() {
                        "Tab: focus pane | j/k: move | Enter: switch session | i: insert (disabled) | : commands | Esc: cancel stream | Ctrl+C: quit"
                    } else {
                        "Tab: focus pane | j/k: move | Enter: switch session | i: insert | : commands | Ctrl+C: quit"
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
                    KeyCode::Tab => {
                        app.pending_g = false;
                        app.normal_focus = match app.normal_focus {
                            NormalFocus::Transcript => NormalFocus::Sessions,
                            NormalFocus::Sessions => NormalFocus::Transcript,
                        };
                        app.status_message = match app.normal_focus {
                            NormalFocus::Transcript => "Focus: transcript".to_string(),
                            NormalFocus::Sessions => "Focus: sessions".to_string(),
                        };
                    }
                    KeyCode::Enter => {
                        app.pending_g = false;
                        if app.normal_focus == NormalFocus::Sessions {
                            switch_to_selected_session(&mut app);
                        }
                    }
                    KeyCode::PageDown => {
                        let page = app.transcript_view_height as u16;
                        if app.normal_focus == NormalFocus::Transcript {
                            scroll_transcript_down(&mut app, page);
                        }
                        app.pending_g = false;
                    }
                    KeyCode::PageUp => {
                        let page = app.transcript_view_height as u16;
                        if app.normal_focus == NormalFocus::Transcript {
                            scroll_transcript_up(&mut app, page);
                        }
                        app.pending_g = false;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        if app.normal_focus == NormalFocus::Sessions {
                            move_session_selection_down(&mut app, 1);
                        } else {
                            scroll_transcript_down(&mut app, 1);
                        }
                        app.pending_g = false;
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if app.normal_focus == NormalFocus::Sessions {
                            move_session_selection_up(&mut app, 1);
                        } else {
                            scroll_transcript_up(&mut app, 1);
                        }
                        app.pending_g = false;
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.normal_focus == NormalFocus::Transcript {
                            let half_page = (app.transcript_view_height / 2).max(1) as u16;
                            scroll_transcript_down(&mut app, half_page);
                        }
                        app.pending_g = false;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        if app.normal_focus == NormalFocus::Transcript {
                            let half_page = (app.transcript_view_height / 2).max(1) as u16;
                            scroll_transcript_up(&mut app, half_page);
                        }
                        app.pending_g = false;
                    }
                    KeyCode::Char('G') => {
                        if app.normal_focus == NormalFocus::Sessions {
                            move_session_selection_to_bottom(&mut app);
                        } else {
                            scroll_transcript_to_bottom(&mut app);
                        }
                        app.pending_g = false;
                    }
                    KeyCode::Char('g') => {
                        if app.pending_g {
                            if app.normal_focus == NormalFocus::Sessions {
                                move_session_selection_to_top(&mut app);
                            } else {
                                scroll_transcript_to_top(&mut app);
                            }
                            app.pending_g = false;
                        } else {
                            app.pending_g = true;
                            app.status_message = "g pressed. Press g again for top.".to_string();
                        }
                    }
                    KeyCode::Char('i') => {
                        app.pending_g = false;
                        if app.is_busy() {
                            app.status_message =
                                "Wait for streaming to finish or press Esc to cancel.".to_string();
                        } else {
                            app.mode = InputMode::Insert;
                            app.status_message = "Insert mode.".to_string();
                        }
                    }
                    KeyCode::Char(':') => {
                        app.pending_g = false;
                        app.mode = InputMode::CommandPalette;
                        app.command_input.clear();
                        app.status_message = "Command palette open.".to_string();
                    }
                    KeyCode::Esc => {
                        app.pending_g = false;
                        if app.is_busy() {
                            cancel_request(&mut app, false);
                        }
                    }
                    _ => {
                        app.pending_g = false;
                    }
                },
                InputMode::Insert => match key.code {
                    KeyCode::Esc => {
                        app.pending_g = false;
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
                        app.pending_g = false;
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
    if let Err(err) = app
        .store
        .insert_message(app.active_session_id, "user", &user_content)
    {
        app.status_message = format!("Failed to persist user message: {err}");
        return;
    }

    app.messages.push(ChatMessage {
        role: "user".to_string(),
        content: user_content,
    });
    app.composer_input.clear();

    let request_messages = app.messages.clone();
    let assistant_message_id =
        match app
            .store
            .insert_message(app.active_session_id, "assistant", "")
        {
            Ok(id) => id,
            Err(err) => {
                app.status_message = format!("Failed to create assistant message: {err}");
                return;
            }
        };
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
        assistant_message_id,
    });
    app.mode = InputMode::Normal;
    app.transcript_follow = true;
    refresh_sessions(app, Some(app.active_session_id));
    app.status_message = "Sending request to Ollama...".to_string();
}

fn process_stream_events(app: &mut AppState) {
    let mut done = false;
    let mut assistant_changed = false;
    let assistant_message_id = {
        let Some(in_flight) = app.in_flight.as_mut() else {
            return;
        };

        let assistant_message_id = in_flight.assistant_message_id;
        loop {
            match in_flight.receiver.try_recv() {
                Ok(StreamEvent::Token(content)) => {
                    if let Some(last) = app.messages.last_mut() {
                        if last.role == "assistant" {
                            last.content.push_str(&content);
                            assistant_changed = true;
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
                            assistant_changed = true;
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
        assistant_message_id
    };

    if assistant_changed {
        persist_last_assistant_message(app, assistant_message_id);
    }

    if done {
        app.in_flight = None;
        refresh_sessions(app, Some(app.active_session_id));
    }
}

fn cancel_request(app: &mut AppState, silent: bool) {
    if let Some(in_flight) = app.in_flight.take() {
        in_flight.handle.abort();
        let mut assistant_changed = false;
        if let Some(last) = app.messages.last_mut() {
            if last.role == "assistant" && last.content.trim().is_empty() {
                last.content = "[cancelled]".to_string();
                assistant_changed = true;
            }
        }
        if assistant_changed {
            persist_last_assistant_message(app, in_flight.assistant_message_id);
        }
        if !silent {
            app.status_message = "Streaming cancelled.".to_string();
        }
    }
}

fn persist_last_assistant_message(app: &mut AppState, assistant_message_id: i64) {
    let Some(last) = app.messages.last() else {
        return;
    };
    if last.role != "assistant" {
        return;
    }

    if let Err(err) = app
        .store
        .update_message_content(assistant_message_id, &last.content)
    {
        app.status_message = format!("Streaming; failed to persist assistant message: {err}");
    }
}

fn session_rows(app: &AppState) -> Vec<String> {
    if app.sessions.is_empty() {
        return vec!["No sessions".to_string()];
    }

    let mut rows = Vec::with_capacity(app.sessions.len() + 1);
    rows.push("Enter: open selected".to_string());
    for (idx, session) in app.sessions.iter().enumerate() {
        let selected_marker = if idx == app.selected_session_index {
            ">"
        } else {
            " "
        };
        let active_marker = if session.id == app.active_session_id {
            "*"
        } else {
            " "
        };
        let label = session
            .title
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| format!("Session #{}", session.id));
        rows.push(format!(
            "{selected_marker}{active_marker} {label} ({})",
            session.message_count
        ));
    }
    rows
}

fn refresh_sessions(app: &mut AppState, preferred_session_id: Option<i64>) {
    let current_selected_id = app
        .sessions
        .get(app.selected_session_index)
        .map(|session| session.id);
    let target_id = preferred_session_id
        .or(current_selected_id)
        .unwrap_or(app.active_session_id);

    match app.store.list_sessions() {
        Ok(sessions) => {
            app.sessions = sessions;
            app.selected_session_index = app
                .sessions
                .iter()
                .position(|session| session.id == target_id)
                .or_else(|| {
                    app.sessions
                        .iter()
                        .position(|session| session.id == app.active_session_id)
                })
                .unwrap_or(0);
        }
        Err(err) => {
            app.status_message = format!("Failed to refresh sessions: {err}");
        }
    }
}

fn move_session_selection_up(app: &mut AppState, lines: usize) {
    if app.sessions.is_empty() {
        return;
    }
    app.selected_session_index = app.selected_session_index.saturating_sub(lines);
}

fn move_session_selection_down(app: &mut AppState, lines: usize) {
    if app.sessions.is_empty() {
        return;
    }
    let max_index = app.sessions.len().saturating_sub(1);
    app.selected_session_index = (app.selected_session_index + lines).min(max_index);
}

fn move_session_selection_to_top(app: &mut AppState) {
    if app.sessions.is_empty() {
        return;
    }
    app.selected_session_index = 0;
    app.status_message = "Sessions: top".to_string();
}

fn move_session_selection_to_bottom(app: &mut AppState) {
    if app.sessions.is_empty() {
        return;
    }
    app.selected_session_index = app.sessions.len().saturating_sub(1);
    app.status_message = "Sessions: bottom".to_string();
}

fn switch_to_selected_session(app: &mut AppState) {
    if app.is_busy() {
        app.status_message =
            "Cannot switch sessions while request is in progress. Press Esc to cancel first."
                .to_string();
        return;
    }

    let Some(selected) = app.sessions.get(app.selected_session_index).cloned() else {
        app.status_message = "No session selected.".to_string();
        return;
    };

    if selected.id == app.active_session_id {
        app.status_message = format!("Session #{} is already active.", selected.id);
        return;
    }

    match app.store.load_messages(selected.id) {
        Ok(messages) => {
            app.active_session_id = selected.id;
            app.messages = messages;
            app.composer_input.clear();
            app.transcript_scroll = 0;
            app.transcript_follow = true;
            refresh_sessions(app, Some(selected.id));
            app.status_message = format!("Switched to session #{}.", selected.id);
        }
        Err(err) => {
            app.status_message = format!("Failed to load session #{}: {err}", selected.id);
        }
    }
}

fn activate_session_or_create(
    app: &mut AppState,
    preferred_session_id: Option<i64>,
) -> Result<i64> {
    let sessions = app.store.list_sessions()?;
    if sessions.is_empty() {
        let session_id = app.store.create_session()?;
        app.active_session_id = session_id;
        app.messages.clear();
        refresh_sessions(app, Some(session_id));
        return Ok(session_id);
    }

    let target_id = preferred_session_id
        .filter(|id| sessions.iter().any(|session| session.id == *id))
        .or_else(|| {
            if sessions
                .iter()
                .any(|session| session.id == app.active_session_id)
            {
                Some(app.active_session_id)
            } else {
                None
            }
        })
        .unwrap_or(sessions[0].id);

    app.active_session_id = target_id;
    app.messages = app.store.load_messages(target_id)?;
    app.composer_input.clear();
    app.transcript_scroll = 0;
    app.transcript_follow = true;
    refresh_sessions(app, Some(target_id));
    Ok(target_id)
}

fn transcript_rows(messages: &[ChatMessage]) -> Vec<String> {
    if messages.is_empty() {
        return vec!["No messages yet. Press i, type, then Enter.".to_string()];
    }

    let mut rows = Vec::new();
    for message in messages {
        let label = match message.role.as_str() {
            "user" => "You",
            "assistant" => "Assistant",
            _ => "System",
        };
        let content = if message.content.is_empty() {
            String::new()
        } else {
            message.content.clone()
        };
        let mut lines = content.lines();
        if let Some(first) = lines.next() {
            rows.push(format!("{label}: {first}"));
            for line in lines {
                rows.push(format!("  {line}"));
            }
        } else {
            rows.push(format!("{label}:"));
        }
        rows.push(String::new());
    }
    rows
}

fn total_wrapped_lines(rows: &[String], width: usize) -> usize {
    rows.iter().map(|row| wrapped_line_count(row, width)).sum()
}

fn wrapped_line_count(text: &str, width: usize) -> usize {
    if width == 0 {
        return 1;
    }
    let chars = text.chars().count();
    let len = chars.max(1);
    len.div_ceil(width)
}

fn max_scroll_for_view(total_lines: usize, view_height: usize) -> u16 {
    total_lines
        .saturating_sub(view_height)
        .min(u16::MAX as usize) as u16
}

fn scroll_transcript_down(app: &mut AppState, lines: u16) {
    let rows = transcript_rows(&app.messages);
    let total_lines = total_wrapped_lines(&rows, app.transcript_view_width);
    let max_scroll = max_scroll_for_view(total_lines, app.transcript_view_height);
    let next = app.transcript_scroll.saturating_add(lines);
    app.transcript_scroll = next.min(max_scroll);
    app.transcript_follow = app.transcript_scroll >= max_scroll;
}

fn scroll_transcript_up(app: &mut AppState, lines: u16) {
    app.transcript_scroll = app.transcript_scroll.saturating_sub(lines);
    app.transcript_follow = false;
}

fn scroll_transcript_to_top(app: &mut AppState) {
    app.transcript_scroll = 0;
    app.transcript_follow = false;
    app.status_message = "Transcript: top".to_string();
}

fn scroll_transcript_to_bottom(app: &mut AppState) {
    let rows = transcript_rows(&app.messages);
    let total_lines = total_wrapped_lines(&rows, app.transcript_view_width);
    let max_scroll = max_scroll_for_view(total_lines, app.transcript_view_height);
    app.transcript_scroll = max_scroll;
    app.transcript_follow = true;
    app.status_message = "Transcript: bottom".to_string();
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
            resp.text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string())
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
    let raw_command = app
        .command_input
        .trim()
        .trim_start_matches(':')
        .trim()
        .to_string();
    let mut parts = raw_command.splitn(2, char::is_whitespace);
    let command = parts.next().unwrap_or("").to_ascii_lowercase();
    let arg = parts.next().unwrap_or("").trim();

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
                match app.store.create_session() {
                    Ok(session_id) => {
                        app.active_session_id = session_id;
                        app.messages.clear();
                        app.composer_input.clear();
                        app.transcript_scroll = 0;
                        app.transcript_follow = true;
                        refresh_sessions(app, Some(session_id));
                        app.status_message = format!("Started new session #{}.", session_id);
                    }
                    Err(err) => {
                        app.status_message = format!("Failed to create session: {err}");
                    }
                }
            }
            false
        }
        "rename" => {
            if app.is_busy() {
                app.status_message =
                    "Cannot rename while request is in progress. Press Esc to cancel first."
                        .to_string();
                return false;
            }

            let title = if arg.is_empty() { None } else { Some(arg) };
            let outcome = app.store.rename_session(app.active_session_id, title);
            match outcome {
                Ok(()) => {
                    refresh_sessions(app, Some(app.active_session_id));
                    app.status_message = if let Some(value) = title {
                        format!("Renamed active session to \"{}\".", value)
                    } else {
                        "Cleared active session title.".to_string()
                    };
                }
                Err(err) => {
                    app.status_message = format!("Failed to rename session: {err}");
                }
            }
            false
        }
        "archive" => {
            if app.is_busy() {
                app.status_message =
                    "Cannot archive while request is in progress. Press Esc to cancel first."
                        .to_string();
                return false;
            }

            let archived_id = app.active_session_id;
            match app.store.archive_session(archived_id) {
                Ok(()) => match activate_session_or_create(app, None) {
                    Ok(new_active_id) => {
                        if new_active_id == archived_id {
                            app.status_message =
                                "Archived session, but it remained active unexpectedly."
                                    .to_string();
                        } else {
                            app.status_message = format!(
                                "Archived session #{}. Active session is now #{}.",
                                archived_id, new_active_id
                            );
                        }
                    }
                    Err(err) => {
                        app.status_message =
                            format!("Archived session, but failed to load replacement: {err}");
                    }
                },
                Err(err) => {
                    app.status_message = format!("Failed to archive session: {err}");
                }
            }
            false
        }
        "delete" => {
            if app.is_busy() {
                app.status_message =
                    "Cannot delete while request is in progress. Press Esc to cancel first."
                        .to_string();
                return false;
            }

            let deleted_id = app.active_session_id;
            match app.store.delete_session(deleted_id) {
                Ok(()) => match activate_session_or_create(app, None) {
                    Ok(new_active_id) => {
                        app.status_message = format!(
                            "Deleted session #{}. Active session is now #{}.",
                            deleted_id, new_active_id
                        );
                    }
                    Err(err) => {
                        app.status_message =
                            format!("Deleted session, but failed to load replacement: {err}");
                    }
                },
                Err(err) => {
                    app.status_message = format!("Failed to delete session: {err}");
                }
            }
            false
        }
        "model" => {
            app.status_message = format!("Active model: {}", app.model);
            false
        }
        "help" => {
            app.status_message =
                "Commands: :help :new :rename [title] :archive :delete :model :quit".to_string();
            false
        }
        _ => {
            app.status_message = format!("Unknown command: :{command}");
            false
        }
    }
}
