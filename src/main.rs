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

        /// Prompt to send to the LLM
        #[arg(trailing_var_arg = true)]
        prompt: Vec<String>,
    }
    let args = Args::parse_from(raw_args);

    if args.configure {
        configure()?;
        return Ok(());
    }

    if args.install {
        install()?;
        return Ok(());
    }

    let interactive = io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut prompt = read_prompt(args.prompt, interactive).await?;

    loop {
        let cmd = generate_command_with_spinner(&prompt).await?;

        if !interactive {
            println!("{}", cmd);
            return Ok(());
        }

        println!("{}", cmd);

        match prompt_next_action()? {
            NextAction::Execute => {
                execute_command(&cmd)?;
                return Ok(());
            }
            NextAction::ReenterPrompt => {
                prompt = prompt_for_line("Prompt")?;
            }
            NextAction::Quit => return Ok(()),
        }
    }
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

async fn llm_generate_command(prompt: &str) -> Result<String> {
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
    let model = env::var("OPENAI_MODEL")
        .ok()
        .or(config.model)
        .unwrap_or_else(|| "gpt-4o-mini".into());
    let prompt_message = format!(
        "You are an assistant that outputs the exact shell command for the following task, nothing else:\n\n{}",
        prompt
    );
    let request_body = ChatCompletionRequest {
        model,
        messages: vec![Message {
            role: "user".into(),
            content: prompt_message,
        }],
    };

    let resp = client
        .post(&url)
        .bearer_auth(&key)
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

    #[derive(serde::Deserialize)]
    struct CompletionChoice {
        message: Message,
    }

    #[derive(serde::Deserialize)]
    struct CompletionResponse {
        choices: Vec<CompletionChoice>,
    }

    let completion: CompletionResponse = resp
        .json()
        .await
        .map_err(|e| anyhow!("JSON parse error: {}", e))?;

    let content = completion
        .choices
        .get(0)
        .ok_or_else(|| anyhow!("No choices returned"))?
        .message
        .content
        .trim()
        .to_string();

    let first_line = extract_command(&content)?;

    if first_line.is_empty() {
        return Err(anyhow!("Empty command received"));
    }

    info!("Command extracted: {}", first_line);
    Ok(first_line)
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

async fn generate_command_with_spinner(prompt: &str) -> Result<String> {
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

    let result = llm_generate_command(prompt).await;
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
        let action = read_line("Choose [e]xecute, [r]e-enter prompt, or [q]uit")?;
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

fn extract_command(content: &str) -> Result<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(anyhow!("No content returned"));
    }

    let mut lines = trimmed
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
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

fn configure() -> Result<()> {
    let path = config_path()?;
    let existing = load_config()?;

    println!("Rosie configuration");
    println!("Press enter to keep the current value.");

    let api_key = prompt_config_value("API key", existing.api_key.as_deref(), false)?;
    let endpoint = prompt_config_value(
        "Endpoint",
        existing
            .endpoint
            .as_deref()
            .or(Some("https://api.openai.com")),
        true,
    )?;
    let model = prompt_config_value(
        "Model",
        existing.model.as_deref().or(Some("gpt-4o-mini")),
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
