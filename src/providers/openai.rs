use crate::credentials::{CredentialManager, NativeAuthStatus};
use crate::provider::{ChatRequest, ChatResponse, Message, Provider, ProviderEvent, Role};
use anyhow::{Result, anyhow};
use serde_json::Value;
use std::env;
use std::future::Future;
use std::pin::Pin;
use std::process::{Command, Stdio};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command as TokioCommand;
use tokio::sync::mpsc;

const DEFAULT_OPENAI_CLI: &str = "codex";
const OPENAI_CLI_ENV_VAR: &str = "ROSIE_OPENAI_CLI";
pub const NATIVE_OPENAI_MODEL_PRESETS: &[&str] = &["gpt-5-codex", "gpt-5"];

pub struct OpenAiProvider {
    model: Option<String>,
    #[allow(dead_code)]
    credentials: CredentialManager,
    cli_path: String,
}

impl OpenAiProvider {
    pub fn new(model: Option<String>, credentials: CredentialManager) -> Self {
        Self {
            model,
            credentials,
            cli_path: openai_cli_path(),
        }
    }

    #[cfg(test)]
    pub fn with_cli_path(model: Option<String>, cli_path: String) -> Self {
        Self {
            model,
            credentials: CredentialManager::new(),
            cli_path,
        }
    }
}

impl Provider for OpenAiProvider {
    fn provider_type(&self) -> &'static str {
        "openai"
    }

    fn default_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        Box::pin(async move {
            let result = run_openai_exec(&self.cli_path, request, None).await?;
            if result.text.trim().is_empty() {
                return Err(anyhow!("OpenAI CLI returned no assistant content"));
            }

            Ok(ChatResponse {
                message: Message {
                    role: Role::Assistant,
                    content: result.text.trim().to_string(),
                },
            })
        })
    }

    fn stream_chat(
        &self,
        request: ChatRequest,
        tx: mpsc::UnboundedSender<ProviderEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let result = run_openai_exec(&self.cli_path, request, Some(tx.clone())).await?;
            if !result.text.trim().is_empty() && !result.emitted_any_tokens {
                let _ = tx.send(ProviderEvent::Token(result.text.trim().to_string()));
            }
            let _ = tx.send(ProviderEvent::Done);
            Ok(())
        })
    }
}

pub fn openai_login_status() -> NativeAuthStatus {
    openai_login_status_for_path(&openai_cli_path())
}

pub fn native_openai_model_presets() -> Vec<String> {
    NATIVE_OPENAI_MODEL_PRESETS
        .iter()
        .map(|model| (*model).to_string())
        .collect()
}

fn openai_login_status_for_path(cli_path: &str) -> NativeAuthStatus {
    let output = Command::new(&cli_path)
        .args(["login", "status"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();

    match output {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if !stdout.is_empty() { stdout } else { stderr };
            NativeAuthStatus {
                cli_available: true,
                logged_in: output.status.success() && detail.contains("Logged in"),
                detail,
            }
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => NativeAuthStatus {
            cli_available: false,
            logged_in: false,
            detail: format!(
                "Install the OpenAI Codex CLI or set {} to its executable path.",
                OPENAI_CLI_ENV_VAR
            ),
        },
        Err(err) => NativeAuthStatus {
            cli_available: false,
            logged_in: false,
            detail: format!("Failed to run OpenAI CLI: {err}"),
        },
    }
}

pub fn run_openai_login() -> Result<()> {
    let cli_path = openai_cli_path();
    let status = Command::new(&cli_path)
        .arg("login")
        .status()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "OpenAI CLI not found. Install Codex or set {} to its executable path.",
                    OPENAI_CLI_ENV_VAR
                )
            } else {
                anyhow!("Failed to launch OpenAI login: {err}")
            }
        })?;

    if status.success() {
        println!("OpenAI login completed.");
        Ok(())
    } else {
        Err(anyhow!(
            "OpenAI login failed with status {}",
            status.code().unwrap_or(1)
        ))
    }
}

pub fn run_openai_logout() -> Result<()> {
    let cli_path = openai_cli_path();
    let status = Command::new(&cli_path)
        .arg("logout")
        .status()
        .map_err(|err| {
            if err.kind() == std::io::ErrorKind::NotFound {
                anyhow!(
                    "OpenAI CLI not found. Install Codex or set {} to its executable path.",
                    OPENAI_CLI_ENV_VAR
                )
            } else {
                anyhow!("Failed to launch OpenAI logout: {err}")
            }
        })?;

    if status.success() {
        println!("OpenAI login removed.");
        Ok(())
    } else {
        Err(anyhow!(
            "OpenAI logout failed with status {}",
            status.code().unwrap_or(1)
        ))
    }
}

fn openai_cli_path() -> String {
    env::var(OPENAI_CLI_ENV_VAR).unwrap_or_else(|_| DEFAULT_OPENAI_CLI.to_string())
}

struct OpenAiExecResult {
    text: String,
    emitted_any_tokens: bool,
}

async fn run_openai_exec(
    cli_path: &str,
    request: ChatRequest,
    tx: Option<mpsc::UnboundedSender<ProviderEvent>>,
) -> Result<OpenAiExecResult> {
    let status = openai_login_status_for_path(cli_path);
    if !status.cli_available {
        return Err(anyhow!(status.detail));
    }
    if !status.logged_in {
        return Err(anyhow!(
            "OpenAI is not logged in. Run `rosie auth login openai`."
        ));
    }

    let prompt = render_prompt(&request.messages);
    let cwd =
        env::current_dir().map_err(|err| anyhow!("Failed to read current directory: {err}"))?;

    let mut command = TokioCommand::new(cli_path);
    command
        .kill_on_drop(true)
        .arg("exec")
        .arg("--json")
        .arg("--skip-git-repo-check")
        .arg("--sandbox")
        .arg("read-only")
        .arg("--color")
        .arg("never")
        .arg("--cd")
        .arg(&cwd)
        .arg("-");

    if !request.model.trim().is_empty() {
        command.arg("--model").arg(&request.model);
    }

    command.stdin(Stdio::piped());
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());

    let mut child = command.spawn().map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            anyhow!(
                "OpenAI CLI not found. Install Codex or set {} to its executable path.",
                OPENAI_CLI_ENV_VAR
            )
        } else {
            anyhow!("Failed to launch OpenAI CLI: {err}")
        }
    })?;

    if let Some(mut stdin) = child.stdin.take() {
        let prompt_copy = prompt.clone();
        tokio::spawn(async move {
            let _ = stdin.write_all(prompt_copy.as_bytes()).await;
            let _ = stdin.shutdown().await;
        });
    }

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| anyhow!("OpenAI CLI stdout was unavailable"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| anyhow!("OpenAI CLI stderr was unavailable"))?;

    let state = Arc::new(tokio::sync::Mutex::new(StreamState::default()));
    let stdout_state = Arc::clone(&state);
    let stdout_tx = tx.clone();
    let stdout_task = tokio::spawn(async move {
        let mut reader = BufReader::new(stdout).lines();
        while let Some(line) = reader.next_line().await? {
            parse_exec_event_line(&line, stdout_tx.as_ref(), &stdout_state).await;
        }
        Ok::<(), anyhow::Error>(())
    });

    let stderr_task = tokio::spawn(async move {
        let mut buffer = String::new();
        let mut reader = BufReader::new(stderr);
        reader.read_to_string(&mut buffer).await?;
        Ok::<String, anyhow::Error>(buffer)
    });

    let status = child
        .wait()
        .await
        .map_err(|err| anyhow!("OpenAI CLI failed: {err}"))?;
    stdout_task
        .await
        .map_err(|err| anyhow!("OpenAI CLI stdout task failed: {err}"))??;
    let stderr_output = stderr_task
        .await
        .map_err(|err| anyhow!("OpenAI CLI stderr task failed: {err}"))??;

    let (streamed_text, emitted_any_tokens) = {
        let state = state.lock().await;
        (state.text.clone(), state.emitted_any_tokens)
    };
    let final_text = streamed_text;

    if status.success() {
        return Ok(OpenAiExecResult {
            text: final_text,
            emitted_any_tokens,
        });
    }

    let stderr_output = stderr_output.trim();
    if stderr_output.contains("Not logged in") || stderr_output.contains("login") {
        return Err(anyhow!(
            "OpenAI is not logged in. Run `rosie auth login openai`."
        ));
    }

    if !stderr_output.is_empty() {
        return Err(anyhow!(
            "OpenAI CLI failed with status {}: {}",
            status.code().unwrap_or(1),
            stderr_output
        ));
    }

    Err(anyhow!(
        "OpenAI CLI failed with status {}",
        status.code().unwrap_or(1)
    ))
}

#[derive(Default)]
struct StreamState {
    text: String,
    emitted_any_tokens: bool,
}

async fn parse_exec_event_line(
    line: &str,
    tx: Option<&mpsc::UnboundedSender<ProviderEvent>>,
    state: &Arc<tokio::sync::Mutex<StreamState>>,
) {
    let Ok(value) = serde_json::from_str::<Value>(line) else {
        return;
    };

    let fragments = extract_text_fragments(&value);
    if fragments.is_empty() {
        return;
    }

    let mut state = state.lock().await;
    for fragment in fragments {
        if fragment.trim().is_empty() {
            continue;
        }

        let delta = if fragment.starts_with(&state.text) {
            fragment[state.text.len()..].to_string()
        } else {
            fragment.clone()
        };

        if delta.is_empty() {
            continue;
        }

        state.text.push_str(&delta);
        if let Some(tx) = tx {
            let _ = tx.send(ProviderEvent::Token(delta));
        }
        state.emitted_any_tokens = true;
    }
}

fn extract_text_fragments(value: &Value) -> Vec<String> {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    match event_type {
        "response.output_text.delta" => value
            .get("delta")
            .and_then(Value::as_str)
            .map(|text| vec![text.to_string()])
            .unwrap_or_default(),
        "item.completed" => extract_completed_item_text(value.get("item")),
        _ => Vec::new(),
    }
}

fn extract_completed_item_text(item: Option<&Value>) -> Vec<String> {
    let Some(item) = item else {
        return Vec::new();
    };
    if matches!(
        item.get("type").and_then(Value::as_str),
        Some("agent_message" | "assistant_message")
    ) && let Some(text) = item.get("text").and_then(Value::as_str)
    {
        return vec![text.to_string()];
    }

    let Some(content) = item.get("content").and_then(Value::as_array) else {
        return Vec::new();
    };

    content
        .iter()
        .filter(|entry| {
            matches!(
                entry.get("type").and_then(Value::as_str),
                Some("output_text" | "text")
            )
        })
        .filter_map(|entry| entry.get("text").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

fn render_prompt(messages: &[Message]) -> String {
    let mut prompt = String::from(
        "You are acting as the chat backend for Rosie.\n\
         Reply to the conversation directly.\n\
         Do not inspect files, run shell commands, modify code, or use tools.\n\
         Do not describe plans or actions unless the user explicitly asked for them.\n\n\
         Conversation:\n",
    );

    for message in messages {
        prompt.push_str(message.role.as_str());
        prompt.push_str(":\n");
        prompt.push_str(message.content.trim());
        prompt.push_str("\n\n");
    }

    prompt.push_str("assistant:\n");
    prompt
}

#[cfg(test)]
mod tests {
    use super::{
        NativeAuthStatus, OpenAiProvider, extract_completed_item_text, extract_text_fragments,
        openai_login_status_for_path,
    };
    use crate::provider::{ChatRequest, Message, Provider, ProviderEvent, Role};
    use serde_json::json;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::sync::mpsc;
    use tokio::time::{sleep, timeout};

    #[test]
    fn extracts_jsonl_delta_text() {
        let value = json!({
            "type": "response.output_text.delta",
            "delta": "hello"
        });

        assert_eq!(extract_text_fragments(&value), vec!["hello".to_string()]);
    }

    #[test]
    fn collects_nested_completed_text() {
        let value = json!({
            "type": "item.completed",
            "item": {
                "type": "assistant_message",
                "content": [
                    {"type": "output_text", "text": "hi"},
                    {"type": "output_text", "text": " there"}
                ]
            }
        });

        assert_eq!(
            extract_completed_item_text(value.get("item")),
            vec!["hi".to_string(), " there".to_string()]
        );
    }

    #[test]
    fn collects_agent_message_text() {
        let value = json!({
            "type": "item.completed",
            "item": {
                "type": "agent_message",
                "text": "hi"
            }
        });

        assert_eq!(extract_text_fragments(&value), vec!["hi".to_string()]);
    }

    #[test]
    fn ignores_unrecognized_event_types() {
        let value = json!({
            "type": "response.completed",
            "text": "should not be emitted"
        });

        assert!(extract_text_fragments(&value).is_empty());
    }

    #[test]
    fn ignores_non_output_text_completed_items() {
        let value = json!({
            "type": "item.completed",
            "item": {
                "content": [
                    {"type": "tool_call", "text": "skip"},
                    {"type": "reasoning", "text": "skip"}
                ]
            }
        });

        assert!(extract_text_fragments(&value).is_empty());
    }

    #[cfg(unix)]
    fn write_fake_cli(script_body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = env::temp_dir().join(format!("rosie-fake-codex-{}-{nanos}", std::process::id()));
        fs::write(&path, script_body).expect("write script");
        let mut perms = fs::metadata(&path).expect("metadata").permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&path, perms).expect("chmod");
        path
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_reads_completed_agent_message() {
        let script = write_fake_cli(
            r#"#!/bin/sh
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
  echo "Logged in using ChatGPT"
  exit 0
fi
if [ "$1" = "exec" ]; then
  echo '{"type":"item.completed","item":{"type":"agent_message","text":"native reply"}}'
  exit 0
fi
exit 1
"#,
        );

        let provider = OpenAiProvider::with_cli_path(None, script.to_string_lossy().to_string());
        let response = provider
            .chat(ChatRequest {
                model: "gpt-5".to_string(),
                messages: vec![Message {
                    role: Role::User,
                    content: "hi".to_string(),
                }],
                temperature: None,
            })
            .await
            .expect("chat response");

        assert_eq!(response.message.content, "native reply");
        let _ = fs::remove_file(script);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_streams_jsonl_deltas() {
        let script = write_fake_cli(
            r#"#!/bin/sh
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
  echo "Logged in using ChatGPT"
  exit 0
fi
if [ "$1" = "exec" ]; then
  echo '{"type":"response.output_text.delta","delta":"Hel"}'
  echo '{"type":"response.output_text.delta","delta":"lo"}'
  exit 0
fi
exit 1
"#,
        );

        let provider = OpenAiProvider::with_cli_path(None, script.to_string_lossy().to_string());
        let (tx, mut rx) = mpsc::unbounded_channel();
        provider
            .stream_chat(
                ChatRequest {
                    model: "gpt-5".to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: "hi".to_string(),
                    }],
                    temperature: None,
                },
                tx,
            )
            .await
            .expect("stream response");

        assert_eq!(
            rx.recv().await,
            Some(ProviderEvent::Token("Hel".to_string()))
        );
        assert_eq!(
            rx.recv().await,
            Some(ProviderEvent::Token("lo".to_string()))
        );
        assert_eq!(rx.recv().await, Some(ProviderEvent::Done));
        let _ = fs::remove_file(script);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn provider_errors_when_not_logged_in() {
        let script = write_fake_cli(
            r#"#!/bin/sh
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
  echo "Not logged in"
  exit 1
fi
exit 1
"#,
        );

        let provider = OpenAiProvider::with_cli_path(None, script.to_string_lossy().to_string());
        let err = provider
            .chat(ChatRequest {
                model: "gpt-5".to_string(),
                messages: vec![Message {
                    role: Role::User,
                    content: "hi".to_string(),
                }],
                temperature: None,
            })
            .await
            .expect_err("logged out provider should fail");

        assert!(err
            .to_string()
            .contains("Run `rosie auth login openai`"));
        let _ = fs::remove_file(script);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn aborting_stream_chat_kills_child_process() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let pid_file = env::temp_dir().join(format!("rosie-openai-pid-{}-{nanos}", std::process::id()));
        let script = write_fake_cli(&format!(
            r#"#!/bin/sh
if [ "$1" = "login" ] && [ "$2" = "status" ]; then
  echo "Logged in using ChatGPT"
  exit 0
fi
if [ "$1" = "exec" ]; then
  echo $$ > "{pid_file}"
  while true; do
    sleep 1
  done
fi
exit 1
"#,
            pid_file = pid_file.display()
        ));

        let provider = OpenAiProvider::with_cli_path(None, script.to_string_lossy().to_string());
        let (tx, _rx) = mpsc::unbounded_channel();
        let task = tokio::spawn(async move {
            let _ = provider
                .stream_chat(
                    ChatRequest {
                        model: "gpt-5".to_string(),
                        messages: vec![Message {
                            role: Role::User,
                            content: "hi".to_string(),
                        }],
                        temperature: None,
                    },
                    tx,
                )
                .await;
        });

        let pid = timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(contents) = fs::read_to_string(&pid_file)
                    && let Ok(pid) = contents.trim().parse::<u32>()
                {
                    break pid;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("pid file should appear");

        task.abort();
        let _ = task.await;

        timeout(Duration::from_secs(5), async {
            loop {
                let status = std::process::Command::new("kill")
                    .args(["-0", &pid.to_string()])
                    .status()
                    .expect("kill -0 should run");
                if !status.success() {
                    break;
                }
                sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("child should exit after abort");

        let _ = fs::remove_file(pid_file);
        let _ = fs::remove_file(script);
    }

    #[cfg(unix)]
    #[test]
    fn login_status_reports_missing_cli() {
        let status = openai_login_status_for_path("/definitely/missing/codex");
        assert_eq!(
            status,
            NativeAuthStatus {
                cli_available: false,
                logged_in: false,
                detail:
                    "Install the OpenAI Codex CLI or set ROSIE_OPENAI_CLI to its executable path."
                        .to_string(),
            }
        );
    }
}
