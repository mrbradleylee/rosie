// src/main.rs
use anyhow::{anyhow, Result};
use dotenv::dotenv;
use log::info;
use std::env;
use tokio::io::{self, AsyncReadExt};

#[tokio::main]
async fn main() -> Result<()> {
    dotenv().ok();
    env_logger::init();

    // Parse optional prompt flag
    use clap::Parser;
    #[derive(Parser, Debug)]
    struct Args {
        /// Prompt to send to the LLM (overrides stdin if provided)
        #[arg(short, long, value_name = "PROMPT")]
        prompt: Option<String>,
    }
    let args = Args::parse();
    let prompt = match args.prompt {
        Some(p) => p,
        None => {
            let mut buffer = String::new();
            io::stdin()
                .read_to_string(&mut buffer)
                .await
                .map_err(|e| anyhow!("stdin error: {}", e))?;
            buffer.trim().to_string()
        }
    };
    if prompt.is_empty() {
        return Err(anyhow!("Prompt cannot be empty"));
    }

    let cmd = llm_generate_command(&prompt).await?;
    println!("{}", cmd);
    Ok(())
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(serde::Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<Message>,
}

async fn llm_generate_command(prompt: &str) -> Result<String> {
    use reqwest::Client;

    let key = env::var("OPENAI_API_KEY")
        .map_err(|_| anyhow!("OPENAI_API_KEY missing"))?;

    let url = env::var("OPENAI_ENDPOINT")
        .unwrap_or_else(|_| "https://api.openai.com".to_string())
        + "/v1/chat/completions";

    let client = Client::new();
    let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into());
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
        return Err(anyhow!("API returned {}: {}", resp.status(), resp.text().await?));
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

    let first_line = content
        .lines()
        .next()
        .ok_or_else(|| anyhow!("No content returned"))?
        .trim()
        .to_string();

    if first_line.is_empty() {
        return Err(anyhow!("Empty command received"));
    }

    info!("Command extracted: {}", first_line);
    Ok(first_line)
}
