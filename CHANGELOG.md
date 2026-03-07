# Changelog

## Unreleased

### Changed

- Simplified the TUI status header to focus on mode/streaming state and removed non-actionable host details
- Moved active session context into the transcript title and model context into the composer title
- Added an in-transcript pending response indicator (`[waiting for model response...]`) while streaming before assistant tokens arrive
- Updated the TUI status header label from `Rosie TUI` to `🤖 Rosie`
- Added built-in TUI theme support with `catppuccin` and `rose-pine` presets via config (`theme`)
- Applied semantic theme styling across status, transcript, composer, footer, and modal surfaces/borders for clearer visual separation
- Changed the default theme to `rose-pine`
- Added `:theme` palette command to view/switch themes at runtime and persist selection to config
- Updated `--configure` to stop prompting for theme selection
- Added file-backed theme loading from `~/.config/rosie/themes/<name>.toml` (or `${XDG_CONFIG_HOME}/rosie/themes/<name>.toml`) using a documented `[colors]` hex schema

## 0.7.0

### Added

- Added `--ask` / `-a` one-shot chat mode and `--cmd` / `-c` command-generation mode as explicit, mutually exclusive runtime modes
- Added mode-specific config keys `ask_model` and `cmd_model`, plus `execution_enabled` for `--cmd` execute control
- Added default no-flag runtime path that launches a minimal TUI shell scaffold
- Added TUI modal interaction with explicit `Normal`/`Insert` modes (`i` to enter input, `Esc` to return to normal)
- Added floating `:` command panel in the TUI scaffold with command execution and a picklist UX
- Added Ollama-backed TUI chat requests from composer input (`Enter` in `Insert`), with assistant tokens streamed into transcript
- Added transcript scrolling controls in TUI `Normal` mode (`j`/`k`, arrow keys, `PageUp`/`PageDown`, `Ctrl+u`/`Ctrl+d`, `gg`, `G`) with auto-follow to newest output during streaming
- Added SQLite-backed TUI session persistence (`sessions` and `messages` tables), including startup load/auto-create of active session and transcript hydration on launch
- Added TUI unit tests covering SQLite session persistence across restart, session switching, and confirmed session deletion flows
- Added `:models` TUI command with a floating model picker that loads available models from Ollama `/api/tags` and applies selection to the active session model
- Added a command picklist in the `:` panel with keyboard selection (`j`/`k` or arrows) and `Enter` to run
- Added session-scoped model persistence so each session can keep its own selected model and restore it on session switch/restart
- Added automatic concise session title generation from the first user message in a new session (improved local heuristic)
- Added asynchronous model-based session title refinement for new sessions, guarded to avoid overwriting manual renames
- Added dedicated `:session` manager modal for session listing/switch/create/rename/delete workflows

### Changed

- Replaced `--chat` mode with `--ask`, while keeping command generation under `--cmd`
- Fixed CLI argument parsing so `--model` works regardless of argument position
- Switched model discovery to Ollama `/api/tags`
- Updated `--configure` to prompt for Ollama host and model defaults (`default_model`, `ask_model`, `cmd_model`)
- Updated README and man page to document `--ask`/`--cmd`, TUI-default entrypoint behavior, and config-only model/host resolution
- Updated `--configure` model prompts so numeric model selection works consistently for default, ask, and cmd model choices, with explicit confirm/reselect after resolving each choice
- Updated TUI key behavior so `Esc` in `Normal` cancels in-flight requests and `Ctrl+C` quits from any mode
- Updated TUI transcript/composer rendering to wrap and stay constrained to pane bounds
- Updated TUI message flow to persist user and assistant messages as they arrive, including streamed assistant updates
- Updated `:help` command output to include the new session management commands
- Updated README to document TUI session persistence, pane focus/session switching controls, and session-management palette commands
- Updated man page (`rosie.1`) to reflect current TUI behavior, delete confirmation flow, and session DB file locations
- Updated README and man page to document `:models` usage and model-picker controls
- Updated TUI send/session-switch/new-session flows to re-focus transcript and follow newest output more reliably
- Updated TUI composer to show a visible cursor in `Insert` mode
- Updated transcript scroll bounds to use Ratatui rendered line counts, improving bottom-of-chat scrolling for long wrapped output
- Updated TUI footer help text to a compact set of core actions and moved full key/command guidance into a dedicated help modal (`?` / `:help`)
- Updated TUI help/docs to remove `:model`, rely on header display plus `:models` for model changes, and document the command-panel picklist
- Updated sessions list ordering to newest-first
- Replaced split-pane session sidebar with a transcript-first layout and an on-demand session manager modal
- Updated header/status display to show active session title/id directly in main UI
- Updated command/help docs to route session actions through `:session` modal controls (`n/r/d/Enter`)

### Removed

- Removed `.env` loading and runtime environment-variable overrides for host/model selection
- Removed `dotenvy` dependency
- Removed archive session command support from the TUI command palette
- Removed `:new`, `:rename`, and `:delete` from the command palette in favor of `:session`

## 0.6.1

### Changed

- Removed the second LLM call for command-summary backfill; when a model response omits a usable summary, Rosie now generates a local heuristic summary instead
- Added a local fallback summary engine for common command families (`git`, `cargo`, `docker`, `kubectl`, package managers, and common shell tools), with a generic fallback for unknown programs
- Consolidated duplicate request-context and spinner logic in command/chat generation paths to reduce repeated code
- Reduced dependency overhead by narrowing `tokio` features and disabling `reqwest` default features while keeping `rustls-tls`

## 0.6.0

### Added

- `--version` / `-V` flag to display the installed version and exit
- `--model <MODEL>` CLI flag to override the default model for a specific request
- Model discovery during `--configure`: automatically fetches available models after endpoint setup, presents numbered list for selection
- Interactive model selection in configure flow: accept number or full ID; falls back to current/default value on invalid input
- Graceful handling of discovery failures (network errors, missing credentials) without aborting configuration
- `allow_dummy_key_endpoints` in `~/.config/rosie/config.toml` to explicitly allow dummy-key fallback for non-local endpoints
- `--configure` prompt for dummy-key fallback endpoints (comma-separated host, host:port, or URL entries)
- On model discovery failure, `--configure` now prompts whether to continue without discovery or exit

### Changed  

- `--configure` now includes automatic model discovery instead of always prompting for a hardcoded default
- Environment variable names changed from `OPENAI_*` to `ROSIE_*` (`ROSIE_API_KEY`, `ROSIE_ENDPOINT`, `ROSIE_MODEL`)
- API key loading is now environment-only via `ROSIE_API_KEY`; API keys are no longer read from `config.toml`
- `--configure` no longer prompts for or stores API key; it assumes env-based API key handling
- Dummy-key fallback now applies to localhost endpoints by default and to remote endpoints only when listed in `allow_dummy_key_endpoints`
- **Breaking**: removed support for `OPENAI_*` environment variables (no compatibility fallback)
- **Breaking**: `config.toml` `api_key` values are ignored for authentication and must be migrated to `ROSIE_API_KEY`

### Removed

- Persistent `api_key` storage in `~/.config/rosie/config.toml`

## 0.5.0

### Added

- `--chat` / `-c` mode for general Q&A instead of command generation (non-interactive)

## 0.4.0

### Added

- Interactive review flow after command generation with options to execute, re-enter the prompt, or quit
- Short natural-language summary shown alongside each generated command

### Changed

- Rosie now accepts prompts interactively when run in a terminal with no trailing prompt arguments  
- Terminal output is now formatted for human readability, with ANSI styling for the command display and action hotkeys
- Structured LLM response parsing is more tolerant of fenced or embedded JSON and no longer falls back to executing malformed JSON fragments as shell commands

## 0.3.2

### Added

- `cargo-release` based release configuration in `Cargo.toml` for local versioning, tagging, and push workflows

### Changed

- `Cargo.lock` is now intended to be tracked for reproducible application releases
- GitHub release notes now extract the section matching the pushed tag version instead of reading `Unreleased`

## 0.3.1

### Added

- `-configure` for persistent local config in `~/.config/rosie/config.toml`
- `-install` to copy the current binary into a local bin directory
- `install.sh` for copy-paste installation from the repo page
- Man page support, installed alongside the binary during local install

### Changed

- Local config now uses TOML instead of `.env` as the primary configuration path, while environment variables still override stored values
- Response parsing now strips fenced markdown command blocks before printing the command

## 0.3.0

### Changed

- **Breaking**: removed `-p` / `--prompt` flag; prompt is now passed as trailing arguments (`rosie <prompt>`)
- Added braille spinner animation on stderr while waiting for LLM response

## [0.2.0]

### Added

- `-p` / `--prompt` flag to pass prompt via CLI argument
- Stdin fallback when no prompt flag is provided

## [0.1.0]

### Added

- Initial release
- LLM-powered shell command generation via OpenAI-compatible API
- `.env` based configuration for API key, endpoint, and model
