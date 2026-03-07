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
    default_model: String,
    model: String,
    composer_input: String,
    command_input: String,
    command_selected_index: usize,
    messages: Vec<ChatMessage>,
    active_session_id: i64,
    sessions: Vec<SessionSummary>,
    selected_session_index: usize,
    session_modal_offset: usize,
    store: SessionStore,
    transcript_scroll: u16,
    transcript_follow: bool,
    transcript_view_width: usize,
    transcript_view_height: usize,
    transcript_max_scroll: u16,
    pending_g: bool,
    in_flight: Option<InFlightRequest>,
    model_fetch: Option<InFlightModelFetch>,
    title_fetches: Vec<InFlightTitleFetch>,
    model_options: Vec<String>,
    model_selected_index: usize,
    model_loading: bool,
    model_error: Option<String>,
    pending_delete_session_id: Option<i64>,
    delete_return_to_session_manager: bool,
    session_rename_input: String,
    status_message: String,
}

impl AppState {
    fn new(
        host: &str,
        model: &str,
        default_model: &str,
        store: SessionStore,
        active_session_id: i64,
        messages: Vec<ChatMessage>,
        sessions: Vec<SessionSummary>,
        selected_session_index: usize,
    ) -> Self {
        Self {
            mode: InputMode::Normal,
            host: host.to_string(),
            default_model: default_model.to_string(),
            model: model.to_string(),
            composer_input: String::new(),
            command_input: String::new(),
            command_selected_index: 0,
            messages,
            active_session_id,
            sessions,
            selected_session_index,
            session_modal_offset: 0,
            store,
            transcript_scroll: 0,
            transcript_follow: true,
            transcript_view_width: 1,
            transcript_view_height: 1,
            transcript_max_scroll: 0,
            pending_g: false,
            in_flight: None,
            model_fetch: None,
            title_fetches: Vec::new(),
            model_options: Vec::new(),
            model_selected_index: 0,
            model_loading: false,
            model_error: None,
            pending_delete_session_id: None,
            delete_return_to_session_manager: false,
            session_rename_input: String::new(),
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
enum InputMode {
    Normal,
    Insert,
    CommandPalette,
    SessionManager,
    SessionRename,
    ConfirmDelete,
    ModelSelect,
    Help,
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
    model: Option<String>,
}

#[derive(Clone)]
struct SessionSummary {
    id: i64,
    title: Option<String>,
    model: Option<String>,
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
        ensure_sessions_model_column(&conn)?;

        Ok(Self { conn })
    }

    fn load_or_create_active_session(&self) -> Result<LoadedSession> {
        let session_id = self
            .conn
            .query_row(
                "SELECT id FROM sessions ORDER BY updated_at DESC, id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(self.create_session()?);

        self.load_session(session_id)
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

    fn load_session(&self, session_id: i64) -> Result<LoadedSession> {
        let model = self
            .conn
            .query_row(
                "SELECT model FROM sessions WHERE id = ?1",
                params![session_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();

        Ok(LoadedSession {
            session_id,
            messages: self.load_messages(session_id)?,
            model,
        })
    }

    fn list_sessions(&self) -> Result<Vec<SessionSummary>> {
        let mut stmt = self.conn.prepare(
            "
            SELECT s.id, s.title, s.model, COUNT(m.id) AS message_count
            FROM sessions s
            LEFT JOIN messages m ON m.session_id = s.id
            GROUP BY s.id
            ORDER BY s.id DESC
            ",
        )?;

        let rows = stmt.query_map([], |row| {
            Ok(SessionSummary {
                id: row.get(0)?,
                title: row.get(1)?,
                model: row.get(2)?,
                message_count: row.get(3)?,
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

    fn delete_session(&self, session_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM sessions WHERE id = ?1", params![session_id])?;
        Ok(())
    }

    fn set_session_model(&self, session_id: i64, model: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET model = ?1, updated_at = ?2 WHERE id = ?3",
            params![model, unix_timestamp(), session_id],
        )?;
        Ok(())
    }

    fn rename_session_if_current_title(
        &self,
        session_id: i64,
        expected_title: Option<&str>,
        new_title: Option<&str>,
    ) -> Result<bool> {
        let changed = self.conn.execute(
            "UPDATE sessions
             SET title = ?1, updated_at = ?2
             WHERE id = ?3 AND COALESCE(title, '') = COALESCE(?4, '')",
            params![new_title, unix_timestamp(), session_id, expected_title],
        )?;
        Ok(changed > 0)
    }
}

fn ensure_sessions_model_column(conn: &Connection) -> Result<()> {
    let mut stmt = conn.prepare("PRAGMA table_info(sessions)")?;
    let cols = stmt.query_map([], |row| row.get::<_, String>(1))?;
    let has_model = cols
        .collect::<rusqlite::Result<Vec<_>>>()?
        .iter()
        .any(|name| name == "model");
    if !has_model {
        conn.execute("ALTER TABLE sessions ADD COLUMN model TEXT", [])?;
    }
    Ok(())
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

enum ModelFetchEvent {
    Loaded(Vec<String>),
    Error(String),
}

enum TitleFetchEvent {
    Generated {
        session_id: i64,
        expected_title: String,
        generated_title: String,
    },
}

struct InFlightRequest {
    receiver: mpsc::UnboundedReceiver<StreamEvent>,
    handle: JoinHandle<()>,
    assistant_message_id: i64,
}

struct InFlightModelFetch {
    receiver: mpsc::UnboundedReceiver<ModelFetchEvent>,
    handle: JoinHandle<()>,
}

struct InFlightTitleFetch {
    receiver: mpsc::UnboundedReceiver<TitleFetchEvent>,
    handle: JoinHandle<()>,
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    host: &str,
    model: &str,
    db_path: &Path,
) -> Result<()> {
    let store = SessionStore::open(db_path)?;
    let session = store.load_or_create_active_session()?;
    let active_model = session.model.clone().unwrap_or_else(|| model.to_string());
    let sessions = store.list_sessions()?;
    let selected_session_index = sessions
        .iter()
        .position(|item| item.id == session.session_id)
        .unwrap_or(0);
    let mut app = AppState::new(
        host,
        &active_model,
        model,
        store,
        session.session_id,
        session.messages,
        sessions,
        selected_session_index,
    );

    loop {
        process_stream_events(&mut app);
        process_model_fetch_events(&mut app);
        process_title_fetch_events(&mut app);

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
                InputMode::SessionManager => "SESSIONS",
                InputMode::SessionRename => "RENAME",
                InputMode::ConfirmDelete => "CONFIRM",
                InputMode::ModelSelect => "MODELS",
                InputMode::Help => "HELP",
            };
            let active_title = active_session_title(&app);
            let header = Paragraph::new(format!(
                "🤖 Rosie | Mode: {mode_label}{}",
                if app.is_busy() { " | Streaming..." } else { "" }
            ))
            .block(Block::default().borders(Borders::ALL).title("Status"))
            .style(Style::default().add_modifier(Modifier::BOLD));
            frame.render_widget(header, root[0]);

            let transcript_inner = root[1].inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 1,
            });
            app.transcript_view_width = transcript_inner.width.max(1) as usize;
            app.transcript_view_height = transcript_inner.height.max(1) as usize;
            let transcript_rows = transcript_rows(&app.messages, app.is_busy());
            let transcript_lines: Vec<Line<'_>> = transcript_rows
                .iter()
                .map(|row| Line::from(row.as_str()))
                .collect();
            let transcript_title = if app.is_busy() {
                format!("Transcript | {active_title} | Streaming...")
            } else {
                format!("Transcript | {active_title}")
            };
            let transcript_base = Paragraph::new(transcript_lines)
                .block(Block::default().borders(Borders::ALL).title(transcript_title))
                .wrap(Wrap { trim: false });
            let total_lines = transcript_base.line_count(app.transcript_view_width as u16);
            let max_scroll = max_scroll_for_view(total_lines, app.transcript_view_height);
            app.transcript_max_scroll = max_scroll;
            if app.transcript_follow {
                app.transcript_scroll = max_scroll;
            } else if app.transcript_scroll > max_scroll {
                app.transcript_scroll = max_scroll;
            }
            let transcript = transcript_base.scroll((app.transcript_scroll, 0));
            frame.render_widget(transcript, root[1]);

            let composer_title = format!("Composer | Model: {}", app.model);
            let composer = Paragraph::new(app.composer_input.as_str())
                .block(Block::default().borders(Borders::ALL).title(composer_title))
                .wrap(Wrap { trim: false });
            frame.render_widget(composer, root[2]);
            if app.mode == InputMode::Insert {
                let composer_inner = root[2].inner(ratatui::layout::Margin {
                    horizontal: 1,
                    vertical: 1,
                });
                let cursor_offset = app.composer_input.chars().count() as u16;
                let cursor_x =
                    composer_inner.x + cursor_offset.min(composer_inner.width.saturating_sub(1));
                frame.set_cursor_position((cursor_x, composer_inner.y));
            }

            let footer_help = match app.mode {
                InputMode::Normal => {
                    if app.is_busy() {
                        "j/k scroll | i compose (disabled) | : cmd | ?: help | Esc cancel stream | Ctrl+C quit"
                    } else {
                        "j/k scroll | i compose | : cmd | ?: help | Ctrl+C quit"
                    }
                }
                InputMode::Insert => "Enter: send | Backspace: edit | Esc: normal",
                InputMode::CommandPalette => "Type command | j/k pick | Enter run | Esc cancel",
                InputMode::SessionManager => {
                    "j/k move | Enter switch | n new | r rename | d delete | Esc close"
                }
                InputMode::SessionRename => "Type title | Enter save | Esc cancel",
                InputMode::ConfirmDelete => "Confirm delete: Enter/y=yes, n/Esc=no",
                InputMode::ModelSelect => "Model picker: j/k move | Enter select | Esc cancel",
                InputMode::Help => "Help: Esc/q/? close",
            };
            let footer = Paragraph::new(format!("{} | {}", footer_help, app.status_message))
                .style(Style::default().add_modifier(Modifier::DIM));
            frame.render_widget(footer, root[3]);

            if app.mode == InputMode::CommandPalette {
                let popup = centered_rect(70, 12, frame.area());
                let mut rows = Vec::new();
                rows.push(format!(":{}", app.command_input));
                rows.push(String::new());
                rows.push("Commands (j/k or arrows to select, Enter to run):".to_string());
                let suggestions = palette_suggestions(&app.command_input);
                if suggestions.is_empty() {
                    rows.push("  (no matching commands)".to_string());
                } else {
                    let selected = app
                        .command_selected_index
                        .min(suggestions.len().saturating_sub(1));
                    for (idx, item) in suggestions.iter().enumerate() {
                        let marker = if idx == selected { ">" } else { " " };
                        rows.push(format!("{marker} {item}"));
                    }
                }
                let lines: Vec<Line<'_>> = rows.iter().map(|row| Line::from(row.as_str())).collect();
                let command = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Command"))
                    .alignment(Alignment::Left)
                    .wrap(Wrap { trim: false });
                frame.render_widget(Clear, popup);
                frame.render_widget(command, popup);
            } else if app.mode == InputMode::SessionManager || app.mode == InputMode::SessionRename {
                let popup = centered_rect(90, 18, frame.area());
                let rows = session_manager_rows(&mut app, popup.height as usize);
                let lines: Vec<Line<'_>> = rows.iter().map(|row| Line::from(row.as_str())).collect();
                let session_modal = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Sessions"))
                    .alignment(Alignment::Left)
                    .wrap(Wrap { trim: false });
                frame.render_widget(Clear, popup);
                frame.render_widget(session_modal, popup);

                if app.mode == InputMode::SessionRename {
                    let inner = popup.inner(ratatui::layout::Margin {
                        horizontal: 1,
                        vertical: 1,
                    });
                    let cursor_offset = app.session_rename_input.chars().count() as u16;
                    let cursor_x = inner.x + (9 + cursor_offset).min(inner.width.saturating_sub(1));
                    let cursor_y = inner.y + 2;
                    frame.set_cursor_position((cursor_x, cursor_y));
                }
            } else if app.mode == InputMode::ConfirmDelete {
                let popup = centered_rect(60, 5, frame.area());
                let target = app
                    .pending_delete_session_id
                    .map(|id| format!("#{id}"))
                    .unwrap_or_else(|| "selected session".to_string());
                let confirm = Paragraph::new(format!(
                    "Delete session {target}?\nThis cannot be undone.\n[Y/n]"
                ))
                .block(Block::default().borders(Borders::ALL).title("Confirm Delete"))
                .alignment(Alignment::Left);
                frame.render_widget(Clear, popup);
                frame.render_widget(confirm, popup);
            } else if app.mode == InputMode::ModelSelect {
                let popup = centered_rect(70, 12, frame.area());
                let mut rows = Vec::new();
                if app.model_loading {
                    rows.push("Loading models from Ollama...".to_string());
                } else if let Some(error) = app.model_error.as_deref() {
                    rows.push(format!("Failed to load models: {error}"));
                } else if app.model_options.is_empty() {
                    rows.push("No models available from /api/tags".to_string());
                } else {
                    rows.push("Select a model (Enter to apply):".to_string());
                    rows.push(String::new());
                    for (idx, model) in app.model_options.iter().enumerate() {
                        let marker = if idx == app.model_selected_index {
                            ">"
                        } else {
                            " "
                        };
                        let active = if *model == app.model { "*" } else { " " };
                        rows.push(format!("{marker}{active} {model}"));
                    }
                }

                let lines: Vec<Line<'_>> = rows.iter().map(|row| Line::from(row.as_str())).collect();
                let picker = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Models"))
                    .alignment(Alignment::Left)
                    .wrap(Wrap { trim: false });
                frame.render_widget(Clear, popup);
                frame.render_widget(picker, popup);
            } else if app.mode == InputMode::Help {
                let popup = centered_rect(78, 16, frame.area());
                let rows = help_rows();
                let lines: Vec<Line<'_>> = rows.iter().map(|row| Line::from(row.as_str())).collect();
                let help = Paragraph::new(lines)
                    .block(Block::default().borders(Borders::ALL).title("Help"))
                    .alignment(Alignment::Left)
                    .wrap(Wrap { trim: false });
                frame.render_widget(Clear, popup);
                frame.render_widget(help, popup);
            }
        })?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
                cancel_request(&mut app, true);
                cancel_model_fetch(&mut app);
                cancel_title_fetches(&mut app);
                break;
            }

            match app.mode {
                InputMode::Normal => match key.code {
                    KeyCode::PageDown => {
                        let page = app.transcript_view_height as u16;
                        scroll_transcript_down(&mut app, page);
                        app.pending_g = false;
                    }
                    KeyCode::PageUp => {
                        let page = app.transcript_view_height as u16;
                        scroll_transcript_up(&mut app, page);
                        app.pending_g = false;
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        scroll_transcript_down(&mut app, 1);
                        app.pending_g = false;
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        scroll_transcript_up(&mut app, 1);
                        app.pending_g = false;
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let half_page = (app.transcript_view_height / 2).max(1) as u16;
                        scroll_transcript_down(&mut app, half_page);
                        app.pending_g = false;
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        let half_page = (app.transcript_view_height / 2).max(1) as u16;
                        scroll_transcript_up(&mut app, half_page);
                        app.pending_g = false;
                    }
                    KeyCode::Char('G') => {
                        scroll_transcript_to_bottom(&mut app);
                        app.pending_g = false;
                    }
                    KeyCode::Char('g') => {
                        if app.pending_g {
                            scroll_transcript_to_top(&mut app);
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
                        app.command_selected_index = 0;
                        app.status_message = "Command palette open.".to_string();
                    }
                    KeyCode::Char('?') => {
                        app.pending_g = false;
                        app.mode = InputMode::Help;
                        app.status_message = "Help open.".to_string();
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
                        app.command_selected_index = 0;
                        app.status_message = "Command cancelled.".to_string();
                    }
                    KeyCode::Enter => {
                        if run_palette_selected_command(&mut app) {
                            cancel_request(&mut app, true);
                            cancel_model_fetch(&mut app);
                            cancel_title_fetches(&mut app);
                            break;
                        }
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        move_palette_selection_down(&mut app);
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        move_palette_selection_up(&mut app);
                    }
                    KeyCode::Backspace => {
                        app.command_input.pop();
                        clamp_palette_selection(&mut app);
                    }
                    KeyCode::Char(ch) => {
                        app.command_input.push(ch);
                        clamp_palette_selection(&mut app);
                    }
                    _ => {}
                },
                InputMode::SessionManager => match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        app.mode = InputMode::Normal;
                        app.status_message = "Session manager closed.".to_string();
                    }
                    KeyCode::Char('j') | KeyCode::Down => move_session_selection_down(&mut app, 1),
                    KeyCode::Char('k') | KeyCode::Up => move_session_selection_up(&mut app, 1),
                    KeyCode::Char('G') => move_session_selection_to_bottom(&mut app),
                    KeyCode::Char('g') => {
                        if app.pending_g {
                            move_session_selection_to_top(&mut app);
                            app.pending_g = false;
                        } else {
                            app.pending_g = true;
                        }
                    }
                    KeyCode::Char('n') => create_and_activate_session(&mut app),
                    KeyCode::Char('r') => open_session_rename(&mut app),
                    KeyCode::Char('d') => open_delete_confirmation_for_selected_session(&mut app),
                    KeyCode::Enter => {
                        if switch_to_selected_session(&mut app) {
                            app.mode = InputMode::Normal;
                        }
                    }
                    _ => {
                        app.pending_g = false;
                    }
                },
                InputMode::SessionRename => match key.code {
                    KeyCode::Esc => {
                        app.mode = InputMode::SessionManager;
                        app.session_rename_input.clear();
                        app.status_message = "Session rename cancelled.".to_string();
                    }
                    KeyCode::Enter => {
                        submit_session_rename(&mut app);
                    }
                    KeyCode::Backspace => {
                        app.session_rename_input.pop();
                    }
                    KeyCode::Char(ch) => {
                        app.session_rename_input.push(ch);
                    }
                    _ => {}
                },
                InputMode::ConfirmDelete => match key.code {
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => {
                        confirm_delete_session(&mut app);
                    }
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        app.pending_delete_session_id = None;
                        app.mode = if app.delete_return_to_session_manager {
                            InputMode::SessionManager
                        } else {
                            InputMode::Normal
                        };
                        app.delete_return_to_session_manager = false;
                        app.status_message = "Delete cancelled.".to_string();
                    }
                    _ => {}
                },
                InputMode::ModelSelect => match key.code {
                    KeyCode::Esc => {
                        cancel_model_fetch(&mut app);
                        app.mode = InputMode::Normal;
                        app.status_message = "Model picker cancelled.".to_string();
                    }
                    KeyCode::Char('j') | KeyCode::Down => {
                        if !app.model_options.is_empty() {
                            app.model_selected_index =
                                (app.model_selected_index + 1).min(app.model_options.len() - 1);
                        }
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        if !app.model_options.is_empty() {
                            app.model_selected_index = app.model_selected_index.saturating_sub(1);
                        }
                    }
                    KeyCode::Enter => {
                        apply_selected_model(&mut app);
                    }
                    _ => {}
                },
                InputMode::Help => match key.code {
                    KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
                        app.mode = InputMode::Normal;
                        app.status_message = "Help closed.".to_string();
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
    maybe_auto_title_session(app, &user_content);
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

fn maybe_auto_title_session(app: &mut AppState, first_user_message: &str) {
    if !app.messages.is_empty() {
        return;
    }

    let has_title = app
        .sessions
        .iter()
        .find(|session| session.id == app.active_session_id)
        .and_then(|session| session.title.as_deref())
        .map(|title| !title.trim().is_empty())
        .unwrap_or(false);
    if has_title {
        return;
    }

    let title = suggest_session_title(first_user_message);
    if let Err(err) = app
        .store
        .rename_session(app.active_session_id, Some(&title))
    {
        app.status_message = format!("Failed to auto-title session: {err}");
        return;
    }
    refresh_sessions(app, Some(app.active_session_id));
    start_generated_session_title(app, app.active_session_id, &title, first_user_message);
}

fn start_generated_session_title(
    app: &mut AppState,
    session_id: i64,
    expected_title: &str,
    first_user_message: &str,
) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let (tx, rx) = mpsc::unbounded_channel();
    let host = app.host.clone();
    let model = app.model.clone();
    let expected_title = expected_title.to_string();
    let first_user_message = first_user_message.to_string();
    let handle = runtime.spawn(async move {
        if let Ok(generated_title) =
            generate_session_title(&host, &model, &first_user_message).await
        {
            let _ = tx.send(TitleFetchEvent::Generated {
                session_id,
                expected_title,
                generated_title,
            });
        }
    });
    app.title_fetches.push(InFlightTitleFetch {
        receiver: rx,
        handle,
    });
}

fn suggest_session_title(message: &str) -> String {
    const LEADING_FILLERS: &[&str] = &[
        "please",
        "can",
        "could",
        "would",
        "you",
        "help",
        "me",
        "i",
        "need",
        "want",
        "to",
        "summarize",
        "explain",
        "tell",
        "show",
        "give",
        "create",
        "write",
        "draft",
        "generate",
    ];
    const BODY_STOPWORDS: &[&str] = &[
        "a",
        "an",
        "and",
        "for",
        "from",
        "in",
        "into",
        "of",
        "on",
        "the",
        "to",
        "with",
        "about",
        "please",
        "suggest",
        "suggestion",
        "suggestions",
        "next",
        "step",
        "steps",
    ];

    let first_line = message.lines().next().unwrap_or("").trim();
    let normalized = first_line
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch.is_whitespace() || ch == '-' || ch == '/' {
                ch
            } else {
                ' '
            }
        })
        .collect::<String>();
    let collapsed = normalized
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    if collapsed.is_empty() {
        return "New chat".to_string();
    }

    let mut words: Vec<&str> = collapsed.split_whitespace().collect();
    while let Some(word) = words.first() {
        let lower = word.to_ascii_lowercase();
        if LEADING_FILLERS.contains(&lower.as_str()) {
            words.remove(0);
        } else {
            break;
        }
    }
    if words.is_empty() {
        return "New chat".to_string();
    }

    let mut selected = Vec::new();
    let mut skipped_tail = false;
    for word in words {
        let lower = word.to_ascii_lowercase();
        if BODY_STOPWORDS.contains(&lower.as_str()) {
            continue;
        }

        let token = format_title_token(word);
        if token.is_empty() {
            continue;
        }
        selected.push(token);
        if selected.len() >= 6 {
            skipped_tail = true;
            break;
        }
    }

    if selected.is_empty() {
        selected = collapsed
            .split_whitespace()
            .take(4)
            .map(format_title_token)
            .filter(|s| !s.is_empty())
            .collect();
    }

    let mut title = selected.join(" ");
    const MAX_LEN: usize = 32;
    if title.chars().count() > MAX_LEN {
        title = title.chars().take(MAX_LEN).collect::<String>();
        while title.ends_with(char::is_whitespace) {
            title.pop();
        }
        skipped_tail = true;
    }

    if skipped_tail {
        title.push_str("...");
    }

    title
}

fn format_title_token(token: &str) -> String {
    if token.is_empty() {
        return String::new();
    }
    if token
        .chars()
        .all(|ch| ch.is_ascii_uppercase() || ch.is_ascii_digit())
    {
        return token.to_string();
    }
    if token.chars().any(|ch| ch.is_ascii_digit()) {
        return token.to_string();
    }

    let mut chars = token.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let first = first.to_ascii_uppercase();
    let rest = chars.as_str().to_ascii_lowercase();
    format!("{first}{rest}")
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

fn process_model_fetch_events(app: &mut AppState) {
    let Some(fetch) = app.model_fetch.as_mut() else {
        return;
    };

    let mut done = false;
    loop {
        match fetch.receiver.try_recv() {
            Ok(ModelFetchEvent::Loaded(models)) => {
                app.model_options = models;
                app.model_loading = false;
                app.model_error = None;
                app.model_selected_index = app
                    .model_options
                    .iter()
                    .position(|name| name == &app.model)
                    .unwrap_or(0);
                app.status_message = if app.model_options.is_empty() {
                    "Model picker loaded: no models found.".to_string()
                } else {
                    format!("Loaded {} model(s).", app.model_options.len())
                };
                done = true;
            }
            Ok(ModelFetchEvent::Error(message)) => {
                app.model_loading = false;
                app.model_options.clear();
                app.model_selected_index = 0;
                app.model_error = Some(message.clone());
                app.status_message = format!("Model discovery error: {message}");
                done = true;
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                done = true;
                break;
            }
        }
    }

    if done {
        app.model_fetch = None;
    }
}

fn process_title_fetch_events(app: &mut AppState) {
    if app.title_fetches.is_empty() {
        return;
    }

    let mut pending = Vec::with_capacity(app.title_fetches.len());
    let fetches = std::mem::take(&mut app.title_fetches);
    for mut fetch in fetches {
        let mut finished = false;
        loop {
            match fetch.receiver.try_recv() {
                Ok(TitleFetchEvent::Generated {
                    session_id,
                    expected_title,
                    generated_title,
                }) => {
                    let cleaned = normalize_generated_title(&generated_title);
                    if !cleaned.is_empty() && cleaned != expected_title {
                        match app.store.rename_session_if_current_title(
                            session_id,
                            Some(&expected_title),
                            Some(&cleaned),
                        ) {
                            Ok(true) => {
                                refresh_sessions(app, Some(app.active_session_id));
                                if session_id == app.active_session_id {
                                    app.status_message =
                                        format!("Auto-renamed session to \"{cleaned}\".");
                                }
                            }
                            Ok(false) => {}
                            Err(err) => {
                                if session_id == app.active_session_id {
                                    app.status_message =
                                        format!("Failed to apply generated title: {err}");
                                }
                            }
                        }
                    }
                    finished = true;
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }

        if finished {
            fetch.handle.abort();
        } else {
            pending.push(fetch);
        }
    }
    app.title_fetches = pending;
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

fn active_session_title(app: &AppState) -> String {
    if let Some(session) = app.sessions.iter().find(|s| s.id == app.active_session_id) {
        return session
            .title
            .clone()
            .unwrap_or_else(|| format!("Session #{}", session.id));
    }
    format!("Session #{}", app.active_session_id)
}

fn session_manager_rows(app: &mut AppState, view_height: usize) -> Vec<String> {
    let mut rows = vec!["Enter=switch n=new r=rename d=delete Esc=close".to_string()];
    if app.mode == InputMode::SessionRename {
        rows.push(String::new());
        rows.push(format!("Rename: {}", app.session_rename_input));
    }
    if app.sessions.is_empty() {
        rows.push("No sessions".to_string());
        return rows;
    }

    let reserved = rows.len();
    let rows_per_session = 1usize;
    let visible_sessions = ((view_height.saturating_sub(reserved + 2)) / rows_per_session).max(1);
    adjust_session_modal_offset(app, visible_sessions);
    let start = app
        .session_modal_offset
        .min(app.sessions.len().saturating_sub(1));
    let end = (start + visible_sessions).min(app.sessions.len());
    for (idx, session) in app.sessions[start..end].iter().enumerate() {
        let absolute_idx = start + idx;
        let selected_marker = if absolute_idx == app.selected_session_index {
            ">"
        } else {
            " "
        };
        let active_marker = if session.id == app.active_session_id {
            "*"
        } else {
            " "
        };
        let title = session
            .title
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| format!("Session #{}", session.id));
        let model = session
            .model
            .as_deref()
            .unwrap_or(app.default_model.as_str());
        rows.push(format!(
            "{selected_marker}{active_marker} {title} [{model}] | {} msgs",
            session.message_count
        ));
    }
    rows
}

fn adjust_session_modal_offset(app: &mut AppState, visible_sessions: usize) {
    if app.sessions.is_empty() {
        app.session_modal_offset = 0;
        return;
    }
    let max_offset = app.sessions.len().saturating_sub(visible_sessions);
    let mut offset = app.session_modal_offset.min(max_offset);
    if app.selected_session_index < offset {
        offset = app.selected_session_index;
    } else if app.selected_session_index >= offset + visible_sessions {
        offset = app.selected_session_index + 1 - visible_sessions;
    }
    app.session_modal_offset = offset.min(max_offset);
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
            let max_offset = app.sessions.len().saturating_sub(1);
            app.session_modal_offset = app.session_modal_offset.min(max_offset);
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

fn switch_to_selected_session(app: &mut AppState) -> bool {
    if app.is_busy() {
        app.status_message =
            "Cannot switch sessions while request is in progress. Press Esc to cancel first."
                .to_string();
        return false;
    }

    let Some(selected) = app.sessions.get(app.selected_session_index).cloned() else {
        app.status_message = "No session selected.".to_string();
        return false;
    };

    if selected.id == app.active_session_id {
        app.status_message = format!("Session #{} is already active.", selected.id);
        return true;
    }

    match app.store.load_session(selected.id) {
        Ok(loaded) => {
            app.active_session_id = selected.id;
            app.messages = loaded.messages;
            app.model = loaded.model.unwrap_or_else(|| app.default_model.clone());
            app.composer_input.clear();
            app.transcript_scroll = 0;
            app.transcript_follow = true;
            refresh_sessions(app, Some(selected.id));
            app.status_message = format!("Switched to session #{}.", selected.id);
            true
        }
        Err(err) => {
            app.status_message = format!("Failed to load session #{}: {err}", selected.id);
            false
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
        app.model = app.default_model.clone();
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

    let loaded = app.store.load_session(target_id)?;
    app.active_session_id = target_id;
    app.messages = loaded.messages;
    app.model = loaded.model.unwrap_or_else(|| app.default_model.clone());
    app.composer_input.clear();
    app.transcript_scroll = 0;
    app.transcript_follow = true;
    refresh_sessions(app, Some(target_id));
    Ok(target_id)
}

fn open_delete_confirmation_for_selected_session(app: &mut AppState) {
    if app.is_busy() {
        app.status_message =
            "Cannot delete while request is in progress. Press Esc to cancel first.".to_string();
        return;
    }

    let Some(selected) = app.sessions.get(app.selected_session_index) else {
        app.status_message = "No session selected.".to_string();
        return;
    };

    app.pending_delete_session_id = Some(selected.id);
    app.delete_return_to_session_manager = app.mode == InputMode::SessionManager;
    app.mode = InputMode::ConfirmDelete;
    app.status_message = format!("Confirm delete session #{}.", selected.id);
}

fn open_session_manager(app: &mut AppState) {
    refresh_sessions(app, Some(app.active_session_id));
    app.mode = InputMode::SessionManager;
    app.session_rename_input.clear();
    app.delete_return_to_session_manager = false;
    app.status_message = "Session manager open.".to_string();
}

fn create_and_activate_session(app: &mut AppState) {
    if app.is_busy() {
        app.status_message =
            "Cannot create while request is in progress. Press Esc to cancel first.".to_string();
        return;
    }

    match app.store.create_session() {
        Ok(session_id) => {
            app.active_session_id = session_id;
            app.messages.clear();
            app.model = app.default_model.clone();
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

fn open_session_rename(app: &mut AppState) {
    if app.is_busy() {
        app.status_message =
            "Cannot rename while request is in progress. Press Esc to cancel first.".to_string();
        return;
    }
    let Some(selected) = app.sessions.get(app.selected_session_index) else {
        app.status_message = "No session selected.".to_string();
        return;
    };
    app.session_rename_input = selected.title.clone().unwrap_or_default();
    app.mode = InputMode::SessionRename;
    app.status_message = format!("Renaming session #{}.", selected.id);
}

fn submit_session_rename(app: &mut AppState) {
    let Some(selected) = app.sessions.get(app.selected_session_index).cloned() else {
        app.mode = InputMode::SessionManager;
        app.status_message = "No session selected.".to_string();
        return;
    };
    let trimmed = app.session_rename_input.trim().to_string();
    let title = if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.as_str())
    };
    match app.store.rename_session(selected.id, title) {
        Ok(()) => {
            refresh_sessions(app, Some(selected.id));
            app.mode = InputMode::SessionManager;
            app.session_rename_input.clear();
            app.status_message = format!("Renamed session #{}.", selected.id);
        }
        Err(err) => {
            app.status_message = format!("Failed to rename session: {err}");
        }
    }
}

fn confirm_delete_session(app: &mut AppState) {
    let Some(deleted_id) = app.pending_delete_session_id.take() else {
        app.mode = if app.delete_return_to_session_manager {
            InputMode::SessionManager
        } else {
            InputMode::Normal
        };
        app.delete_return_to_session_manager = false;
        app.status_message = "No session selected for deletion.".to_string();
        return;
    };

    let was_active = deleted_id == app.active_session_id;
    match app.store.delete_session(deleted_id) {
        Ok(()) => {
            let preferred = if was_active {
                None
            } else {
                Some(app.active_session_id)
            };
            match activate_session_or_create(app, preferred) {
                Ok(new_active_id) => {
                    app.mode = if app.delete_return_to_session_manager {
                        InputMode::SessionManager
                    } else {
                        InputMode::Normal
                    };
                    app.delete_return_to_session_manager = false;
                    if was_active {
                        app.status_message = format!(
                            "Deleted session #{}. Active session is now #{}.",
                            deleted_id, new_active_id
                        );
                    } else {
                        app.status_message = format!("Deleted session #{}.", deleted_id);
                    }
                }
                Err(err) => {
                    app.mode = if app.delete_return_to_session_manager {
                        InputMode::SessionManager
                    } else {
                        InputMode::Normal
                    };
                    app.delete_return_to_session_manager = false;
                    app.status_message =
                        format!("Deleted session, but failed to load replacement: {err}");
                }
            }
        }
        Err(err) => {
            app.mode = if app.delete_return_to_session_manager {
                InputMode::SessionManager
            } else {
                InputMode::Normal
            };
            app.delete_return_to_session_manager = false;
            app.status_message = format!("Failed to delete session: {err}");
        }
    }
}

fn open_model_picker(app: &mut AppState) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        app.mode = InputMode::Normal;
        app.status_message = "Model picker unavailable outside async runtime.".to_string();
        return;
    };
    cancel_model_fetch(app);
    app.mode = InputMode::ModelSelect;
    app.model_options.clear();
    app.model_selected_index = 0;
    app.model_loading = true;
    app.model_error = None;
    app.status_message = "Loading models from Ollama...".to_string();

    let (tx, rx) = mpsc::unbounded_channel();
    let host = app.host.clone();
    let handle = runtime.spawn(async move {
        match fetch_ollama_models(&host).await {
            Ok(models) => {
                let _ = tx.send(ModelFetchEvent::Loaded(models));
            }
            Err(err) => {
                let _ = tx.send(ModelFetchEvent::Error(err.to_string()));
            }
        }
    });

    app.model_fetch = Some(InFlightModelFetch {
        receiver: rx,
        handle,
    });
}

fn cancel_model_fetch(app: &mut AppState) {
    if let Some(fetch) = app.model_fetch.take() {
        fetch.handle.abort();
    }
    app.model_loading = false;
}

fn cancel_title_fetches(app: &mut AppState) {
    for fetch in app.title_fetches.drain(..) {
        fetch.handle.abort();
    }
}

fn apply_selected_model(app: &mut AppState) {
    if app.model_loading {
        app.status_message = "Still loading models...".to_string();
        return;
    }

    let Some(selected) = app.model_options.get(app.model_selected_index).cloned() else {
        app.status_message = "No model selected.".to_string();
        return;
    };

    match app
        .store
        .set_session_model(app.active_session_id, &selected)
    {
        Ok(()) => {
            app.model = selected.clone();
            refresh_sessions(app, Some(app.active_session_id));
            app.mode = InputMode::Normal;
            app.status_message = format!("Session model set to {selected}");
        }
        Err(err) => {
            app.status_message = format!("Failed to persist session model: {err}");
        }
    }
}

fn help_rows() -> Vec<String> {
    vec![
        "Navigation".to_string(),
        "  j/k or arrows: scroll transcript".to_string(),
        "  PageUp/PageDown: full-page scroll".to_string(),
        "  Ctrl+u / Ctrl+d: half-page scroll".to_string(),
        "  gg / G: top / bottom in transcript".to_string(),
        "".to_string(),
        "Composer".to_string(),
        "  i: enter insert mode, Enter: send, Esc: return to normal".to_string(),
        "".to_string(),
        "Commands".to_string(),
        "  :session  :models  :help  :quit".to_string(),
        "  ':' palette shows a picklist; use j/k (or arrows) + Enter".to_string(),
        "  :help or ? opens this panel".to_string(),
        "".to_string(),
        "Session manager (:session)".to_string(),
        "  j/k move, Enter switch, n new, r rename, d delete, Esc close".to_string(),
        "".to_string(),
        "Global: Ctrl+C quits; Esc cancels in-flight stream in normal mode".to_string(),
    ]
}

const PALETTE_COMMANDS: &[&str] = &["help", "session", "models", "quit"];

fn palette_suggestions(input: &str) -> Vec<&'static str> {
    let trimmed = input
        .trim()
        .trim_start_matches(':')
        .trim()
        .to_ascii_lowercase();
    let stem = trimmed.split_whitespace().next().unwrap_or("");
    if stem.is_empty() {
        return PALETTE_COMMANDS.to_vec();
    }

    PALETTE_COMMANDS
        .iter()
        .copied()
        .filter(|command| command.starts_with(stem))
        .collect()
}

fn clamp_palette_selection(app: &mut AppState) {
    let suggestions = palette_suggestions(&app.command_input);
    if suggestions.is_empty() {
        app.command_selected_index = 0;
    } else if app.command_selected_index >= suggestions.len() {
        app.command_selected_index = suggestions.len() - 1;
    }
}

fn move_palette_selection_up(app: &mut AppState) {
    clamp_palette_selection(app);
    app.command_selected_index = app.command_selected_index.saturating_sub(1);
}

fn move_palette_selection_down(app: &mut AppState) {
    clamp_palette_selection(app);
    let suggestions = palette_suggestions(&app.command_input);
    if suggestions.is_empty() {
        return;
    }
    app.command_selected_index = (app.command_selected_index + 1).min(suggestions.len() - 1);
}

fn run_palette_selected_command(app: &mut AppState) -> bool {
    let trimmed = app.command_input.trim().trim_start_matches(':').trim();
    let suggestions = palette_suggestions(&app.command_input);
    if suggestions.is_empty() {
        app.status_message = "No matching command.".to_string();
        return false;
    }
    let selected = suggestions[app.command_selected_index.min(suggestions.len() - 1)];

    if trimmed.is_empty() {
        app.command_input = selected.to_string();
        return run_palette_command(app);
    }

    let has_args = trimmed.contains(char::is_whitespace);
    let stem = trimmed
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_exact = PALETTE_COMMANDS.iter().any(|item| *item == stem);

    if !has_args && !is_exact {
        app.command_input = selected.to_string();
    }

    run_palette_command(app)
}

fn transcript_rows(messages: &[ChatMessage], is_busy: bool) -> Vec<String> {
    if messages.is_empty() {
        return vec!["No messages yet. Press i, type, then Enter.".to_string()];
    }

    let mut rows = Vec::new();
    for (idx, message) in messages.iter().enumerate() {
        let label = match message.role.as_str() {
            "user" => "You",
            "assistant" => "Assistant",
            _ => "System",
        };
        let is_pending_assistant = is_busy
            && message.role == "assistant"
            && idx + 1 == messages.len()
            && message.content.trim().is_empty();
        let content = if is_pending_assistant {
            "[waiting for model response...]".to_string()
        } else if message.content.is_empty() {
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

fn max_scroll_for_view(total_lines: usize, view_height: usize) -> u16 {
    total_lines
        .saturating_sub(view_height)
        .min(u16::MAX as usize) as u16
}

fn scroll_transcript_down(app: &mut AppState, lines: u16) {
    let next = app.transcript_scroll.saturating_add(lines);
    app.transcript_scroll = next.min(app.transcript_max_scroll);
    app.transcript_follow = app.transcript_scroll >= app.transcript_max_scroll;
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
    app.transcript_scroll = app.transcript_max_scroll;
    app.transcript_follow = true;
    app.status_message = "Transcript: bottom".to_string();
}

#[derive(Serialize)]
struct OllamaChatRequest {
    model: String,
    messages: Vec<ChatMessage>,
    stream: bool,
}

#[derive(Serialize)]
struct OllamaGenerateRequest {
    model: String,
    prompt: String,
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

#[derive(Deserialize)]
struct OllamaGenerateResponse {
    response: Option<String>,
    error: Option<String>,
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

async fn fetch_ollama_models(host: &str) -> Result<Vec<String>> {
    #[derive(Deserialize)]
    struct OllamaModel {
        name: String,
    }

    #[derive(Deserialize)]
    struct OllamaTagsResponse {
        models: Vec<OllamaModel>,
    }

    let url = format!("{}/api/tags", host.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| anyhow!("HTTP send error for model discovery: {e}"))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "Model discovery returned {}: {}",
            resp.status(),
            resp.text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string())
        ));
    }

    let parsed: OllamaTagsResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse model discovery JSON: {e}"))?;

    Ok(parsed.models.into_iter().map(|model| model.name).collect())
}

async fn generate_session_title(
    host: &str,
    model: &str,
    first_user_message: &str,
) -> Result<String> {
    let prompt = format!(
        "Create a concise session title from this user message.\nRules:\n- 3 to 6 words\n- Title Case\n- No quotes\n- No ending punctuation\n- Keep it specific\nReturn only the title.\n\nUser message:\n{}",
        first_user_message.trim()
    );
    let request = OllamaGenerateRequest {
        model: model.to_string(),
        prompt,
        stream: false,
    };
    let url = format!("{}/api/generate", host.trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(url)
        .json(&request)
        .send()
        .await
        .map_err(|e| anyhow!("HTTP send error for title generation: {e}"))?;
    if !resp.status().is_success() {
        return Err(anyhow!(
            "Title generation returned {}: {}",
            resp.status(),
            resp.text()
                .await
                .unwrap_or_else(|_| "<no body>".to_string())
        ));
    }
    let parsed: OllamaGenerateResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse title generation JSON: {e}"))?;
    if let Some(error) = parsed.error
        && !error.trim().is_empty()
    {
        return Err(anyhow!("Title generation error: {error}"));
    }
    Ok(parsed.response.unwrap_or_default())
}

fn normalize_generated_title(raw: &str) -> String {
    let mut value = raw.lines().next().unwrap_or("").trim().to_string();
    if value.starts_with('"') && value.ends_with('"') && value.len() >= 2 {
        value = value[1..value.len() - 1].trim().to_string();
    }
    if value.starts_with('\'') && value.ends_with('\'') && value.len() >= 2 {
        value = value[1..value.len() - 1].trim().to_string();
    }
    value = value
        .trim_matches(|ch: char| ch.is_ascii_punctuation())
        .trim()
        .to_string();
    if value.is_empty() {
        return String::new();
    }

    const MAX_LEN: usize = 40;
    let mut out = value.chars().take(MAX_LEN).collect::<String>();
    while out.ends_with(char::is_whitespace) {
        out.pop();
    }
    out
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
    let _arg = parts.next().unwrap_or("").trim();

    app.command_input.clear();
    app.mode = InputMode::Normal;

    match command.as_str() {
        "" => {
            app.status_message = "No command entered.".to_string();
            false
        }
        "quit" | "q" => true,
        "session" => {
            open_session_manager(app);
            false
        }
        "models" => {
            open_model_picker(app);
            false
        }
        "help" => {
            app.mode = InputMode::Help;
            app.status_message = "Help open.".to_string();
            false
        }
        _ => {
            app.status_message = format!("Unknown command: :{command}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_db_path(test_name: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!("rosie-{test_name}-{ts}-{n}.sqlite3"))
    }

    fn build_app_from_store(
        store: SessionStore,
        active_session_id: i64,
        messages: Vec<ChatMessage>,
    ) -> AppState {
        let sessions = store.list_sessions().expect("list sessions");
        let selected_session_index = sessions
            .iter()
            .position(|session| session.id == active_session_id)
            .unwrap_or(0);
        AppState::new(
            "http://localhost:11434",
            "test-model",
            "test-model",
            store,
            active_session_id,
            messages,
            sessions,
            selected_session_index,
        )
    }

    #[test]
    fn persists_messages_across_restart_and_deletes_selected_session_with_confirmation() {
        let db_path = temp_db_path("persist-switch-delete");
        let (session_one, session_two);

        {
            let store = SessionStore::open(&db_path).expect("open store");
            session_one = store
                .load_or_create_active_session()
                .expect("load or create")
                .session_id;
            store
                .insert_message(session_one, "user", "hello from session one")
                .expect("insert user");
            store
                .insert_message(session_one, "assistant", "response one")
                .expect("insert assistant");

            session_two = store.create_session().expect("create second session");
            store
                .insert_message(session_two, "user", "hello from session two")
                .expect("insert second session message");
        }

        {
            let store = SessionStore::open(&db_path).expect("reopen store");
            let loaded = store.load_or_create_active_session().expect("load session");
            assert_eq!(loaded.session_id, session_two);

            let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);

            app.selected_session_index = app
                .sessions
                .iter()
                .position(|session| session.id == session_one)
                .expect("session one in list");
            switch_to_selected_session(&mut app);

            assert_eq!(app.active_session_id, session_one);
            assert!(
                app.messages
                    .iter()
                    .any(|m| m.content.contains("hello from session one"))
            );

            app.selected_session_index = app
                .sessions
                .iter()
                .position(|session| session.id == session_two)
                .expect("session two in list");
            open_delete_confirmation_for_selected_session(&mut app);
            assert!(matches!(app.mode, InputMode::ConfirmDelete));
            assert_eq!(app.pending_delete_session_id, Some(session_two));

            confirm_delete_session(&mut app);

            assert!(matches!(app.mode, InputMode::Normal));
            assert_eq!(app.active_session_id, session_one);
            assert!(app.sessions.iter().all(|session| session.id != session_two));
        }

        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn session_command_opens_manager_and_help_works() {
        let db_path = temp_db_path("palette-delete");

        {
            let store = SessionStore::open(&db_path).expect("open store");
            let first = store
                .load_or_create_active_session()
                .expect("load")
                .session_id;
            let second = store.create_session().expect("create second session");
            store
                .insert_message(first, "user", "first")
                .expect("insert first");
            store
                .insert_message(second, "user", "second")
                .expect("insert second");
        }

        {
            let store = SessionStore::open(&db_path).expect("reopen store");
            let loaded = store.load_or_create_active_session().expect("load active");
            let original_active = loaded.session_id;
            let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);

            app.command_input = ":session".to_string();
            assert!(!run_palette_command(&mut app));
            assert!(matches!(app.mode, InputMode::SessionManager));

            open_delete_confirmation_for_selected_session(&mut app);
            assert!(matches!(app.mode, InputMode::ConfirmDelete));
            assert_eq!(app.pending_delete_session_id, Some(original_active));

            confirm_delete_session(&mut app);
            assert!(matches!(app.mode, InputMode::SessionManager));
            assert_ne!(app.active_session_id, original_active);
            assert!(
                app.sessions
                    .iter()
                    .all(|session| session.id != original_active)
            );

            app.command_input = ":help".to_string();
            assert!(!run_palette_command(&mut app));
            assert!(matches!(app.mode, InputMode::Help));
            assert_eq!(app.status_message, "Help open.");
        }

        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn command_palette_picklist_behaves_as_expected() {
        let db_path = temp_db_path("palette-picklist");

        {
            let store = SessionStore::open(&db_path).expect("open store");
            let loaded = store.load_or_create_active_session().expect("load");
            let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
            app.mode = InputMode::CommandPalette;
            app.command_selected_index = 1; // session
            assert!(!run_palette_selected_command(&mut app));
            assert!(matches!(app.mode, InputMode::SessionManager));

            app.mode = InputMode::CommandPalette;
            app.command_input = "he".to_string();
            app.command_selected_index = 0;
            assert!(!run_palette_selected_command(&mut app));
            assert!(matches!(app.mode, InputMode::Help));
        }

        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn session_model_is_persisted_and_restored_on_switch() {
        let db_path = temp_db_path("session-model-switch");
        let (session_one, session_two);

        {
            let store = SessionStore::open(&db_path).expect("open store");
            session_one = store
                .load_or_create_active_session()
                .expect("load")
                .session_id;
            session_two = store.create_session().expect("create");
            store
                .set_session_model(session_one, "qwen2.5-coder")
                .expect("set model one");
            store
                .set_session_model(session_two, "llama3.2")
                .expect("set model two");
        }

        {
            let store = SessionStore::open(&db_path).expect("reopen");
            let loaded = store.load_or_create_active_session().expect("load active");
            let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);

            app.selected_session_index = app
                .sessions
                .iter()
                .position(|session| session.id == session_one)
                .expect("find session one");
            switch_to_selected_session(&mut app);
            assert_eq!(app.model, "qwen2.5-coder");

            app.selected_session_index = app
                .sessions
                .iter()
                .position(|session| session.id == session_two)
                .expect("find session two");
            switch_to_selected_session(&mut app);
            assert_eq!(app.model, "llama3.2");
        }

        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn sessions_list_is_ordered_by_id_descending() {
        let db_path = temp_db_path("session-order");

        {
            let store = SessionStore::open(&db_path).expect("open");
            let first = store
                .load_or_create_active_session()
                .expect("load")
                .session_id;
            let second = store.create_session().expect("second");
            let third = store.create_session().expect("third");
            let list = store.list_sessions().expect("list");
            let ids: Vec<i64> = list.into_iter().map(|session| session.id).collect();
            assert_eq!(ids, vec![third, second, first]);
        }

        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn auto_titles_new_session_from_first_message() {
        let db_path = temp_db_path("auto-title");

        {
            let store = SessionStore::open(&db_path).expect("open");
            let loaded = store.load_or_create_active_session().expect("load");
            let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
            maybe_auto_title_session(
                &mut app,
                "Summarize quarterly revenue trends for 2025 and suggest next steps",
            );

            let title = app
                .sessions
                .iter()
                .find(|session| session.id == app.active_session_id)
                .and_then(|session| session.title.clone())
                .expect("auto title should exist");
            assert_eq!(title, "Quarterly Revenue Trends 2025");
        }

        let _ = fs::remove_file(&db_path);
    }

    #[test]
    fn suggest_session_title_is_concise() {
        let title = suggest_session_title(
            "This is a very long request that should become a concise title for a session with truncation applied",
        );
        assert!(title.len() <= 35);
        assert!(title.ends_with("..."));
    }

    #[test]
    fn generated_title_is_normalized() {
        let title = normalize_generated_title(" \"release checklist for mvp.\" \nextra");
        assert_eq!(title, "release checklist for mvp");
    }

    #[test]
    fn conditional_title_update_respects_expected_value() {
        let db_path = temp_db_path("conditional-title-update");

        {
            let store = SessionStore::open(&db_path).expect("open");
            let session_id = store
                .load_or_create_active_session()
                .expect("load")
                .session_id;

            store
                .rename_session(session_id, Some("Initial"))
                .expect("seed title");
            let changed = store
                .rename_session_if_current_title(
                    session_id,
                    Some("Initial"),
                    Some("Generated Title"),
                )
                .expect("conditional rename");
            assert!(changed);

            store
                .rename_session(session_id, Some("Manual Override"))
                .expect("manual rename");
            let changed = store
                .rename_session_if_current_title(
                    session_id,
                    Some("Generated Title"),
                    Some("Should Not Apply"),
                )
                .expect("conditional rename no-op");
            assert!(!changed);

            let title = store
                .list_sessions()
                .expect("list")
                .into_iter()
                .find(|session| session.id == session_id)
                .and_then(|session| session.title)
                .expect("title");
            assert_eq!(title, "Manual Override");
        }

        let _ = fs::remove_file(&db_path);
    }
}
