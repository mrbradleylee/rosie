# Changelog

## [0.2.1] - 2026-03-02

### Changed
- Prompt is now passed as trailing arguments (`rosie <prompt>`) instead of requiring the `-p` flag
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
