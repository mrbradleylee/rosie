use crate::credentials::{CredentialManager, CredentialTarget};
use crate::provider::{ChatRequest, ChatResponse, Message, Provider, Role};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

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
}
