// src/main.rs
mod tui;

use anyhow::{Result, anyhow};
use log::info;
use serde::{Deserialize, Serialize};
use std::env;
use std::ffi::OsStr;
use std::fs;
use std::future::Future;
use std::io;
use std::io::BufRead;
use std::io::IsTerminal;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::io::{self as tokio_io, AsyncReadExt};

const MAN_PAGE: &str = include_str!("../man/rosie.1");
const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();

    use clap::Parser;
    #[derive(Parser, Debug)]
    struct Args {
        /// Configure stored Ollama settings
        #[arg(long)]
        configure: bool,

        /// Install the current binary into a local bin directory
        #[arg(long)]
        install: bool,

        /// Quick one-shot chat (non-TUI)
        #[arg(short = 'a', long = "ask", conflicts_with = "cmd_mode")]
        ask_mode: bool,

        /// Command-generation mode (existing non-TUI flow)
        #[arg(short = 'c', long = "cmd", conflicts_with = "ask_mode")]
        cmd_mode: bool,

        /// Override the default model for this request
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,

        /// Prompt to send to the LLM
        #[arg(value_name = "PROMPT")]
        prompt: Vec<String>,

        /// Display version information (short form: -v)
        #[arg(short = 'V', long)]
        version: bool,
    }

    let raw_args = rewrite_configure_flag(env::args_os());
    let args = Args::parse_from(raw_args);

    // Handle version flag
    if args.version {
        println!("rosie {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.configure {
        configure().await?;
        return Ok(());
    }

    if args.install {
        install()?;
        return Ok(());
    }

    // Capture runtime model override from CLI flag
    let runtime_model = args.model.clone();

    if !args.ask_mode && !args.cmd_mode {
        launch_tui(args.model.as_deref())?;
        return Ok(());
    }

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut prompt = read_prompt(args.prompt, interactive).await?;

    if args.ask_mode {
        let chat_response = generate_chat_with_spinner(&prompt, runtime_model.as_deref()).await?;
        println!("{}", chat_response);
        return Ok(());
    }

    let config = load_config()?;
    let execution_enabled = config.execution_enabled.unwrap_or(true);

    // --cmd mode: preserve existing command generation flow
    loop {
        let generated = generate_command_with_spinner(&prompt, runtime_model.as_deref()).await?;

        if !interactive {
            print_generated_command(&generated);
            return Ok(());
        }

        print_generated_command(&generated);

        match prompt_next_action(execution_enabled)? {
            NextAction::Execute => {
                execute_command(&generated.command)?;
                return Ok(());
            }
            NextAction::ReenterPrompt => {
                prompt = prompt_for_line("Prompt")?;
            }
            NextAction::Quit => return Ok(()),
        }
    }
}

fn launch_tui(runtime_model: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let host = config
        .ollama_host
        .as_deref()
        .unwrap_or(DEFAULT_OLLAMA_HOST)
        .to_string();
    let model = runtime_model
        .map(str::to_owned)
        .or_else(|| config.effective_default_model())
        .unwrap_or_else(|| "(auto from Ollama /api/tags)".to_string());

    tui::run(&host, &model)?;
    Ok(())
}

#[derive(Serialize, Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
}

struct GeneratedCommand {
    command: String,
    summary: String,
}

struct RequestContext {
    client: reqwest::Client,
    url: String,
    model: String,
}

enum RuntimeMode {
    Ask,
    Cmd,
}

async fn llm_generate_command(
    prompt: &str,
    runtime_model: Option<&str>,
) -> Result<GeneratedCommand> {
    let ctx = resolve_request_context(RuntimeMode::Cmd, runtime_model).await?;
    let command_prompt = format!(
        "You are an assistant that returns JSON with exactly two string fields: \
         \"command\" for the exact shell command, and \"summary\" for a brief \
         explanation of what the command does. Return JSON only, with no \
         markdown fences or extra text.\n\nTask: {}",
        prompt
    );
    let content = send_chat_completion(&ctx.client, &ctx.url, &ctx.model, command_prompt).await?;
    let mut generated = extract_generated_command(&content)?;

    if generated.command.is_empty() {
        return Err(anyhow!("Empty command received"));
    }

    if generated.summary.is_empty() || generated.summary == "Generated shell command." {
        generated.summary = fallback_summary(&generated.command);
    }

    info!("Command extracted: {}", generated.command);
    Ok(generated)
}

async fn llm_generate_chat(prompt: &str, runtime_model: Option<&str>) -> Result<String> {
    let ctx = resolve_request_context(RuntimeMode::Ask, runtime_model).await?;
    let chat_prompt = format!(
        "You are a helpful assistant that answers questions naturally.\n\nAnswer the user's question clearly and concisely. Return your answer directly, without markdown fences or extra text.\n\nTask: {}",
        prompt
    );

    send_chat_completion(&ctx.client, &ctx.url, &ctx.model, chat_prompt).await
}

async fn generate_chat_with_spinner(prompt: &str, runtime_model: Option<&str>) -> Result<String> {
    with_spinner(llm_generate_chat(prompt, runtime_model)).await
}

async fn send_chat_completion(
    client: &reqwest::Client,
    url: &str,
    model: &str,
    content: String,
) -> Result<String> {
    let request_body = ChatCompletionRequest {
        model: model.to_string(),
        messages: vec![Message {
            role: "user".into(),
            content,
        }],
    };

    let resp = client
        .post(url)
        .json(&request_body)
        .send()
        .await
        .map_err(|e| anyhow!("HTTP send error: {}", e))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "API returned {}: {}",
            resp.status(),
            resp.text().await?
        ));
    }

    #[derive(Deserialize)]
    struct CompletionChoice {
        message: Message,
    }

    #[derive(Deserialize)]
    struct CompletionResponse {
        choices: Vec<CompletionChoice>,
    }

    let completion: CompletionResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("JSON parse error: {}", e))?;

    completion
        .choices
        .first()
        .map(|choice| choice.message.content.trim().to_string())
        .ok_or_else(|| anyhow!("No choices returned"))
}

/// Discover available models from Ollama.
async fn discover_ollama_models(host: &str) -> Result<Vec<String>> {
    use reqwest::Client;

    let url = format!("{}/api/tags", host.trim_end_matches('/'));

    let client = Client::new();

    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| anyhow!("HTTP send error for model discovery: {}", e))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "Model discovery returned {}: {}",
            resp.status(),
            resp.text().await?
        ));
    }

    #[derive(serde::Deserialize)]
    struct OllamaModel {
        name: String,
    }

    #[derive(serde::Deserialize)]
    struct OllamaTagsResponse {
        models: Vec<OllamaModel>,
    }

    let response: OllamaTagsResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse model discovery JSON: {}", e))?;

    Ok(response.models.into_iter().map(|m| m.name).collect())
}

async fn read_prompt(args_prompt: Vec<String>, interactive: bool) -> Result<String> {
    let prompt = if args_prompt.is_empty() {
        if interactive {
            prompt_for_line("Prompt")?
        } else {
            let mut buffer = String::new();
            tokio_io::stdin()
                .read_to_string(&mut buffer)
                .await
                .map_err(|e| anyhow!("stdin error: {}", e))?;
            buffer.trim().to_string()
        }
    } else {
        args_prompt.join(" ")
    };

    if prompt.is_empty() {
        return Err(anyhow!("Prompt cannot be empty"));
    }

    Ok(prompt)
}

async fn generate_command_with_spinner(
    prompt: &str,
    runtime_model: Option<&str>,
) -> Result<GeneratedCommand> {
    with_spinner(llm_generate_command(prompt, runtime_model)).await
}

async fn with_spinner<T, Fut>(future: Fut) -> Result<T>
where
    Fut: Future<Output = Result<T>>,
{
    let stop = Arc::new(AtomicBool::new(false));
    let stop_clone = Arc::clone(&stop);
    let spinner = tokio::task::spawn_blocking(move || {
        let frames = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
        let mut i = 0usize;
        let stderr = std::io::stderr();
        while !stop_clone.load(Ordering::Relaxed) {
            {
                let mut err = stderr.lock();
                let _ = write!(err, "\r{} Thinking...", frames[i % frames.len()]);
                let _ = err.flush();
            }
            std::thread::sleep(std::time::Duration::from_millis(80));
            i += 1;
        }
        let mut err = stderr.lock();
        let _ = write!(err, "\r\x1b[2K");
        let _ = err.flush();
    });

    let result = future.await;
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.await;
    result
}

async fn resolve_request_context(
    mode: RuntimeMode,
    runtime_model: Option<&str>,
) -> Result<RequestContext> {
    let config = load_config()?;
    let endpoint = config
        .ollama_host
        .clone()
        .unwrap_or_else(|| DEFAULT_OLLAMA_HOST.to_string());

    reqwest::Url::parse(&endpoint).map_err(|_| {
        anyhow!(
            "Invalid ollama_host '{}'. Set a full URL such as http://localhost:11434",
            endpoint
        )
    })?;

    let model = runtime_model
        .map(str::to_owned)
        .or_else(|| match mode {
            RuntimeMode::Ask => config.ask_model.clone(),
            RuntimeMode::Cmd => config.cmd_model.clone(),
        })
        .or_else(|| config.effective_default_model());

    let model = match model {
        Some(model) => model,
        None => discover_ollama_models(&endpoint)
            .await?
            .into_iter()
            .next()
            .ok_or_else(|| anyhow!("No Ollama models found. Run `ollama pull <model>` and retry."))?,
    };

    Ok(RequestContext {
        client: reqwest::Client::new(),
        url: format!("{}/v1/chat/completions", endpoint.trim_end_matches('/')),
        model,
    })
}

enum NextAction {
    Execute,
    ReenterPrompt,
    Quit,
}

fn prompt_next_action(execution_enabled: bool) -> Result<NextAction> {
    loop {
        let action = read_line(&action_prompt(execution_enabled))?;
        match action.trim().to_ascii_lowercase().as_str() {
            "e" | "execute" => {
                if execution_enabled {
                    return Ok(NextAction::Execute);
                }
                println!(
                    "Execution is disabled. Set `execution_enabled = true` in config to enable."
                );
            }
            "r" | "reenter" | "re-enter" => return Ok(NextAction::ReenterPrompt),
            "q" | "quit" => return Ok(NextAction::Quit),
            _ => println!("Enter e, r, or q."),
        }
    }
}

fn prompt_for_line(label: &str) -> Result<String> {
    let input = read_line(label)?;

    if input.is_empty() {
        return Err(anyhow!("Prompt cannot be empty"));
    }

    Ok(input)
}

fn read_line(label: &str) -> Result<String> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "{}: ", label)?;
    stdout.flush()?;

    let stdin = io::stdin();
    let mut input = String::new();
    stdin.lock().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

fn execute_command(command: &str) -> Result<()> {
    #[cfg(unix)]
    let status = Command::new(env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string()))
        .arg("-c")
        .arg(command)
        .status()?;

    #[cfg(windows)]
    let status = Command::new("cmd").args(["/C", command]).status()?;

    if status.success() {
        Ok(())
    } else {
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn print_generated_command(generated: &GeneratedCommand) {
    println!();
    println!("{}", ansi("1;36", "Command"));
    println!("  {}", generated.command);
    println!();
    println!("  {}", ansi("2", &generated.summary));
    println!();
}

fn extract_generated_command(content: &str) -> Result<GeneratedCommand> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("No content returned"));
    }

    if let Some(response) = parse_generated_response(trimmed)? {
        return Ok(GeneratedCommand {
            command: response.command.trim().to_string(),
            summary: response.summary.trim().to_string(),
        });
    }

    if looks_like_structured_response(trimmed) {
        return Err(anyhow!(
            "Unable to parse structured command response from model output"
        ));
    }

    Ok(GeneratedCommand {
        command: extract_command(trimmed)?,
        summary: "Generated shell command.".to_string(),
    })
}

fn parse_generated_response(content: &str) -> Result<Option<ParsedGeneratedResponse>> {
    if let Ok(response) = serde_json::from_str::<ParsedGeneratedResponse>(content) {
        return Ok(Some(response));
    }

    if let Some(unfenced) = strip_code_fence(content)
        && let Ok(response) = serde_json::from_str::<ParsedGeneratedResponse>(unfenced)
    {
        return Ok(Some(response));
    }

    if let Some(json_slice) = extract_json_object(content)
        && let Ok(response) = serde_json::from_str::<ParsedGeneratedResponse>(json_slice)
    {
        return Ok(Some(response));
    }

    if let Some(response) = extract_response_fields(content) {
        return Ok(Some(response));
    }

    Ok(None)
}

#[derive(Deserialize)]
struct ParsedGeneratedResponse {
    command: String,
    summary: String,
}

fn strip_code_fence(content: &str) -> Option<&str> {
    let trimmed = content.trim();
    let fenced = trimmed.strip_prefix("```")?;
    let (_, rest) = fenced.split_once('\n')?;
    let end = rest.rfind("```")?;
    Some(rest[..end].trim())
}

fn extract_json_object(content: &str) -> Option<&str> {
    let start = content.find('{')?;
    let end = content.rfind('}')?;
    (start < end).then_some(content[start..=end].trim())
}

fn extract_response_fields(content: &str) -> Option<ParsedGeneratedResponse> {
    let command = extract_quoted_json_field(content, "command")?;
    let summary = extract_quoted_json_field(content, "summary")?;
    Some(ParsedGeneratedResponse { command, summary })
}

fn extract_quoted_json_field(content: &str, field: &str) -> Option<String> {
    let key = format!("\"{}\"", field);
    let start = content.find(&key)?;
    let after_key = &content[start + key.len()..];
    let colon = after_key.find(':')?;
    let after_colon = after_key[colon + 1..].trim_start();
    let quoted = after_colon.strip_prefix('"')?;
    let mut escaped = false;
    let mut value = String::new();

    for ch in quoted.chars() {
        if escaped {
            value.push(match ch {
                'n' => '\n',
                'r' => '\r',
                't' => '\t',
                '"' => '"',
                '\\' => '\\',
                other => other,
            });
            escaped = false;
            continue;
        }

        match ch {
            '\\' => escaped = true,
            '"' => return Some(value),
            other => value.push(other),
        }
    }

    None
}

fn looks_like_structured_response(content: &str) -> bool {
    let trimmed = content.trim_start();
    trimmed.starts_with('{')
        || trimmed.starts_with("```")
        || trimmed.contains("\"command\"")
        || trimmed.contains("\"summary\"")
}

fn ansi(code: &str, text: &str) -> String {
    if io::stdout().is_terminal() {
        format!("\x1b[{}m{}\x1b[0m", code, text)
    } else {
        text.to_string()
    }
}

fn action_prompt(execution_enabled: bool) -> String {
    let base = format!(
        "[{}]{}, [{}]{}, or [{}]{}",
        ansi("1;95", "e"),
        "xecute",
        ansi("1;95", "r"),
        "e-enter prompt",
        ansi("1;95", "q"),
        "uit"
    );
    if execution_enabled {
        base
    } else {
        format!("{} (execute disabled)", base)
    }
}

fn extract_command(content: &str) -> Result<String> {
    let mut lines = content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let first = lines.next().ok_or_else(|| anyhow!("No content returned"))?;

    if first.starts_with("```") {
        if let Some(line) = lines.next() {
            if line.starts_with("```") {
                return Err(anyhow!("Empty command received"));
            }
            return Ok(line.to_string());
        }
        return Err(anyhow!("Empty command received"));
    }

    Ok(first.to_string())
}

fn fallback_summary(command: &str) -> String {
    let segment = first_command_segment(command);
    let tokens = shellish_tokens(segment);
    let Some((program, args)) = extract_program_and_args(&tokens) else {
        return "Runs shell command.".to_string();
    };

    if let Some(summary) = summarize_known_command(program, args) {
        return summary.to_string();
    }

    format!("Runs {}.", summarize_program_name(program))
}

fn first_command_segment(command: &str) -> &str {
    command
        .split("&&")
        .next()
        .unwrap_or(command)
        .split("||")
        .next()
        .unwrap_or(command)
        .split(['|', ';'])
        .next()
        .unwrap_or(command)
        .trim()
}

fn shellish_tokens(segment: &str) -> Vec<String> {
    segment
        .split_whitespace()
        .map(|token| {
            token
                .trim_matches(|c| matches!(c, '"' | '\'' | '`' | '(' | ')' | '{' | '}'))
                .to_string()
        })
        .filter(|token| !token.is_empty())
        .collect()
}

fn extract_program_and_args(tokens: &[String]) -> Option<(&str, &[String])> {
    let mut index = 0usize;

    while let Some(token) = tokens.get(index) {
        if matches!(
            token.as_str(),
            "sudo" | "env" | "command" | "nohup" | "time"
        ) {
            index += 1;
            continue;
        }

        if is_env_assignment(token) {
            index += 1;
            continue;
        }

        return Some((token.as_str(), &tokens[index + 1..]));
    }

    None
}

fn is_env_assignment(token: &str) -> bool {
    let Some((name, _value)) = token.split_once('=') else {
        return false;
    };

    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn summarize_known_command(program: &str, args: &[String]) -> Option<&'static str> {
    let subcommand = args.first().map(String::as_str).unwrap_or("");

    match program {
        "git" => match subcommand {
            "status" => Some("Shows the current git working tree status."),
            "add" => Some("Stages files with git."),
            "commit" => Some("Creates a git commit."),
            "push" => Some("Pushes git commits to a remote."),
            "pull" => Some("Fetches and merges remote git changes."),
            "fetch" => Some("Fetches changes from a git remote."),
            "clone" => Some("Clones a git repository."),
            "checkout" | "switch" => Some("Switches git branches or revisions."),
            "restore" => Some("Restores files from git."),
            "diff" => Some("Shows git changes."),
            "log" => Some("Shows git commit history."),
            "branch" => Some("Lists or manages git branches."),
            "merge" => Some("Merges git branches."),
            "rebase" => Some("Rebases git commits onto another base."),
            "reset" => Some("Moves or resets git history."),
            "clean" => Some("Removes untracked files from a git repository."),
            _ => Some("Runs a git command."),
        },
        "cargo" => match subcommand {
            "build" => Some("Builds the Rust project."),
            "run" => Some("Builds and runs the Rust project."),
            "test" => Some("Runs Rust tests."),
            "check" => Some("Checks the Rust project for compilation errors."),
            "fmt" => Some("Formats Rust source files."),
            "clippy" => Some("Runs Rust lints with Clippy."),
            "update" => Some("Updates Cargo dependencies."),
            "install" => Some("Installs a Rust binary with Cargo."),
            _ => Some("Runs a Cargo command."),
        },
        "docker" => match subcommand {
            "build" => Some("Builds a Docker image."),
            "run" => Some("Runs a Docker container."),
            "exec" => Some("Runs a command inside a Docker container."),
            "ps" => Some("Lists Docker containers."),
            "images" => Some("Lists Docker images."),
            "logs" => Some("Shows Docker container logs."),
            "pull" => Some("Pulls a Docker image."),
            "push" => Some("Pushes a Docker image."),
            "compose" => Some("Runs a Docker Compose command."),
            _ => Some("Runs a Docker command."),
        },
        "kubectl" => match subcommand {
            "get" => Some("Lists Kubernetes resources."),
            "describe" => Some("Shows detailed Kubernetes resource information."),
            "apply" => Some("Applies Kubernetes configuration."),
            "delete" => Some("Deletes Kubernetes resources."),
            "logs" => Some("Shows Kubernetes pod logs."),
            "exec" => Some("Runs a command in a Kubernetes container."),
            _ => Some("Runs a kubectl command."),
        },
        "npm" | "pnpm" | "yarn" | "bun" => match subcommand {
            "install" | "add" => Some("Installs project dependencies."),
            "run" => Some("Runs a package script."),
            "test" => Some("Runs project tests."),
            "publish" => Some("Publishes a package."),
            _ => Some("Runs a package manager command."),
        },
        "python" | "python3" => Some("Runs a Python command."),
        "node" => Some("Runs a Node.js command."),
        "pip" | "pip3" => Some("Installs or manages Python packages."),
        "make" => Some("Runs a Make target."),
        "grep" | "rg" => Some("Searches for matching text."),
        "find" => Some("Finds files or directories."),
        "ls" => Some("Lists directory contents."),
        "cat" => Some("Prints file contents."),
        "cp" => Some("Copies files or directories."),
        "mv" => Some("Moves or renames files."),
        "rm" => Some("Removes files or directories."),
        "mkdir" => Some("Creates directories."),
        "chmod" => Some("Changes file permissions."),
        "chown" => Some("Changes file ownership."),
        "curl" | "wget" => Some("Fetches data from a URL."),
        "ssh" => Some("Connects to a remote shell over SSH."),
        "scp" => Some("Copies files over SSH."),
        "rsync" => Some("Synchronizes files between locations."),
        "tar" => Some("Creates or extracts tar archives."),
        "zip" => Some("Creates a zip archive."),
        "unzip" => Some("Extracts a zip archive."),
        "ps" => Some("Lists running processes."),
        "kill" | "killall" | "pkill" => Some("Stops running processes."),
        "sed" => Some("Transforms text with sed."),
        "awk" => Some("Processes text with awk."),
        "sort" => Some("Sorts input lines."),
        "uniq" => Some("Filters repeated lines."),
        "head" => Some("Shows the first lines of input."),
        "tail" => Some("Shows the last lines of input."),
        "du" => Some("Shows disk usage."),
        "df" => Some("Shows filesystem usage."),
        "cd" => Some("Changes into a directory."),
        _ => None,
    }
}

fn summarize_program_name(program: &str) -> String {
    let name = program
        .rsplit(['/', '\\'])
        .next()
        .filter(|value| !value.is_empty())
        .unwrap_or("command");
    format!("`{}`", name)
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredConfig {
    ollama_host: Option<String>,
    default_model: Option<String>,
    ask_model: Option<String>,
    cmd_model: Option<String>,
    execution_enabled: Option<bool>,
    #[serde(default, alias = "model", skip_serializing)]
    legacy_model: Option<String>,
}

impl StoredConfig {
    fn effective_default_model(&self) -> Option<String> {
        self.default_model
            .clone()
            .or_else(|| self.legacy_model.clone())
    }
}

fn rewrite_configure_flag<I>(args: I) -> Vec<std::ffi::OsString>
where
    I: IntoIterator<Item = std::ffi::OsString>,
{
    args.into_iter()
        .map(|arg| {
            if arg == OsStr::new("-configure") {
                "--configure".into()
            } else if arg == OsStr::new("-install") {
                "--install".into()
            } else {
                arg
            }
        })
        .collect()
}

fn install() -> Result<()> {
    let source = env::current_exe()?;
    let bin_dir = local_bin_dir()?;
    fs::create_dir_all(&bin_dir)?;

    let file_name = source
        .file_name()
        .ok_or_else(|| anyhow!("Unable to determine executable name"))?;
    let destination = bin_dir.join(file_name);

    if source == destination {
        println!("Rosie is already installed at {}", destination.display());
        return Ok(());
    }

    fs::copy(&source, &destination)?;
    set_executable_permissions(&destination)?;

    println!("Installed Rosie to {}", destination.display());

    let man_page_path = install_man_page()?;
    println!("Installed man page to {}", man_page_path.display());

    if !path_contains(&bin_dir) {
        println!(
            "{} is not on your PATH. Add it to run `rosie` directly.",
            bin_dir.display()
        );
    }

    let man_root = man_page_path
        .ancestors()
        .nth(2)
        .map(|path| path.to_path_buf())
        .unwrap_or_else(|| man_page_path.clone());
    if !manpath_contains(&man_root) {
        println!(
            "{} is not on your MANPATH. You may need to add it to use `man rosie` directly.",
            man_root.display()
        );
    }

    Ok(())
}

async fn configure() -> Result<()> {
    let path = config_path()?;
    let existing = load_config()?;

    println!("Rosie configuration");
    println!("Press enter to keep the current value.");
    let ollama_host = prompt_config_value(
        "Ollama host",
        existing.ollama_host.as_deref().or(Some(DEFAULT_OLLAMA_HOST)),
        true,
    )?;

    let discovered_models = match discover_ollama_models(&ollama_host).await {
        Ok(models) => {
            println!();
            println!("Available models:");

            for (i, model_id) in models.iter().enumerate() {
                println!("  {}. {}", i + 1, model_id);
            }

            if models.is_empty() { None } else { Some(models) }
        }
        Err(err) => {
            let message = format!("Model discovery failed: {}", err);
            if prompt_continue_or_exit(&message)? {
                None
            } else {
                return Ok(());
            }
        }
    };

    let default_model = match discovered_models.as_deref() {
        Some(models) if !models.is_empty() => prompt_model_with_confirmation(
            "Default model",
            existing.effective_default_model().as_deref(),
            true,
            models,
            None,
        )?,
        _ => prompt_config_value(
            "Default model",
            existing.effective_default_model().as_deref(),
            true,
        )?,
    };
    let ask_model = match discovered_models.as_deref() {
        Some(models) if !models.is_empty() => prompt_model_with_confirmation(
            "Ask model (optional, falls back to default model)",
            existing.ask_model.as_deref(),
            true,
            models,
            Some("(fallback to default model)"),
        )?,
        _ => prompt_config_value(
            "Ask model (optional, falls back to default model)",
            existing.ask_model.as_deref(),
            true,
        )?,
    };
    let cmd_model = match discovered_models.as_deref() {
        Some(models) if !models.is_empty() => prompt_model_with_confirmation(
            "Cmd model (optional, falls back to default model)",
            existing.cmd_model.as_deref(),
            true,
            models,
            Some("(fallback to default model)"),
        )?,
        _ => prompt_config_value(
            "Cmd model (optional, falls back to default model)",
            existing.cmd_model.as_deref(),
            true,
        )?,
    };
    let execution_enabled = prompt_bool_config_value(
        "Enable command execution for --cmd",
        existing.execution_enabled.unwrap_or(true),
    )?;

    let config = StoredConfig {
        ollama_host: normalize_config_value(ollama_host),
        default_model: normalize_config_value(default_model),
        ask_model: normalize_config_value(ask_model),
        cmd_model: normalize_config_value(cmd_model),
        execution_enabled: Some(execution_enabled),
        legacy_model: None,
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let serialized = toml::to_string_pretty(&config)?;
    fs::write(&path, serialized)?;

    println!("Saved configuration to {}", path.display());
    Ok(())
}

fn prompt_config_value(label: &str, current: Option<&str>, allow_empty: bool) -> Result<String> {
    let mut stdout = io::stdout().lock();
    match current {
        Some(value) if !value.is_empty() => write!(stdout, "{} [{}]: ", label, value)?,
        _ => write!(stdout, "{}: ", label)?,
    }
    stdout.flush()?;

    let stdin = io::stdin();
    let mut input = String::new();
    stdin.lock().read_line(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() {
        if let Some(value) = current {
            return Ok(value.to_string());
        }
        if allow_empty {
            return Ok(String::new());
        }
        return Err(anyhow!("{} is required", label));
    }

    Ok(input)
}

fn prompt_model_config_value(
    label: &str,
    current: Option<&str>,
    allow_empty: bool,
    models: &[String],
) -> Result<String> {
    loop {
        let input = prompt_config_value(label, current, allow_empty)?;
        if let Ok(num) = input.parse::<usize>() {
            if (1..=models.len()).contains(&num) {
                return Ok(models[num - 1].clone());
            }

            println!(
                "Invalid selection. Enter a number from 1 to {}.",
                models.len()
            );
            continue;
        }

        return Ok(input);
    }
}

fn prompt_model_with_confirmation(
    label: &str,
    current: Option<&str>,
    allow_empty: bool,
    models: &[String],
    empty_selection_label: Option<&str>,
) -> Result<String> {
    let mut suggested = current.map(str::to_owned);

    loop {
        let selection = prompt_model_config_value(label, suggested.as_deref(), allow_empty, models)?;

        if selection.is_empty() {
            println!(
                "Selected {}: {}",
                label,
                empty_selection_label.unwrap_or("(none)")
            );
        } else {
            println!("Selected {}: {}", label, selection);
        }

        if prompt_confirm_or_reselect()? {
            return Ok(selection);
        }

        suggested = normalize_config_value(selection);
    }
}

fn prompt_confirm_or_reselect() -> Result<bool> {
    loop {
        let choice = prompt_config_value("[c]onfirm or [r]eselect", Some("c"), true)?;
        match choice.trim().to_ascii_lowercase().as_str() {
            "" | "c" | "confirm" => return Ok(true),
            "r" | "reselect" | "re-select" => return Ok(false),
            _ => println!("Please enter 'c' to confirm or 'r' to reselect."),
        }
    }
}

fn normalize_config_value(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn prompt_bool_config_value(label: &str, current: bool) -> Result<bool> {
    loop {
        let current_value = if current { "true" } else { "false" };
        let input = prompt_config_value(label, Some(current_value), true)?;
        match input.trim().to_ascii_lowercase().as_str() {
            "true" | "t" | "yes" | "y" | "1" => return Ok(true),
            "false" | "f" | "no" | "n" | "0" => return Ok(false),
            _ => println!("Please enter true/false (or yes/no)."),
        }
    }
}

fn prompt_continue_or_exit(reason: &str) -> Result<bool> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Ok(true);
    }

    println!("{}", reason);
    println!("Continue without model discovery?");

    loop {
        let choice = prompt_config_value("[c]ontinue or [e]xit", Some("c"), true)?;
        let normalized = choice.trim().to_ascii_lowercase();

        if normalized.is_empty() || normalized == "c" || normalized == "continue" {
            return Ok(true);
        }

        if normalized == "e" || normalized == "exit" {
            return Ok(false);
        }

        println!("Please enter 'c' to continue or 'e' to exit.");
    }
}

fn load_config() -> Result<StoredConfig> {
    let path = config_path()?;
    match fs::read_to_string(path) {
        Ok(contents) => Ok(toml::from_str(&contents)?),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(StoredConfig::default()),
        Err(err) => Err(err.into()),
    }
}

fn config_path() -> Result<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .ok_or_else(|| anyhow!("Unable to determine config directory"))?;
    Ok(base.join("rosie").join("config.toml"))
}

fn local_bin_dir() -> Result<PathBuf> {
    env::var_os("XDG_BIN_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/bin")))
        .ok_or_else(|| anyhow!("Unable to determine local bin directory"))
}

fn local_man_dir() -> Result<PathBuf> {
    env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
        .map(|path| path.join("man").join("man1"))
        .ok_or_else(|| anyhow!("Unable to determine local man directory"))
}

fn install_man_page() -> Result<PathBuf> {
    let man_dir = local_man_dir()?;
    fs::create_dir_all(&man_dir)?;
    let man_page_path = man_dir.join("rosie.1");
    fs::write(&man_page_path, MAN_PAGE)?;
    Ok(man_page_path)
}

fn path_contains(dir: &std::path::Path) -> bool {
    env::var_os("PATH")
        .map(|path| env::split_paths(&path).any(|entry| entry == dir))
        .unwrap_or(false)
}

fn manpath_contains(dir: &std::path::Path) -> bool {
    env::var_os("MANPATH")
        .map(|path| env::split_paths(&path).any(|entry| entry == dir))
        .unwrap_or(false)
}

#[cfg(unix)]
fn set_executable_permissions(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn set_executable_permissions(_path: &std::path::Path) -> Result<()> {
    Ok(())
}
