use crate::credentials::{CredentialManager, CredentialTarget};
use crate::provider::{ChatRequest, ChatResponse, Message, Provider, ProviderEvent, Role};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;
use tokio::sync::mpsc;

pub const DEFAULT_ANTHROPIC_ENDPOINT: &str = "https://api.anthropic.com/v1/messages";

pub struct AnthropicProvider {
    endpoint: String,
    model: Option<String>,
    credentials: CredentialManager,
}

impl AnthropicProvider {
    pub fn new(
        endpoint: Option<String>,
        model: Option<String>,
        credentials: CredentialManager,
    ) -> Self {
        Self {
            endpoint: endpoint.unwrap_or_else(|| DEFAULT_ANTHROPIC_ENDPOINT.to_string()),
            model,
            credentials,
        }
    }
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    system: Option<String>,
    messages: Vec<AnthropicMessage>,
}

#[derive(Serialize)]
struct AnthropicMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<AnthropicContentBlock>,
}

#[derive(Deserialize)]
struct AnthropicContentBlock {
    #[serde(rename = "type")]
    content_type: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct AnthropicStreamEvent {
    #[serde(rename = "type")]
    event_type: String,
    delta: Option<AnthropicStreamDelta>,
}

#[derive(Deserialize)]
struct AnthropicStreamDelta {
    text: Option<String>,
}

impl Provider for AnthropicProvider {
    fn provider_type(&self) -> &'static str {
        "anthropic"
    }

    fn default_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        Box::pin(async move {
            let api_key = self
                .credentials
                .resolve(&CredentialTarget::Anthropic, None)?
                .ok_or_else(|| anyhow!("Missing Anthropic API key. Set ANTHROPIC_API_KEY or run `rosie auth add anthropic`."))?
                .secret;

            let mut system_prompt = None;
            let mut messages = Vec::new();
            for message in request.messages {
                match message.role {
                    Role::System => {
                        let text = message.content.trim();
                        if !text.is_empty() {
                            system_prompt = Some(match system_prompt {
                                Some(existing) => format!("{existing}\n\n{text}"),
                                None => text.to_string(),
                            });
                        }
                    }
                    Role::Assistant | Role::User => messages.push(AnthropicMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    }),
                    Role::Tool => {}
                }
            }

            let request_body = AnthropicRequest {
                model: request.model,
                max_tokens: 1024,
                stream: false,
                system: system_prompt,
                messages,
            };

            let resp = reqwest::Client::new()
                .post(&self.endpoint)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
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

            let response: AnthropicResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("JSON parse error: {e}"))?;
            let content = response
                .content
                .into_iter()
                .filter(|block| block.content_type == "text")
                .filter_map(|block| block.text)
                .collect::<Vec<_>>()
                .join("\n");

            if content.trim().is_empty() {
                return Err(anyhow!("No text content returned"));
            }

            Ok(ChatResponse {
                message: Message {
                    role: Role::Assistant,
                    content,
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
            let api_key = self
                .credentials
                .resolve(&CredentialTarget::Anthropic, None)?
                .ok_or_else(|| anyhow!("Missing Anthropic API key. Set ANTHROPIC_API_KEY or run `rosie auth add anthropic`."))?
                .secret;

            let mut system_prompt = None;
            let mut messages = Vec::new();
            for message in request.messages {
                match message.role {
                    Role::System => {
                        let text = message.content.trim();
                        if !text.is_empty() {
                            system_prompt = Some(match system_prompt {
                                Some(existing) => format!("{existing}\n\n{text}"),
                                None => text.to_string(),
                            });
                        }
                    }
                    Role::Assistant | Role::User => messages.push(AnthropicMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    }),
                    Role::Tool => {}
                }
            }

            let request_body = AnthropicRequest {
                model: request.model,
                max_tokens: 1024,
                stream: true,
                system: system_prompt,
                messages,
            };

            let mut resp = reqwest::Client::new()
                .post(&self.endpoint)
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("accept", "text/event-stream")
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
                    if line.is_empty() || line.starts_with("event:") {
                        continue;
                    }
                    parse_anthropic_stream_line(&line, &tx)?;
                }
            }

            if !buffer.trim().is_empty() {
                parse_anthropic_stream_line(buffer.trim(), &tx)?;
            }

            let _ = tx.send(ProviderEvent::Done);
            Ok(())
        })
    }
}

fn parse_anthropic_stream_line(
    line: &str,
    tx: &mpsc::UnboundedSender<ProviderEvent>,
) -> Result<()> {
    let payload = line.strip_prefix("data: ").unwrap_or(line).trim();
    let parsed: AnthropicStreamEvent =
        serde_json::from_str(payload).map_err(|e| anyhow!("Failed to parse stream JSON: {e}"))?;

    if parsed.event_type == "content_block_delta"
        && let Some(delta) = parsed.delta
        && let Some(text) = delta.text
        && !text.is_empty()
    {
        let _ = tx.send(ProviderEvent::Token(text));
    }

    Ok(())
}
