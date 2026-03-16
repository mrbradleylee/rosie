# Rosie Current Runtime and TUI Spec

This document reflects shipped behavior in the current codebase.

## Runtime entrypoints

- `rosie`
  - Launches the full-screen TUI.
  - Starts on a centered landing page.
- `rosie --ask <prompt...>`
  - Runs one-shot chat and exits.
- `rosie --cmd <prompt...>`
  - Runs command-generation mode with `e/r/q` review flow.

`--ask` and `--cmd` are mutually exclusive.

Current provider scope:
- `--ask`, `--cmd`, and TUI chat are wired through the multi-provider runtime
- model discovery in the TUI depends on whether the active provider supports listing models

## CLI/config behavior

- Config path:
  - `${XDG_CONFIG_HOME:-~/.config}/rosie/config.toml`
- Core keys:
  - `active_provider`
  - `[providers.<name>]`
  - `theme`
  - `execution_enabled`
- Auth commands:
  - `rosie auth add <provider>`
  - `rosie auth list`
  - `rosie auth remove <provider>`
- Model resolution order:
  1. `--model`
  2. active provider config `model`
  3. first provider-discovered model when supported

## TUI flow

- Landing mode:
  - Centered title + `Start Chat` input.
  - `Enter` starts/continues chat.
  - `:` or `Ctrl+P` opens command palette.
  - `?`/`F1` opens help.
- Chat mode:
  - `Normal` and `Insert` modes.
  - `i` enters `Insert`.
  - `Enter` sends prompt in `Insert`.
  - `Esc` returns to `Normal`.
  - `Esc` in `Normal` cancels in-flight request.
  - `Ctrl+C` quits.

## Key interactions

- Transcript navigation:
  - `j`/`k` or arrows scroll.
  - `PageUp`/`PageDown` full-page scroll.
  - `Ctrl+u`/`Ctrl+d` half-page scroll.
  - `gg` top, `G` bottom.
  - `[`/`]` jump between assistant response blocks.
- Command palette:
  - `:help`
  - `:session`
  - `:models`
  - `:theme` and `:theme <name>`
  - `:quit`
- Session manager modal:
  - `j`/`k` select
  - `Enter` switch
  - `n` new
  - `r` rename
  - `d` delete (with confirmation)
  - `Esc` close

## Sessions and persistence

- SQLite persistence:
  - `${XDG_DATA_HOME}/rosie/sessions.sqlite3` or
  - `~/.local/share/rosie/sessions.sqlite3`
- Startup behavior:
  - if sessions exist, restore last active
  - if none exist, create lazily when chat/model action requires session
- Per-session model is persisted and restored.

## Themes

- Built-in packaged themes are shipped in `themes/*.toml`.
- `rosie --install` syncs bundled themes into:
  - `${XDG_CONFIG_HOME:-~/.config}/rosie/themes`
- Runtime theme switching:
  - `:theme` opens picker
  - `:theme <name>` applies directly
- Theme files use semantic sections:
  - `[ui]`, `[state]`, `[syntax]`, `[highlight]`
  - legacy `[colors]` remains supported for compatibility

## Transcript rendering

- Assistant output supports lightweight markdown rendering:
  - headings, lists, quotes, horizontal rules
  - inline emphasis, inline code, links
- Fenced code blocks:
  - framed rendering
  - syntax highlighting when available
