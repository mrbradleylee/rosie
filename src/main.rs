// src/main.rs
use anyhow::{Result, anyhow};
use dotenvy::dotenv;
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

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::init();

    use clap::Parser;
    #[derive(Parser, Debug)]
    struct Args {
        /// Configure stored API settings
        #[arg(long)]
        configure: bool,

        /// Install the current binary into a local bin directory
        #[arg(long)]
        install: bool,

        /// Force chat mode (general Q&A instead of command generation)
        #[arg(short = 'c', long = "chat")]
        chat_mode: bool,

        /// Override the default model for this request
        #[arg(long, value_name = "MODEL")]
        model: Option<String>,

        /// Prompt to send to the LLM
        #[arg(trailing_var_arg = true)]
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

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut prompt = read_prompt(args.prompt, interactive).await?;

    if args.chat_mode {
        // Chat mode: general Q&A - non-interactive by default
        let chat_response = generate_chat_with_spinner(&prompt, runtime_model.as_deref()).await?;

        // Print simple response without formatting (not terminal command style)
        println!("{}", chat_response);
    } else {
        // Command mode: original behavior
        loop {
            let generated =
                generate_command_with_spinner(&prompt, runtime_model.as_deref()).await?;

            if !interactive {
                print_generated_command(&generated);
                return Ok(());
            }

            print_generated_command(&generated);

            match prompt_next_action()? {
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
    key: String,
    model: String,
}

async fn llm_generate_command(
    prompt: &str,
    runtime_model: Option<&str>,
) -> Result<GeneratedCommand> {
    let ctx = resolve_request_context(runtime_model)?;
    let command_prompt = format!(
        "You are an assistant that returns JSON with exactly two string fields: \
         \"command\" for the exact shell command, and \"summary\" for a brief \
         explanation of what the command does. Return JSON only, with no \
         markdown fences or extra text.\n\nTask: {}",
        prompt
    );
    let content =
        send_chat_completion(&ctx.client, &ctx.url, &ctx.key, &ctx.model, command_prompt).await?;
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
    let ctx = resolve_request_context(runtime_model)?;
    let chat_prompt = format!(
        "You are a helpful assistant that answers questions naturally.\n\nAnswer the user's question clearly and concisely. Return your answer directly, without markdown fences or extra text.\n\nTask: {}",
        prompt
    );

    send_chat_completion(&ctx.client, &ctx.url, &ctx.key, &ctx.model, chat_prompt).await
}

async fn generate_chat_with_spinner(prompt: &str, runtime_model: Option<&str>) -> Result<String> {
    with_spinner(llm_generate_chat(prompt, runtime_model)).await
}

async fn send_chat_completion(
    client: &reqwest::Client,
    url: &str,
    key: &str,
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
        .bearer_auth(key)
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

/// Discover available models from the API endpoint
async fn discover_models(endpoint: &str, key: &str) -> Result<Vec<String>> {
    use reqwest::Client;

    let url = format!("{}/v1/models", endpoint.trim_end_matches('/'));

    let client = Client::new();

    // GET /v1/models - returns list of available models with their IDs
    let resp = client
        .get(&url)
        .bearer_auth(key)
        .send()
        .await
        .map_err(|e| anyhow!("HTTP send error for model discovery: {}", e))?;

    if !resp.status().is_success() {
        return Err(anyhow!(
            "Model discovery API returned {}: {}",
            resp.status(),
            resp.text().await?
        ));
    }

    #[derive(serde::Deserialize)]
    struct ModelInfo {
        id: String,
    }

    #[derive(serde::Deserialize)]
    struct ModelsListResponse {
        data: Vec<ModelInfo>,
    }

    let response: ModelsListResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse model discovery JSON: {}", e))?;

    Ok(response.data.iter().map(|m| m.id.clone()).collect())
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

fn resolve_request_context(runtime_model: Option<&str>) -> Result<RequestContext> {
    let config = load_config()?;
    let endpoint = env::var("ROSIE_ENDPOINT")
        .ok()
        .or(config.endpoint)
        .unwrap_or_else(|| "https://api.openai.com".to_string());
    let key = resolve_api_key(&endpoint, config.allow_dummy_key_endpoints.as_deref())?;
    let model = runtime_model
        .map(str::to_owned)
        .or_else(|| env::var("ROSIE_MODEL").ok().or(config.model))
        .unwrap_or_else(|| "gpt-4o-mini".to_string());

    Ok(RequestContext {
        client: reqwest::Client::new(),
        url: format!("{}/v1/chat/completions", endpoint.trim_end_matches('/')),
        key,
        model,
    })
}

enum NextAction {
    Execute,
    ReenterPrompt,
    Quit,
}

fn prompt_next_action() -> Result<NextAction> {
    loop {
        let action = read_line(&action_prompt())?;
        match action.trim().to_ascii_lowercase().as_str() {
            "e" | "execute" => return Ok(NextAction::Execute),
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

fn action_prompt() -> String {
    format!(
        "[{}]{}, [{}]{}, or [{}]{}",
        ansi("1;95", "e"),
        "xecute",
        ansi("1;95", "r"),
        "e-enter prompt",
        ansi("1;95", "q"),
        "uit"
    )
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
    endpoint: Option<String>,
    model: Option<String>,
    allow_dummy_key_endpoints: Option<Vec<String>>,
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
    println!("ROSIE_API_KEY is read from the environment and is not stored on disk.");

    // Use default endpoint if not set for discovery, then prompt user
    let endpoint = prompt_config_value("Endpoint", existing.endpoint.as_deref(), true)?;

    let current_dummy_key_allowlist = existing
        .allow_dummy_key_endpoints
        .as_ref()
        .map(|values| values.join(","));
    let dummy_key_allowlist = prompt_config_value(
        "Dummy-key fallback endpoints (comma-separated hosts, host:port, or URLs)",
        current_dummy_key_allowlist.as_deref(),
        true,
    )?;

    let parsed_dummy_key_allowlist = parse_csv_list(dummy_key_allowlist);

    let selected_from_discovery =
        match resolve_api_key(&endpoint, parsed_dummy_key_allowlist.as_deref()) {
            Ok(api_key) => match discover_models(&endpoint, &api_key).await {
                Ok(models) => {
                    println!();
                    println!("Available models:");

                    // Display numbered list of available models
                    for (i, model_id) in models.iter().enumerate() {
                        println!("  {}. {}", i + 1, model_id);
                    }

                    if !models.is_empty() {
                        println!();

                        let prompt_text =
                            String::from("Select a model by number or enter the full model ID: ");
                        let input = prompt_config_value(&prompt_text, None::<&str>, false)?;

                        // Parse selection - try number first, then exact match, then fallback to current
                        if !input.is_empty() {
                            if let Ok(num) = input.parse::<usize>() {
                                // Try to find matching index (1-based user input -> 0-based vector index)
                                if num <= models.len() && num >= 1 {
                                    Some(models[num - 1].clone())
                                } else {
                                    None
                                }
                            } else {
                                // Try exact match with full model ID
                                models
                                    .iter()
                                    .position(|m| m == &input)
                                    .map(|idx| models[idx].clone())
                            }
                        } else {
                            None
                        }
                    } else {
                        // No available models - skip selection, use default below
                        None
                    }
                }
                Err(err) => {
                    // Discovery failed (network/API/etc.), keep current config model value
                    let message = format!("Model discovery failed: {}", err);
                    if prompt_continue_or_exit(&message)? {
                        None
                    } else {
                        return Ok(());
                    }
                }
            },
            Err(err) => {
                let message = format!("Model discovery unavailable: {}", err);
                if prompt_continue_or_exit(&message)? {
                    None
                } else {
                    return Ok(());
                }
            }
        };

    let model = prompt_config_value(
        "Model",
        existing
            .model
            .as_deref()
            .or(selected_from_discovery.as_deref()),
        true,
    )?;

    let config = StoredConfig {
        endpoint: normalize_config_value(endpoint),
        model: normalize_config_value(model),
        allow_dummy_key_endpoints: parsed_dummy_key_allowlist,
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

fn normalize_config_value(value: String) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn parse_csv_list(value: String) -> Option<Vec<String>> {
    let values = value
        .split(',')
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .map(|part| part.to_string())
        .collect::<Vec<_>>();

    if values.is_empty() {
        None
    } else {
        Some(values)
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

fn resolve_api_key(endpoint: &str, allow_dummy_key_endpoints: Option<&[String]>) -> Result<String> {
    if let Ok(key) = env::var("ROSIE_API_KEY") {
        let trimmed = key.trim();
        if !trimmed.is_empty() {
            return Ok(trimmed.to_string());
        }
    }

    if is_local_endpoint(endpoint) || endpoint_in_allowlist(endpoint, allow_dummy_key_endpoints) {
        // OpenAI-compatible local providers (for example Ollama) may require
        // a non-empty Authorization header but do not validate the token value.
        return Ok("ollama".to_string());
    }

    Err(anyhow!(
        "ROSIE_API_KEY missing; set the environment variable, use a localhost endpoint, or add the endpoint to allow_dummy_key_endpoints in config.toml"
    ))
}

fn is_local_endpoint(endpoint: &str) -> bool {
    let Some((host, _port)) = parse_endpoint_host_and_port(endpoint) else {
        return false;
    };

    host.eq_ignore_ascii_case("localhost") || host == "127.0.0.1" || host == "::1"
}

fn endpoint_in_allowlist(endpoint: &str, allow_dummy_key_endpoints: Option<&[String]>) -> bool {
    let Some((host, port)) = parse_endpoint_host_and_port(endpoint) else {
        return false;
    };

    let Some(allowlist) = allow_dummy_key_endpoints else {
        return false;
    };

    for entry in allowlist {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }

        if let Some((allowed_host, allowed_port)) = parse_endpoint_host_and_port(trimmed)
            && host.eq_ignore_ascii_case(&allowed_host)
            && (allowed_port.is_none() || allowed_port == port)
        {
            return true;
        }
    }

    false
}

fn parse_endpoint_host_and_port(value: &str) -> Option<(String, Option<u16>)> {
    let normalized = if value.contains("://") {
        value.to_string()
    } else {
        format!("https://{}", value)
    };

    let url = reqwest::Url::parse(&normalized).ok()?;
    let host = url.host_str()?.to_string();
    let port = url.port();
    Some((host, port))
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
