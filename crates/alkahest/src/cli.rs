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

/// Where `alkahest search` looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SearchSource {
    /// Hugging Face and the Ollama library.
    All,
    /// Hugging Face only.
    Hf,
    /// The Ollama library only.
    Ollama,
}

/// Sort order of Hugging Face results.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SearchSort {
    /// Most downloaded first.
    Downloads,
    /// Most liked first.
    Likes,
    /// Recently updated first.
    Recent,
}

/// Arguments of `alkahest search`.
#[derive(Debug, Args)]
pub struct SearchArgs {
    /// Search terms.
    pub query: Vec<String>,
    /// Where to search.
    #[arg(long, value_enum, default_value = "all")]
    pub source: SearchSource,
    /// Hugging Face: only GGUF repositories (the default).
    #[arg(long, conflicts_with_all = ["safetensors", "all_formats"])]
    pub gguf: bool,
    /// Hugging Face: only safetensors repositories.
    #[arg(long, conflicts_with = "all_formats")]
    pub safetensors: bool,
    /// Hugging Face: do not filter by file format.
    #[arg(long = "all", id = "all_formats")]
    pub all_formats: bool,
    /// Hugging Face: restrict to a task (pipeline tag), for example `text-generation`.
    #[arg(long, value_name = "TASK")]
    pub task: Option<String>,
    /// Hugging Face: sort order.
    #[arg(long, value_enum, default_value = "downloads")]
    pub sort: SearchSort,
    /// Maximum results per source.
    #[arg(long, default_value_t = 20, value_name = "N")]
    pub limit: u32,
}

/// Arguments of `alkahest show`.
#[derive(Debug, Args)]
pub struct ShowArgs {
    /// Model reference: a name, `ollama:<name>`, `hf.co/org/repo:Q4_K_M`, a digest or the
    /// first characters of the `ID` column.
    pub model: String,
    /// Print a Modelfile that recreates the model.
    #[arg(long)]
    pub modelfile: bool,
    /// Print only the prompt template.
    #[arg(long)]
    pub template: bool,
    /// Print only the default parameters.
    #[arg(long)]
    pub parameters: bool,
    /// Print only the licence.
    #[arg(long)]
    pub license: bool,
    /// Print only the system prompt.
    #[arg(long)]
    pub system: bool,
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
        /// Talk to the registry over plain HTTP (private registries without TLS).
        #[arg(long)]
        insecure: bool,
        /// Download into the Alkahest store even when the Ollama store already has the model.
        #[arg(long, visible_alias = "copy")]
        force: bool,
    },
    /// Search the Ollama library and Hugging Face for models.
    Search(SearchArgs),
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
    /// Delete blobs no model references and abandoned temporary files.
    Gc {
        /// Report what would be deleted without deleting it.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show details of a model.
    Show(ShowArgs),
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
    /// Manage the Hugging Face token (`login`, `logout`, `whoami`).
    Hf {
        /// Hugging Face action.
        #[command(subcommand)]
        action: HfAction,
    },
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

/// `alkahest hf ...`
#[derive(Debug, Subcommand)]
pub enum HfAction {
    /// Store a Hugging Face token in the OS keyring. The token is read from stdin (a prompt
    /// without echo on a terminal, one line when piped) and never printed.
    Login {
        /// Store the token without checking it against the Hub first.
        #[arg(long)]
        no_verify: bool,
    },
    /// Remove the stored token from the OS keyring.
    Logout,
    /// Show the account the current token belongs to.
    Whoami,
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
    /// Download and install an engine from its recipe.
    Install {
        /// Engine id (`alkahest engines ls` lists them).
        engine: String,
        /// Accelerator build; the CPU build when omitted.
        #[arg(long)]
        variant: Option<String>,
        /// Reinstall even when the engine is already installed.
        #[arg(long)]
        force: bool,
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
    /// Show or edit the configuration overlay of one model.
    Model {
        /// Model reference (an installed model, or a name to configure ahead of its pull).
        model: String,
        /// What to do; `show` when omitted.
        #[command(subcommand)]
        action: Option<ModelConfigAction>,
    },
}

/// `alkahest config model <name> ...`
#[derive(Debug, Subcommand)]
pub enum ModelConfigAction {
    /// Print the overlay, the recommended defaults and the effective settings.
    Show,
    /// Set one or more settings, `key=value` (for example `num_ctx=8192 kv_cache_type_k=q8_0`).
    Set {
        /// Assignments.
        #[arg(required = true, value_name = "KEY=VALUE")]
        assignments: Vec<String>,
    },
    /// Remove settings from the overlay.
    Unset {
        /// Setting names.
        #[arg(required = true)]
        keys: Vec<String>,
    },
    /// Print the path of the overlay file (edit it by hand if you prefer).
    Path,
}

impl Command {
    /// The story that implements this command, or `None` when it already works.
    pub fn owner(&self) -> Option<&'static str> {
        Some(match self {
            Self::Search(_) => "ALK-US-0021",
            Self::Ls | Self::Show(_) => "ALK-US-0012",
            Self::Ps | Self::Stop { .. } => "ALK-US-0031",
            Self::Rm { .. } | Self::Cp { .. } | Self::Gc { .. } => "ALK-US-0013",
            Self::Run { .. } => "ALK-US-0039",
            Self::Serve => "ALK-US-0033",
            Self::Desktop => "ALK-US-0064",
            Self::Keys { .. } => "ALK-US-0041",
            Self::Service { .. } => "ALK-US-0045",
            Self::Pull { .. }
            | Self::Config { .. }
            | Self::Completions { .. }
            | Self::Sysinfo
            | Self::Engines { .. }
            | Self::Hf { .. } => {
                return None;
            }
        })
    }

    /// Space-separated command path for messages (`keys create`).
    pub fn name(&self) -> &'static str {
        match self {
            Self::Pull { .. } => "pull",
            Self::Search(_) => "search",
            Self::Ls => "ls",
            Self::Ps => "ps",
            Self::Rm { .. } => "rm",
            Self::Gc { .. } => "gc",
            Self::Show(_) => "show",
            Self::Cp { .. } => "cp",
            Self::Run { .. } => "run",
            Self::Stop { .. } => "stop",
            Self::Serve => "serve",
            Self::Desktop => "desktop",
            Self::Hf { .. } => "hf",
            Self::Keys { .. } => "keys",
            Self::Engines { .. } => "engines",
            Self::Service { .. } => "service",
            Self::Sysinfo => "sysinfo",
            Self::Config { .. } => "config",
            Self::Completions { .. } => "completions",
        }
    }
}
