# Changelog

## Unreleased

### Added

- Added `--ask` / `-a` one-shot chat mode and `--cmd` / `-c` command-generation mode as explicit, mutually exclusive runtime modes
- Added mode-specific config keys `ask_model` and `cmd_model`, plus `execution_enabled` for `--cmd` execute control
- Added default no-flag runtime path that launches a minimal TUI shell scaffold
- Added TUI modal interaction with explicit `Normal`/`Insert` modes (`i` to enter input, `Esc` to return to normal)
- Added floating `:` command panel in the TUI scaffold with initial commands (`:help`, `:new`, `:model`, `:quit`)
- Added Ollama-backed TUI chat requests from composer input (`Enter` in `Insert`), with assistant tokens streamed into transcript
- Added transcript scrolling controls in TUI `Normal` mode (`j`/`k`, arrow keys, `PageUp`/`PageDown`, `Ctrl+u`/`Ctrl+d`, `gg`, `G`) with auto-follow to newest output during streaming
- Added SQLite-backed TUI session persistence (`sessions` and `messages` tables), including startup load/auto-create of active session and transcript hydration on launch
- Added interactive TUI session list behavior: pane focus toggle (`Tab`), keyboard selection (`j`/`k`, `gg`, `G`), and `Enter` to switch/load persisted sessions
- Added TUI session management commands in `:` palette: `:rename [title]`, `:archive`, and `:delete`

### Changed

- Replaced `--chat` mode with `--ask`, while keeping command generation under `--cmd`
- Fixed CLI argument parsing so `--model` works regardless of argument position
- Switched model discovery to Ollama `/api/tags`
- Updated `--configure` to prompt for Ollama host and model defaults (`default_model`, `ask_model`, `cmd_model`)
- Updated README and man page to document `--ask`/`--cmd`, TUI-default entrypoint behavior, and config-only model/host resolution
- Updated `--configure` model prompts so numeric model selection works consistently for default, ask, and cmd model choices, with explicit confirm/reselect after resolving each choice
- Updated TUI key behavior so `Esc` in `Normal` cancels in-flight requests and `Ctrl+C` quits from any mode
- Updated TUI transcript/composer rendering to wrap and stay constrained to pane bounds
- Updated TUI message flow to persist user and assistant messages as they arrive, including streamed assistant updates and `:new` creating a new persisted session
- Updated sessions pane to render real persisted session data (active marker, selection marker, message counts) instead of placeholder text
- Updated `:help` command output to include the new session management commands

### Removed

- Removed `.env` loading and runtime environment-variable overrides for host/model selection
- Removed `dotenvy` dependency

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
