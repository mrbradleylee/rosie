use crate::config::{ProviderConfig, StoredConfig};
use crate::provider::{
    ChatRequest as ProviderChatRequest, Message as ProviderMessage, ProviderEvent, ProviderRouter,
    Role as ProviderRole,
};
use crate::theme::{ThemePalette, config_dir_from_env, discover_config_theme_names, resolve_theme};
use anyhow::{Result, anyhow};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use std::fs;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tree_sitter::Language;
use tree_sitter_highlight::{Highlight, HighlightConfiguration, HighlightEvent, Highlighter};

pub fn run(
    config: StoredConfig,
    model: &str,
    theme_key: &str,
    theme: ThemePalette,
    db_path: &Path,
) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, config, model, theme_key, theme, db_path);
    let cleanup_result = cleanup_terminal(&mut terminal);

    result.and(cleanup_result)
}

struct AppState {
    mode: InputMode,
    config: StoredConfig,
    provider_name: String,
    host: String,
    default_model: String,
    model: String,
    composer_input: String,
    command_input: String,
    command_selected_index: usize,
    messages: Vec<ChatMessage>,
    active_session_id: i64,
    sessions: Vec<SessionSummary>,
    sessions_dirty: bool,
    selected_session_index: usize,
    session_modal_offset: usize,
    store: SessionStore,
    transcript_scroll: u16,
    transcript_follow: bool,
    transcript_view_width: usize,
    transcript_view_height: usize,
    transcript_max_scroll: u16,
    transcript_assistant_markers: Vec<u16>,
    transcript_render_cache: HashMap<usize, CachedAssistantRender>,
    pending_g: bool,
    in_flight: Option<InFlightRequest>,
    model_fetch: Option<InFlightModelFetch>,
    title_fetches: Vec<InFlightTitleFetch>,
    model_options: Vec<String>,
    model_selected_index: usize,
    model_loading: bool,
    model_error: Option<String>,
    model_cache: Vec<String>,
    model_cache_host: Option<String>,
    model_cache_fetched_at: Option<i64>,
    theme_options: Vec<String>,
    theme_selected_index: usize,
    pending_delete_session_id: Option<i64>,
    delete_return_to_session_manager: bool,
    session_rename_input: String,
    status_message: String,
    theme_key: String,
    theme: ThemePalette,
}

struct AppStateInit {
    config: StoredConfig,
    provider_name: String,
    host: String,
    model: String,
    default_model: String,
    theme_key: String,
    theme: ThemePalette,
    store: SessionStore,
    active_session_id: i64,
    messages: Vec<ChatMessage>,
    sessions: Vec<SessionSummary>,
    selected_session_index: usize,
}

impl AppState {
    fn new(init: AppStateInit) -> Self {
        Self {
            mode: InputMode::Normal,
            config: init.config,
            provider_name: init.provider_name,
            host: init.host,
            default_model: init.default_model,
            model: init.model,
            composer_input: String::new(),
            command_input: String::new(),
            command_selected_index: 0,
            messages: init.messages,
            active_session_id: init.active_session_id,
            sessions: init.sessions,
            sessions_dirty: false,
            selected_session_index: init.selected_session_index,
            session_modal_offset: 0,
            store: init.store,
            transcript_scroll: 0,
            transcript_follow: true,
            transcript_view_width: 1,
            transcript_view_height: 1,
            transcript_max_scroll: 0,
            transcript_assistant_markers: Vec::new(),
            transcript_render_cache: HashMap::new(),
            pending_g: false,
            in_flight: None,
            model_fetch: None,
            title_fetches: Vec::new(),
            model_options: Vec::new(),
            model_selected_index: 0,
            model_loading: false,
            model_error: None,
            model_cache: Vec::new(),
            model_cache_host: None,
            model_cache_fetched_at: None,
            theme_options: Vec::new(),
            theme_selected_index: 0,
            pending_delete_session_id: None,
            delete_return_to_session_manager: false,
            session_rename_input: String::new(),
            status_message: format!(
                "Loaded session #{}. Press i to enter Insert mode.",
                init.active_session_id
            ),
            theme_key: init.theme_key,
            theme: init.theme,
        }
    }

    fn is_busy(&self) -> bool {
        self.in_flight.is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum InputMode {
    Landing,
    Normal,
    Insert,
    CommandPalette,
    SessionManager,
    SessionRename,
    ConfirmDelete,
    ModelSelect,
    ThemeSelect,
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
                model TEXT
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

            CREATE TABLE IF NOT EXISTS app_state (
                key TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            ",
        )?;

        Ok(Self { conn })
    }

    #[cfg(test)]
    fn load_or_create_active_session(&self) -> Result<LoadedSession> {
        if let Some(session) = self.load_active_session_if_any()? {
            return Ok(session);
        }
        let session_id = self.create_session()?;
        self.set_last_active_session_id(session_id)?;
        self.load_session(session_id)
    }

    fn load_active_session_if_any(&self) -> Result<Option<LoadedSession>> {
        if let Some(last_active_id) = self.get_last_active_session_id()?
            && self.session_exists(last_active_id)?
        {
            return self.load_session(last_active_id).map(Some);
        }

        let Some(session_id) = self.latest_session_id()? else {
            return Ok(None);
        };
        self.set_last_active_session_id(session_id)?;
        self.load_session(session_id).map(Some)
    }

    fn latest_session_id(&self) -> Result<Option<i64>> {
        self.conn
            .query_row(
                "SELECT id FROM sessions ORDER BY updated_at DESC, id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(Into::into)
    }

    fn session_exists(&self, session_id: i64) -> Result<bool> {
        let exists = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sessions WHERE id = ?1)",
            params![session_id],
            |row| row.get::<_, i64>(0),
        )?;
        Ok(exists != 0)
    }

    fn get_last_active_session_id(&self) -> Result<Option<i64>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM app_state WHERE key = 'last_active_session_id'",
                [],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(value.and_then(|raw| raw.parse::<i64>().ok()))
    }

    fn set_last_active_session_id(&self, session_id: i64) -> Result<()> {
        self.conn.execute(
            "INSERT INTO app_state (key, value) VALUES ('last_active_session_id', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![session_id.to_string()],
        )?;
        Ok(())
    }

    fn create_session(&self) -> Result<i64> {
        let now = unix_timestamp();
        self.conn.execute(
            "INSERT INTO sessions (created_at, updated_at, title, model) VALUES (?1, ?2, NULL, NULL)",
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
    config: StoredConfig,
    model: &str,
    theme_key: &str,
    theme: ThemePalette,
    db_path: &Path,
) -> Result<()> {
    let store = SessionStore::open(db_path)?;
    let sessions = store.list_sessions()?;
    let startup_session = store.load_active_session_if_any()?;
    let (active_session_id, messages, active_model) = if let Some(session) = startup_session {
        (
            session.session_id,
            session.messages,
            session.model.unwrap_or_else(|| model.to_string()),
        )
    } else {
        (0, Vec::new(), model.to_string())
    };
    let selected_session_index = sessions
        .iter()
        .position(|item| item.id == active_session_id)
        .unwrap_or(0);
    let (provider_name, _provider) = config.active_provider_entry()?;
    let provider_name = provider_name.to_string();
    let cache_key = provider_cache_key(&config)?;
    let mut app = AppState::new(AppStateInit {
        config,
        provider_name,
        host: cache_key,
        model: active_model,
        default_model: model.to_string(),
        theme_key: theme_key.to_string(),
        theme,
        store,
        active_session_id,
        messages,
        sessions,
        selected_session_index,
    });
    app.mode = InputMode::Landing;
    app.status_message =
        "Welcome to Rosie. Type a message and press Enter to start, or Ctrl+P for commands."
            .to_string();

    loop {
        process_stream_events(&mut app);
        process_model_fetch_events(&mut app);
        process_title_fetch_events(&mut app);

        terminal.draw(|frame| draw_frame(frame, &mut app))?;

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
            && handle_key_event(&mut app, key)
        {
            break;
        }
    }

    Ok(())
}

fn handle_key_event(app: &mut AppState, key: KeyEvent) -> bool {
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        cancel_request(app, true);
        cancel_model_fetch(app);
        cancel_title_fetches(app);
        return true;
    }

    match app.mode {
        InputMode::Landing => handle_landing_input(app, key),
        InputMode::Normal => handle_normal_input(app, key),
        InputMode::Insert => handle_insert_input(app, key),
        InputMode::CommandPalette => handle_command_palette_input(app, key),
        InputMode::SessionManager => handle_session_manager_input(app, key),
        InputMode::SessionRename => handle_session_rename_input(app, key),
        InputMode::ConfirmDelete => handle_confirm_delete_input(app, key),
        InputMode::ModelSelect => handle_model_select_input(app, key),
        InputMode::ThemeSelect => handle_theme_select_input(app, key),
        InputMode::Help => handle_help_input(app, key),
    }
}

fn open_command_palette(app: &mut AppState) {
    app.mode = InputMode::CommandPalette;
    app.command_input.clear();
    app.command_selected_index = 0;
    app.status_message = "Command palette open.".to_string();
}

fn handle_landing_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            open_command_palette(app);
        }
        KeyCode::Char(':') => {
            open_command_palette(app);
        }
        KeyCode::F(1) => {
            app.mode = InputMode::Help;
            app.status_message = "Help open.".to_string();
        }
        KeyCode::Enter => {
            submit_composer_message(app);
        }
        KeyCode::Backspace => {
            app.composer_input.pop();
        }
        KeyCode::Esc => {
            app.composer_input.clear();
            app.status_message = "Landing input cleared.".to_string();
        }
        KeyCode::Char(ch) => {
            app.composer_input.push(ch);
        }
        _ => {}
    }
    false
}

fn handle_normal_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::PageDown => {
            let page = app.transcript_view_height as u16;
            scroll_transcript_down(app, page);
            app.pending_g = false;
        }
        KeyCode::PageUp => {
            let page = app.transcript_view_height as u16;
            scroll_transcript_up(app, page);
            app.pending_g = false;
        }
        KeyCode::Char('j') | KeyCode::Down => {
            scroll_transcript_down(app, 1);
            app.pending_g = false;
        }
        KeyCode::Char('k') | KeyCode::Up => {
            scroll_transcript_up(app, 1);
            app.pending_g = false;
        }
        KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let half_page = (app.transcript_view_height / 2).max(1) as u16;
            scroll_transcript_down(app, half_page);
            app.pending_g = false;
        }
        KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            let half_page = (app.transcript_view_height / 2).max(1) as u16;
            scroll_transcript_up(app, half_page);
            app.pending_g = false;
        }
        KeyCode::Char(']') => {
            jump_to_next_assistant_block(app);
            app.pending_g = false;
        }
        KeyCode::Char('[') => {
            jump_to_prev_assistant_block(app);
            app.pending_g = false;
        }
        KeyCode::Char('G') => {
            scroll_transcript_to_bottom(app);
            app.pending_g = false;
        }
        KeyCode::Char('g') => {
            if app.pending_g {
                scroll_transcript_to_top(app);
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
            open_command_palette(app);
        }
        KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
            app.pending_g = false;
            open_command_palette(app);
        }
        KeyCode::Char('?') => {
            app.pending_g = false;
            app.mode = InputMode::Help;
            app.status_message = "Help open.".to_string();
        }
        KeyCode::Esc => {
            app.pending_g = false;
            if app.is_busy() {
                cancel_request(app, false);
            }
        }
        _ => {
            app.pending_g = false;
        }
    }
    false
}

fn handle_insert_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.mode = InputMode::Normal;
            app.status_message = "Normal mode.".to_string();
        }
        KeyCode::Enter => {
            submit_composer_message(app);
        }
        KeyCode::Backspace => {
            app.composer_input.pop();
        }
        KeyCode::Char(ch) => {
            app.composer_input.push(ch);
        }
        _ => {}
    }
    false
}

fn handle_command_palette_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.mode = primary_mode_for_app(app);
            app.command_input.clear();
            app.command_selected_index = 0;
            app.status_message = "Command cancelled.".to_string();
        }
        KeyCode::Enter => {
            if run_palette_selected_command(app) {
                cancel_request(app, true);
                cancel_model_fetch(app);
                cancel_title_fetches(app);
                return true;
            }
        }
        KeyCode::Char('j') | KeyCode::Down => move_palette_selection_down(app),
        KeyCode::Char('k') | KeyCode::Up => move_palette_selection_up(app),
        KeyCode::Backspace => {
            app.command_input.pop();
            clamp_palette_selection(app);
        }
        KeyCode::Char(ch) => {
            app.command_input.push(ch);
            clamp_palette_selection(app);
        }
        _ => {}
    }
    false
}

fn handle_session_manager_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            app.mode = primary_mode_for_app(app);
            app.status_message = "Session manager closed.".to_string();
        }
        KeyCode::Char('j') | KeyCode::Down => move_session_selection_down(app, 1),
        KeyCode::Char('k') | KeyCode::Up => move_session_selection_up(app, 1),
        KeyCode::Char('G') => move_session_selection_to_bottom(app),
        KeyCode::Char('g') => {
            if app.pending_g {
                move_session_selection_to_top(app);
                app.pending_g = false;
            } else {
                app.pending_g = true;
            }
        }
        KeyCode::Char('n') => create_and_activate_session(app),
        KeyCode::Char('r') => open_session_rename(app),
        KeyCode::Char('d') => open_delete_confirmation_for_selected_session(app),
        KeyCode::Enter => {
            if switch_to_selected_session(app) {
                app.mode = InputMode::Normal;
            }
        }
        _ => {
            app.pending_g = false;
        }
    }
    false
}

fn handle_session_rename_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.mode = InputMode::SessionManager;
            app.session_rename_input.clear();
            app.status_message = "Session rename cancelled.".to_string();
        }
        KeyCode::Enter => submit_session_rename(app),
        KeyCode::Backspace => {
            app.session_rename_input.pop();
        }
        KeyCode::Char(ch) => {
            app.session_rename_input.push(ch);
        }
        _ => {}
    }
    false
}

fn handle_confirm_delete_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('Y') => confirm_delete_session(app),
        KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
            app.pending_delete_session_id = None;
            app.mode = if app.delete_return_to_session_manager {
                InputMode::SessionManager
            } else {
                primary_mode_for_app(app)
            };
            app.delete_return_to_session_manager = false;
            app.status_message = "Delete cancelled.".to_string();
        }
        _ => {}
    }
    false
}

fn handle_model_select_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            cancel_model_fetch(app);
            app.mode = primary_mode_for_app(app);
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
        KeyCode::Enter => apply_selected_model(app),
        _ => {}
    }
    false
}

fn handle_theme_select_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc => {
            app.mode = primary_mode_for_app(app);
            app.status_message = "Theme picker cancelled.".to_string();
        }
        KeyCode::Char('j') | KeyCode::Down => {
            if !app.theme_options.is_empty() {
                app.theme_selected_index =
                    (app.theme_selected_index + 1).min(app.theme_options.len() - 1);
            }
        }
        KeyCode::Char('k') | KeyCode::Up => {
            if !app.theme_options.is_empty() {
                app.theme_selected_index = app.theme_selected_index.saturating_sub(1);
            }
        }
        KeyCode::Enter => apply_selected_theme(app),
        _ => {}
    }
    false
}

fn handle_help_input(app: &mut AppState, key: KeyEvent) -> bool {
    match key.code {
        KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Enter => {
            app.mode = primary_mode_for_app(app);
            app.status_message = "Help closed.".to_string();
        }
        _ => {}
    }
    false
}

fn draw_frame(frame: &mut ratatui::Frame<'_>, app: &mut AppState) {
    let theme = app.theme;
    frame.render_widget(
        Block::default().style(Style::default().bg(theme.base)),
        frame.area(),
    );
    let root = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(1),
            Constraint::Length(3),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, app, root[0], theme);
    if app.mode == InputMode::Landing {
        render_landing(frame, app, root[1], root[2], theme);
    } else {
        render_main_panes(frame, app, root[1], root[2], theme);
    }
    render_footer(frame, app, root[3], theme);
    render_modal(frame, app, theme);
}

fn render_header(frame: &mut ratatui::Frame<'_>, app: &AppState, area: Rect, theme: ThemePalette) {
    let header_session_title = if app.active_session_id > 0 {
        active_session_title(app)
    } else {
        "New chat".to_string()
    };
    let is_streaming = app.is_busy();
    let streaming_label = streaming_status_label(is_streaming);
    let streaming_color = if is_streaming {
        theme.warn
    } else {
        theme.success
    };
    let stream_width = streaming_label.chars().count();
    let fixed_width = "Session: ".chars().count() + " | ".chars().count() + stream_width;
    let max_width = area.width.saturating_sub(2) as usize;
    let session_max = max_width.saturating_sub(fixed_width);
    let header_session = truncate_with_ellipsis(&header_session_title, session_max);
    let header_line = Line::from(vec![
        Span::styled("Session: ", Style::default().fg(theme.title_meta)),
        Span::styled(
            header_session,
            Style::default()
                .fg(theme.title_value)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" | ", Style::default().fg(theme.title_meta)),
        Span::styled(streaming_label, Style::default().fg(streaming_color)),
    ]);
    let header = Paragraph::new(header_line)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![Span::styled(
                    "🤖 Rosie",
                    Style::default()
                        .fg(theme.title_label)
                        .add_modifier(Modifier::BOLD),
                )]))
                .style(Style::default().bg(theme.surface).fg(theme.text))
                .border_style(Style::default().fg(theme.highlight_high)),
        )
        .style(
            Style::default()
                .bg(theme.surface)
                .fg(theme.text)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(header, area);
}

fn render_landing(
    frame: &mut ratatui::Frame<'_>,
    app: &mut AppState,
    transcript_area: Rect,
    composer_area: Rect,
    theme: ThemePalette,
) {
    let landing_space = Rect {
        x: transcript_area.x,
        y: transcript_area.y,
        width: transcript_area.width,
        height: transcript_area.height.saturating_add(composer_area.height),
    };
    let landing_height = 16u16.min(landing_space.height).max(8);
    let landing_rect = centered_rect(72, landing_height, landing_space);
    let landing_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(3),
        ])
        .split(landing_rect);

    let hero_lines = vec![
        Line::styled(
            "🤖 Rosie".to_string(),
            Style::default()
                .fg(theme.title_label)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "Fast local chat in your terminal.".to_string(),
            Style::default().fg(theme.text),
        ),
        Line::raw(""),
        Line::styled(
            "Quick commands".to_string(),
            Style::default()
                .fg(theme.title_value)
                .add_modifier(Modifier::BOLD),
        ),
        Line::styled(
            "  :session   open sessions list".to_string(),
            Style::default().fg(theme.text),
        ),
        Line::styled(
            "  :models    switch model".to_string(),
            Style::default().fg(theme.text),
        ),
        Line::styled(
            "  :theme     switch theme".to_string(),
            Style::default().fg(theme.text),
        ),
        Line::styled(
            "  Ctrl+P     open command palette".to_string(),
            Style::default().fg(theme.text),
        ),
    ];
    let hero = Paragraph::new(hero_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![Span::styled(
                    "Welcome",
                    Style::default()
                        .fg(theme.title_label)
                        .add_modifier(Modifier::BOLD),
                )]))
                .style(Style::default().bg(theme.highlight_low).fg(theme.text))
                .border_style(Style::default().fg(theme.border_active)),
        )
        .style(Style::default().bg(theme.highlight_low).fg(theme.text))
        .alignment(Alignment::Left)
        .wrap(Wrap { trim: false });
    frame.render_widget(hero, landing_layout[0]);

    let prompt = Paragraph::new(app.composer_input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![
                    Span::styled(
                        "Start Chat",
                        Style::default()
                            .fg(theme.title_label)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" | ", Style::default().fg(theme.title_meta)),
                    Span::styled("Enter to apply", Style::default().fg(theme.title_value_alt)),
                    Span::styled(" | ", Style::default().fg(theme.title_meta)),
                    Span::styled("Default:", Style::default().fg(theme.title_meta)),
                    Span::styled(" ", Style::default().fg(theme.title_meta)),
                    Span::styled(
                        app.default_model.clone(),
                        Style::default().fg(theme.title_value_alt),
                    ),
                ]))
                .style(Style::default().bg(theme.surface_alt).fg(theme.text))
                .border_style(Style::default().fg(theme.accent)),
        )
        .style(Style::default().bg(theme.surface_alt).fg(theme.text))
        .wrap(Wrap { trim: false });
    frame.render_widget(prompt, landing_layout[2]);
    let prompt_inner = landing_layout[2].inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    let cursor_offset = app.composer_input.chars().count() as u16;
    let cursor_x = prompt_inner.x + cursor_offset.min(prompt_inner.width.saturating_sub(1));
    frame.set_cursor_position((cursor_x, prompt_inner.y));
}

fn render_main_panes(
    frame: &mut ratatui::Frame<'_>,
    app: &mut AppState,
    transcript_area: Rect,
    composer_area: Rect,
    theme: ThemePalette,
) {
    let transcript_active = app.mode == InputMode::Normal;
    let composer_active = app.mode == InputMode::Insert;
    let transcript_inner = transcript_area.inner(ratatui::layout::Margin {
        horizontal: 1,
        vertical: 1,
    });
    app.transcript_view_width = transcript_inner.width.max(1) as usize;
    app.transcript_view_height = transcript_inner.height.max(1) as usize;
    let transcript_render = transcript_lines_cached(app, app.transcript_view_width);
    app.transcript_assistant_markers = transcript_render.assistant_markers;
    let conversation_title = conversation_header_title(app);
    let mut transcript_title_spans = vec![Span::styled(
        conversation_title,
        Style::default()
            .fg(theme.title_label)
            .add_modifier(Modifier::BOLD),
    )];
    transcript_title_spans.push(Span::styled(" | ", Style::default().fg(theme.title_meta)));
    transcript_title_spans.push(Span::styled(
        transcript_position_label(app.transcript_scroll, app.transcript_max_scroll),
        Style::default().fg(theme.title_meta),
    ));
    if !app.transcript_follow && app.transcript_scroll > 0 {
        transcript_title_spans.push(Span::styled(" | ", Style::default().fg(theme.title_meta)));
        transcript_title_spans.push(Span::styled("↑ older", Style::default().fg(theme.muted)));
    }
    if !app.transcript_follow && app.transcript_scroll < app.transcript_max_scroll {
        transcript_title_spans.push(Span::styled(" | ", Style::default().fg(theme.title_meta)));
        transcript_title_spans.push(Span::styled("↓ new", Style::default().fg(theme.success)));
    }
    let transcript_base = Paragraph::new(transcript_render.lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(transcript_title_spans))
                .style(Style::default().bg(theme.highlight_low).fg(theme.text))
                .border_style(Style::default().fg(if transcript_active {
                    theme.border_active
                } else {
                    theme.border
                })),
        )
        .style(Style::default().bg(theme.highlight_low).fg(theme.text))
        .wrap(Wrap { trim: false });
    let total_lines = transcript_base.line_count(app.transcript_view_width as u16);
    let max_scroll = max_scroll_for_view(total_lines, app.transcript_view_height);
    app.transcript_max_scroll = max_scroll;
    if app.transcript_follow || app.transcript_scroll > max_scroll {
        app.transcript_scroll = max_scroll;
    }
    let transcript = transcript_base.scroll((app.transcript_scroll, 0));
    frame.render_widget(transcript, transcript_area);

    let composer = Paragraph::new(app.composer_input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title(Line::from(vec![
                    Span::styled(
                        "Chat",
                        Style::default()
                            .fg(theme.title_label)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(" | ", Style::default().fg(theme.title_meta)),
                    Span::styled(
                        format!("[{}]", app.model),
                        Style::default()
                            .fg(theme.title_value_alt)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]))
                .style(Style::default().bg(theme.surface_alt).fg(theme.text))
                .border_style(Style::default().fg(if composer_active {
                    theme.accent
                } else {
                    theme.border
                })),
        )
        .style(Style::default().bg(theme.surface_alt).fg(theme.text))
        .wrap(Wrap { trim: false });
    frame.render_widget(composer, composer_area);
    if app.mode == InputMode::Insert {
        let composer_inner = composer_area.inner(ratatui::layout::Margin {
            horizontal: 1,
            vertical: 1,
        });
        let cursor_offset = app.composer_input.chars().count() as u16;
        let cursor_x = composer_inner.x + cursor_offset.min(composer_inner.width.saturating_sub(1));
        frame.set_cursor_position((cursor_x, composer_inner.y));
    }
}

fn mode_label(mode: InputMode) -> &'static str {
    match mode {
        InputMode::Landing => "LANDING",
        InputMode::Normal => "NORMAL",
        InputMode::Insert => "INSERT",
        InputMode::CommandPalette => "COMMAND",
        InputMode::SessionManager => "SESSIONS",
        InputMode::SessionRename => "RENAME",
        InputMode::ConfirmDelete => "CONFIRM",
        InputMode::ModelSelect => "MODELS",
        InputMode::ThemeSelect => "THEMES",
        InputMode::Help => "HELP",
    }
}

fn footer_help_for_mode(mode: InputMode, is_busy: bool) -> &'static str {
    match mode {
        InputMode::Landing => "Type message | Enter send | Ctrl+P/: cmd | ?: help",
        InputMode::Normal => {
            if is_busy {
                "[/] assistant jump | i compose (disabled) | : cmd | ?: help | Esc cancel stream"
            } else {
                "[/] assistant jump | i compose | : cmd | ?: help"
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
        InputMode::ThemeSelect => "Theme picker: j/k move | Enter select | Esc cancel",
        InputMode::Help => "Help: Esc/q/? close",
    }
}

fn render_footer(frame: &mut ratatui::Frame<'_>, app: &AppState, area: Rect, theme: ThemePalette) {
    let footer_help = footer_help_for_mode(app.mode, app.is_busy());
    let footer_width = area.width as usize;
    let compact_help = if footer_width < 80 {
        compact_footer_help(app.mode, app.is_busy())
    } else {
        footer_help.to_string()
    };
    let mode_chip = mode_label(app.mode).to_string();
    let status_text = if app.status_message.trim().is_empty() {
        "Ready.".to_string()
    } else {
        app.status_message.clone()
    };
    let mut footer_spans = vec![Span::styled(
        format!("[{mode_chip}] "),
        Style::default()
            .bg(theme.surface_alt)
            .fg(status_mode_color(app.mode, theme))
            .add_modifier(Modifier::BOLD),
    )];
    footer_spans.push(Span::styled(compact_help, Style::default().fg(theme.text)));
    footer_spans.push(Span::styled(
        " | ",
        Style::default().fg(theme.highlight_mid),
    ));
    footer_spans.push(Span::styled(status_text, Style::default().fg(theme.text)));
    let footer = Paragraph::new(Line::from(footer_spans))
        .style(Style::default().bg(theme.surface_alt).fg(theme.text));
    frame.render_widget(footer, area);
}

fn render_modal(frame: &mut ratatui::Frame<'_>, app: &mut AppState, theme: ThemePalette) {
    if app.mode == InputMode::CommandPalette {
        let popup = centered_rect(70, 12, frame.area());
        let mut rows: Vec<Line<'_>> = Vec::new();
        rows.push(Line::from(vec![
            Span::styled(":", Style::default().fg(theme.title_meta)),
            Span::styled(
                app.command_input.clone(),
                Style::default().fg(theme.title_value_alt),
            ),
        ]));
        rows.push(Line::raw(""));
        rows.push(Line::styled(
            "Commands (j/k or arrows to select, Enter to run):".to_string(),
            Style::default()
                .fg(theme.title_label)
                .add_modifier(Modifier::BOLD),
        ));
        let suggestions = palette_suggestions(&app.command_input);
        if suggestions.is_empty() {
            rows.push(Line::styled(
                "  (no matching commands)".to_string(),
                Style::default().fg(theme.muted),
            ));
        } else {
            let selected = app
                .command_selected_index
                .min(suggestions.len().saturating_sub(1));
            for (idx, item) in suggestions.iter().enumerate() {
                let marker = if idx == selected { ">" } else { " " };
                if idx == selected {
                    let selected_fg = contrast_text_for_bg(theme.success, theme);
                    rows.push(Line::from(vec![
                        Span::styled(
                            marker.to_string(),
                            Style::default()
                                .fg(theme.modal_selected_bg)
                                .bg(theme.success)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            " ".to_string(),
                            Style::default().fg(selected_fg).bg(theme.success),
                        ),
                        Span::styled(
                            item.to_string(),
                            Style::default()
                                .fg(selected_fg)
                                .bg(theme.success)
                                .add_modifier(Modifier::BOLD),
                        ),
                    ]));
                } else {
                    let item_color = if *item == "quit" {
                        theme.warn
                    } else {
                        theme.text
                    };
                    rows.push(Line::from(vec![
                        Span::styled(marker.to_string(), Style::default().fg(theme.muted)),
                        Span::raw(" "),
                        Span::styled(item.to_string(), Style::default().fg(item_color)),
                    ]));
                }
            }
        }
        let command = Paragraph::new(rows)
            .block(modal_block(theme, "Command"))
            .style(Style::default().bg(theme.modal_bg).fg(theme.text))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        frame.render_widget(Clear, popup);
        frame.render_widget(command, popup);
    } else if app.mode == InputMode::SessionManager || app.mode == InputMode::SessionRename {
        let popup = centered_rect(90, 18, frame.area());
        let rows = session_manager_rows(app, popup.height as usize);
        let lines = modal_lines_from_rows(&rows, theme);
        let session_modal = Paragraph::new(lines)
            .block(modal_block(theme, "Sessions"))
            .style(Style::default().bg(theme.modal_bg).fg(theme.text))
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
        let confirm_lines = vec![
            Line::styled(
                format!("Delete session {target}?"),
                Style::default().fg(theme.warn).add_modifier(Modifier::BOLD),
            ),
            Line::styled(
                "This cannot be undone.".to_string(),
                Style::default().fg(theme.error),
            ),
            Line::styled(
                "[Y/n]".to_string(),
                Style::default()
                    .fg(theme.title_label)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        let confirm = Paragraph::new(confirm_lines)
            .block(modal_block(theme, "Confirm Delete"))
            .style(Style::default().bg(theme.modal_bg).fg(theme.text))
            .alignment(Alignment::Left);
        frame.render_widget(Clear, popup);
        frame.render_widget(confirm, popup);
    } else if app.mode == InputMode::ModelSelect {
        let popup = centered_rect(70, 12, frame.area());
        let mut rows = Vec::new();
        if app.model_loading {
            rows.push(format!("Loading models from {}...", app.provider_name));
        } else if let Some(error) = app.model_error.as_deref() {
            rows.push(format!("Failed to load models: {error}"));
        } else if app.model_options.is_empty() {
            rows.push(format!("No models available from {}.", app.provider_name));
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

        let lines = modal_lines_from_rows(&rows, theme);
        let picker = Paragraph::new(lines)
            .block(modal_block(theme, "Models"))
            .style(Style::default().bg(theme.modal_bg).fg(theme.text))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        frame.render_widget(Clear, popup);
        frame.render_widget(picker, popup);
    } else if app.mode == InputMode::ThemeSelect {
        let popup = centered_rect(70, 12, frame.area());
        let mut rows = Vec::new();
        if app.theme_options.is_empty() {
            rows.push("No themes found in config/themes".to_string());
            rows.push("Add *.toml files under ~/.config/rosie/themes".to_string());
        } else {
            rows.push("Select a theme (Enter to apply):".to_string());
            rows.push(String::new());
            for (idx, theme_name) in app.theme_options.iter().enumerate() {
                let marker = if idx == app.theme_selected_index {
                    ">"
                } else {
                    " "
                };
                let active = if *theme_name == app.theme_key {
                    "*"
                } else {
                    " "
                };
                rows.push(format!("{marker}{active} {theme_name}"));
            }
        }
        let lines = modal_lines_from_rows(&rows, theme);
        let picker = Paragraph::new(lines)
            .block(modal_block(theme, "Themes"))
            .style(Style::default().bg(theme.modal_bg).fg(theme.text))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        frame.render_widget(Clear, popup);
        frame.render_widget(picker, popup);
    } else if app.mode == InputMode::Help {
        let popup = centered_rect(78, 16, frame.area());
        let lines = help_lines(theme);
        let help = Paragraph::new(lines)
            .block(modal_block(theme, "Help"))
            .style(Style::default().bg(theme.modal_bg).fg(theme.text))
            .alignment(Alignment::Left)
            .wrap(Wrap { trim: false });
        frame.render_widget(Clear, popup);
        frame.render_widget(help, popup);
    }
}

fn submit_composer_message(app: &mut AppState) {
    if app.is_busy() {
        app.status_message = "A response is already streaming.".to_string();
        return;
    }

    let user_content = app.composer_input.trim().to_string();
    if user_content.is_empty() {
        return;
    }
    if app.mode == InputMode::Landing && !create_fresh_session_for_landing_submit(app) {
        return;
    }
    if !ensure_active_session_for_chat(app) {
        return;
    }
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
    let config = app.config.clone();
    let model = app.model.clone();

    let handle = tokio::spawn(async move {
        if let Err(err) = stream_provider_chat(&config, &model, request_messages, tx.clone()).await
        {
            let _ = tx.send(StreamEvent::Error(err.to_string()));
        }
    });

    app.in_flight = Some(InFlightRequest {
        receiver: rx,
        handle,
        assistant_message_id,
    });
    app.sessions_dirty = true;
    app.mode = InputMode::Normal;
    app.transcript_follow = true;
    refresh_sessions(app, Some(app.active_session_id), false);
    app.status_message = format!("Sending request to {}...", app.provider_name);
}

fn create_fresh_session_for_landing_submit(app: &mut AppState) -> bool {
    match app.store.create_session() {
        Ok(session_id) => {
            if let Err(err) = app.store.set_last_active_session_id(session_id) {
                app.status_message = format!(
                    "Started new session #{}, but failed to persist active session: {err}",
                    session_id
                );
            }
            app.active_session_id = session_id;
            app.messages.clear();
            app.model = app.default_model.clone();
            app.transcript_scroll = 0;
            app.transcript_follow = true;
            app.sessions_dirty = true;
            refresh_sessions(app, Some(session_id), false);
            true
        }
        Err(err) => {
            app.status_message = format!("Failed to create session: {err}");
            false
        }
    }
}

fn ensure_active_session_for_chat(app: &mut AppState) -> bool {
    if app.active_session_id > 0
        && app
            .sessions
            .iter()
            .any(|session| session.id == app.active_session_id)
    {
        return true;
    }

    match activate_session_or_create(app, None) {
        Ok(_) => true,
        Err(err) => {
            app.status_message = format!("Failed to initialize session: {err}");
            false
        }
    }
}

fn primary_mode_for_app(app: &AppState) -> InputMode {
    if app.active_session_id <= 0 && app.messages.is_empty() {
        InputMode::Landing
    } else {
        InputMode::Normal
    }
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
    app.sessions_dirty = true;
    refresh_sessions(app, Some(app.active_session_id), false);
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
    let config = app.config.clone();
    let model = app.model.clone();
    let expected_title = expected_title.to_string();
    let first_user_message = first_user_message.to_string();
    let handle = runtime.spawn(async move {
        if let Ok(generated_title) =
            generate_session_title(&config, &model, &first_user_message).await
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
            if ch.is_ascii_alphanumeric()
                || ch.is_whitespace()
                || ch == '-'
                || ch == '/'
                || ch == '\''
            {
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
    let mut should_persist = false;
    let assistant_message_id = {
        let Some(in_flight) = app.in_flight.as_mut() else {
            return;
        };

        let assistant_message_id = in_flight.assistant_message_id;
        loop {
            match in_flight.receiver.try_recv() {
                Ok(StreamEvent::Token(content)) => {
                    if let Some(last) = app.messages.last_mut()
                        && last.role == "assistant"
                    {
                        last.content.push_str(&content);
                    }
                    app.status_message = "Streaming response...".to_string();
                }
                Ok(StreamEvent::Done) => {
                    app.status_message = "Response complete.".to_string();
                    should_persist = true;
                    done = true;
                    break;
                }
                Ok(StreamEvent::Error(message)) => {
                    if let Some(last) = app.messages.last_mut()
                        && last.role == "assistant"
                        && last.content.trim().is_empty()
                    {
                        last.content = format!("[error] {message}");
                    }
                    app.status_message = format!("Request error: {message}");
                    should_persist = true;
                    done = true;
                    break;
                }
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) => break,
                Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                    should_persist = true;
                    done = true;
                    break;
                }
            }
        }
        assistant_message_id
    };

    if should_persist {
        persist_last_assistant_message(app, assistant_message_id);
    }

    if done {
        app.in_flight = None;
        refresh_sessions(app, Some(app.active_session_id), false);
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
                app.model_options = models.clone();
                app.model_loading = false;
                app.model_error = None;
                app.model_cache = models;
                app.model_cache_host = Some(app.host.clone());
                app.model_cache_fetched_at = Some(unix_timestamp());
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
                app.model_cache.clear();
                app.model_cache_host = None;
                app.model_cache_fetched_at = None;
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
                            app.sessions_dirty = true;
                            refresh_sessions(app, Some(app.active_session_id), false);
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
            }
            Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {}
            Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                finished = true;
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
        if let Some(last) = app.messages.last_mut()
            && last.role == "assistant"
            && last.content.trim().is_empty()
        {
            last.content = "[cancelled]".to_string();
        }
        persist_last_assistant_message(app, in_flight.assistant_message_id);
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

fn conversation_header_title(app: &AppState) -> String {
    if let Some(session) = app.sessions.iter().find(|s| s.id == app.active_session_id)
        && let Some(title) = session.title.as_deref()
    {
        let trimmed = title.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    "Untitled Session".to_string()
}

fn status_mode_color(mode: InputMode, theme: ThemePalette) -> Color {
    match mode {
        InputMode::Normal => theme.success,
        InputMode::CommandPalette => theme.warn,
        InputMode::Insert => theme.error,
        _ => theme.title_value_alt,
    }
}

fn streaming_status_label(is_streaming: bool) -> String {
    if !is_streaming {
        return "Idle".to_string();
    }
    let ticks = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() / 300)
        .unwrap_or(0);
    match ticks % 3 {
        0 => "Streaming.".to_string(),
        1 => "Streaming..".to_string(),
        _ => "Streaming...".to_string(),
    }
}

fn truncate_with_ellipsis(value: &str, max_width: usize) -> String {
    if value.chars().count() <= max_width {
        return value.to_string();
    }
    if max_width <= 1 {
        return "…".to_string();
    }
    let mut out: String = value.chars().take(max_width - 1).collect();
    out.push('…');
    out
}

fn session_manager_rows(app: &mut AppState, view_height: usize) -> Vec<String> {
    let mut rows = vec!["Enter to apply | [n] new [r] rename [d] delete | Esc close".to_string()];
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

fn refresh_sessions(app: &mut AppState, preferred_session_id: Option<i64>, force: bool) {
    if !force && !app.sessions_dirty {
        return;
    }
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
            app.sessions_dirty = false;
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
            if let Err(err) = app.store.set_last_active_session_id(selected.id) {
                app.status_message = format!(
                    "Switched to session #{}, but failed to persist active session: {err}",
                    selected.id
                );
            }
            app.messages = loaded.messages;
            app.model = loaded.model.unwrap_or_else(|| app.default_model.clone());
            app.composer_input.clear();
            app.transcript_scroll = 0;
            app.transcript_follow = true;
            refresh_sessions(app, Some(selected.id), false);
            if !app.status_message.starts_with("Switched to session #") {
                app.status_message = format!("Switched to session #{}.", selected.id);
            }
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
        app.store.set_last_active_session_id(session_id)?;
        app.active_session_id = session_id;
        app.messages.clear();
        app.model = app.default_model.clone();
        app.sessions_dirty = true;
        refresh_sessions(app, Some(session_id), false);
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
    app.store.set_last_active_session_id(target_id)?;
    app.active_session_id = target_id;
    app.messages = loaded.messages;
    app.model = loaded.model.unwrap_or_else(|| app.default_model.clone());
    app.composer_input.clear();
    app.transcript_scroll = 0;
    app.transcript_follow = true;
    refresh_sessions(app, Some(target_id), false);
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
    refresh_sessions(app, Some(app.active_session_id), true);
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
            if let Err(err) = app.store.set_last_active_session_id(session_id) {
                app.status_message = format!(
                    "Started new session #{}, but failed to persist active session: {err}",
                    session_id
                );
            }
            app.active_session_id = session_id;
            app.messages.clear();
            app.model = app.default_model.clone();
            app.composer_input.clear();
            app.transcript_scroll = 0;
            app.transcript_follow = true;
            app.sessions_dirty = true;
            refresh_sessions(app, Some(session_id), false);
            if !app.status_message.starts_with("Started new session #") {
                app.status_message = format!("Started new session #{}.", session_id);
            }
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
            app.sessions_dirty = true;
            refresh_sessions(app, Some(selected.id), false);
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
            app.sessions_dirty = true;
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
        app.mode = primary_mode_for_app(app);
        app.status_message = "Model picker unavailable outside async runtime.".to_string();
        return;
    };
    cancel_model_fetch(app);
    app.mode = InputMode::ModelSelect;
    app.model_error = None;
    if apply_cached_model_options(app) {
        return;
    }

    app.model_options.clear();
    app.model_selected_index = 0;
    app.model_loading = true;
    app.status_message = format!("Loading models from {}...", app.provider_name);

    let (tx, rx) = mpsc::unbounded_channel();
    let config = app.config.clone();
    let handle = runtime.spawn(async move {
        match fetch_provider_models(&config).await {
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

const MODEL_CACHE_TTL_SECS: i64 = 60;

fn apply_cached_model_options(app: &mut AppState) -> bool {
    if !has_fresh_model_cache(app, unix_timestamp()) {
        return false;
    }

    app.model_options = app.model_cache.clone();
    app.model_loading = false;
    app.model_selected_index = app
        .model_options
        .iter()
        .position(|name| name == &app.model)
        .unwrap_or(0);
    app.status_message = if app.model_options.is_empty() {
        "Loaded cached models: no models found.".to_string()
    } else {
        format!("Loaded {} cached model(s).", app.model_options.len())
    };
    true
}

fn has_fresh_model_cache(app: &AppState, now: i64) -> bool {
    let Some(fetched_at) = app.model_cache_fetched_at else {
        return false;
    };
    let Some(host) = app.model_cache_host.as_deref() else {
        return false;
    };
    host == app.host && now.saturating_sub(fetched_at) < MODEL_CACHE_TTL_SECS
}

fn open_theme_picker(app: &mut AppState) {
    let config_dir = match config_dir_from_env() {
        Ok(path) => path,
        Err(err) => {
            app.mode = primary_mode_for_app(app);
            app.status_message = format!("Theme picker unavailable: {err}");
            return;
        }
    };
    app.mode = InputMode::ThemeSelect;
    app.theme_options = discover_config_theme_names(&config_dir);
    app.theme_selected_index = app
        .theme_options
        .iter()
        .position(|name| name == &app.theme_key)
        .unwrap_or(0);
    if app.theme_options.is_empty() {
        app.status_message = "No themes found in config/themes.".to_string();
    } else {
        app.status_message = format!("Loaded {} theme(s).", app.theme_options.len());
    }
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
    if !ensure_active_session_for_chat(app) {
        return;
    }

    match app
        .store
        .set_session_model(app.active_session_id, &selected)
    {
        Ok(()) => {
            app.model = selected.clone();
            app.sessions_dirty = true;
            refresh_sessions(app, Some(app.active_session_id), false);
            app.mode = primary_mode_for_app(app);
            app.status_message = format!("Session model set to {selected}");
        }
        Err(err) => {
            app.status_message = format!("Failed to persist session model: {err}");
        }
    }
}

fn apply_selected_theme(app: &mut AppState) {
    let Some(selected) = app.theme_options.get(app.theme_selected_index).cloned() else {
        app.status_message = "No theme selected.".to_string();
        return;
    };

    let config_dir = match config_dir_from_env() {
        Ok(path) => path,
        Err(err) => {
            app.status_message = format!("Unable to resolve theme config directory: {err}");
            return;
        }
    };
    let resolved = match resolve_theme(&selected, &config_dir) {
        Ok(theme) => theme,
        Err(err) => {
            app.status_message = format!("Failed to load theme '{selected}': {err}");
            return;
        }
    };

    app.theme = resolved.palette;
    app.theme_key = resolved.key.clone();
    app.mode = primary_mode_for_app(app);
    match persist_theme_config(&resolved.key) {
        Ok(()) => {
            app.status_message = format!("Theme set to {}.", resolved.key);
        }
        Err(err) => {
            app.status_message = format!(
                "Theme set to {}, but failed to persist: {err}",
                resolved.key
            );
        }
    }
}

fn help_rows() -> Vec<String> {
    vec![
        "Navigation".to_string(),
        "  j/k or arrows: scroll transcript".to_string(),
        "  [ / ]: previous / next assistant block".to_string(),
        "  PageUp/PageDown: full-page scroll".to_string(),
        "  Ctrl+u / Ctrl+d: half-page scroll".to_string(),
        "  gg / G: top / bottom in transcript".to_string(),
        "".to_string(),
        "Composer".to_string(),
        "  i: enter insert mode, Enter: send, Esc: return to normal".to_string(),
        "".to_string(),
        "Commands".to_string(),
        command_shortcuts_line(),
        "  :theme <name> sets directly; :theme opens picker".to_string(),
        "  ':' palette shows a picklist; use j/k (or arrows) + Enter".to_string(),
        "  :help or ? opens this panel".to_string(),
        "".to_string(),
        "Session manager (:session)".to_string(),
        "  j/k move, Enter switch, n new, r rename, d delete, Esc close".to_string(),
        "".to_string(),
        "Global: Ctrl+C quits; Esc cancels in-flight stream in normal mode".to_string(),
    ]
}

fn help_lines(theme: ThemePalette) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for row in help_rows() {
        if row.is_empty() {
            lines.push(Line::raw(""));
        } else if !row.starts_with("  ") {
            lines.push(Line::styled(
                row,
                Style::default()
                    .fg(theme.title_label)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if row.trim_start().starts_with(':') {
            lines.push(Line::styled(
                row,
                Style::default().fg(theme.title_value_alt),
            ));
        } else {
            lines.push(Line::styled(row, Style::default().fg(theme.text)));
        }
    }
    lines
}

fn modal_block(theme: ThemePalette, title: &str) -> Block<'_> {
    Block::default()
        .title(Span::styled(
            title.to_string(),
            Style::default()
                .fg(theme.modal_title)
                .add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme.modal_border))
        .style(Style::default().bg(theme.modal_bg))
}

fn modal_lines_from_rows(rows: &[String], theme: ThemePalette) -> Vec<Line<'_>> {
    rows.iter()
        .map(|row| modal_line_from_row(row, theme))
        .collect()
}

fn modal_line_from_row(row: &str, theme: ThemePalette) -> Line<'_> {
    if row.is_empty() {
        return Line::raw("");
    }

    if row.starts_with('>') {
        let selected_fg = contrast_text_for_bg(theme.success, theme);
        let rest = row.chars().skip(1).collect::<String>();
        return Line::from(vec![
            Span::styled(
                ">".to_string(),
                Style::default()
                    .fg(theme.modal_selected_bg)
                    .bg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                rest,
                Style::default()
                    .fg(selected_fg)
                    .bg(theme.success)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);
    }

    if row.starts_with("Failed to load") {
        return Line::styled(row.to_string(), Style::default().fg(theme.error));
    }
    if row.starts_with("Loading ") {
        return Line::styled(row.to_string(), Style::default().fg(theme.info));
    }
    if row.starts_with("No ") {
        return Line::styled(row.to_string(), Style::default().fg(theme.muted));
    }
    if row == "Enter to apply | [n] new [r] rename [d] delete | Esc close" {
        return Line::from(vec![
            Span::styled(
                "Enter to apply".to_string(),
                Style::default()
                    .fg(theme.title_label)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" | ".to_string(), Style::default().fg(theme.title_meta)),
            Span::styled("[n] new".to_string(), Style::default().fg(theme.success)),
            Span::styled(" ".to_string(), Style::default().fg(theme.title_meta)),
            Span::styled("[r] rename".to_string(), Style::default().fg(theme.warn)),
            Span::styled(" ".to_string(), Style::default().fg(theme.title_meta)),
            Span::styled("[d] delete".to_string(), Style::default().fg(theme.error)),
            Span::styled(
                " | Esc close".to_string(),
                Style::default().fg(theme.title_meta),
            ),
        ]);
    }
    if row.starts_with("Select a ")
        || row.starts_with("Commands (")
        || row.starts_with("Enter=switch ")
    {
        return Line::styled(
            row.to_string(),
            Style::default()
                .fg(theme.title_label)
                .add_modifier(Modifier::BOLD),
        );
    }
    if let Some(rename_value) = row.strip_prefix("Rename: ") {
        return Line::from(vec![
            Span::styled(
                "Rename: ".to_string(),
                Style::default()
                    .fg(theme.title_label)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                rename_value.to_string(),
                Style::default().fg(theme.title_value),
            ),
        ]);
    }

    let mut chars = row.chars();
    let selected = chars.next();
    let active = chars.next();
    let remainder: String = chars.collect();
    if matches!(selected, Some(' ')) && matches!(active, Some('*') | Some(' ')) {
        let active_color = if active == Some('*') {
            theme.title_value
        } else {
            theme.title_meta
        };
        return Line::from(vec![
            Span::styled(" ".to_string(), Style::default().fg(theme.title_meta)),
            Span::styled(
                active.unwrap_or(' ').to_string(),
                Style::default()
                    .fg(active_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(remainder, Style::default().fg(theme.text)),
        ]);
    }

    Line::styled(row.to_string(), Style::default().fg(theme.text))
}

fn contrast_text_for_bg(bg: Color, theme: ThemePalette) -> Color {
    let candidates = [theme.text, theme.base, theme.modal_selected_fg];
    let mut best = candidates[0];
    let mut best_ratio = contrast_ratio(bg, best);
    for candidate in candidates.iter().copied().skip(1) {
        let ratio = contrast_ratio(bg, candidate);
        if ratio > best_ratio {
            best = candidate;
            best_ratio = ratio;
        }
    }
    best
}

fn contrast_ratio(a: Color, b: Color) -> f32 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    if la > lb {
        (la + 0.05) / (lb + 0.05)
    } else {
        (lb + 0.05) / (la + 0.05)
    }
}

fn relative_luminance(color: Color) -> f32 {
    let (r, g, b) = color_to_rgb(color);
    let r_lin = srgb_channel_to_linear(r as f32 / 255.0);
    let g_lin = srgb_channel_to_linear(g as f32 / 255.0);
    let b_lin = srgb_channel_to_linear(b as f32 / 255.0);
    0.2126 * r_lin + 0.7152 * g_lin + 0.0722 * b_lin
}

fn srgb_channel_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn color_to_rgb(color: Color) -> (u8, u8, u8) {
    match color {
        Color::Rgb(r, g, b) => (r, g, b),
        Color::Black => (0, 0, 0),
        Color::Red => (128, 0, 0),
        Color::Green => (0, 128, 0),
        Color::Yellow => (128, 128, 0),
        Color::Blue => (0, 0, 128),
        Color::Magenta => (128, 0, 128),
        Color::Cyan => (0, 128, 128),
        Color::Gray => (192, 192, 192),
        Color::DarkGray => (128, 128, 128),
        Color::LightRed => (255, 0, 0),
        Color::LightGreen => (0, 255, 0),
        Color::LightYellow => (255, 255, 0),
        Color::LightBlue => (0, 0, 255),
        Color::LightMagenta => (255, 0, 255),
        Color::LightCyan => (0, 255, 255),
        Color::White => (255, 255, 255),
        _ => (255, 255, 255),
    }
}

const PALETTE_COMMANDS: [&str; 5] = ["help", "session", "models", "theme", "quit"];
const CMD_HELP: usize = 0;
const CMD_SESSION: usize = 1;
const CMD_MODELS: usize = 2;
const CMD_THEME: usize = 3;
const CMD_QUIT: usize = 4;

fn command_shortcuts_line() -> String {
    format!(
        "  {}",
        PALETTE_COMMANDS
            .iter()
            .map(|name| format!(":{name}"))
            .collect::<Vec<_>>()
            .join("  ")
    )
}

fn parse_palette_command(stem: &str) -> Option<usize> {
    if stem == "q" {
        return Some(CMD_QUIT);
    }
    PALETTE_COMMANDS.iter().position(|name| *name == stem)
}

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
    let is_exact = parse_palette_command(&stem).is_some();

    if !has_args && !is_exact {
        app.command_input = selected.to_string();
    }

    run_palette_command(app)
}

struct TranscriptRender {
    lines: Vec<Line<'static>>,
    assistant_markers: Vec<u16>,
}

#[derive(Clone)]
struct CachedAssistantRender {
    content: String,
    theme_key: String,
    view_width: usize,
    rows: Vec<Line<'static>>,
    assistant_marker_offset: Option<u16>,
}

struct MessageRenderCtx<'a> {
    label: &'a str,
    label_color: Color,
    is_assistant: bool,
    wrap_pad: &'a str,
    text_style: Style,
    code_style: Style,
    rail_style: Style,
    gutter_style: Style,
    theme: ThemePalette,
}

#[cfg(test)]
fn transcript_lines(
    messages: &[ChatMessage],
    is_busy: bool,
    theme: ThemePalette,
    _view_width: usize,
) -> TranscriptRender {
    if messages.is_empty() {
        return TranscriptRender {
            lines: vec![Line::styled(
                "No messages yet. Press i, type, then Enter.".to_string(),
                Style::default().fg(theme.muted),
            )],
            assistant_markers: Vec::new(),
        };
    }

    let mut rows = Vec::new();
    let mut assistant_markers = Vec::new();
    for (idx, message) in messages.iter().enumerate() {
        let block = render_message_block(message, is_busy, idx, messages.len(), theme);
        if let Some(offset) = block.assistant_marker_offset {
            assistant_markers.push((rows.len() as u16).saturating_add(offset));
        }
        rows.extend(block.rows);
    }
    TranscriptRender {
        lines: rows,
        assistant_markers,
    }
}

fn transcript_lines_cached(app: &mut AppState, view_width: usize) -> TranscriptRender {
    let messages = &app.messages;
    let theme = app.theme;
    if messages.is_empty() {
        return TranscriptRender {
            lines: vec![Line::styled(
                "No messages yet. Press i, type, then Enter.".to_string(),
                Style::default().fg(theme.muted),
            )],
            assistant_markers: Vec::new(),
        };
    }

    app.transcript_render_cache
        .retain(|idx, _| *idx < messages.len());

    let mut rows = Vec::new();
    let mut assistant_markers = Vec::new();
    for (idx, message) in messages.iter().enumerate() {
        let content = message_content_for_render(message, app.is_busy(), idx, messages.len());
        let cached_rows = if message.role == "assistant" {
            if let Some(cached) = app.transcript_render_cache.get(&idx) {
                if cached.content == content
                    && cached.theme_key == app.theme_key
                    && cached.view_width == view_width
                {
                    Some((cached.rows.clone(), cached.assistant_marker_offset))
                } else {
                    None
                }
            } else {
                None
            }
        } else {
            None
        };

        let (block_rows, assistant_marker_offset) = if let Some(cached) = cached_rows {
            cached
        } else {
            let block = render_message_block(message, app.is_busy(), idx, messages.len(), theme);
            if message.role == "assistant" {
                app.transcript_render_cache.insert(
                    idx,
                    CachedAssistantRender {
                        content,
                        theme_key: app.theme_key.clone(),
                        view_width,
                        rows: block.rows.clone(),
                        assistant_marker_offset: block.assistant_marker_offset,
                    },
                );
            }
            (block.rows, block.assistant_marker_offset)
        };

        if let Some(offset) = assistant_marker_offset {
            assistant_markers.push((rows.len() as u16).saturating_add(offset));
        }
        rows.extend(block_rows);
    }

    TranscriptRender {
        lines: rows,
        assistant_markers,
    }
}

struct MessageRenderBlock {
    rows: Vec<Line<'static>>,
    assistant_marker_offset: Option<u16>,
}

fn render_message_block(
    message: &ChatMessage,
    is_busy: bool,
    idx: usize,
    total_messages: usize,
    theme: ThemePalette,
) -> MessageRenderBlock {
    let (label, label_color) = match message.role.as_str() {
        "user" => ("You", theme.user),
        "assistant" => ("Assistant", theme.assistant),
        _ => ("System", theme.system),
    };
    let is_assistant = message.role == "assistant";
    let ctx = MessageRenderCtx {
        label,
        label_color,
        is_assistant,
        wrap_pad: "  ",
        text_style: Style::default().fg(theme.text),
        code_style: Style::default().fg(theme.text).bg(theme.surface_alt),
        rail_style: Style::default()
            .fg(theme.title_meta)
            .bg(theme.highlight_low),
        gutter_style: Style::default()
            .fg(theme.title_value_alt)
            .bg(theme.surface_alt),
        theme,
    };

    let mut rows = Vec::new();
    let mut assistant_markers = Vec::new();
    if ctx.is_assistant {
        push_assistant_separator(&mut rows, &mut assistant_markers, theme);
    }

    let content = message_content_for_render(message, is_busy, idx, total_messages);
    let mut in_code_block = false;
    let mut code_language: Option<String> = None;
    let mut code_lines: Vec<String> = Vec::new();
    let mut wrote_any = false;
    let mut wrote_first_content_line = false;

    for line in content.lines() {
        let is_fence = line.trim_start().starts_with("```");
        if is_fence {
            if !in_code_block {
                let lang = line.trim_start().trim_start_matches("```").trim();
                let code_label = if lang.is_empty() {
                    "code".to_string()
                } else {
                    format!("code: {lang}")
                };
                rows.push(Line::from(vec![
                    Span::styled("  ╭─ ".to_string(), ctx.rail_style),
                    Span::styled(code_label, ctx.rail_style.add_modifier(Modifier::BOLD)),
                ]));
                code_language =
                    normalize_code_language(if lang.is_empty() { None } else { Some(lang) })
                        .map(str::to_string);
                code_lines.clear();
                in_code_block = true;
                wrote_any = true;
            } else {
                flush_code_block(
                    &mut rows,
                    &code_lines,
                    code_language.as_deref(),
                    &mut wrote_first_content_line,
                    &ctx,
                );
                code_lines.clear();
                code_language = None;
                in_code_block = false;
                wrote_any = true;
            }
            continue;
        }

        if in_code_block {
            code_lines.push(line.to_string());
            wrote_any = true;
            continue;
        }

        if ctx.is_assistant {
            rows.push(Line::from(markdown_line_spans(line, ctx.theme)));
            wrote_first_content_line = true;
            wrote_any = true;
            continue;
        }

        push_non_assistant_text_line(&mut rows, line, &mut wrote_first_content_line, &ctx);
        wrote_any = true;
    }

    if in_code_block {
        flush_code_block(
            &mut rows,
            &code_lines,
            code_language.as_deref(),
            &mut wrote_first_content_line,
            &ctx,
        );
    }

    if !wrote_any && !ctx.is_assistant {
        rows.push(Line::styled(
            format!("{}:", ctx.label),
            Style::default()
                .fg(ctx.label_color)
                .add_modifier(Modifier::BOLD),
        ));
    }
    rows.push(Line::raw(""));

    MessageRenderBlock {
        rows,
        assistant_marker_offset: assistant_markers.first().copied(),
    }
}

fn push_assistant_separator(
    rows: &mut Vec<Line<'static>>,
    assistant_markers: &mut Vec<u16>,
    theme: ThemePalette,
) {
    rows.push(Line::raw(""));
    assistant_markers.push(rows.len() as u16);
    rows.push(Line::from(vec![
        Span::styled(
            "──────────────── ".to_string(),
            Style::default().fg(theme.title_meta),
        ),
        Span::styled("🤖 ".to_string(), Style::default().fg(theme.title_label)),
        Span::styled(
            "Rosie".to_string(),
            Style::default()
                .fg(theme.title_label)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " ────────────────".to_string(),
            Style::default().fg(theme.title_meta),
        ),
    ]));
    rows.push(Line::raw(""));
}

fn message_content_for_render(
    message: &ChatMessage,
    is_busy: bool,
    idx: usize,
    total_messages: usize,
) -> String {
    let is_assistant = message.role == "assistant";
    let is_pending_assistant =
        is_busy && is_assistant && idx + 1 == total_messages && message.content.trim().is_empty();
    if is_pending_assistant {
        "[waiting for model response...]".to_string()
    } else if message.content.is_empty() {
        String::new()
    } else {
        message.content.clone()
    }
}

fn flush_code_block(
    rows: &mut Vec<Line<'static>>,
    code_lines: &[String],
    code_language: Option<&str>,
    wrote_first_content_line: &mut bool,
    ctx: &MessageRenderCtx<'_>,
) {
    let block_width = code_lines
        .iter()
        .map(|l| l.chars().count())
        .max()
        .unwrap_or(0);
    let highlighted_lines =
        highlighted_code_lines(code_lines, code_language, ctx.code_style, ctx.theme);
    for (line_idx, highlighted_segments) in highlighted_lines.into_iter().enumerate() {
        let code_line = code_lines.get(line_idx).map(String::as_str).unwrap_or("");
        let mut spans: Vec<Span<'static>> = Vec::new();
        if !ctx.is_assistant && !*wrote_first_content_line && line_idx == 0 {
            spans.push(Span::styled(
                format!("{}:", ctx.label),
                Style::default()
                    .fg(ctx.label_color)
                    .add_modifier(Modifier::BOLD),
            ));
        } else if !ctx.is_assistant {
            spans.push(Span::styled(ctx.wrap_pad.to_string(), ctx.text_style));
        }
        spans.push(Span::styled("  │ ".to_string(), ctx.gutter_style));
        if highlighted_segments.is_empty() {
            spans.push(Span::styled(code_line.to_string(), ctx.code_style));
        } else {
            spans.extend(highlighted_segments);
        }
        let pad = block_width.saturating_sub(code_line.chars().count());
        if pad > 0 {
            spans.push(Span::styled(" ".repeat(pad), ctx.code_style));
        }
        rows.push(Line::from(spans));
        *wrote_first_content_line = true;
    }
    rows.push(Line::styled(
        "  ╰────────────────".to_string(),
        ctx.rail_style,
    ));
}

fn push_non_assistant_text_line(
    rows: &mut Vec<Line<'static>>,
    line: &str,
    wrote_first_content_line: &mut bool,
    ctx: &MessageRenderCtx<'_>,
) {
    if !*wrote_first_content_line {
        rows.push(Line::from(vec![
            Span::styled(
                format!("{}:", ctx.label),
                Style::default()
                    .fg(ctx.label_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!(" {line}"), ctx.text_style),
        ]));
        *wrote_first_content_line = true;
    } else {
        rows.push(Line::styled(
            format!("{}{}", ctx.wrap_pad, line),
            ctx.text_style,
        ));
    }
}
const TREE_SITTER_HIGHLIGHT_NAMES: &[&str] = &[
    "attribute",
    "comment",
    "constant",
    "constant.builtin",
    "constructor",
    "embedded",
    "function",
    "function.builtin",
    "keyword",
    "module",
    "number",
    "operator",
    "property",
    "punctuation",
    "punctuation.bracket",
    "punctuation.delimiter",
    "string",
    "string.special",
    "tag",
    "type",
    "type.builtin",
    "variable",
    "variable.builtin",
    "variable.parameter",
];

struct TreeSitterAssets {
    rust: HighlightConfiguration,
    python: HighlightConfiguration,
    javascript: HighlightConfiguration,
    typescript: HighlightConfiguration,
    tsx: HighlightConfiguration,
    json: HighlightConfiguration,
    bash: HighlightConfiguration,
    yaml: HighlightConfiguration,
}

fn syntax_assets() -> &'static TreeSitterAssets {
    static ASSETS: OnceLock<TreeSitterAssets> = OnceLock::new();
    ASSETS.get_or_init(|| TreeSitterAssets {
        rust: build_highlight_config(
            tree_sitter_rust::LANGUAGE.into(),
            "rust",
            tree_sitter_rust::HIGHLIGHTS_QUERY,
            tree_sitter_rust::INJECTIONS_QUERY,
            "",
        ),
        python: build_highlight_config(
            tree_sitter_python::LANGUAGE.into(),
            "python",
            tree_sitter_python::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        javascript: build_highlight_config(
            tree_sitter_javascript::LANGUAGE.into(),
            "javascript",
            tree_sitter_javascript::HIGHLIGHT_QUERY,
            tree_sitter_javascript::INJECTIONS_QUERY,
            tree_sitter_javascript::LOCALS_QUERY,
        ),
        typescript: build_highlight_config(
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            "typescript",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        tsx: build_highlight_config(
            tree_sitter_typescript::LANGUAGE_TSX.into(),
            "tsx",
            tree_sitter_typescript::HIGHLIGHTS_QUERY,
            "",
            tree_sitter_typescript::LOCALS_QUERY,
        ),
        json: build_highlight_config(
            tree_sitter_json::LANGUAGE.into(),
            "json",
            tree_sitter_json::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
        bash: build_highlight_config(
            tree_sitter_bash::LANGUAGE.into(),
            "bash",
            tree_sitter_bash::HIGHLIGHT_QUERY,
            "",
            "",
        ),
        yaml: build_highlight_config(
            tree_sitter_yaml::LANGUAGE.into(),
            "yaml",
            tree_sitter_yaml::HIGHLIGHTS_QUERY,
            "",
            "",
        ),
    })
}

fn build_highlight_config(
    language: Language,
    name: &str,
    highlights_query: &str,
    injections_query: &str,
    locals_query: &str,
) -> HighlightConfiguration {
    let mut config = HighlightConfiguration::new(
        language,
        name,
        highlights_query,
        injections_query,
        locals_query,
    )
    .expect("load tree-sitter highlight config");
    config.configure(TREE_SITTER_HIGHLIGHT_NAMES);
    config
}

fn normalize_code_language(language: Option<&str>) -> Option<&'static str> {
    let lower = language?.trim().to_ascii_lowercase();
    match lower.as_str() {
        "bash" | "sh" | "shell" | "zsh" => Some("bash"),
        "js" | "javascript" | "mjs" | "cjs" => Some("javascript"),
        "json" => Some("json"),
        "py" | "python" => Some("python"),
        "rs" | "rust" => Some("rust"),
        "ts" | "typescript" => Some("typescript"),
        "tsx" => Some("tsx"),
        "yaml" | "yml" => Some("yaml"),
        _ => None,
    }
}

fn highlight_config_for_language(
    language: Option<&str>,
) -> Option<&'static HighlightConfiguration> {
    let assets = syntax_assets();
    match normalize_code_language(language)? {
        "bash" => Some(&assets.bash),
        "javascript" => Some(&assets.javascript),
        "json" => Some(&assets.json),
        "python" => Some(&assets.python),
        "rust" => Some(&assets.rust),
        "tsx" => Some(&assets.tsx),
        "typescript" => Some(&assets.typescript),
        "yaml" => Some(&assets.yaml),
        _ => None,
    }
}

fn highlighted_code_lines(
    code_lines: &[String],
    language: Option<&str>,
    fallback_style: Style,
    theme: ThemePalette,
) -> Vec<Vec<Span<'static>>> {
    if code_lines.is_empty() {
        return Vec::new();
    }

    let Some(config) = highlight_config_for_language(language) else {
        return code_lines
            .iter()
            .map(|line| vec![Span::styled(line.to_string(), fallback_style)])
            .collect();
    };

    let source = code_lines.join("\n");
    let mut highlighter = Highlighter::new();
    let Ok(events) = highlighter.highlight(config, source.as_bytes(), None, |_| None) else {
        return code_lines
            .iter()
            .map(|line| vec![Span::styled(line.to_string(), fallback_style)])
            .collect();
    };

    let mut lines = vec![Vec::new()];
    let mut stack: Vec<Highlight> = Vec::new();

    for event in events {
        let Ok(event) = event else {
            return code_lines
                .iter()
                .map(|line| vec![Span::styled(line.to_string(), fallback_style)])
                .collect();
        };
        match event {
            HighlightEvent::HighlightStart(highlight) => stack.push(highlight),
            HighlightEvent::HighlightEnd => {
                let _ = stack.pop();
            }
            HighlightEvent::Source { start, end } => {
                let segment = &source[start..end];
                let style = style_for_highlight(stack.last().copied(), fallback_style, theme);
                append_highlighted_segment(&mut lines, segment, style);
            }
        }
    }

    while lines.len() < code_lines.len() {
        lines.push(Vec::new());
    }

    lines
}

fn append_highlighted_segment(lines: &mut Vec<Vec<Span<'static>>>, segment: &str, style: Style) {
    for (idx, piece) in segment.split('\n').enumerate() {
        if idx > 0 {
            lines.push(Vec::new());
        }
        if !piece.is_empty() {
            lines
                .last_mut()
                .expect("at least one line")
                .push(Span::styled(piece.to_string(), style));
        }
    }
}

fn style_for_highlight(
    highlight: Option<Highlight>,
    fallback_style: Style,
    theme: ThemePalette,
) -> Style {
    let Some(Highlight(index)) = highlight else {
        return fallback_style;
    };
    let Some(name) = TREE_SITTER_HIGHLIGHT_NAMES.get(index).copied() else {
        return fallback_style;
    };

    let token_style = if name.starts_with("comment") {
        Style::default()
            .fg(theme.muted)
            .add_modifier(Modifier::ITALIC)
    } else if name.starts_with("string") {
        Style::default().fg(theme.warn)
    } else if name.starts_with("keyword") || name.starts_with("operator") {
        Style::default()
            .fg(theme.title_value)
            .add_modifier(Modifier::BOLD)
    } else if name.starts_with("function") || name == "constructor" {
        Style::default().fg(theme.title_value_alt)
    } else if name.starts_with("type") || name == "attribute" {
        Style::default().fg(theme.user)
    } else if name.starts_with("number") || name.starts_with("constant") {
        Style::default().fg(theme.info)
    } else if name.starts_with("variable.parameter") {
        Style::default()
            .fg(theme.title_meta)
            .add_modifier(Modifier::ITALIC)
    } else if name.starts_with("punctuation") {
        Style::default().fg(theme.title_meta)
    } else if name == "tag" || name == "property" || name == "module" {
        Style::default().fg(theme.assistant)
    } else {
        Style::default().fg(theme.text)
    };

    fallback_style.patch(token_style)
}

fn markdown_line_spans(line: &str, theme: ThemePalette) -> Vec<Span<'static>> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return vec![Span::raw("")];
    }

    if is_markdown_hr(trimmed) {
        return vec![Span::styled(
            "────────────────────────────────".to_string(),
            Style::default().fg(theme.title_meta),
        )];
    }

    if let Some(rest) = trimmed.strip_prefix("### ") {
        let mut spans = vec![Span::styled(
            "### ".to_string(),
            Style::default().fg(theme.title_meta),
        )];
        spans.extend(markdown_inline_spans(
            rest,
            Style::default()
                .fg(theme.title_value_alt)
                .add_modifier(Modifier::BOLD),
            theme,
        ));
        return spans;
    }
    if let Some(rest) = trimmed.strip_prefix("## ") {
        let mut spans = vec![Span::styled(
            "## ".to_string(),
            Style::default().fg(theme.title_meta),
        )];
        spans.extend(markdown_inline_spans(
            rest,
            Style::default()
                .fg(theme.title_value)
                .add_modifier(Modifier::BOLD),
            theme,
        ));
        return spans;
    }
    if let Some(rest) = trimmed.strip_prefix("# ") {
        let mut spans = vec![Span::styled(
            "# ".to_string(),
            Style::default().fg(theme.title_meta),
        )];
        spans.extend(markdown_inline_spans(
            rest,
            Style::default()
                .fg(theme.title_label)
                .add_modifier(Modifier::BOLD),
            theme,
        ));
        return spans;
    }
    if let Some(rest) = trimmed.strip_prefix("> ") {
        let mut spans = vec![Span::styled(
            "▎ ".to_string(),
            Style::default().fg(theme.title_meta),
        )];
        spans.extend(markdown_inline_spans(
            rest,
            Style::default().fg(theme.muted),
            theme,
        ));
        return spans;
    }
    if let Some(rest) = parse_markdown_list_item(trimmed) {
        let mut spans = vec![Span::styled(
            "• ".to_string(),
            Style::default().fg(theme.success),
        )];
        spans.extend(markdown_inline_spans(
            rest,
            Style::default().fg(theme.text),
            theme,
        ));
        return spans;
    }

    markdown_inline_spans(trimmed, Style::default().fg(theme.text), theme)
}

fn markdown_inline_spans(text: &str, base: Style, theme: ThemePalette) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    let mut bold = false;
    let mut italic = false;
    let mut code = false;

    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("**") {
            bold = !bold;
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('*') {
            italic = !italic;
            rest = after;
            continue;
        }
        if let Some(after) = rest.strip_prefix('`') {
            code = !code;
            rest = after;
            continue;
        }
        if let Some((label, after_link)) = parse_markdown_link(rest) {
            let mut style = base.fg(theme.info).add_modifier(Modifier::UNDERLINED);
            if bold {
                style = style.add_modifier(Modifier::BOLD);
            }
            if italic {
                style = style.add_modifier(Modifier::ITALIC);
            }
            if code {
                style = style.bg(theme.surface_alt);
            }
            spans.push(Span::styled(label, style));
            rest = after_link;
            continue;
        }

        let next_idx = next_markdown_token_index(rest);
        let (chunk, after) = rest.split_at(next_idx);
        let mut style = base;
        if bold {
            style = style.add_modifier(Modifier::BOLD);
        }
        if italic {
            style = style.add_modifier(Modifier::ITALIC);
        }
        if code {
            style = style
                .fg(theme.title_value_alt)
                .bg(theme.surface_alt)
                .add_modifier(Modifier::BOLD);
        }
        spans.push(Span::styled(chunk.to_string(), style));
        rest = after;
    }

    if spans.is_empty() {
        vec![Span::styled(String::new(), base)]
    } else {
        spans
    }
}

fn parse_markdown_link(input: &str) -> Option<(String, &str)> {
    if !input.starts_with('[') {
        return None;
    }
    let label_end = input.find("](")?;
    let label = &input[1..label_end];
    let url_end_rel = input[label_end + 2..].find(')')?;
    let after = &input[label_end + 2 + url_end_rel + 1..];
    Some((label.to_string(), after))
}

fn next_markdown_token_index(input: &str) -> usize {
    let mut idx = input.len();
    for token in ["**", "*", "`", "["] {
        if let Some(pos) = input.find(token) {
            idx = idx.min(pos);
        }
    }
    idx.max(1)
}

fn is_markdown_hr(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed == "---" || trimmed == "***" || trimmed == "___"
}

fn parse_markdown_list_item(line: &str) -> Option<&str> {
    if let Some(rest) = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))
        .or_else(|| line.strip_prefix("+ "))
    {
        return Some(rest);
    }
    let mut digits = 0usize;
    for ch in line.chars() {
        if ch.is_ascii_digit() {
            digits += 1;
            continue;
        }
        break;
    }
    if digits > 0 && line[digits..].starts_with(". ") {
        return Some(&line[digits + 2..]);
    }
    None
}

fn compact_footer_help(mode: InputMode, is_busy: bool) -> String {
    match mode {
        InputMode::Landing => "Enter send | Ctrl+P cmd".to_string(),
        InputMode::Normal => {
            if is_busy {
                "[/] jump | Esc cancel".to_string()
            } else {
                "[/] jump | i compose".to_string()
            }
        }
        InputMode::Insert => "Enter send | Esc normal".to_string(),
        InputMode::CommandPalette => "j/k pick | Enter run".to_string(),
        InputMode::SessionManager => "j/k move | Enter apply".to_string(),
        InputMode::SessionRename => "Type title | Enter save".to_string(),
        InputMode::ConfirmDelete => "Enter/y yes | n/Esc no".to_string(),
        InputMode::ModelSelect => "j/k move | Enter select".to_string(),
        InputMode::ThemeSelect => "j/k move | Enter select".to_string(),
        InputMode::Help => "Esc/q/? close".to_string(),
    }
}

fn transcript_position_label(scroll: u16, max_scroll: u16) -> String {
    if max_scroll == 0 {
        return "1/1".to_string();
    }
    format!(
        "{}/{}",
        scroll.saturating_add(1),
        max_scroll.saturating_add(1)
    )
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

fn jump_to_next_assistant_block(app: &mut AppState) {
    let Some(target) = app
        .transcript_assistant_markers
        .iter()
        .copied()
        .find(|marker| *marker > app.transcript_scroll)
    else {
        app.status_message = "No newer assistant block.".to_string();
        return;
    };
    app.transcript_scroll = target.min(app.transcript_max_scroll);
    app.transcript_follow = app.transcript_scroll >= app.transcript_max_scroll;
    app.status_message = "Jumped to next assistant block.".to_string();
}

fn jump_to_prev_assistant_block(app: &mut AppState) {
    let Some(target) = app
        .transcript_assistant_markers
        .iter()
        .copied()
        .rfind(|marker| *marker < app.transcript_scroll)
    else {
        app.status_message = "No previous assistant block.".to_string();
        return;
    };
    app.transcript_scroll = target.min(app.transcript_max_scroll);
    app.transcript_follow = false;
    app.status_message = "Jumped to previous assistant block.".to_string();
}

async fn stream_provider_chat(
    config: &StoredConfig,
    model: &str,
    messages: Vec<ChatMessage>,
    tx: mpsc::UnboundedSender<StreamEvent>,
) -> Result<()> {
    let router = ProviderRouter::from_config(config)?;
    let request = ProviderChatRequest {
        model: model.to_string(),
        messages: messages
            .into_iter()
            .map(provider_message_from_chat)
            .collect(),
        temperature: None,
    };
    let (provider_tx, mut provider_rx) = mpsc::unbounded_channel();
    let forwarder = tokio::spawn(async move {
        while let Some(event) = provider_rx.recv().await {
            match event {
                ProviderEvent::Token(content) => {
                    let _ = tx.send(StreamEvent::Token(content));
                }
                ProviderEvent::Done => {
                    let _ = tx.send(StreamEvent::Done);
                }
            }
        }
    });
    let result = router.stream_chat(request, provider_tx).await;
    let _ = forwarder.await;
    result
}

async fn fetch_provider_models(config: &StoredConfig) -> Result<Vec<String>> {
    let router = ProviderRouter::from_config(config)?;
    router.list_models().await
}

async fn generate_session_title(
    config: &StoredConfig,
    model: &str,
    first_user_message: &str,
) -> Result<String> {
    let router = ProviderRouter::from_config(config)?;
    let prompt = format!(
        "Create a concise session title from this user message.\nRules:\n- 3 to 6 words\n- Title Case\n- No quotes\n- No ending punctuation\n- Keep it specific\nReturn only the title.\n\nUser message:\n{}",
        first_user_message.trim()
    );
    let response = router
        .chat(ProviderChatRequest {
            model: model.to_string(),
            messages: vec![ProviderMessage {
                role: ProviderRole::User,
                content: prompt,
            }],
            temperature: None,
        })
        .await?;
    Ok(response.message.content)
}

fn provider_message_from_chat(message: ChatMessage) -> ProviderMessage {
    ProviderMessage {
        role: match message.role.as_str() {
            "system" => ProviderRole::System,
            "assistant" => ProviderRole::Assistant,
            "tool" => ProviderRole::Tool,
            _ => ProviderRole::User,
        },
        content: message.content,
    }
}

fn provider_cache_key(config: &StoredConfig) -> Result<String> {
    let (provider_name, provider) = config.active_provider_entry()?;
    let key = match provider {
        ProviderConfig::Ollama { endpoint, .. } => format!("{provider_name}:{endpoint}"),
        ProviderConfig::OpenAi { endpoint, .. } => format!(
            "{provider_name}:{}",
            endpoint.as_deref().unwrap_or("https://api.openai.com/v1")
        ),
        ProviderConfig::Anthropic { endpoint, .. } => format!(
            "{provider_name}:{}",
            endpoint
                .as_deref()
                .unwrap_or("https://api.anthropic.com/v1/messages")
        ),
        ProviderConfig::OpenAiCompatible { endpoint, .. } => format!("{provider_name}:{endpoint}"),
    };
    Ok(key)
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

    let Some(command_idx) = parse_palette_command(&command) else {
        if command.is_empty() {
            app.status_message = "No command entered.".to_string();
        } else {
            app.status_message = format!("Unknown command: :{command}");
        }
        return false;
    };

    match command_idx {
        CMD_QUIT => true,
        CMD_SESSION => {
            open_session_manager(app);
            false
        }
        CMD_MODELS => {
            open_model_picker(app);
            false
        }
        CMD_HELP => {
            app.mode = InputMode::Help;
            app.status_message = "Help open.".to_string();
            false
        }
        CMD_THEME => {
            apply_theme_command(app, arg);
            false
        }
        _ => false,
    }
}

fn apply_theme_command(app: &mut AppState, arg: &str) {
    if arg.is_empty() {
        open_theme_picker(app);
        return;
    }

    let config_dir = match config_dir_from_env() {
        Ok(path) => path,
        Err(err) => {
            app.status_message = format!("Unable to resolve theme config directory: {err}");
            return;
        }
    };
    let resolved = match resolve_theme(arg, &config_dir) {
        Ok(theme) => theme,
        Err(err) => {
            app.status_message = format!("Failed to load theme '{arg}': {err}");
            return;
        }
    };

    app.theme = resolved.palette;
    app.theme_key = resolved.key.clone();
    match persist_theme_config(&resolved.key) {
        Ok(()) => {
            app.status_message = format!("Theme set to {}.", resolved.key);
        }
        Err(err) => {
            app.status_message = format!(
                "Theme set to {}, but failed to persist: {err}",
                resolved.key
            );
        }
    }
}

fn persist_theme_config(theme_key: &str) -> Result<()> {
    let path = tui_config_path()?;
    let mut value = match fs::read_to_string(&path) {
        Ok(contents) => toml::from_str::<toml::Value>(&contents)
            .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new())),
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            toml::Value::Table(toml::map::Map::new())
        }
        Err(err) => return Err(err.into()),
    };

    let table = value
        .as_table_mut()
        .ok_or_else(|| anyhow!("Config file root must be a TOML table"))?;
    table.insert(
        "theme".to_string(),
        toml::Value::String(theme_key.to_string()),
    );

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, toml::to_string_pretty(&value)?)?;
    Ok(())
}

fn tui_config_path() -> Result<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| anyhow!("Unable to determine config directory"))?;
    Ok(base.join("rosie").join("config.toml"))
}

#[cfg(test)]
mod tests;
