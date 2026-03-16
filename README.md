# Rosie

Rosie is a Rust CLI that can either:
- run quick one-shot chat (`--ask`), or
- generate shell commands (`--cmd`) with an interactive execute/re-enter/quit loop.

Running `rosie` with no mode flag launches the full-screen TUI chat interface (landing screen, then conversation/chat panes).

## Features

- `--ask` quick chat mode for one-shot responses
- `--cmd` command-generation mode with existing `e/r/q` interactive flow
- Default no-flag TUI chat mode with persisted local sessions
- Built-in TUI themes with runtime switching (`:theme`)
- `--model <MODEL>` runtime model override for both `--ask` and `--cmd`
- Provider-driven model defaults in `~/.config/rosie/config.toml`
- Interactive `--config` flow with provider-aware setup
- Local install helper via `rosie -install`
- Cross-platform Rust binary (Linux/macOS/Windows with Rust toolchain)

The provider foundation is in place, but this first integration pass only wires Ollama end-to-end. Other provider types are part of the new config shape, but their live request and auth flows are still follow-up work.

## Installation

Rosie is a single binary crate.

Quick install from the repo page:

```bash
curl -fsSL https://raw.githubusercontent.com/mrbradleylee/rosie/main/install.sh | sh
```

Manual build/install:

```bash
git clone https://github.com/mrbradleylee/rosie
cd rosie
cargo build --release
./target/release/rosie -install
```

By default Rosie installs itself into `~/.local/bin/rosie` (or `$XDG_BIN_HOME/rosie` if set), and installs the man page to `~/.local/share/man/man1/rosie.1` (or `$XDG_DATA_HOME/man/man1/rosie.1`).
`rosie -install` also syncs bundled theme files into `${XDG_CONFIG_HOME:-~/.config}/rosie/themes`.

## Usage

```bash
# Default entrypoint (TUI chat mode)
rosie

# Configure Rosie provider settings
rosie --config

# Install current binary
rosie -install

# Quick one-shot chat
rosie --ask "What's the capital of France?"

# Command generation mode
rosie --cmd "show top 10 processes by memory"

# Override model for a single invocation
rosie --model qwen2.5-coder --cmd "list files modified in git"
rosie --ask "summarize this topic" --model llama3.2

# Prompt from stdin
echo "explain DNS in one paragraph" | rosie --ask
echo "find large files in current directory" | rosie --cmd
```

In `--cmd` mode on interactive terminals, Rosie prints a generated command + summary, then prompts:

```text
[e]xecute, [r]e-enter prompt, or [q]uit:
```

In `--ask` mode, Rosie prints the model response once and exits.

In the default TUI:
- startup opens a centered landing screen with a `Start Chat` input
- from landing, type and press `Enter` to begin chatting, use `Ctrl+P`/`:` for command palette, and `F1` for help
- once in chat, `Normal` mode is active by default
- press `i` to enter `Insert` mode
- in `Insert`, type in the composer, use `Backspace` to edit, and press `Enter` to send to Ollama
- press `Esc` to return to `Normal`
- assistant tokens stream into transcript as they arrive
- use `j`/`k` (or arrow keys) in `Normal` to scroll transcript
- use `[`/`]` in `Normal` to jump between assistant response blocks
- use `PageUp`/`PageDown` for full-page scroll and `Ctrl+u`/`Ctrl+d` for half-page scroll
- use `gg` to jump to top and `G` to jump to bottom of transcript
- top status shows current session + streaming state
- new sessions auto-title from the first user message (local heuristic first, then model-refined when available)
- if existing sessions are present, Rosie restores the last active session on launch; if none exist, Rosie creates one when chat starts
- press `?` in `Normal` (or run `:help`) to open the full key/command help panel
- press `:` or `Ctrl+P` to open the floating command panel, then run:
  - `:help`
  - `:session` (open session manager modal)
  - `:models` (open model picker from Ollama `/api/tags` for the active session)
  - `:theme` (open picker from config dir themes)
  - `:theme <name>` (set TUI theme directly)
  - `:quit`
- in the `:` command panel, use `j`/`k` (or arrows) to select from the command picklist and `Enter` to run
- in session manager, use `j`/`k` to select and `Enter` switch, `n` new, `r` rename, `d` delete (with confirmation), `Esc` close
- delete actions require confirmation (`[Y/n]`; `Enter` defaults to `Y`)
- in the model picker, use `j`/`k` (or arrows) to move, `Enter` to apply, and `Esc` to cancel
- selected models are persisted per session and restored when you switch sessions/restart
- press `Esc` in `Normal` to cancel an in-flight request
- `Ctrl+C` quits from any mode

The TUI currently expects the active provider to be Ollama during this first provider refactor pass.

Transcript and composer text are wrapped to pane width so output stays constrained to visible layout bounds.
Assistant output includes lightweight markdown rendering (headings/lists/quotes/rules/inline emphasis/code/links) and fenced code blocks are framed and syntax-highlighted when possible.
TUI sessions/messages are persisted in local SQLite at:
- `${XDG_DATA_HOME}/rosie/sessions.sqlite3` (when `XDG_DATA_HOME` is set)
- `~/.local/share/rosie/sessions.sqlite3` (fallback)

## Configuration

Rosie configuration is file-based.

Config file path:
- `~/.config/rosie/config.toml`
- or `${XDG_CONFIG_HOME}/rosie/config.toml` when `XDG_CONFIG_HOME` is set

Preferred setup flow:

```bash
rosie --config
```

That command creates/updates config and prompts for:
- `active_provider`
- one provider block under `[providers.<name>]`
- `execution_enabled` (controls whether execute is allowed in `--cmd`)

Theme is controlled in TUI with `:theme` and persisted into config as `theme`.
`theme` accepts a packaged theme name from the repo `themes/` directory or a user file theme name loaded from `~/.config/rosie/themes/<name>.toml` (or `${XDG_CONFIG_HOME}/rosie/themes/<name>.toml`).
Packaged defaults include Rose Pine variants (`rose-pine`, `rose-pine-moon`, `rose-pine-dawn`).

Example config:

```toml
active_provider = "ollama"
theme = "<built-in-or-file-theme-name>"
execution_enabled = true

[providers.ollama]
type = "ollama"
endpoint = "http://localhost:11434"
model = "llama3.2"
```

Theme file schema (preferred):

```toml
name = "my-theme"

[ui]
bg = "#191724"
panel = "#1f1d2e"
panel_alt = "#26233a"
text = "#e0def4"
text_muted = "#908caa"
border = "#403d52"
border_active = "#524f67"
title_label = "#e0def4"
title_value = "#c4a7e7"
title_value_alt = "#ebbcba"
title_meta = "#908caa"
modal_bg = "#1f1d2e"
modal_border = "#524f67"
modal_title = "#e0def4"
modal_selected_bg = "#403d52"
modal_selected_fg = "#e0def4"

[state]
accent = "#c4a7e7"
success = "#9ccfd8"
info = "#31748f"
warning = "#f6c177"
error = "#eb6f92"

[syntax]
user = "#c4a7e7"
assistant = "#e0def4"
system = "#908caa"

[highlight]
low = "#21202e"
mid = "#403d52"
high = "#524f67"
```

Legacy `[colors]` files are still supported for backward compatibility.

Model resolution order:
1. `--model`
2. active provider `model`
3. first available provider-discovered model when supported

If no model can be resolved, Rosie exits with an actionable error.

## Releasing

Use `cargo-release` for local release prep:

```bash
cargo install cargo-release
```

Before cutting a release, move `## Unreleased` notes in `CHANGELOG.md` into a versioned section and add a fresh empty `## Unreleased` section.

Preview release locally:

```bash
cargo release 0.3.2 --no-publish
```

Execute release locally:

```bash
cargo release 0.3.2 --no-publish --execute
```

## License

This project is licensed under the MIT license; see [LICENSE](LICENSE).

## Contributing

Pull requests are welcome. Please check the issues tracker and open an issue before starting.

Before opening a PR, run:

```bash
./scripts/check-command-docs.sh
./scripts/check-quality.sh
```
