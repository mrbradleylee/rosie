# Rosie

Rosie is a small CLI written in Rust that takes a natural‑language description
of a task, sends it to an OpenAI LLM, and returns the exact shell command plus
a short explanation of what it does. Alternatively, with `--chat` mode (or `-c`), 
it can answer general questions naturally without generating commands. It can read 
the prompt as trailing arguments or from standard input, making it useful as a 
wrapper for `ssh`, `make`, or as a helper in scripts.

## Features

- 🎯 Turns plain‑text prompts into shell commands using the OpenAI API
- 💬 `--chat` / `-c` mode for general Q&A without command generation  
- 💡 Supports custom models via `ROSIE_MODEL`
- 🔐 Keeps API keys in environment variables (not on disk)
- 📥 Can install itself into a local bin directory with `rosie -install`
- 📦 Built in Rust, fast, and has zero runtime dependencies other than standard crates
- 📦 Cross‑platform (Linux/macOS/Windows with Rust toolchain)

## Installation

Rosie is a single binary crate.

Quick install from the repo page:

```bash
curl -fsSL https://raw.githubusercontent.com/mrbradleylee/rosie/main/install.sh | sh
```

That script clones the repo, builds a release binary with Cargo, and installs
it into your local bin directory. It requires `git` and `cargo` to already be
available on your machine.

```bash
# clone the repo
git clone https://github.com/mrbradleylee/rosie
cd rosie

# build (release for smaller binary)
cargo build --release

# install into ~/.local/bin/rosie
./target/release/rosie -install
```

By default Rosie installs itself into `~/.local/bin/rosie` on Unix-like systems,
or `$XDG_BIN_HOME/rosie` if `XDG_BIN_HOME` is set. It also installs a man page
to `~/.local/share/man/man1/rosie.1`, or `$XDG_DATA_HOME/man/man1/rosie.1` if
`XDG_DATA_HOME` is set. If you prefer not to install, the built binary is still
available in `target/release/rosie`:

```bash
./target/release/rosie create a virtualenv
```

If you rebuild Rosie from source, rerun `./target/release/rosie -install` to
copy the updated binary into your local bin directory. If `~/.local/bin` is not
on your `PATH`, Rosie will warn after install. This is a common extra step on
macOS. You may also need to add `~/.local/share/man` to `MANPATH` for
`man rosie` to work directly.

## Usage

```bash
# Prompt as trailing arguments
rosie show me the top 10 processes by memory usage

# Prompt interactively when no arguments are provided
rosie

# Configure persisted settings (with auto-model discovery)
rosie -configure

# Install the current binary into your local bin directory
rosie -install

# Prompt from stdin
echo "Add a new file to the repository" | rosie

# Environment variables override stored config
ROSIE_API_KEY="sk-..." ROSIE_MODEL="gpt-4o-mini" rosie list open ports

# Override model for this specific request (CLI takes precedence)
rosie --model gpt-3.5-turbo "echo hello world"

# Logging
RUST_LOG=info rosie list open ports

# Chat mode: general Q&A without command generation (non-interactive)
rosie --chat "What's the capital of France?"
```

In an interactive terminal, Rosie will show the generated command, print a
short summary, and let you choose to execute it, re-enter your prompt, or quit.

During `--configure`, after setting the endpoint, Rosie automatically discovers 
available models from the API (if authenticated) and presents them in a numbered list.
You can select by number, enter the full model ID, or press Enter to keep the current/default value.
If discovery fails due to network issues or missing credentials, it falls back gracefully without interrupting configuration.

In `--chat` / `-c` mode, Rosie answers questions naturally without command generation.
Chat mode is non-interactive - it simply prints the answer once and exits. No execute
or re-enter options are available.

Example interactive output (command mode):

```text
Command
  git add .

  Stages modified and tracked files in the current directory.

[e]xecute, [r]e-enter prompt, or [q]uit:
```

In `--chat` / `-c` mode, Rosie answers questions naturally without command generation.
Chat mode is non-interactive - it simply prints the answer once and exits. No execute
or re-enter options are available.

Example output (chat mode):

```text
Paris is the capital city of France.
```

In non-interactive mode, Rosie prints the command/answer together in a
human-readable format.

## Configuration

Rosie reads configuration in this order:

1. Environment variables
2. Local config file at `~/.config/rosie/config.toml` (for endpoint/model and dummy-key allowlist)
3. Built-in defaults

The preferred setup flow is interactive configuration:

```bash
rosie -configure
```

That command creates or updates `~/.config/rosie/config.toml`. Rosie stores
endpoint, model, and an optional dummy-key allowlist. The API key is read from
`ROSIE_API_KEY` and is never stored on disk.

| Variable | Purpose | Required |
|----------|---------|----------|
| `ROSIE_API_KEY` | OpenAI API key | ✅ (except localhost endpoints that use dummy-key fallback) |
| `ROSIE_ENDPOINT` | Custom OpenAI compatible endpoint (e.g. Anthropic, OpenRouter) | ❌ (defaults to `https://api.openai.com`) |
| `ROSIE_MODEL` | The model name used for chat completions | ❌ (defaults to `gpt-4o-mini`) |

Example config file:

```toml
endpoint = "https://api.openai.com"
model = "gpt-4o-mini"
allow_dummy_key_endpoints = ["ollama.lan:11434", "http://10.0.0.42:11434"]
```

For localhost endpoints such as Ollama (`http://localhost:11434`), Rosie uses
`ROSIE_API_KEY` when set, and otherwise sends a built-in dummy token (`ollama`)
to satisfy OpenAI-compatible clients that require a non-empty key.
For remote endpoints, dummy-key fallback is disabled unless the endpoint host
is listed in `allow_dummy_key_endpoints`.

`.env` files are still loaded if present, but they are now a compatibility
layer for local development rather than the primary setup path.

## Releasing

Use `cargo-release` for local release prep:

```bash
cargo install cargo-release
```

Before cutting a release, move the current `Unreleased` notes in
`CHANGELOG.md` into a versioned section such as `## 0.3.2`, then add a fresh
empty `## Unreleased` section above it.

Preview the release locally without changing git state:

```bash
cargo release 0.3.2 --no-publish
```

Execute the release locally, creating the release commit and `v0.3.2` tag:

```bash
cargo release 0.3.2 --no-publish --execute
```

The release configuration in `Cargo.toml` enforces releases from `main`, uses
`v`-prefixed tags, and keeps the version at the released value instead of
bumping to the next development version automatically.

## License

This project is licensed under the MIT license – see the [LICENSE](LICENSE)
file for details.

## Contributing

Pull requests are welcome! Please check the issues tracker and open an issue
before starting.
