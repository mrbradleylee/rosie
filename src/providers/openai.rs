use crate::credentials::{CredentialManager, CredentialTarget};
use crate::provider::{ChatRequest, ChatResponse, Message, Provider, ProviderEvent, Role};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;

pub const DEFAULT_OPENAI_ENDPOINT: &str = "https://api.openai.com/v1";

pub struct OpenAiProvider {
    endpoint: String,
    model: Option<String>,
    credentials: CredentialManager,
}

impl OpenAiProvider {
    pub fn new(
        endpoint: Option<String>,
        model: Option<String>,
        credentials: CredentialManager,
    ) -> Self {
        Self {
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_OPENAI_ENDPOINT.to_string()),
            model,
            credentials,
        }
    }
}

#[derive(Serialize)]
struct OpenAiChatRequest {
    model: String,
    messages: Vec<OpenAiMessage>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
}

#[derive(Serialize)]
struct OpenAiMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAiChoice {
    message: OpenAiResponseMessage,
}

#[derive(Deserialize)]
struct OpenAiResponseMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct OpenAiStreamChoice {
    delta: OpenAiStreamDelta,
}

#[derive(Deserialize)]
struct OpenAiStreamDelta {
    content: Option<String>,
}

#[derive(Deserialize)]
struct OpenAiStreamResponse {
    choices: Vec<OpenAiStreamChoice>,
}

#[derive(Deserialize)]
struct OpenAiChatResponse {
    choices: Vec<OpenAiChoice>,
}

#[derive(Deserialize)]
struct OpenAiModelItem {
    id: String,
}

#[derive(Deserialize)]
struct OpenAiModelsResponse {
    data: Vec<OpenAiModelItem>,
}

impl Provider for OpenAiProvider {
    fn provider_type(&self) -> &'static str {
        "openai"
    }

    fn default_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn supports_model_discovery(&self) -> bool {
        true
    }

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        Box::pin(async move {
            let api_key = self
                .credentials
                .resolve(&CredentialTarget::OpenAi, None)?
                .ok_or_else(|| {
                    anyhow!(
                        "Missing OpenAI API key. Set OPENAI_API_KEY or run `rosie auth add openai`."
                    )
                })?
                .secret;

            let request_body = OpenAiChatRequest {
                model: request.model,
                messages: request
                    .messages
                    .into_iter()
                    .map(|message| OpenAiMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    })
                    .collect(),
                stream: false,
            };

            let resp = reqwest::Client::new()
                .post(format!(
                    "{}/chat/completions",
                    self.endpoint.trim_end_matches('/')
                ))
                .bearer_auth(api_key)
                .json(&request_body)
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send error: {e}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "API returned {}: {}",
                    resp.status(),
                    resp.text().await?
                ));
            }

            let completion: OpenAiChatResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("JSON parse error: {e}"))?;

            completion
                .choices
                .into_iter()
                .next()
                .map(|choice| ChatResponse {
                    message: Message {
                        role: role_from_str(&choice.message.role),
                        content: choice.message.content.trim().to_string(),
                    },
                })
                .ok_or_else(|| anyhow!("No choices returned"))
        })
    }

    fn list_models(&self) -> Pin<Box<dyn Future<Output = Result<Vec<String>>> + Send + '_>> {
        Box::pin(async move {
            let api_key = self
                .credentials
                .resolve(&CredentialTarget::OpenAi, None)?
                .ok_or_else(|| {
                    anyhow!(
                        "Missing OpenAI API key. Set OPENAI_API_KEY or run `rosie auth add openai`."
                    )
                })?
                .secret;

            let resp = reqwest::Client::new()
                .get(format!("{}/models", self.endpoint.trim_end_matches('/')))
                .bearer_auth(api_key)
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send error for model discovery: {e}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "Model discovery returned {}: {}",
                    resp.status(),
                    resp.text().await?
                ));
            }

            let response: OpenAiModelsResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("Failed to parse model discovery JSON: {e}"))?;
            Ok(response.data.into_iter().map(|item| item.id).collect())
        })
    }

    fn stream_chat(
        &self,
        request: ChatRequest,
        tx: mpsc::UnboundedSender<ProviderEvent>,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>> {
        Box::pin(async move {
            let api_key = self
                .credentials
                .resolve(&CredentialTarget::OpenAi, None)?
                .ok_or_else(|| {
                    anyhow!(
                        "Missing OpenAI API key. Set OPENAI_API_KEY or run `rosie auth add openai`."
                    )
                })?
                .secret;

            let request_body = OpenAiChatRequest {
                model: request.model,
                messages: request
                    .messages
                    .into_iter()
                    .map(|message| OpenAiMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    })
                    .collect(),
                stream: true,
            };

            let mut resp = reqwest::Client::new()
                .post(format!(
                    "{}/chat/completions",
                    self.endpoint.trim_end_matches('/')
                ))
                .bearer_auth(api_key)
                .json(&request_body)
                .send()
                .await
                .map_err(|e| anyhow!("HTTP send error: {e}"))?;

            if !resp.status().is_success() {
                return Err(anyhow!(
                    "API returned {}: {}",
                    resp.status(),
                    resp.text().await?
                ));
            }

            let mut buffer = String::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|e| anyhow!("Stream read error: {e}"))?
            {
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                while let Some(newline_pos) = buffer.find('\n') {
                    let line = buffer[..newline_pos].trim().to_string();
                    buffer = buffer[newline_pos + 1..].to_string();
                    if line.is_empty() {
                        continue;
                    }
                    parse_openai_stream_line(&line, &tx)?;
                }
            }

            if !buffer.trim().is_empty() {
                parse_openai_stream_line(buffer.trim(), &tx)?;
            }

            let _ = tx.send(ProviderEvent::Done);
            Ok(())
        })
    }
}

fn role_from_str(value: &str) -> Role {
    match value {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

fn parse_openai_stream_line(line: &str, tx: &mpsc::UnboundedSender<ProviderEvent>) -> Result<()> {
    let Some(payload) = sse_payload(line) else {
        return Ok(());
    };
    if payload == "[DONE]" {
        return Ok(());
    }

    let parsed: OpenAiStreamResponse =
        serde_json::from_str(payload).map_err(|e| anyhow!("Failed to parse stream JSON: {e}"))?;
    for choice in parsed.choices {
        if let Some(content) = choice.delta.content
            && !content.is_empty()
        {
            let _ = tx.send(ProviderEvent::Token(content));
        }
    }
    Ok(())
}

fn sse_payload(line: &str) -> Option<&str> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with(':') || trimmed.starts_with("event:") {
        return None;
    }

    let payload = trimmed.strip_prefix("data:").unwrap_or(trimmed).trim();
    (!payload.is_empty()).then_some(payload)
}

#[cfg(test)]
mod tests {
    use super::{OpenAiProvider, parse_openai_stream_line};
    use crate::credentials::{CredentialManager, CredentialTarget, SecretStore};
    use crate::provider::{ChatRequest, Message, Provider, ProviderEvent, Role};
    use anyhow::Result;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::mpsc;

    struct MemoryStore {
        secrets: Mutex<BTreeMap<String, String>>,
    }

    impl MemoryStore {
        fn new() -> Self {
            Self {
                secrets: Mutex::new(BTreeMap::new()),
            }
        }
    }

    impl SecretStore for MemoryStore {
        fn get_secret(&self, target: &CredentialTarget) -> Result<Option<String>> {
            Ok(self
                .secrets
                .lock()
                .expect("lock")
                .get(&target.to_string())
                .cloned())
        }

        fn set_secret(&self, target: &CredentialTarget, secret: &str) -> Result<()> {
            self.secrets
                .lock()
                .expect("lock")
                .insert(target.to_string(), secret.to_string());
            Ok(())
        }

        fn delete_secret(&self, target: &CredentialTarget) -> Result<()> {
            self.secrets
                .lock()
                .expect("lock")
                .remove(&target.to_string());
            Ok(())
        }
    }

    #[test]
    fn parser_ignores_non_data_sse_lines() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        parse_openai_stream_line(": keep-alive", &tx).expect("comment ignored");
        parse_openai_stream_line("event: message", &tx).expect("event ignored");
        parse_openai_stream_line("data:", &tx).expect("empty data ignored");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn provider_streams_openai_sse_response() {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).await.expect("read");
            let body = concat!(
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
                "event: message\n",
                "data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n",
                "data: [DONE]\n\n"
            );
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\n\r\n{}",
                body.len(),
                body
            );
            stream
                .write_all(response.as_bytes())
                .await
                .expect("write response");
        });

        let store = Arc::new(MemoryStore::new());
        store
            .set_secret(&CredentialTarget::OpenAi, "test-key")
            .expect("set secret");
        let provider = OpenAiProvider::new(
            Some(format!("http://{addr}/v1")),
            Some("gpt-test".to_string()),
            CredentialManager::with_store(store),
        );
        let (tx, mut rx) = mpsc::unbounded_channel();

        provider
            .stream_chat(
                ChatRequest {
                    model: "gpt-test".to_string(),
                    messages: vec![Message {
                        role: Role::User,
                        content: "hello".to_string(),
                    }],
                    temperature: None,
                },
                tx,
            )
            .await
            .expect("stream chat");

        let mut chunks = Vec::new();
        while let Ok(event) = rx.try_recv() {
            chunks.push(event);
        }

        assert_eq!(
            chunks,
            vec![
                ProviderEvent::Token("Hel".to_string()),
                ProviderEvent::Token("lo".to_string()),
                ProviderEvent::Done,
            ]
        );

        server.await.expect("server task");
    }
}
