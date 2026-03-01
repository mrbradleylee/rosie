# Rosie

Rosie is a small CLI written in Rust that takes a natural‑language description of a task, sends it to an OpenAI LLM, and returns the exact shell command that accomplishes the task. It can read the prompt from the `--prompt` flag or from standard input, making it useful as a wrapper for `ssh`, `make`, or as a helper in scripts.

## Features

- 🎯 Turns plain‑text prompts into shell commands using the OpenAI API
- 💡 Supports custom models via `OPENAI_MODEL`
- 🔐 Securely loads the OpenAI API key from a `.env` file, environment variable, or `OPENAI_API_KEY`
- 📦 Built in Rust, fast, and has zero runtime dependencies other than standard crates
- 📦 Cross‑platform (Linux/macOS/Windows with Rust toolchain)

## Installation

Rosie is a single binary crate.
Install Rust from https://rustup.rs if you don't already have it.

```bash
# clone the repo
git clone https://github.com/your-username/rosie
cd rosie

# build (release for smaller binary)
cargo build --release
```

The binary will be in `target/release/rosie`. Add that directory to your `PATH` or call it directly:

```bash
./target/release/rosie -p "Create a virtualenv"
```

## Usage

```bash
# Prompt from stdin
echo "Add a new file to the repository" | ./target/release/rosie

# Prompt from the --prompt flag
./target/release/rosie -p "Show me the top 10 processes by memory usage"

# Use environment variable
OPENAI_API_KEY="sk-..." OPENAI_MODEL="gpt-4o-mini" ./target/release/rosie -p "..."

# Logging
RUST_LOG=info ./target/release/rosie -p "..."
```

The program will **print** the generated command. Pipe it to `bash -c` if you want to execute it:

```bash
echo "List all Git branches" | ./target/release/rosie | bash -c
```

## Configuration

Rosie expects certain environment variables:

| Variable | Purpose | Required |
|----------|---------|----------|
| `OPENAI_API_KEY` | OpenAI API key | ✅ |
| `OPENAI_ENDPOINT` | Custom OpenAI compatible endpoint (e.g. Anthropic, OpenRouter) | ❌ (defaults to `https://api.openai.com`) |
| `OPENAI_MODEL` | The model name used for chat completions | ❌ (defaults to `gpt-4o-mini`) |

Create a `.env` file in the project root or current working directory:

```dotenv
# .env
OPENAI_API_KEY="sk-..."
```

Rosie uses `dotenvy` to load this file automatically.

## License

This project is licensed under the MIT license – see the [LICENSE](LICENSE) file for details.

## Contributing

Pull requests are welcome! Please check the issues tracker and open an issue before starting.
