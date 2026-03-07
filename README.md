# Rosie

Rosie is a Rust CLI that can either:
- run quick one-shot chat (`--ask`), or
- generate shell commands (`--cmd`) with an interactive execute/re-enter/quit loop.

Running `rosie` with no mode flag launches the full-screen TUI chat interface (sessions/transcript/composer panes).

## Features

- `--ask` quick chat mode for one-shot responses
- `--cmd` command-generation mode with existing `e/r/q` interactive flow
- Default no-flag TUI chat mode with persisted local sessions
- Built-in TUI themes (`catppuccin` and `rose-pine`)
- `--model <MODEL>` runtime model override for both `--ask` and `--cmd`
- Config-driven Ollama host/model defaults in `~/.config/rosie/config.toml`
- Interactive `--configure` flow with model discovery from Ollama
- Local install helper via `rosie -install`
- Cross-platform Rust binary (Linux/macOS/Windows with Rust toolchain)

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

## Usage

```bash
# Default entrypoint (TUI chat mode)
rosie

# Configure Ollama host and model defaults
rosie -configure

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
- `Normal` mode starts by default
- press `i` to enter `Insert` mode
- in `Insert`, type in the composer, use `Backspace` to edit, and press `Enter` to send to Ollama
- press `Esc` to return to `Normal`
- assistant tokens stream into transcript as they arrive
- use `j`/`k` (or arrow keys) in `Normal` to scroll transcript
- use `PageUp`/`PageDown` for full-page scroll and `Ctrl+u`/`Ctrl+d` for half-page scroll
- use `gg` to jump to top and `G` to jump to bottom of transcript
- header shows the active session title/id
- new sessions auto-title from the first user message (local heuristic first, then model-refined when available)
- press `?` in `Normal` (or run `:help`) to open the full key/command help panel
- press `:` in `Normal` to open the floating command panel, then run:
  - `:help`
  - `:session` (open session manager modal)
  - `:models` (open model picker from Ollama `/api/tags` for the active session)
  - `:theme <catppuccin|rose-pine>` (set TUI theme)
  - `:quit`
- in the `:` command panel, use `j`/`k` (or arrows) to select from the command picklist and `Enter` to run
- in session manager, use `j`/`k` to select and `Enter` switch, `n` new, `r` rename, `d` delete (with confirmation), `Esc` close
- delete actions require confirmation (`[Y/n]`; `Enter` defaults to `Y`)
- in the model picker, use `j`/`k` (or arrows) to move, `Enter` to apply, and `Esc` to cancel
- selected models are persisted per session and restored when you switch sessions/restart
- press `Esc` in `Normal` to cancel an in-flight request
- `Ctrl+C` quits from any mode

Transcript and composer text are wrapped to pane width so output stays constrained to visible layout bounds.
TUI sessions/messages are persisted in local SQLite at:
- `${XDG_DATA_HOME}/rosie/sessions.sqlite3` (when `XDG_DATA_HOME` is set)
- `~/.local/share/rosie/sessions.sqlite3` (fallback)

## Configuration

Rosie configuration is file-based (no environment override path for host/model selection).

Config file path:
- `~/.config/rosie/config.toml`
- or `${XDG_CONFIG_HOME}/rosie/config.toml` when `XDG_CONFIG_HOME` is set

Preferred setup flow:

```bash
rosie -configure
```

That command creates/updates config and prompts for:
- `ollama_host`
- `default_model`
- `ask_model` (optional)
- `cmd_model` (optional)
- `execution_enabled` (controls whether execute is allowed in `--cmd`)

Theme is controlled in TUI with `:theme` and persisted into config as `theme`.
`theme` accepts either a built-in (`catppuccin`, `rose-pine`) or a file theme name loaded from `~/.config/rosie/themes/<name>.toml` (or `${XDG_CONFIG_HOME}/rosie/themes/<name>.toml`).

Example config:

```toml
ollama_host = "http://localhost:11434"
default_model = "llama3.2"
ask_model = "llama3.2"
cmd_model = "qwen2.5-coder"
theme = "rose-pine"
execution_enabled = true
```

Theme file schema:

```toml
name = "my-theme"

[colors]
base = "#191724"
surface = "#1f1d2e"
surface_alt = "#26233a"
text = "#e0def4"
muted = "#908caa"
accent = "#c4a7e7"
success = "#9ccfd8"
warn = "#f6c177"
error = "#eb6f92"
border = "#403d52"
border_active = "#524f67"
```

Model resolution order:
1. `--model`
2. mode-specific model (`ask_model` / `cmd_model`)
3. `default_model`
4. first available local Ollama model from `/api/tags`

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
