use crate::config::load_config;
use anyhow::{Result, anyhow};
use log::info;
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::io::Write;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

pub const DEFAULT_OLLAMA_HOST: &str = "http://localhost:11434";

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

pub struct GeneratedCommand {
    pub command: String,
    pub summary: String,
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

pub async fn generate_chat_with_spinner(
    prompt: &str,
    runtime_model: Option<&str>,
) -> Result<String> {
    with_spinner(llm_generate_chat(prompt, runtime_model)).await
}

pub async fn generate_command_with_spinner(
    prompt: &str,
    runtime_model: Option<&str>,
) -> Result<GeneratedCommand> {
    with_spinner(llm_generate_command(prompt, runtime_model)).await
}

pub async fn discover_ollama_models(host: &str) -> Result<Vec<String>> {
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
            .ok_or_else(|| {
                anyhow!("No Ollama models found. Run `ollama pull <model>` and retry.")
            })?,
    };

    Ok(RequestContext {
        client: reqwest::Client::new(),
        url: format!("{}/v1/chat/completions", endpoint.trim_end_matches('/')),
        model,
    })
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
