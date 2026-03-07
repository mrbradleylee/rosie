use clap::Parser;
use std::ffi::OsStr;

#[derive(Parser, Debug)]
pub struct Args {
    /// Configure stored Ollama settings
    #[arg(long)]
    pub configure: bool,

    /// Install the current binary into a local bin directory
    #[arg(long)]
    pub install: bool,

    /// Quick one-shot chat (non-TUI)
    #[arg(short = 'a', long = "ask", conflicts_with = "cmd_mode")]
    pub ask_mode: bool,

    /// Command-generation mode (existing non-TUI flow)
    #[arg(short = 'c', long = "cmd", conflicts_with = "ask_mode")]
    pub cmd_mode: bool,

    /// Override the default model for this request
    #[arg(long, value_name = "MODEL")]
    pub model: Option<String>,

    /// Prompt to send to the LLM
    #[arg(value_name = "PROMPT")]
    pub prompt: Vec<String>,

    /// Display version information (short form: -V)
    #[arg(short = 'V', long)]
    pub version: bool,
}

pub fn parse_args() -> Args {
    Args::parse_from(rewrite_legacy_flags(std::env::args_os()))
}

fn rewrite_legacy_flags<I>(args: I) -> Vec<std::ffi::OsString>
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
