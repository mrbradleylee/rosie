use crate::credentials::{CredentialManager, CredentialTarget};
use crate::provider::{ChatRequest, ChatResponse, Message, Provider, Role};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::pin::Pin;

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
}

fn role_from_str(value: &str) -> Role {
    match value {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}
