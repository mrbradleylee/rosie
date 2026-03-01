# Rosie

> A tiny Rust CLI that turns natural‑language prompts into shell commands using an LLM.

## 📦 Overview
Rosie is a **minimal, opinionated wrapper** around the OpenAI API (or any compatible endpoint) that:

* Accepts a natural‑language description of a task.
* Sends the description to the model with a prompt that instructs it to return **exactly one shell command**.
* Prints the command to stdout, so you can pipe it directly into a terminal or another program.

It is useful for one‑off, “just‑do‑it‑now” work, or as a building block for more advanced automation tools.

## 🚀 Quick Start
```bash
# Clone the repo
git clone https://github.com/your/rosie.git
cd rosie

# Build the release binary
cargo build --release

# (Optional) Install globally via `cargo install` 
# cargo install --path .
```

### Set up your credentials
Rosie expects three environment variables:

| Variable | Description | Example |
|---|---|---|
| `OPENAI_API_KEY` | API key or an arbitrary token for a local LLM provider (e.g., Ollama). | `ollama` |
| `OPENAI_ENDPOINT` | Base URL of the LLM provider’s `/v1/chat/completions` endpoint. | `http://127.0.0.1:11434` |
| `OPENAI_MODEL` | The model name. | `gpt-4o-mini` |

You can add them to a `.env` file in the project root — `dotenv` will load it automatically. An example file is provided as `.env.example`.

```bash
cp .env.example .env
# edit .env accordingly
```

### Run Rosie
You can pass the prompt either via the short flag `-p`/`--prompt` or pipe it through stdin.

```bash
# Using the flag
./target/release/rosie -p "list all git branches"
# Output: git branch -a

# Reading from stdin
echo "install the latest rust toolchain" | ./target/release/rosie
# Output: rustup update -y
```

## 🛠️ How it Works
1. **CLI** – Built with `clap 4`. Parses an optional `-p/--prompt` argument. If omitted, standard input is used.
2. **LLM Request** – Uses `reqwest` to POST a chat completion request. The system message is:
   ```
   You are an assistant that outputs the exact shell command for the following task, nothing else:
   <YOUR PROMPT>
   ```
3. **Response Handling** – Expects the first line of the returned message to be the command.
4. **Logging** – `env_logger` logs the command extraction; enable via `RUST_LOG=info`.

## 📚 Common Use‑Cases
| Prompt | Generated Command |
|---|---|
| `check the status of services` | `systemctl status` |
| `update rustup and tools` | `rustup update && rustup component add rust-src` |
| `find broken symlinks under /usr/lib` | `find /usr/lib -xtype l` |

Feel free to adapt the system prompt in `src/main.rs` if you need more control over the output format.

## 🧪 Development & Testing
```bash
# Run the binary
cargo run -- --prompt "foo"

# Build for release
cargo build --release
```

> **Tip:** When developing locally, you can point Rosie's `OPENAI_ENDPOINT` to a local instance of Ollama:
> ```bash
> export OPENAI_ENDPOINT=http://localhost:11434
> ```

## 📄 License
MIT – see [LICENSE](LICENSE).

---

Happy automating!
