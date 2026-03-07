# Rosie TUI Migration Plan (Ollama-Only)

## 1) Goals
- Convert Rosie from a prompt/response CLI into a full terminal UI (TUI).
- Standardize on Ollama as the only backend.
- Add built-in model selection in the UI.
- Add persistent, manageable chat history (sessions + messages).
- Keep a safe command-generation workflow inside the TUI.

## 2) Non-Goals (for initial release)
- Supporting OpenAI-compatible endpoints beyond Ollama.
- Multi-user synchronization/cloud history.
- Plugin system/tool ecosystem.
- Rich markdown rendering (defer to later).

## 3) Success Criteria (v1)
- User can launch `rosie` and stay in a single TUI loop.
- User can create/switch/rename/archive/delete chat sessions.
- User can select a model from installed Ollama models.
- User can stream model output in real time.
- Chat and command conversations persist across restarts.
- Command execution always requires explicit confirmation.

## 4) Product Decisions to Lock Early
- Runtime entrypoints (locked):
  - `rosie` launches full-screen TUI by default.
  - `rosie --ask "..."` runs one-shot quick chat and does not persist history.
  - `rosie --cmd "..."` remains the existing non-TUI command flow.
- TUI scope (locked):
  - TUI is chat-only for v1 (no command generation/execution inside TUI).
- Session behavior:
  - One model per session in TUI.
- Context policy:
  - Sliding window by token/character budget.
  - Optional summary record for long conversations later.

## 5) Architecture Plan
Split current `src/main.rs` into modules:
- `src/main.rs`: argument parsing + app bootstrap.
- `src/app.rs`: application state and event loop orchestration.
- `src/ui/`: TUI rendering and key handling.
- `src/ollama/`: Ollama API client + streaming parser.
- `src/storage/`: SQLite schema, queries, migrations.
- `src/domain/`: types for sessions/messages/modes.
- `src/config.rs`: local config (paths, defaults, flags).
- `src/command_exec.rs`: guarded shell execution.

## 6) Dependencies
Add:
- `ratatui` (rendering)
- `crossterm` (terminal/events)
- `tokio-stream` or `futures-util` (stream handling)
- `rusqlite` (or `sqlx` if async is preferred)
- `directories` (robust cross-platform app dirs)
- `tracing` + `tracing-subscriber` (observability)

Keep:
- `tokio`, `reqwest`, `serde`, `serde_json`, `anyhow`

## 7) Ollama Integration Scope
Required endpoints:
- `GET /api/tags` for model list
- `POST /api/chat` for chat generation (streaming)

Client requirements:
- Configurable Ollama host (default `http://localhost:11434`)
- Health check on startup
- Stream parser tolerant of chunk boundaries and partial lines
- User-facing errors for unavailable models or connection issues

## 8) Storage Design (SQLite)
Path:
- `${XDG_DATA_HOME:-~/.local/share}/rosie/rosie.db`

Tables:
- `sessions`
  - `id` (TEXT/UUID primary key)
  - `title` (TEXT)
  - `mode` (`chat` | `command`)
  - `model` (TEXT)
  - `archived` (INTEGER bool)
  - `created_at`, `updated_at` (INTEGER epoch)
- `messages`
  - `id` (TEXT/UUID primary key)
  - `session_id` (FK -> sessions.id)
  - `role` (`system` | `user` | `assistant`)
  - `content` (TEXT)
  - `created_at` (INTEGER epoch)
- `session_meta` (optional for future)
  - `session_id`
  - `key`
  - `value`

Indexes:
- `messages(session_id, created_at)`
- `sessions(updated_at)`
- `sessions(archived, updated_at)`

## 9) TUI Layout (v1)
- Header: active session, mode, model, connection status.
- Left pane: session list + search/filter.
- Main pane: transcript with streaming assistant output.
- Bottom pane: multiline composer and hints.
- Footer: keybind help.

Suggested keybinds:
- `Ctrl+n`: new session
- `Ctrl+o`: model selector
- `Ctrl+f`: find session
- `Tab`: cycle focus panes
- `Enter`: send message
- `Ctrl+e`: execute generated command (in command mode)
- `Ctrl+r`: regenerate last response
- `Ctrl+d`: archive/delete session
- `q`: quit

## 10) Command Safety Model
In `--cmd` mode (outside TUI):
- Keep existing prompt/confirm behavior (`e/r/q`) for initial migration.
- Preserve explicit user confirmation before execution.
- Keep non-zero exit status behavior and output handling.
- Keep `--model` override support.

## 11) Delivery Phases
### Phase 0: Foundation refactor
- Break `main.rs` into modules without changing user-facing behavior.
- Add integration tests around current command/chat generation parsing.

Exit criteria:
- Existing CLI behavior unchanged.
- Codebase organized for TUI and storage additions.

### Phase 1: Ollama-only backend
- Remove endpoint compatibility abstraction.
- Implement Ollama client + model listing + chat request path.
- Replace config/env docs and flags accordingly.

Exit criteria:
- CLI mode works against Ollama only.
- Model discovery via `/api/tags` works.

### Phase 2: Persistent storage
- Add SQLite DB init/migrations.
- Persist sessions and messages.
- Add retrieval APIs and basic search.

Exit criteria:
- Conversation history survives restart.
- Session list sorted by recent activity.

### Phase 3: TUI MVP
- Implement event loop, rendering, input composer.
- Hook sending/streaming/persistence to UI.
- Implement model selector modal.

Exit criteria:
- User can chat fully inside TUI with persistent sessions.

### Phase 4: Command mode UX + hardening
- Command mode UI flow with explicit execute/abort.
- Add resilience: retries, cancellation, better empty/error states.
- Add tests and manual QA checklist for keybinds and persistence.

Exit criteria:
- Safe command workflow stable in TUI.

## 12) Risks and Mitigations
- Streaming complexity in terminal:
  - Mitigation: isolate stream-to-event adapter with unit tests.
- SQLite lock/contention issues:
  - Mitigation: single writer task; short transactions.
- Scope creep on advanced UI:
  - Mitigation: freeze MVP feature list per phase.
- Context growth/token limits:
  - Mitigation: implement deterministic truncation early.

## 13) Testing Plan
- Unit tests:
  - stream chunk parser
  - context-window truncation
  - command JSON extraction and validation
- Integration tests:
  - DB schema creation + CRUD
  - session list ordering and archive behavior
- Manual test matrix:
  - startup without Ollama
  - invalid model selected
  - long streaming response
  - command execution failure path

## 14) Migration / Compatibility Notes
- Existing config file keys for endpoint/api-key become deprecated.
- New config focuses on:
  - `ollama_host`
  - `default_model`
  - `execution_enabled`
- If old config exists, best-effort migration should preserve model where possible.

Model selection precedence (all runtimes):
1. CLI flag `--model` (highest priority)
2. Runtime/session model:
   - TUI: current session model
   - `--ask`: n/a
   - `--cmd`: n/a
3. Mode-specific config model:
   - `--ask`: `ask_model`
   - `--cmd`: `cmd_model`
4. Config `default_model`
5. Built-in fallback model (lowest priority)

Effective runtime precedence (frozen):
- TUI: `--model` > session model > `default_model` > `/api/tags` > error
- `--ask`: `--model` > `ask_model` > `default_model` > `/api/tags` > error
- `--cmd`: `--model` > `cmd_model` > `default_model` > `/api/tags` > error

Default-model resolution by runtime:
- TUI startup:
  1. Use persisted session model when opening an existing session.
  2. For new sessions, use config `default_model` when set.
  3. Otherwise, pick first available local Ollama model from `/api/tags`.
  4. If no models exist, show actionable error and block sending.
- `--ask`:
  1. Use `--model` when provided.
  2. Otherwise use `ask_model` when set.
  3. Otherwise use config `default_model`.
  4. Otherwise use first available local Ollama model from `/api/tags`.
  5. If no models exist, exit with actionable error.
- `--cmd`:
  1. Use `--model` when provided.
  2. Otherwise use `cmd_model` when set.
  3. Otherwise use config `default_model`.
  4. Otherwise use first available local Ollama model from `/api/tags`.
  5. If no models exist, exit with actionable error.

## 15) Immediate Next Actions (Week 1)
1. Create module skeleton (`app`, `ui`, `ollama`, `storage`, `domain`, `config`).
2. Introduce storage layer with schema + migration bootstrap.
3. Implement Ollama model listing and one-shot chat in a non-TUI command path.
4. Add thin event loop prototype rendering static panes in `ratatui`.
5. Decide and document final keybind map before wiring interactions.
