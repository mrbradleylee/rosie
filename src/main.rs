// src/main.rs
mod cli;
mod config;
mod credentials;
mod install;
mod llm;
mod paths;
mod provider;
mod providers;
mod theme;
mod tui;

use anyhow::{Result, anyhow};
use cli::{AuthAction, Command as CliCommand, parse_args};
use config::{ProviderConfig, StoredConfig, load_config};
use credentials::{
    AuthKind, CredentialManager, credential_target_from_name, env_var_name,
};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use install::install;
use llm::{GeneratedCommand, generate_chat_with_spinner, generate_command_with_spinner};
use paths::{app_data_dir, config_path};
use providers::ollama::{DEFAULT_OLLAMA_ENDPOINT, discover_ollama_models};
use providers::openai::{
    native_openai_model_presets, openai_login_status, run_openai_login, run_openai_logout,
};
use std::env;
use std::fs;
use std::io;
use std::io::BufRead;
use std::io::IsTerminal;
use std::io::Write;
use std::process::Command;
use theme::{config_dir_from_env, default_theme, resolve_theme};
use tokio::io::{self as tokio_io, AsyncReadExt};

const MAN_PAGE: &str = include_str!("../man/rosie.1");

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::init();
    let args = parse_args();

    // Handle version flag
    if args.version {
        println!("rosie {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    if args.config {
        configure().await?;
        return Ok(());
    }

    if args.install {
        install(MAN_PAGE)?;
        return Ok(());
    }

    if let Some(command) = args.command {
        handle_command(command)?;
        return Ok(());
    }

    // Capture runtime model override from CLI flag
    let runtime_model = args.model.clone();

    if !args.ask_mode && !args.cmd_mode {
        launch_tui(args.model.as_deref()).await?;
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

async fn launch_tui(runtime_model: Option<&str>) -> Result<()> {
    let config = load_config()?;
    let provider_router = provider::ProviderRouter::from_config(&config)?;
    let model = provider_router.resolve_model(runtime_model).await?;

    let data_dir = app_data_dir()?;
    fs::create_dir_all(&data_dir)?;
    let db_path = data_dir.join("sessions.sqlite3");
    let config_dir = config_dir_from_env()?;
    let default_theme_key = default_theme().key;
    let theme_key = config
        .theme
        .as_deref()
        .unwrap_or(default_theme_key.as_str())
        .to_string();
    let resolved_theme = resolve_theme(&theme_key, &config_dir)?;

    tui::run(
        config,
        &model,
        &resolved_theme.key,
        resolved_theme.palette,
        &db_path,
    )?;
    Ok(())
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

async fn configure() -> Result<()> {
    let path = config_path()?;
    let existing = match load_config() {
        Ok(config) => config,
        Err(err) => {
            println!("Existing config could not be loaded: {err}");
            println!("Starting from default configuration values instead.");
            StoredConfig::default()
        }
    };

    println!("Rosie configuration");
    println!("Press enter to keep the current value.");
    let (existing_name, existing_provider) = existing.active_provider_entry()?;
    let current_provider_kind = provider_type_label(existing_provider);
    let provider_kind = prompt_provider_kind(current_provider_kind)?;
    let provider_name = prompt_config_value("Provider name", Some(existing_name), false)?;
    let provider = configure_provider(provider_kind, existing_provider).await?;
    let theme = existing
        .theme
        .clone()
        .unwrap_or_else(|| default_theme().key);
    let execution_enabled = prompt_bool_config_value(
        "Enable command execution for --cmd",
        existing.execution_enabled.unwrap_or(true),
    )?;

    let config = StoredConfig {
        active_provider: Some(provider_name.clone()),
        providers: std::iter::once((provider_name, provider)).collect(),
        theme: Some(theme),
        execution_enabled: Some(execution_enabled),
    };

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let serialized = toml::to_string_pretty(&config)?;
    fs::write(&path, serialized)?;

    println!("Saved configuration to {}", path.display());
    Ok(())
}

fn handle_command(command: CliCommand) -> Result<()> {
    match command {
        CliCommand::Auth(auth) => handle_auth_command(auth.action),
    }
}

fn handle_auth_command(action: AuthAction) -> Result<()> {
    let config = load_config().ok();
    let manager = CredentialManager::new();

    match action {
        AuthAction::Add { provider } => {
            let target = credential_target_from_name(config.as_ref(), &provider)?;
            let secret = if io::stdin().is_terminal() {
                prompt_secret(&format!("API key for {provider}"))?
            } else {
                let mut value = String::new();
                io::stdin().lock().read_line(&mut value)?;
                value.trim().to_string()
            };
            manager.set(&target, &secret)?;
            println!("Stored credential for {}", target);
            if let Some(env_var) = env_var_name(&target) {
                println!("Env override remains available via {}", env_var);
            }
            Ok(())
        }
        AuthAction::Login { provider } => {
            if provider != "openai" {
                return Err(anyhow!(
                    "Native login is currently only supported for 'openai'."
                ));
            }
            run_openai_login()
        }
        AuthAction::List => {
            let statuses =
                manager.list_provider_auth_statuses(config.as_ref(), |provider_name| {
                    (provider_name == "openai").then(openai_login_status)
                })?;
            for status in statuses {
                match status.auth_kind {
                    AuthKind::Native => {
                        println!(
                            "{}: native cli={} login={}",
                            status.provider_name,
                            if status.cli_available {
                                "available"
                            } else {
                                "missing"
                            },
                            if status.logged_in {
                                "logged-in"
                            } else {
                                "logged-out"
                            }
                        );
                    }
                    AuthKind::ApiKey => {
                        let env_label = status.env_var.unwrap_or_else(|| "-".to_string());
                        println!(
                            "{}: env={} ({}) keychain={}",
                            status.provider_name,
                            env_label,
                            if status.has_env { "set" } else { "unset" },
                            if status.has_keychain {
                                "stored"
                            } else {
                                "empty"
                            }
                        );
                    }
                }
                if let Some(detail) = status.detail.filter(|value| !value.trim().is_empty()) {
                    println!("  {}", detail.trim());
                }
            }
            Ok(())
        }
        AuthAction::Logout { provider } => {
            if provider != "openai" {
                return Err(anyhow!(
                    "Native logout is currently only supported for 'openai'."
                ));
            }
            run_openai_logout()
        }
        AuthAction::Remove { provider } => {
            let target = credential_target_from_name(config.as_ref(), &provider)?;
            manager.remove(&target)?;
            println!("Removed keychain credential for {}", target);
            Ok(())
        }
    }
}

fn provider_type_label(provider: &ProviderConfig) -> &'static str {
    match provider {
        ProviderConfig::Ollama { .. } => "ollama",
        ProviderConfig::OpenAi { .. } => "openai",
        ProviderConfig::Anthropic { .. } => "anthropic",
        ProviderConfig::OpenAiCompatible { .. } => "openai-compatible",
    }
}

fn prompt_provider_kind(current: &str) -> Result<String> {
    loop {
        let value = prompt_config_value(
            "Provider type (ollama/openai/anthropic/openai-compatible)",
            Some(current),
            false,
        )?;
        match value.as_str() {
            "ollama" | "openai" | "anthropic" | "openai-compatible" => return Ok(value),
            _ => println!("Enter one of: ollama, openai, anthropic, openai-compatible."),
        }
    }
}

async fn configure_provider(
    provider_kind: String,
    existing_provider: &ProviderConfig,
) -> Result<ProviderConfig> {
    match provider_kind.as_str() {
        "ollama" => configure_ollama_provider(existing_provider).await,
        "openai" => {
            let current_model = match existing_provider {
                ProviderConfig::OpenAi { model, .. } => model.as_deref(),
                _ => None,
            };
            let models = native_openai_model_presets();
            Ok(ProviderConfig::OpenAi {
                model: normalize_config_value(prompt_model_with_confirmation(
                    "Model",
                    current_model,
                    false,
                    &models,
                    None,
                )?),
                endpoint: None,
            })
        }
        "anthropic" => {
            let (current_endpoint, current_model) = match existing_provider {
                ProviderConfig::Anthropic { endpoint, model } => {
                    (endpoint.as_deref(), model.as_deref())
                }
                _ => (None, None),
            };
            let endpoint = prompt_config_value(
                "Anthropic endpoint (optional, defaults to official API)",
                current_endpoint,
                true,
            )?;
            Ok(ProviderConfig::Anthropic {
                endpoint: normalize_config_value(endpoint),
                model: normalize_config_value(prompt_config_value("Model", current_model, false)?),
            })
        }
        "openai-compatible" => {
            let (current_endpoint, current_model, current_allow_insecure_http) =
                match existing_provider {
                    ProviderConfig::OpenAiCompatible {
                        endpoint,
                        model,
                        allow_insecure_http,
                    } => (
                        Some(endpoint.as_str()),
                        model.as_deref(),
                        *allow_insecure_http,
                    ),
                    _ => (None, None, false),
                };
            let endpoint = prompt_config_value("Endpoint", current_endpoint, false)?;
            let model = prompt_config_value("Model", current_model, false)?;
            let allow_insecure_http = prompt_bool_config_value(
                "Allow insecure HTTP for non-local hosts",
                current_allow_insecure_http,
            )?;
            Ok(ProviderConfig::OpenAiCompatible {
                endpoint,
                model: normalize_config_value(model),
                allow_insecure_http,
            })
        }
        _ => Err(anyhow!("Unsupported provider type '{}'", provider_kind)),
    }
}

#[cfg(test)]
mod tests {
    use super::handle_auth_command;
    use crate::cli::AuthAction;

    #[test]
    fn auth_add_openai_is_deprecated() {
        let err = handle_auth_command(AuthAction::Add {
            provider: "openai".to_string(),
        })
        .expect_err("openai add should be rejected");
        assert!(err
            .to_string()
            .contains("Run `rosie auth login openai`"));
    }

    #[test]
    fn auth_remove_openai_is_deprecated() {
        let err = handle_auth_command(AuthAction::Remove {
            provider: "openai".to_string(),
        })
        .expect_err("openai remove should be rejected");
        assert!(err
            .to_string()
            .contains("Run `rosie auth login openai`"));
    }

    #[test]
    fn auth_login_only_supports_openai() {
        let err = handle_auth_command(AuthAction::Login {
            provider: "anthropic".to_string(),
        })
        .expect_err("native login should be limited to openai");
        assert!(err
            .to_string()
            .contains("currently only supported for 'openai'"));
    }

    #[test]
    fn auth_logout_only_supports_openai() {
        let err = handle_auth_command(AuthAction::Logout {
            provider: "anthropic".to_string(),
        })
        .expect_err("native logout should be limited to openai");
        assert!(err
            .to_string()
            .contains("currently only supported for 'openai'"));
    }
}

async fn configure_ollama_provider(existing_provider: &ProviderConfig) -> Result<ProviderConfig> {
    let (current_endpoint, current_model) = match existing_provider {
        ProviderConfig::Ollama { endpoint, model } => (Some(endpoint.as_str()), model.as_deref()),
        _ => (Some(DEFAULT_OLLAMA_ENDPOINT), None),
    };
    let endpoint = prompt_config_value("Ollama endpoint", current_endpoint, false)?;

    let discovered_models = match discover_ollama_models(&endpoint).await {
        Ok(models) => {
            println!();
            println!("Available models:");

            for (i, model_id) in models.iter().enumerate() {
                println!("  {}. {}", i + 1, model_id);
            }

            if models.is_empty() {
                None
            } else {
                Some(models)
            }
        }
        Err(err) => {
            let message = format!("Model discovery failed: {}", err);
            if prompt_continue_or_exit(&message)? {
                None
            } else {
                return Err(anyhow!("Configuration cancelled"));
            }
        }
    };

    let model = match discovered_models.as_deref() {
        Some(models) if !models.is_empty() => {
            prompt_model_with_confirmation("Default model", current_model, true, models, None)?
        }
        _ => prompt_config_value("Default model", current_model, true)?,
    };

    Ok(ProviderConfig::Ollama {
        endpoint,
        model: normalize_config_value(model),
    })
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
        let selection =
            prompt_model_config_value(label, suggested.as_deref(), allow_empty, models)?;

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

fn prompt_secret(label: &str) -> Result<String> {
    let mut stdout = io::stdout().lock();
    write!(stdout, "{}: ", label)?;
    stdout.flush()?;

    enable_raw_mode()?;
    let mut secret = String::new();

    loop {
        if let Event::Key(event) = event::read()? {
            match event.code {
                KeyCode::Enter => break,
                KeyCode::Char('c') if event.modifiers.contains(KeyModifiers::CONTROL) => {
                    disable_raw_mode()?;
                    return Err(anyhow!("Credential entry cancelled"));
                }
                KeyCode::Backspace => {
                    secret.pop();
                }
                KeyCode::Char(ch) if !event.modifiers.contains(KeyModifiers::CONTROL) => {
                    secret.push(ch);
                }
                _ => {}
            }
        }
    }

    disable_raw_mode()?;
    println!();

    if secret.trim().is_empty() {
        return Err(anyhow!("Credential cannot be empty"));
    }

    Ok(secret)
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
