//! The clap command tree. Behaviour lives in `main.rs` and, later, in per-command modules.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use clap_complete::Shell;

/// Top-level command-line arguments.
#[derive(Debug, Parser)]
#[command(
    name = "alkahest",
    version,
    about = "Local AI model server and manager",
    propagate_version = true
)]
pub struct Cli {
    /// Flags accepted before or after any subcommand.
    #[command(flatten)]
    pub global: GlobalArgs,
    /// The command to run.
    #[command(subcommand)]
    pub command: Command,
}

/// Flags shared by every command.
#[derive(Debug, Args)]
pub struct GlobalArgs {
    /// Server address: binds `serve`, targets client commands. Accepts `host`, `host:port`,
    /// `:port` or a URL. Overrides `ALKAHEST_HOST` and the configuration file.
    #[arg(long, global = true, value_name = "HOST[:PORT]")]
    pub host: Option<String>,
    /// Configuration file to use on top of the system and user files.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// More log output: `-v` debug, `-vv` trace.
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Log level or filter directive; beats `-v` and the configuration file.
    #[arg(long, global = true, value_name = "LEVEL")]
    pub log_level: Option<String>,
    /// Machine-readable JSON output where the command supports it.
    #[arg(long, global = true)]
    pub json: bool,
}

/// Every command of the tool. Variants that are not built yet name their owning story in
/// [`Command::owner`].
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Print a shell completion script to stdout.
    Completions {
        /// Target shell.
        shell: Shell,
    },
    /// Download a model from the Ollama registry or Hugging Face.
    Pull {
        /// Model reference, for example `llama3.2:3b` or `hf.co/org/repo:Q4_K_M`.
        model: String,
    },
    /// Search the Ollama library and Hugging Face for models.
    Search {
        /// Search terms.
        query: Vec<String>,
    },
    /// List local models.
    #[command(visible_alias = "list")]
    Ls,
    /// Show the models currently loaded by the server.
    Ps,
    /// Delete local models.
    Rm {
        /// Models to delete.
        #[arg(required = true)]
        models: Vec<String>,
    },
    /// Show details of a model.
    Show {
        /// Model reference.
        model: String,
    },
    /// Copy a model under a new name.
    Cp {
        /// Existing model.
        source: String,
        /// New name.
        destination: String,
    },
    /// Chat with a model interactively, or answer one prompt.
    Run {
        /// Model reference.
        model: String,
        /// Optional one-shot prompt.
        prompt: Vec<String>,
    },
    /// Unload a running model.
    Stop {
        /// Model reference.
        model: String,
    },
    /// Start the API server.
    Serve,
    /// Open the desktop app (needs a build with the `desktop` feature).
    Desktop,
    /// Manage API keys.
    Keys {
        /// Key action.
        #[command(subcommand)]
        action: KeysAction,
    },
    /// Manage inference engines.
    Engines {
        /// Engine action.
        #[command(subcommand)]
        action: EnginesAction,
    },
    /// Install or control the background service.
    Service {
        /// Service action.
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Report CPU, memory and accelerators.
    Sysinfo,
    /// Inspect the effective configuration.
    Config {
        /// Config action.
        #[command(subcommand)]
        action: ConfigAction,
    },
}

/// `alkahest keys ...`
#[derive(Debug, Subcommand)]
pub enum KeysAction {
    /// List keys.
    #[command(visible_alias = "list")]
    Ls,
    /// Create a key.
    Create {
        /// Label of the new key.
        name: String,
    },
    /// Revoke a key.
    Revoke {
        /// Key id.
        id: String,
    },
}

/// `alkahest engines ...`
#[derive(Debug, Subcommand)]
pub enum EnginesAction {
    /// List installed and available engines.
    #[command(visible_alias = "list")]
    Ls,
    /// Install an engine from its recipe.
    Install {
        /// Engine id.
        engine: String,
    },
}

/// `alkahest service ...`
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    /// Install the service unit.
    Install,
    /// Remove the service unit.
    Uninstall,
    /// Show service status.
    Status,
}

/// `alkahest config ...`
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the effective configuration and where each layer came from.
    Show,
    /// Print the configuration file locations and whether they exist.
    Path,
}

impl Command {
    /// The story that implements this command, or `None` when it already works.
    pub fn owner(&self) -> Option<&'static str> {
        Some(match self {
            Self::Pull { .. } => "ALK-US-0017",
            Self::Search { .. } => "ALK-US-0021",
            Self::Ls | Self::Show { .. } => "ALK-US-0012",
            Self::Ps | Self::Stop { .. } => "ALK-US-0031",
            Self::Rm { .. } | Self::Cp { .. } => "ALK-US-0013",
            Self::Run { .. } => "ALK-US-0039",
            Self::Serve => "ALK-US-0033",
            Self::Desktop => "ALK-US-0064",
            Self::Keys { .. } => "ALK-US-0041",
            Self::Engines { .. } => "ALK-US-0049",
            Self::Service { .. } => "ALK-US-0045",
            Self::Sysinfo => "ALK-US-0006",
            Self::Config { .. } | Self::Completions { .. } => return None,
        })
    }

    /// Space-separated command path for messages (`keys create`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pull { .. } => "pull",
            Self::Search { .. } => "search",
            Self::Ls => "ls",
            Self::Ps => "ps",
            Self::Rm { .. } => "rm",
            Self::Show { .. } => "show",
            Self::Cp { .. } => "cp",
            Self::Run { .. } => "run",
            Self::Stop { .. } => "stop",
            Self::Serve => "serve",
            Self::Desktop => "desktop",
            Self::Keys { .. } => "keys",
            Self::Engines { .. } => "engines",
            Self::Service { .. } => "service",
            Self::Sysinfo => "sysinfo",
            Self::Config { .. } => "config",
            Self::Completions { .. } => "completions",
        }
    }
}
