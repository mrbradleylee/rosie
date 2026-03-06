// src/main.rs
use anyhow::{Result, anyhow};
use dotenvy::dotenv;
use log::info;
use serde::{Deserialize, Serialize};
use std::env;
use std::ffi::OsStr;
use std::fs;
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

    let raw_args = rewrite_configure_flag(env::args_os());

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
    }
    let args = Args::parse_from(raw_args);

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
            let generated = generate_command_with_spinner(&prompt, runtime_model.as_deref()).await?;

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

async fn llm_generate_command(prompt: &str, runtime_model: Option<&str>) -> Result<GeneratedCommand> {
    use reqwest::Client;

    let config = load_config()?;
    let key = env::var("OPENAI_API_KEY")
        .ok()
        .or(config.api_key)
        .ok_or_else(|| {
            anyhow!(
                "OPENAI_API_KEY missing; run `rosie -configure` or set the environment variable"
            )
        })?;

    let endpoint = env::var("OPENAI_ENDPOINT")
        .ok()
        .or(config.endpoint)
        .unwrap_or_else(|| "https://api.openai.com".to_string());

    let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));

    let client = Client::new();
    
    // Use CLI override if provided, otherwise fall back to env or config model
    let model_str = runtime_model.map(String::from)
        .or_else(|| {
            env::var("OPENAI_MODEL").ok().map(String::from).or(config.model.map(String::from))
        })
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    
    let command_prompt = format!(
        "You are an assistant that returns JSON with exactly two string fields: \
         \"command\" for the exact shell command, and \"summary\" for a brief \
         explanation of what the command does. Return JSON only, with no \
         markdown fences or extra text.\n\nTask: {}",
        prompt
    );
    let content = send_chat_completion(&client, &url, &key, model_str.as_str(), command_prompt).await?;
    let mut generated = extract_generated_command(&content)?;

    if generated.command.is_empty() {
        return Err(anyhow!("Empty command received"));
    }

    if generated.summary.is_empty() || generated.summary == "Generated shell command." {
        // Pass model_override here as well for summary generation
        let summary = generate_summary(&client, &url, &key, model_str.as_str(), prompt, &generated.command).await?;
        generated.summary = summary;
    }

    info!("Command extracted: {}", generated.command);
    Ok(generated)
}

async fn llm_generate_chat(prompt: &str, runtime_model: Option<&str>) -> Result<String> {
    use reqwest::Client;

    let config = load_config()?;
    let key = env::var("OPENAI_API_KEY")
        .ok()
        .or(config.api_key)
        .ok_or_else(|| {
            anyhow!(
                "OPENAI_API_KEY missing; run `rosie -configure` or set the environment variable"
            )
        })?;

    let endpoint = env::var("OPENAI_ENDPOINT")
        .ok()
        .or(config.endpoint)
        .unwrap_or_else(|| "https://api.openai.com".to_string());

    let url = format!("{}/v1/chat/completions", endpoint.trim_end_matches('/'));

    let client = Client::new();
    
    // Use CLI override if provided, otherwise fall back to env or config model
    let model_str = runtime_model.map(String::from)
        .or_else(|| {
            env::var("OPENAI_MODEL").ok().map(String::from).or(config.model.map(String::from))
        })
        .unwrap_or_else(|| "gpt-4o-mini".to_string());
    
    // Different system prompt for chat mode vs command generation
    let chat_prompt = format!(
        "You are a helpful assistant that answers questions naturally.\n\nAnswer the user's question clearly and concisely. Return your answer directly, without markdown fences or extra text.\n\nTask: {}",
        prompt
    );
    
    Ok(send_chat_completion(&client, &url, &key, model_str.as_str(), chat_prompt).await?)
}

async fn generate_chat_with_spinner(prompt: &str, runtime_model: Option<&str>) -> Result<String> {
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

    let result = llm_generate_chat(prompt, runtime_model).await;
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.await;
    result
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

    let response: ModelsListResponse = resp.json().await.map_err(|e| {
        anyhow!("Failed to parse model discovery JSON: {}", e)
    })?;

    Ok(response.data.iter().map(|m| m.id.clone()).collect())
}

async fn generate_summary(
    client: &reqwest::Client,
    url: &str,
    key: &str,
    model: &str,
    prompt: &str,
    command: &str,
) -> Result<String> {
    let summary_prompt = format!(
        "Write one concise sentence explaining what this shell command does. \
         Keep it under 12 words when possible. Return the sentence only.\n\nTask: {}\nCommand: {}",
        prompt, command
    );

    let summary = send_chat_completion(client, url, key, model, summary_prompt).await?;
    let summary = summary.trim().trim_matches('"').to_string();

    if summary.is_empty() {
        return Err(anyhow!("Empty summary received"));
    }

    Ok(summary)
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

async fn generate_command_with_spinner(prompt: &str, runtime_model: Option<&str>) -> Result<GeneratedCommand> {
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

    let result = llm_generate_command(prompt, runtime_model).await;
    stop.store(true, Ordering::Relaxed);
    let _ = spinner.await;
    result
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

    if let Some(unfenced) = strip_code_fence(content) {
        if let Ok(response) = serde_json::from_str::<ParsedGeneratedResponse>(unfenced) {
            return Ok(Some(response));
        }
    }

    if let Some(json_slice) = extract_json_object(content) {
        if let Ok(response) = serde_json::from_str::<ParsedGeneratedResponse>(json_slice) {
            return Ok(Some(response));
        }
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
    let mut lines = content.lines().map(str::trim).filter(|line| !line.is_empty());
    let first = lines.next().ok_or_else(|| anyhow!("No content returned"))?;

    if first.starts_with("```") {
        for line in lines {
            if line.starts_with("```") {
                break;
            }
            return Ok(line.to_string());
        }
        return Err(anyhow!("Empty command received"));
    }

    Ok(first.to_string())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct StoredConfig {
    api_key: Option<String>,
    endpoint: Option<String>,
    model: Option<String>,
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

    let api_key = prompt_config_value("API key", existing.api_key.as_deref(), false)?;
    
    // Use default endpoint if not set for discovery, then prompt user
    let endpoint = prompt_config_value(
        "Endpoint",
        existing.endpoint.as_deref(),
        true,
    )?;

    // After configuring endpoint, perform model discovery if we have API key
    let selected_from_discovery = if !api_key.is_empty() {
        match discover_models(&endpoint, &api_key).await {
            Ok(models) => {
                println!();
                println!("Available models:");
                
                // Display numbered list of available models
                for (i, model_id) in models.iter().enumerate() {
                    println!("  {}. {}", i + 1, model_id);
                }

                if !models.is_empty() {
                    println!();
                    
                    let prompt_text = String::from("Select a model by number or enter the full model ID: ");
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
                            let idx = models.iter().position(|m| m == &input);
                            if let Some(idx) = idx {
                                Some(models[idx].clone())
                            } else {
                                None
                            }
                        }
                    } else {
                        None
                    }
                } else {
                    // No available models - skip selection, use default below
                    None
                }
            }
            Err(_) => {
                // Discovery failed (network error, API error), keep current config model value
                println!("Model discovery unavailable. Using the default model.");
                None
            }
        }
    } else {
        // No API key - skip discovery, use default below  
        None
    };

    let model = prompt_config_value(
        "Model",
        existing.model.as_deref().or(selected_from_discovery.as_ref().map(|s| s.as_str())),
        true,
    )?;

    let config = StoredConfig {
        api_key: normalize_config_value(api_key),
        endpoint: normalize_config_value(endpoint),
        model: normalize_config_value(model),
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
