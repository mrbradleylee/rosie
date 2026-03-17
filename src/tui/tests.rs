use super::*;
use crate::config::{ProviderConfig, StoredConfig};
use crate::theme::{DEFAULT_THEME_KEY, default_theme, resolve_theme};
use std::collections::BTreeMap;
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
    let resolved = default_theme();
    AppState::new(AppStateInit {
        config: StoredConfig::default(),
        provider_name: "ollama".to_string(),
        host: "http://localhost:11434".to_string(),
        model: "test-model".to_string(),
        default_model: "test-model".to_string(),
        theme_key: resolved.key,
        theme: resolved.palette,
        store,
        active_session_id,
        messages,
        sessions,
        selected_session_index,
    })
}

fn anthropic_config() -> StoredConfig {
    let mut providers = BTreeMap::new();
    providers.insert(
        "anthropic".to_string(),
        ProviderConfig::Anthropic {
            endpoint: None,
            model: Some("claude-3-7-sonnet".to_string()),
        },
    );

    StoredConfig {
        active_provider: Some("anthropic".to_string()),
        providers,
        theme: None,
        execution_enabled: Some(true),
    }
}

fn native_openai_config() -> StoredConfig {
    let mut providers = BTreeMap::new();
    providers.insert(
        "openai".to_string(),
        ProviderConfig::OpenAi {
            model: Some("gpt-5".to_string()),
            endpoint: None,
        },
    );

    StoredConfig {
        active_provider: Some("openai".to_string()),
        providers,
        theme: None,
        execution_enabled: Some(true),
    }
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect::<String>()
}

fn render_lines_text(messages: Vec<ChatMessage>, is_busy: bool) -> (Vec<String>, Vec<u16>) {
    let theme = default_theme().palette;
    let render = transcript_lines(&messages, is_busy, theme, 80);
    let lines = render.lines.iter().map(line_text).collect::<Vec<_>>();
    (lines, render.assistant_markers)
}

fn render_cached_lines_text(app: &mut AppState, view_width: usize) -> (Vec<String>, Vec<u16>) {
    let render = transcript_lines_cached(app, view_width);
    let lines = render.lines.iter().map(line_text).collect::<Vec<_>>();
    (lines, render.assistant_markers)
}

fn last_persisted_message_content(store: &SessionStore, session_id: i64) -> String {
    store
        .load_messages(session_id)
        .expect("load messages")
        .last()
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

#[test]
fn fresh_model_cache_matches_host_and_ttl() {
    let db_path = temp_db_path("model-cache-freshness");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
        let now = 1_000;

        app.model_cache = vec!["qwen".to_string()];
        app.model_cache_host = Some(app.host.clone());
        app.model_cache_fetched_at = Some(now - 10);
        assert!(has_fresh_model_cache(&app, now));

        app.model_cache_fetched_at = Some(now - MODEL_CACHE_TTL_SECS);
        assert!(!has_fresh_model_cache(&app, now));

        app.model_cache_fetched_at = Some(now - 10);
        app.model_cache_host = Some("http://other-host:11434".to_string());
        assert!(!has_fresh_model_cache(&app, now));
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn apply_cached_model_options_uses_recent_cache() {
    let db_path = temp_db_path("model-cache-apply");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);

        app.model = "llama3.2".to_string();
        app.model_cache = vec!["qwen2.5-coder".to_string(), "llama3.2".to_string()];
        app.model_cache_host = Some(app.host.clone());
        app.model_cache_fetched_at = Some(unix_timestamp());

        assert!(apply_cached_model_options(&mut app));
        assert_eq!(
            app.model_options,
            vec!["qwen2.5-coder".to_string(), "llama3.2".to_string()]
        );
        assert_eq!(app.model_selected_index, 1);
        assert_eq!(app.status_message, "Loaded 2 cached model(s).");
        assert!(!app.model_loading);
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn trailing_visible_text_tracks_the_input_tail() {
    let (visible, cursor) = trailing_visible_text("hello", 10);
    assert_eq!(visible, "hello");
    assert_eq!(cursor, 5);

    let (visible, cursor) = trailing_visible_text("abcdefghijklmnopqrstuvwxyz", 8);
    assert_eq!(visible, "stuvwxyz");
    assert_eq!(cursor, 8);
}

#[tokio::test]
async fn open_model_picker_uses_manual_entry_when_discovery_is_unavailable() {
    let db_path = temp_db_path("model-picker-manual-entry");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
        app.config = anthropic_config();
        app.provider_name = "anthropic".to_string();
        app.model = "claude-3-7-sonnet".to_string();

        open_model_picker(&mut app);

        assert_eq!(app.mode, InputMode::ModelSelect);
        assert!(app.model_manual_entry);
        assert_eq!(app.model_input, "claude-3-7-sonnet");
        assert!(!app.model_loading);
        assert!(app.model_options.is_empty());
        assert_eq!(
            app.status_message,
            "Model discovery isn't available for anthropic. Type a model name to continue."
        );
    }

    let _ = fs::remove_file(&db_path);
}

#[tokio::test]
async fn open_model_picker_uses_native_openai_presets() {
    let db_path = temp_db_path("model-picker-openai-presets");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
        app.config = native_openai_config();
        app.provider_name = "openai".to_string();
        app.model = "gpt-5".to_string();

        open_model_picker(&mut app);

        assert_eq!(app.mode, InputMode::ModelSelect);
        assert!(!app.model_manual_entry);
        assert!(!app.model_loading);
        assert_eq!(app.model_selected_index, 1);
        assert_eq!(
            app.model_options,
            vec!["gpt-5-codex".to_string(), "gpt-5".to_string()]
        );
        assert_eq!(
            app.status_message,
            "Loaded 2 built-in model preset(s). Press i for manual entry."
        );
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn model_picker_can_switch_from_presets_to_manual_entry() {
    let db_path = temp_db_path("model-picker-openai-manual-switch");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
        app.config = native_openai_config();
        app.provider_name = "openai".to_string();
        app.model = "gpt-5".to_string();
        app.mode = InputMode::ModelSelect;
        app.model_options = vec!["gpt-5".to_string(), "gpt-5-mini".to_string()];

        handle_model_select_input(
            &mut app,
            KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE),
        );

        assert!(app.model_manual_entry);
        assert!(app.model_options.is_empty());
        assert_eq!(app.model_input, "gpt-5");
        assert_eq!(
            app.status_message,
            "Preset list open for openai. Type a model name and press Enter to apply it."
        );
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn apply_selected_model_persists_manual_model_entry() {
    let db_path = temp_db_path("model-picker-manual-apply");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
        app.model_manual_entry = true;
        app.model_input = "claude-3-7-sonnet-latest".to_string();
        app.mode = InputMode::ModelSelect;

        apply_selected_model(&mut app);

        assert_eq!(app.model, "claude-3-7-sonnet-latest");
        assert_eq!(app.mode, InputMode::Normal);
        assert_eq!(
            app.status_message,
            "Session model set to claude-3-7-sonnet-latest"
        );

        let loaded = app
            .store
            .load_session(app.active_session_id)
            .expect("reload persisted session");
        assert_eq!(loaded.model.as_deref(), Some("claude-3-7-sonnet-latest"));
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn refresh_sessions_skips_clean_state_unless_forced() {
    let db_path = temp_db_path("session-refresh-dirty-flag");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);

        assert_eq!(app.sessions.len(), 1);
        app.store.create_session().expect("create hidden session");

        let active_session_id = app.active_session_id;
        refresh_sessions(&mut app, Some(active_session_id), false);
        assert_eq!(app.sessions.len(), 1);

        refresh_sessions(&mut app, Some(active_session_id), true);
        assert_eq!(app.sessions.len(), 2);
        assert!(!app.sessions_dirty);
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn cached_transcript_render_matches_uncached_render() {
    let db_path = temp_db_path("cached-render-match");
    let theme = default_theme().palette;

    {
        let store = SessionStore::open(&db_path).expect("open store");
        let loaded = store.load_or_create_active_session().expect("load");
        let messages = vec![
            ChatMessage {
                role: "user".to_string(),
                content: "Show me some Rust".to_string(),
            },
            ChatMessage {
                role: "assistant".to_string(),
                content: "# Example\n\n```rust\nfn main() {\n    println!(\"hi\");\n}\n```\n"
                    .to_string(),
            },
        ];
        let mut app = build_app_from_store(store, loaded.session_id, messages.clone());

        let uncached = transcript_lines(&messages, false, theme, 80);
        let cached = transcript_lines_cached(&mut app, 80);

        let uncached_lines = uncached.lines.iter().map(line_text).collect::<Vec<_>>();
        let cached_lines = cached.lines.iter().map(line_text).collect::<Vec<_>>();
        assert_eq!(cached_lines, uncached_lines);
        assert_eq!(cached.assistant_markers, uncached.assistant_markers);
        assert_eq!(app.transcript_render_cache.len(), 1);
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn cached_transcript_render_invalidates_on_content_and_width_change() {
    let db_path = temp_db_path("cached-render-invalidation");

    {
        let store = SessionStore::open(&db_path).expect("open store");
        let loaded = store.load_or_create_active_session().expect("load");
        let messages = vec![ChatMessage {
            role: "assistant".to_string(),
            content: "first".to_string(),
        }];
        let mut app = build_app_from_store(store, loaded.session_id, messages);

        let (first_lines, first_markers) = render_cached_lines_text(&mut app, 80);
        let cached = app
            .transcript_render_cache
            .get(&0)
            .expect("assistant cache entry");
        assert_eq!(cached.content, "first");

        app.messages[0].content = "second".to_string();
        let (second_lines, second_markers) = render_cached_lines_text(&mut app, 80);
        let updated = app
            .transcript_render_cache
            .get(&0)
            .expect("updated assistant cache entry");
        assert_eq!(updated.content, "second");
        assert_ne!(second_lines, first_lines);
        assert_eq!(second_markers, first_markers);

        let before_width_rows = updated.rows.clone();
        let _ = render_cached_lines_text(&mut app, 100);
        let width_updated = app
            .transcript_render_cache
            .get(&0)
            .expect("width-updated assistant cache entry");
        assert_eq!(width_updated.view_width, 100);
        assert_eq!(width_updated.content, "second");
        assert_eq!(width_updated.rows, before_width_rows);
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn stream_tokens_are_not_persisted_until_completion() {
    let db_path = temp_db_path("stream-persist-on-complete");

    {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let store = SessionStore::open(&db_path).expect("open store");
        let session_id = store
            .load_or_create_active_session()
            .expect("load session")
            .session_id;
        let assistant_message_id = store
            .insert_message(session_id, "assistant", "")
            .expect("insert assistant placeholder");
        let mut app = build_app_from_store(
            store,
            session_id,
            vec![ChatMessage {
                role: "assistant".to_string(),
                content: String::new(),
            }],
        );
        let (tx, rx) = mpsc::unbounded_channel();
        app.in_flight = Some(InFlightRequest {
            receiver: rx,
            handle: runtime.spawn(async {}),
            assistant_message_id,
        });

        tx.send(StreamEvent::Token("hello".to_string()))
            .expect("send token");
        process_stream_events(&mut app);

        assert_eq!(app.messages[0].content, "hello");
        assert_eq!(last_persisted_message_content(&app.store, session_id), "");

        tx.send(StreamEvent::Done).expect("send done");
        process_stream_events(&mut app);

        assert_eq!(
            last_persisted_message_content(&app.store, session_id),
            "hello"
        );
        assert!(app.in_flight.is_none());
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn cancelling_stream_persists_partial_assistant_content() {
    let db_path = temp_db_path("stream-persist-on-cancel");

    {
        let runtime = tokio::runtime::Runtime::new().expect("runtime");
        let store = SessionStore::open(&db_path).expect("open store");
        let session_id = store
            .load_or_create_active_session()
            .expect("load session")
            .session_id;
        let assistant_message_id = store
            .insert_message(session_id, "assistant", "")
            .expect("insert assistant placeholder");
        let mut app = build_app_from_store(
            store,
            session_id,
            vec![ChatMessage {
                role: "assistant".to_string(),
                content: String::new(),
            }],
        );
        let (tx, rx) = mpsc::unbounded_channel();
        app.in_flight = Some(InFlightRequest {
            receiver: rx,
            handle: runtime.spawn(async {}),
            assistant_message_id,
        });

        tx.send(StreamEvent::Token("partial".to_string()))
            .expect("send token");
        process_stream_events(&mut app);
        assert_eq!(last_persisted_message_content(&app.store, session_id), "");

        cancel_request(&mut app, false);

        assert_eq!(
            last_persisted_message_content(&app.store, session_id),
            "partial"
        );
        assert!(app.in_flight.is_none());
        assert_eq!(app.status_message, "Streaming cancelled.");
    }

    let _ = fs::remove_file(&db_path);
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
        store
            .set_last_active_session_id(session_two)
            .expect("persist active session");
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
fn theme_command_switches_theme_in_memory() {
    let db_path = temp_db_path("palette-theme");
    let config_dir = config_dir_from_env().expect("config dir");
    let first = DEFAULT_THEME_KEY;
    let second = if resolve_theme("rose-pine-moon", &config_dir).is_ok() {
        "rose-pine-moon"
    } else if resolve_theme("rose-pine-dawn", &config_dir).is_ok() {
        "rose-pine-dawn"
    } else {
        DEFAULT_THEME_KEY
    };

    {
        let store = SessionStore::open(&db_path).expect("open store");
        let loaded = store.load_or_create_active_session().expect("load");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
        let first_resolved = resolve_theme(first, &config_dir).expect("resolve first theme");
        let second_resolved = resolve_theme(second, &config_dir).expect("resolve second theme");

        app.command_input = format!(":theme {first}");
        assert!(!run_palette_command(&mut app));
        assert_eq!(app.theme_key, first_resolved.key);

        app.command_input = format!(":theme {second}");
        assert!(!run_palette_command(&mut app));
        assert_eq!(app.theme_key, second_resolved.key);
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
            .position(|session| session.id == session_two)
            .expect("find session two");
        switch_to_selected_session(&mut app);
        assert_eq!(app.model, "llama3.2");

        app.selected_session_index = app
            .sessions
            .iter()
            .position(|session| session.id == session_one)
            .expect("find session one");
        switch_to_selected_session(&mut app);
        assert_eq!(app.model, "qwen2.5-coder");
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
fn startup_uses_last_active_session_or_creates_when_empty() {
    let db_path = temp_db_path("startup-last-active");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let first = store
            .load_or_create_active_session()
            .expect("load/create first")
            .session_id;
        let second = store.create_session().expect("create second");

        store
            .set_last_active_session_id(second)
            .expect("persist second active");
        let loaded = store.load_or_create_active_session().expect("load second");
        assert_eq!(loaded.session_id, second);

        store
            .set_last_active_session_id(first)
            .expect("persist first active");
        let loaded = store.load_or_create_active_session().expect("load first");
        assert_eq!(loaded.session_id, first);
    }

    {
        let empty_db_path = temp_db_path("startup-empty-creates");
        let store = SessionStore::open(&empty_db_path).expect("open empty");
        let loaded = store
            .load_or_create_active_session()
            .expect("create on empty");
        assert!(loaded.session_id > 0);
        let list = store.list_sessions().expect("list");
        assert_eq!(list.len(), 1);
        let _ = fs::remove_file(&empty_db_path);
    }

    let _ = fs::remove_file(&db_path);
}

#[test]
fn landing_submit_creates_new_session_instead_of_reusing_loaded_one() {
    let db_path = temp_db_path("landing-new-session");

    {
        let store = SessionStore::open(&db_path).expect("open");
        let first = store
            .load_or_create_active_session()
            .expect("load/create first")
            .session_id;
        store
            .insert_message(first, "user", "existing history")
            .expect("insert existing history");
    }

    {
        let store = SessionStore::open(&db_path).expect("reopen");
        let loaded = store.load_or_create_active_session().expect("load active");
        let mut app = build_app_from_store(store, loaded.session_id, loaded.messages);
        let original_session_id = app.active_session_id;
        let original_session_count = app.sessions.len();

        app.mode = InputMode::Landing;
        app.composer_input = "new chat prompt".to_string();

        assert!(create_fresh_session_for_landing_submit(&mut app));
        assert_ne!(app.active_session_id, original_session_id);
        assert_eq!(app.sessions.len(), original_session_count + 1);
        assert!(app.messages.is_empty());
        assert_eq!(app.composer_input, "new chat prompt");
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
fn suggest_session_title_preserves_contractions() {
    let title = suggest_session_title("What's the difference between beef and chicken?");
    assert!(title.starts_with("What's "));
    assert!(!title.starts_with("What S "));
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
            .rename_session_if_current_title(session_id, Some("Initial"), Some("Generated Title"))
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

#[test]
fn transcript_marks_assistant_separator_and_marker() {
    let messages = vec![
        ChatMessage {
            role: "user".to_string(),
            content: "hello".to_string(),
        },
        ChatMessage {
            role: "assistant".to_string(),
            content: "world".to_string(),
        },
    ];
    let (lines, markers) = render_lines_text(messages, false);
    assert_eq!(markers.len(), 1);
    let marker_idx = markers[0] as usize;
    assert!(marker_idx < lines.len());
    assert!(lines[marker_idx].contains("🤖 "));
    assert!(lines[marker_idx].contains("Rosie"));
}

#[test]
fn transcript_code_block_has_frame_and_padded_width() {
    let messages = vec![ChatMessage {
        role: "assistant".to_string(),
        content: "```rs\nlet value = 10;\nx\n```".to_string(),
    }];
    let (lines, _) = render_lines_text(messages, false);
    let start_idx = lines
        .iter()
        .position(|line| line.starts_with("  ╭─ code: rs"))
        .expect("code block start line");
    assert!(lines.iter().any(|line| line.starts_with("  ╰")));

    let body = lines
        .iter()
        .skip(start_idx + 1)
        .take_while(|line| !line.starts_with("  ╰"))
        .filter(|line| line.starts_with("  │ "))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(body.len(), 2);
    assert_eq!(body[0].chars().count(), body[1].chars().count());
}

#[test]
fn transcript_user_prefix_only_on_first_non_assistant_line() {
    let messages = vec![ChatMessage {
        role: "user".to_string(),
        content: "alpha\nbeta".to_string(),
    }];
    let (lines, _) = render_lines_text(messages, false);
    assert!(lines.iter().any(|line| line == "You: alpha"));
    assert!(lines.iter().any(|line| line == "  beta"));
    let prefixed = lines.iter().filter(|line| line.starts_with("You:")).count();
    assert_eq!(prefixed, 1);
}

#[test]
fn transcript_assistant_markdown_line_invariants() {
    let messages = vec![ChatMessage {
        role: "assistant".to_string(),
        content: "## Heading\n- item\n> quote\n---".to_string(),
    }];
    let (lines, _) = render_lines_text(messages, false);
    assert!(lines.iter().any(|line| line == "## Heading"));
    assert!(lines.iter().any(|line| line == "• item"));
    assert!(lines.iter().any(|line| line == "▎ quote"));
    assert!(
        lines
            .iter()
            .any(|line| line == "────────────────────────────────")
    );
}
