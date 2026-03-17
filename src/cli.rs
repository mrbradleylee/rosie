use clap::Parser;

#[derive(Parser, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct Args {
    /// Configure stored Rosie settings
    #[arg(long)]
    pub config: bool,

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

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// Manage stored provider credentials
    Auth(AuthCommand),
}

#[derive(clap::Args, Debug)]
pub struct AuthCommand {
    #[command(subcommand)]
    pub action: AuthAction,
}

#[derive(clap::Subcommand, Debug)]
pub enum AuthAction {
    Add { provider: String },
    Login { provider: String },
    List,
    Logout { provider: String },
    Remove { provider: String },
}

pub fn parse_args() -> Args {
    Args::parse()
}
