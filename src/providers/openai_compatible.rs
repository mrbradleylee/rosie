use crate::credentials::{CredentialManager, CredentialTarget};
use crate::provider::{ChatRequest, ChatResponse, Message, Provider, Role};
use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};
use std::future::Future;
use std::net::IpAddr;
use std::pin::Pin;

pub struct OpenAiCompatibleProvider {
    provider_name: String,
    endpoint: String,
    model: Option<String>,
    allow_insecure_http: bool,
    credentials: CredentialManager,
}

impl OpenAiCompatibleProvider {
    pub fn new(
        provider_name: String,
        endpoint: String,
        model: Option<String>,
        allow_insecure_http: bool,
        credentials: CredentialManager,
    ) -> Self {
        Self {
            provider_name,
            endpoint,
            model,
            allow_insecure_http,
            credentials,
        }
    }

    fn credential_target(&self) -> CredentialTarget {
        CredentialTarget::NamedProvider(self.provider_name.clone())
    }
}

#[derive(Serialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<OpenAiCompatibleMessage>,
}

#[derive(Serialize)]
struct OpenAiCompatibleMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct CompletionChoice {
    message: OpenAiCompatibleResponseMessage,
}

#[derive(Deserialize)]
struct OpenAiCompatibleResponseMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionsResponse {
    choices: Vec<CompletionChoice>,
}

#[derive(Deserialize)]
struct ModelListResponse {
    data: Vec<ModelItem>,
}

#[derive(Deserialize)]
struct ModelItem {
    id: String,
}

impl Provider for OpenAiCompatibleProvider {
    fn provider_type(&self) -> &'static str {
        "openai-compatible"
    }

    fn default_model(&self) -> Option<&str> {
        self.model.as_deref()
    }

    fn chat(
        &self,
        request: ChatRequest,
    ) -> Pin<Box<dyn Future<Output = Result<ChatResponse>> + Send + '_>> {
        Box::pin(async move {
            let auth = resolve_auth(&self.credentials, &self.credential_target())?;
            validate_compatible_endpoint(&self.endpoint, self.allow_insecure_http, auth.is_some())?;

            let request_body = ChatCompletionsRequest {
                model: request.model,
                messages: request
                    .messages
                    .into_iter()
                    .map(|message| OpenAiCompatibleMessage {
                        role: message.role.as_str().to_string(),
                        content: message.content,
                    })
                    .collect(),
            };

            let client = reqwest::Client::new();
            let request = client.post(format!(
                "{}/chat/completions",
                self.endpoint.trim_end_matches('/')
            ));
            let request = if let Some(secret) = auth {
                request.bearer_auth(secret)
            } else {
                request
            };
            let resp = request
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

            let completion: ChatCompletionsResponse = resp
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
            let auth = resolve_auth(&self.credentials, &self.credential_target())?;
            validate_compatible_endpoint(&self.endpoint, self.allow_insecure_http, auth.is_some())?;

            let client = reqwest::Client::new();
            let request = client.get(format!("{}/models", self.endpoint.trim_end_matches('/')));
            let request = if let Some(secret) = auth {
                request.bearer_auth(secret)
            } else {
                request
            };
            let resp = request
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

            let response: ModelListResponse = resp
                .json()
                .await
                .map_err(|e| anyhow!("Failed to parse model discovery JSON: {e}"))?;
            Ok(response.data.into_iter().map(|item| item.id).collect())
        })
    }
}

pub fn validate_compatible_endpoint(
    endpoint: &str,
    allow_insecure_http: bool,
    has_auth: bool,
) -> Result<()> {
    let url =
        reqwest::Url::parse(endpoint).map_err(|_| anyhow!("Invalid endpoint '{endpoint}'"))?;
    match url.scheme() {
        "https" => return Ok(()),
        "http" => {}
        scheme => return Err(anyhow!("Unsupported endpoint scheme '{scheme}'")),
    }

    let host = url
        .host_str()
        .ok_or_else(|| anyhow!("Endpoint '{endpoint}' is missing a host"))?;
    let trusted_local = is_trusted_insecure_host(host);
    if trusted_local {
        return Ok(());
    }
    if allow_insecure_http {
        return Ok(());
    }
    if has_auth {
        return Err(anyhow!(
            "Insecure HTTP is disabled for non-local provider '{}'. Use HTTPS or set `allow_insecure_http = true`.",
            host
        ));
    }
    Err(anyhow!(
        "Refusing insecure non-local endpoint '{}'. Use HTTPS or set `allow_insecure_http = true`.",
        host
    ))
}

fn resolve_auth(
    credentials: &CredentialManager,
    target: &CredentialTarget,
) -> Result<Option<String>> {
    Ok(credentials
        .resolve(target, None)?
        .map(|resolved| resolved.secret))
}

fn is_trusted_insecure_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }

    host.parse::<IpAddr>()
        .map(|ip| match ip {
            IpAddr::V4(v4) => v4.is_loopback() || (v4.octets()[0] == 192 && v4.octets()[1] == 168),
            IpAddr::V6(v6) => v6.is_loopback(),
        })
        .unwrap_or(false)
}

fn role_from_str(value: &str) -> Role {
    match value {
        "system" => Role::System,
        "assistant" => Role::Assistant,
        "tool" => Role::Tool,
        _ => Role::User,
    }
}

#[cfg(test)]
mod tests {
    use super::validate_compatible_endpoint;

    #[test]
    fn allows_local_network_http_by_default() {
        validate_compatible_endpoint("http://192.168.1.15:8080/v1", false, false)
            .expect("192.168 should be allowed");
    }

    #[test]
    fn rejects_remote_http_without_opt_in() {
        let err = validate_compatible_endpoint("http://10.0.0.9:8080/v1", false, false)
            .expect_err("remote insecure http should fail");
        assert!(
            err.to_string()
                .contains("Refusing insecure non-local endpoint")
        );
    }
}
