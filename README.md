# Rosie

Rosie is a small CLI written in Rust that takes a natural‑language description
of a task, sends it to an OpenAI LLM, and returns the exact shell command that
accomplishes the task. It can read the prompt as trailing arguments or from
standard input, making it useful as a wrapper for `ssh`, `make`, or as a helper
in scripts.

## Features

- 🎯 Turns plain‑text prompts into shell commands using the OpenAI API
- 💡 Supports custom models via `OPENAI_MODEL`
- 🔐 Supports persistent local configuration plus environment variable overrides
- 📦 Built in Rust, fast, and has zero runtime dependencies other than standard crates
- 📦 Cross‑platform (Linux/macOS/Windows with Rust toolchain)

## Installation

Rosie is a single binary crate.

```bash
# clone the repo
git clone https://github.com/your-username/rosie
cd rosie

# build (release for smaller binary)
cargo build --release
```

The binary will be in `target/release/rosie`. Add that directory to your `PATH`
or call it directly:

```bash
./target/release/rosie create a virtualenv
```

## Usage

```bash
# Prompt as trailing arguments
rosie show me the top 10 processes by memory usage

# Configure persisted settings
rosie -configure

# Prompt from stdin
echo "Add a new file to the repository" | rosie

# Environment variables override stored config
OPENAI_API_KEY="sk-..." OPENAI_MODEL="gpt-4o-mini" rosie list open ports

# Logging
RUST_LOG=info rosie list open ports
```

The program will **print** the generated command. Pipe it to `bash` if you
want to execute it:

```bash
rosie list all git branches | bash
```

## Configuration

Rosie reads configuration in this order:

1. Environment variables
2. Local config file at `~/.config/rosie/config.toml`
3. Built-in defaults

You can create or update the local config interactively:

```bash
rosie -configure
```

Rosie stores these values:

| Variable | Purpose | Required |
|----------|---------|----------|
| `OPENAI_API_KEY` | OpenAI API key | ✅ |
| `OPENAI_ENDPOINT` | Custom OpenAI compatible endpoint (e.g. Anthropic, OpenRouter) | ❌ (defaults to `https://api.openai.com`) |
| `OPENAI_MODEL` | The model name used for chat completions | ❌ (defaults to `gpt-4o-mini`) |

Example config file:

```toml
api_key = "sk-..."
endpoint = "https://api.openai.com"
model = "gpt-4o-mini"
```

`.env` files are still loaded if present, which makes them a convenient
compatibility layer for local development.

## License

This project is licensed under the MIT license – see the [LICENSE](LICENSE)
file for details.

## Contributing

Pull requests are welcome! Please check the issues tracker and open an issue
before starting.
