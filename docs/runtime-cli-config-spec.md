# Rosie Runtime, CLI, and Config Spec

## 1) Scope
Defines runtime entrypoints, CLI flags, config schema, and resolution rules for:
- default `rosie` (chat-only TUI)
- `rosie --ask` (one-shot chat)
- `rosie --cmd` (legacy non-TUI command flow)

This spec is the source of truth for implementation unless explicitly revised.

## 2) Runtime Entrypoints
- `rosie`
  - Launches full-screen chat-only TUI.
  - Uses persistent sessions/messages.
- `rosie --ask <prompt...>`
  - Runs one-shot chat.
  - Prints response and exits.
  - No persistence.
- `rosie --cmd <prompt...>`
  - Runs existing non-TUI command generation flow.
  - Keeps existing `e/r/q` confirmation behavior.
  - No TUI integration.

Mutual exclusivity:
- `--ask` and `--cmd` are mutually exclusive.
- If both are provided, exit with usage error.

## 3) CLI Contract
Required/primary flags:
- `--ask` / `-a`
  - Treat trailing args/stdin as quick-chat prompt.
- `--cmd` / `-c`
  - Treat trailing args/stdin as command-generation prompt.
- `--model <MODEL>`
  - Runtime model override.
  - Applies to `rosie`, `--ask`, and `--cmd`.

Secondary flags:
- `--configure`
  - Opens interactive configuration flow.
- `--version` / `-V`
  - Prints version and exits.

Prompt source behavior for `--ask` and `--cmd`:
- If trailing args are present, join as prompt.
- Else if stdin is piped, read stdin as prompt.
- Else prompt interactively for a single line.

## 4) Model Resolution Rules
Global precedence:
1. CLI `--model`
2. Runtime model source:
   - TUI: active session model
   - `--ask`: none
   - `--cmd`: none
3. Mode-specific config model:
   - `--ask`: `ask_model`
   - `--cmd`: `cmd_model`
   - TUI: n/a
4. Config `default_model`
5. First locally available Ollama model (`/api/tags`)
6. Error (no model available)

Runtime-specific behavior:
- TUI existing session:
  - Keep persisted session model unless user changes it.
- TUI new session:
  - Use config `default_model`, else first model from `/api/tags`.
- `--ask` and `--cmd`:
  - Resolve model per precedence list at invocation time.

## 5) Ollama Host Resolution
Precedence:
1. Config `ollama_host`
2. Built-in default `http://localhost:11434`

Host validation:
- Accept full URL with scheme.
- Reject invalid URLs with actionable error.

## 6) Config File Schema
Path:
- `${XDG_CONFIG_HOME:-~/.config}/rosie/config.toml`

Schema (v1):
```toml
ollama_host = "http://localhost:11434"
default_model = "llama3.2"
ask_model = "llama3.2"
cmd_model = "qwen2.5-coder"
execution_enabled = true
```

Fields:
- `ollama_host` (string)
  - Base URL for Ollama API.
- `default_model` (string, optional)
  - Global fallback model for all runtimes.
- `ask_model` (string, optional)
  - Preferred default model for `--ask`.
- `cmd_model` (string, optional)
  - Preferred default model for `--cmd`.
- `execution_enabled` (bool, default `true`)
  - Controls whether `--cmd` execute action is allowed.
  - If false, `--cmd` can generate but not execute.

## 7) Environment Variables
No runtime environment variable overrides are supported for host/model selection.
Configuration comes from CLI flags plus config file only.

## 8) Error Semantics
Startup/runtime failures should be actionable and explicit:
- Ollama unreachable:
  - "Cannot reach Ollama at <host>. Start Ollama or update ollama_host in config."
- No local models:
  - "No Ollama models found. Run `ollama pull <model>` and retry."
- Unknown model selected:
  - "Model '<name>' is not installed locally. Choose from model selector or pull it first."
- Mutually exclusive flags:
  - "Use either --ask or --cmd, not both."

Exit codes:
- `0` success
- `2` usage/configuration error
- `1` runtime/network/model resolution error
- `N` preserve child process code for executed `--cmd` command failures

## 9) Backward Compatibility and Migration
Deprecated in Ollama-only transition:
- `ROSIE_ENDPOINT`
- `ROSIE_API_KEY`
- endpoint allowlists/dummy-key settings
- `ROSIE_OLLAMA_HOST`
- `ROSIE_MODEL`
- `.env`-driven configuration

Migration behavior:
- Ignore deprecated keys/env vars if present.
- Preserve old `model` key by migrating it to `default_model` on write.
- Do not fail startup because old keys exist.

## 10) Configure Flow (`--configure`)
Prompts should cover:
1. `ollama_host`
2. model discovery from `/api/tags`
3. `default_model` selection
4. `ask_model` selection (optional; default to `default_model` when unset)
5. `cmd_model` selection (optional; default to `default_model` when unset)
6. `execution_enabled` toggle for `--cmd`

Write behavior:
- Create parent dir if missing.
- Write normalized TOML.
- Keep unknown keys only if implementing a round-trip parser; otherwise document overwrite behavior.

Configuration surfaces:
- `--configure` is the canonical configuration path and must remain fully supported.
- TUI settings editor may be added later as a convenience surface that edits the same config file.
- TUI settings must not be the only way to manage configuration.

## 11) Implementation Order
1. Add CLI parsing contract and mutual-exclusion validation.
2. Add config struct + load/save + env overlays.
3. Add model/host resolution helpers and tests.
4. Hook into existing `--cmd` and new `--ask` paths.
5. Wire TUI startup to resolved host/model defaults.

## 12) Decisions (Frozen for Initial Implementation)
- `--ask` prompt input behavior:
  - Keep flexible input: trailing args, then piped stdin, then interactive single-line prompt fallback.
  - This matches existing UX patterns and supports script + interactive usage.
- `execution_enabled=false` behavior in `--cmd`:
  - Keep menu shape stable and show execute as disabled with an explicit message.
  - If user selects execute while disabled, do not run the command and print actionable guidance to enable execution in config.
- Environment and `.env` behavior:
  - Do not honor environment-variable overrides for model/host selection.
  - Remove `.env` loading for runtime configuration.
