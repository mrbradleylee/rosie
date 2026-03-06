# Changelog

## Unreleased

### Added
- `--model <MODEL>` CLI flag to override the default model for a specific request
- Model discovery during `--configure`: automatically fetches available models after endpoint setup, presents numbered list for selection
- Interactive model selection in configure flow: accept number or full ID; falls back to current/default value on invalid input
- Graceful handling of discovery failures (network errors, missing credentials) without aborting configuration

### Changed  
- `--configure` now includes automatic model discovery instead of always prompting for a hardcoded default

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
