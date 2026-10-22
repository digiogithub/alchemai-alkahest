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
    /// Server port: binds `serve`, targets client commands. Beats the port of `--host`,
    /// `ALKAHEST_HOST` and the configuration file (`alkahest config set server.port=N` saves
    /// a new default).
    #[arg(long, global = true, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
    pub port: Option<u16>,
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
    /// The git catalogs, Hugging Face and the Ollama library.
    All,
    /// Hugging Face only.
    Hf,
    /// The Ollama library only.
    Ollama,
    /// The git catalogs only (local snapshots, `alkahest/<namespace>/<model>` names).
    Catalog,
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
    /// Catalog results: only entries of this configured catalog.
    #[arg(long, value_name = "NAME")]
    pub catalog: Option<String>,
}

/// Arguments of `alkahest desktop`.
#[derive(Debug, Args)]
pub struct DesktopArgs {
    /// Open the window, wait for the first frame to be rendered, then exit with status 0
    /// (used by CI; fails with status 3 when no frame arrives within 30 s).
    #[arg(long)]
    pub smoke_test: bool,
}

/// Arguments of `alkahest run`.
#[derive(Debug, Clone, Args)]
pub struct RunArgs {
    /// Model reference, or the path of a local `.gguf` file or model folder (imported with
    /// `--link` under its default name first, then run).
    pub model: String,
    /// One-shot prompt (words are joined with spaces, after any piped input).
    pub prompt: Vec<String>,
    /// Response format: `json`, or a JSON schema.
    #[arg(long, value_name = "FORMAT")]
    pub format: Option<String>,
    /// Thinking for models that support it: `true`/`false`, or `high`/`medium`/`low`.
    /// `--think` alone means `true`.
    #[arg(long, value_name = "WHEN", num_args = 0..=1, default_missing_value = "true")]
    pub think: Option<String>,
    /// Do not print the model's thinking (it still thinks when `--think` asks for it).
    #[arg(long)]
    pub hidethinking: bool,
    /// How long the model stays loaded after the last request (`5m`, `1h`, `0`, `-1`).
    #[arg(long, value_name = "DURATION")]
    pub keepalive: Option<String>,
    /// Do not offer to start the server when it is not running.
    #[arg(long)]
    pub no_start: bool,
}

/// Arguments of `alkahest create`.
#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Name of the new model (replaced when it exists in the Alkahest store).
    pub name: String,
    /// A `.gguf` file or a model folder to import (split shards, `mmproj` projector,
    /// `Modelfile`, or a Hugging Face safetensors folder).
    #[arg(long, value_name = "PATH")]
    pub from: Option<PathBuf>,
    /// Modelfile to build from, as `ollama create -f`: `FROM ./file.gguf`, `FROM ./folder`
    /// (relative to the Modelfile) or `FROM <model>`, plus `TEMPLATE`, `PARAMETER`,
    /// `SYSTEM`, `LICENSE`. `--from` replaces its `FROM`.
    #[arg(short = 'f', long = "file", value_name = "MODELFILE")]
    pub modelfile: Option<PathBuf>,
    /// Register the files where they are instead of copying them: a hard link on the same
    /// filesystem, otherwise a symbolic link checked before every load. The files must stay
    /// unchanged.
    #[arg(long)]
    pub link: bool,
}

/// Arguments of `alkahest import`.
#[derive(Debug, Args)]
pub struct ImportArgs {
    /// A `.gguf` file or a model folder; with `--store`, another Alkahest store.
    pub path: PathBuf,
    /// Model name; defaults to the file stem or folder name.
    #[arg(long, conflicts_with = "store")]
    pub name: Option<String>,
    /// Register the files where they are instead of copying them (see `create --link`).
    #[arg(long, conflicts_with = "store")]
    pub link: bool,
    /// PATH is another Alkahest store (a data folder such as `~/.local/share/alkahest`, or its
    /// `models` folder): copy every model in it, with its settings, into this store. Models
    /// already here are skipped; the source is not changed. For example, to give the system
    /// service the models pulled as a user:
    /// `sudo env ALKAHEST_HOME=/var/lib/alkahest alkahest import --store ~/.local/share/alkahest`.
    #[arg(long)]
    pub store: bool,
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

/// Arguments of `alkahest update`.
#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Install exactly this release (`X.Y.Z` or `vX.Y.Z`), which may be a downgrade or a
    /// reinstall; pre-release versions are allowed here. Without it, the newest stable
    /// release.
    #[arg(value_name = "VERSION")]
    pub version: Option<String>,
    /// Only report what would change; exit 10 when another release would be installed.
    #[arg(long)]
    pub check: bool,
    /// Consider pre-releases (GitHub pre-release flag or a `-rc.N` style tag) when looking for
    /// the newest release.
    #[arg(long)]
    pub prerelease: bool,
    /// Replace the binary even when a package manager owns it (deb/rpm, Homebrew, the app
    /// bundle, the Windows installer, an AppImage) or it is a development build.
    #[arg(long)]
    pub force: bool,
    /// Do not ask for confirmation (required when stdin is not a terminal).
    #[arg(short, long)]
    pub yes: bool,
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
    /// Download a model from the Ollama registry, Hugging Face or a git catalog.
    Pull {
        /// Model reference, for example `llama3.2:3b`, `hf.co/org/repo:Q4_K_M` or the catalog
        /// model `alkahest/qwen/qwen2.5-0.5b-instruct:q4_k_m`.
        model: String,
        /// Talk to the registry over plain HTTP (private registries without TLS).
        #[arg(long)]
        insecure: bool,
        /// Download into the Alkahest store even when the Ollama store already has the model.
        #[arg(long, visible_alias = "copy")]
        force: bool,
        /// Download the safetensors snapshot of a Hugging Face repository for vLLM instead of
        /// GGUF (chosen automatically when the repository has no GGUF files).
        #[arg(long)]
        safetensors: bool,
        /// Catalog models: take the entry from this configured catalog instead of the
        /// highest layer that has it.
        #[arg(long, value_name = "NAME")]
        catalog: Option<String>,
    },
    /// Search the git catalogs, the Ollama library and Hugging Face for models.
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
    /// Create a model from a Modelfile, a local GGUF file or a model folder.
    ///
    /// Copies the files into the Alkahest store (or links them with `--link`); the source
    /// files are never modified. Without `--from` and `-f`, a `Modelfile` in the current
    /// directory is used, like `ollama create`.
    Create(CreateArgs),
    /// Import a local GGUF file or model folder (`create --from <path>` with a default name).
    Import(ImportArgs),
    /// Copy a model under a new name.
    Cp {
        /// Existing model.
        source: String,
        /// New name.
        destination: String,
    },
    /// Chat with a model interactively, or answer one prompt.
    ///
    /// With a prompt, or with input piped on stdin, prints one answer and exits; otherwise
    /// opens an interactive session (`/?` for help, `/bye` to leave). Needs a running
    /// server (`alkahest serve`). For this command `--verbose` prints timing statistics
    /// after each answer instead of raising the log level.
    Run(RunArgs),
    /// Unload a running model.
    Stop {
        /// Model reference.
        model: String,
    },
    /// Start the API server.
    Serve {
        /// Start on a non-loopback address even though remote clients would not need a key
        /// (`auth.require_key_for_remote = false`) or no API key exists yet.
        #[arg(long)]
        allow_unauthenticated: bool,
        /// Also answer on Ollama's port (`server.ollama_port`, 11434) when it is free, so
        /// Ollama clients work unchanged; a port Ollama already holds is skipped silently.
        /// Overrides `server.ollama_compat` for this run.
        #[arg(long, overrides_with = "no_ollama_compat")]
        ollama_compat: bool,
        /// Do not take over the Ollama port this run, whatever the configuration says.
        #[arg(long)]
        no_ollama_compat: bool,
        /// The Ollama-compatible port to try instead of `server.ollama_port`.
        #[arg(long, value_name = "PORT", value_parser = clap::value_parser!(u16).range(1..))]
        ollama_port: Option<u16>,
        /// Run under the Windows service control manager (what `alkahest service install`
        /// registers); not for interactive use.
        #[arg(long, hide = true)]
        service: bool,
    },
    /// Open the desktop app (needs a build with the `desktop` feature).
    Desktop(DesktopArgs),
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
    /// Manage git catalogs of engine recipes and models (`[[catalogs]]`).
    ///
    /// Catalogs are folders of TOML in git repositories (the official one is `catalog/` of
    /// digiogithub/alchemai-alkahest). Their snapshots live under `<data dir>/catalogs/` and
    /// refresh in the background when older than `refresh_interval` (24h by default);
    /// `ALKAHEST_CATALOG_AUTO_SYNC=0` turns the automatic refresh off. A private catalog's
    /// token comes from the OS keyring (service `alkahest`, user `catalog:<name>`) or
    /// `ALKAHEST_CATALOG_TOKEN_<NAME>`.
    Catalog {
        /// Catalog action.
        #[command(subcommand)]
        action: CatalogAction,
    },
    /// Manage TLS certificates for HTTPS (`[server.tls]`).
    Certs {
        /// Certificate action.
        #[command(subcommand)]
        action: CertsAction,
    },
    /// Install or control the background service.
    Service {
        /// Service action.
        #[command(subcommand)]
        action: ServiceAction,
    },
    /// Report CPU, memory and accelerators.
    Sysinfo,
    /// Update alkahest to the newest stable GitHub release (or a given version).
    ///
    /// Downloads the headless archive for this platform from the GitHub releases of
    /// digiogithub/alchemai-alkahest, verifies it against the release SHA256SUMS and replaces
    /// the running binary, plus `engines/` and the licence files when they sit next to it.
    /// Nothing installed changes before the download is verified. Installs owned by a package
    /// manager are refused with the right upgrade command unless `--force` is given.
    /// Exit codes: 0 updated or up to date, 1 failure, 3 refused or cancelled, 4 verification
    /// failed, 10 `--check` found another release. `GITHUB_TOKEN` raises the API rate limit;
    /// `ALKAHEST_UPDATE_API_URL` and `ALKAHEST_UPDATE_REPO` point at another release source.
    #[command(disable_version_flag = true)]
    Update(UpdateArgs),
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
    /// Create a key and print it once; only a salted hash is stored.
    Create {
        /// Label of the new key (unique among active keys).
        name: String,
        /// Scope to grant: `inference` (default) or `admin` (includes inference). Repeatable.
        #[arg(long = "scope", value_name = "SCOPE")]
        scopes: Vec<String>,
        /// Restrict the key to this model. Repeatable; default: every model.
        #[arg(long = "model", value_name = "MODEL")]
        models: Vec<String>,
        /// Expiry: a duration such as `12h`, `30d`, `8w`, or an RFC 3339 time.
        #[arg(long, value_name = "WHEN")]
        expires: Option<String>,
    },
    /// Revoke a key; the next request with it is refused.
    Revoke {
        /// Key id (`abcd1234` or `alk_abcd1234`) or active key name.
        id: String,
    },
}

/// `alkahest certs ...`
#[derive(Debug, Subcommand)]
pub enum CertsAction {
    /// Write a self-signed server certificate, its key (owner only) and the CA clients trust
    /// to the `[server.tls]` paths (default `<data_dir>/tls/`).
    Generate {
        /// Extra DNS name or IP address the certificate must cover (localhost, the loopback
        /// addresses and this host's name are always included). Repeatable.
        #[arg(long = "name", value_name = "NAME")]
        hosts: Vec<String>,
        /// Validity in days (at most 825, the limit Apple platforms accept).
        #[arg(long, default_value_t = 397)]
        days: u32,
        /// Replace existing files.
        #[arg(long)]
        force: bool,
    },
}

/// `alkahest engines ...`
#[derive(Debug, Subcommand)]
pub enum EnginesAction {
    /// List installed and available engines.
    #[command(visible_alias = "list")]
    Ls,
    /// Download and install an engine from its recipe.
    #[command(disable_version_flag = true)]
    Install {
        /// Engine id (`alkahest engines ls` lists them).
        engine: String,
        /// Accelerator build (`cpu`, `cuda12.8`, `cuda12`, `vulkan`, ...); the best one for
        /// this machine when omitted.
        #[arg(long)]
        variant: Option<String>,
        /// Engine version to install; the newest one when omitted. Several versions can be
        /// installed side by side.
        #[arg(long = "version")]
        engine_version: Option<String>,
        /// Build from the recipe's source steps (needs git, cmake and ninja) instead of
        /// downloading the prebuilt archive.
        #[arg(long)]
        from_source: bool,
        /// Reinstall even when the engine is already installed.
        #[arg(long)]
        force: bool,
        /// Also unpack the CUDA runtime libraries (cudart, cublas; 150 to 600 MB) next to the
        /// engine, for a CUDA build on a machine without a system-wide CUDA runtime.
        #[arg(long)]
        with_runtime: bool,
        /// Also install the optional dependency with this id (see `engines info`); repeatable.
        #[arg(long = "with", value_name = "ID")]
        with: Vec<String>,
        /// Build the native variant of the engine from source for this machine: on a host
        /// with an NVIDIA GPU the CUDA one (llama.cpp `cuda-native`,
        /// `-DCMAKE_CUDA_ARCHITECTURES=native`), otherwise the CPU one (`cpu-native`,
        /// `-DGGML_NATIVE=ON`). Never picked automatically; the build is not portable.
        #[arg(long, conflicts_with = "variant")]
        native: bool,
        /// CUDA GPU architectures to compile a CUDA source build for, replacing `native`
        /// (`89`, or a list such as `86;89`); implies a source build.
        #[arg(long, value_name = "LIST")]
        cuda_arch: Option<String>,
        /// Print what would be installed (recipe, variant, source build flags) and stop.
        #[arg(long)]
        dry_run: bool,
    },
    /// Remove an installed engine version.
    #[command(visible_alias = "remove", disable_version_flag = true)]
    Rm {
        /// Engine id.
        engine: String,
        /// Version to remove; every installed version when omitted.
        #[arg(long = "version")]
        engine_version: Option<String>,
        /// Variant to remove; every installed variant when omitted.
        #[arg(long)]
        variant: Option<String>,
    },
    /// Show an engine's versions, variants, requirements and what is installed.
    Info {
        /// Engine id.
        engine: String,
    },
}

/// `alkahest catalog ...`
#[derive(Debug, Subcommand)]
pub enum CatalogAction {
    /// List the configured catalogs: ref, commit, last sync, entry counts and issues.
    #[command(visible_alias = "list")]
    Ls,
    /// Fetch catalogs now (every enabled one, or the one named).
    Sync {
        /// Catalog name.
        name: Option<String>,
        /// Run silently as the detached background refresh started by other commands.
        #[arg(long, hide = true)]
        background: bool,
    },
    /// Add a catalog source after the existing ones (a later catalog overrides earlier ones).
    Add {
        /// Catalog name (lower-case letters, digits, `-`, `_`, `.`).
        name: String,
        /// Git repository URL (`https://`), or `file://` path of a catalog directory.
        url: String,
        /// Branch or tag to follow.
        #[arg(long = "ref", default_value = "main")]
        git_ref: String,
        /// Folder of the catalog inside the repository.
        #[arg(long, default_value = ".")]
        subdir: String,
        /// Freeze the catalog at this commit id (never refreshed).
        #[arg(long)]
        pin: Option<String>,
    },
    /// Remove a catalog source and its snapshot. The official catalog is disabled instead.
    #[command(visible_alias = "remove")]
    Rm {
        /// Catalog name.
        name: String,
        /// Also delete the catalog's token from the OS keyring.
        #[arg(long)]
        delete_token: bool,
    },
    /// Turn a catalog on (it contributes entries again).
    Enable {
        /// Catalog name.
        name: String,
    },
    /// Turn a catalog off without removing it (its snapshot is kept).
    Disable {
        /// Catalog name.
        name: String,
    },
    /// Change a catalog source: ref, subdir, pin, refresh interval.
    Edit {
        /// Catalog name.
        name: String,
        /// Branch or tag to follow.
        #[arg(long = "ref")]
        git_ref: Option<String>,
        /// Folder of the catalog inside the repository.
        #[arg(long)]
        subdir: Option<String>,
        /// Freeze the catalog at this commit id (never refreshed).
        #[arg(long, conflicts_with = "unpin")]
        pin: Option<String>,
        /// Remove the pin: follow the ref again.
        #[arg(long)]
        unpin: bool,
        /// Refresh interval (`24h`, `30m`).
        #[arg(long)]
        refresh_interval: Option<String>,
    },
    /// Manage the access token of a private catalog (OS keyring, service `alkahest`, user
    /// `catalog:<name>`).
    Token {
        /// Token action.
        #[command(subcommand)]
        action: CatalogTokenAction,
    },
    /// Write `index.toml` for a catalog directory (catalog authors).
    Index {
        /// Catalog directory (holding `catalog.toml`).
        dir: std::path::PathBuf,
    },
}

/// `alkahest catalog token ...`
#[derive(Debug, Subcommand)]
pub enum CatalogTokenAction {
    /// Store the token of a private catalog in the OS keyring. The token is read from stdin (a
    /// prompt without echo on a terminal, one line when piped), never from the command line,
    /// and never printed.
    Set {
        /// Catalog name.
        name: String,
    },
    /// Remove the token of a catalog from the OS keyring.
    #[command(visible_alias = "rm")]
    Clear {
        /// Catalog name.
        name: String,
    },
}

/// `alkahest service ...`
#[derive(Debug, Subcommand)]
pub enum ServiceAction {
    /// Install the service unit and start it.
    Install(ServiceArgs),
    /// Stop the service and remove the unit (the service user and its data stay).
    Uninstall(ServiceArgs),
    /// Start the installed service.
    Start(ServiceArgs),
    /// Stop the installed service.
    Stop(ServiceArgs),
    /// Show whether the service is running and where it listens.
    Status(ServiceArgs),
}

/// Service managers `alkahest service` can target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ServiceManager {
    /// systemd (Linux).
    Systemd,
    /// launchd (macOS).
    Launchd,
    /// The Windows service control manager.
    Windows,
}

impl ServiceManager {
    /// The manager of the platform this binary was built for.
    pub fn native() -> Option<Self> {
        if cfg!(target_os = "linux") {
            Some(Self::Systemd)
        } else if cfg!(target_os = "macos") {
            Some(Self::Launchd)
        } else if cfg!(windows) {
            Some(Self::Windows)
        } else {
            None
        }
    }
}

/// Options shared by the `service` actions.
#[derive(Debug, Clone, Default, clap::Args)]
pub struct ServiceArgs {
    /// Manage a per-user service (systemd `--user` unit, launchd LaunchAgent) instead of the
    /// system service.
    #[arg(long)]
    pub user: bool,
    /// Print the unit and the commands without writing or running anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Write below this directory and run no command (packaging, tests).
    #[arg(long, value_name = "DIR")]
    pub root: Option<std::path::PathBuf>,
    /// Binary the unit starts; defaults to the running executable.
    #[arg(long, value_name = "PATH")]
    pub bin: Option<std::path::PathBuf>,
    /// Service manager to target; defaults to this platform's. Another platform's manager
    /// only works with `--dry-run` or `--root` (packaging, review).
    #[arg(long, value_enum)]
    pub manager: Option<ServiceManager>,
}

/// `alkahest config ...`
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the effective configuration and where each layer came from.
    Show,
    /// Print the configuration file locations and whether they exist.
    Path,
    /// Save settings to the configuration file (`--config`, else the user file), dotted
    /// `KEY=VALUE`: `server.port=11500`, `server.ollama_compat=true`,
    /// `server.host=0.0.0.0`. Applies on the next server start.
    Set {
        /// Assignments.
        #[arg(required = true, value_name = "KEY=VALUE")]
        assignments: Vec<String>,
    },
    /// Remove settings from the configuration file so their defaults apply again.
    Unset {
        /// Dotted keys (`server.port`).
        #[arg(required = true, value_name = "KEY")]
        keys: Vec<String>,
    },
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
    /// `stop` takes a comma separated list; `extra_args` one shell-quoted string of engine
    /// arguments appended after `engines.overrides.<id>.extra_args`
    /// (`"extra_args=--cache-type-k turbo3 --override-kv 'a=str:b c'"`); `engine` names a
    /// built-in or fork engine, optionally `@<version>`.
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
            Self::Rm { .. } | Self::Cp { .. } | Self::Gc { .. } => "ALK-US-0013",
            Self::Service { .. } => "ALK-US-0045",
            Self::Pull { .. }
            | Self::Create(_)
            | Self::Import(_)
            | Self::Run(_)
            | Self::Ps
            | Self::Stop { .. }
            | Self::Config { .. }
            | Self::Completions { .. }
            | Self::Sysinfo
            | Self::Update(_)
            | Self::Serve { .. }
            | Self::Keys { .. }
            | Self::Engines { .. }
            | Self::Catalog { .. }
            | Self::Certs { .. }
            | Self::Hf { .. }
            | Self::Desktop(_) => {
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
            Self::Create(_) => "create",
            Self::Import(_) => "import",
            Self::Run(_) => "run",
            Self::Stop { .. } => "stop",
            Self::Serve { .. } => "serve",
            Self::Desktop(_) => "desktop",
            Self::Hf { .. } => "hf",
            Self::Keys { .. } => "keys",
            Self::Engines { .. } => "engines",
            Self::Catalog { .. } => "catalog",
            Self::Certs { .. } => "certs",
            Self::Service { .. } => "service",
            Self::Sysinfo => "sysinfo",
            Self::Update(_) => "update",
            Self::Config { .. } => "config",
            Self::Completions { .. } => "completions",
        }
    }
}
