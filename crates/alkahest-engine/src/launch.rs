//! What the scheduler asks an engine to run ([`LaunchRequest`]) and what the engine answers
//! with: a fully resolved process description ([`LaunchSpec`]) the supervisor can spawn.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use alkahest_gguf::{ModelFormat, ModelKind};
use alkahest_store::{CatalogEntry, Effective, ModelSettings};
use alkahest_sysinfo::Accelerator;
use serde::{Deserialize, Serialize};

use crate::error::{EngineError, Result};
use crate::id::{ApiDialect, EngineId};

/// One model to load: the weights, the effective settings and where the engine must listen.
///
/// Built by the scheduler from a [`CatalogEntry`] and its [`Effective`] settings
/// ([`LaunchRequest::from_catalog`]) or field by field in tests ([`LaunchRequest::new`]).
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchRequest {
    /// Model name the engine reports and that clients use (`qwen3:8b`).
    pub model: String,
    /// The weights file (GGUF) or directory (safetensors).
    pub model_path: PathBuf,
    /// Multimodal projector next to the weights.
    pub projector_path: Option<PathBuf>,
    /// Weight format.
    pub format: ModelFormat,
    /// What the model does; decides the engine mode (`--embeddings`, `--reranking`).
    pub kind: ModelKind,
    /// Quantisation (`Q4_K_M`, `PQ2_0`).
    pub quantization: Option<String>,
    /// The weights need the PrismML fork (PQ2_0 / PTQ1_0 tensors seen in the header).
    pub needs_prism: bool,
    /// Effective settings after layering (request > model > recommended > global).
    pub settings: ModelSettings,
    /// Backend the engine should offload to.
    pub accelerator: Accelerator,
    /// Parallel request slots the scheduler granted (`scheduler.num_parallel`); `None` lets
    /// the engine decide.
    pub num_parallel: Option<u32>,
    /// CPU threads for generation; `None` lets the engine pick from the hardware.
    pub threads: Option<u32>,
    /// Built-in chat template name (`chatml`, `llama3`) matched from the model's Ollama Go
    /// `TEMPLATE` layer by [`crate::llama_cpp::builtin_template_for_go`], used only when the
    /// GGUF carries no template (ADR 0001, case 3).
    pub builtin_template: Option<String>,
    /// vLLM: fraction of the GPU's memory the engine may pre-allocate (`--gpu-memory-utilization`).
    /// vLLM claims `fraction * total VRAM` at start whatever the model needs, so the scheduler
    /// sets it from its estimate; `None` leaves vLLM's own default (0.9).
    pub gpu_memory_utilization: Option<f64>,
    /// vLLM: GPUs to shard the model over (`--tensor-parallel-size`); `None` or 1 is one GPU.
    pub tensor_parallel_size: Option<u32>,
    /// Address the engine binds; always loopback, the API server is the only client.
    pub host: IpAddr,
    /// Port the engine binds; `0` until the supervisor allocates one.
    pub port: u16,
}

impl LaunchRequest {
    /// A request with CPU, loopback, port 0 and no settings.
    pub fn new(
        model: impl Into<String>,
        model_path: impl Into<PathBuf>,
        format: ModelFormat,
        kind: ModelKind,
    ) -> Self {
        Self {
            model: model.into(),
            model_path: model_path.into(),
            projector_path: None,
            format,
            kind,
            quantization: None,
            needs_prism: false,
            settings: ModelSettings::default(),
            accelerator: Accelerator::Cpu,
            num_parallel: None,
            threads: None,
            builtin_template: None,
            gpu_memory_utilization: None,
            tensor_parallel_size: None,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 0,
        }
    }

    /// Builds the request for a resolved catalog entry and its effective settings.
    ///
    /// Fails with [`EngineError::MissingModelPath`] when the entry has no weights on disk
    /// (callers normally resolve with `Catalog::resolve_loadable` first).
    pub fn from_catalog(
        entry: &CatalogEntry,
        effective: &Effective,
        accelerator: Accelerator,
        port: u16,
    ) -> Result<Self> {
        let model = entry.entry.name.display_short();
        let model_path =
            entry
                .entry
                .weights_path()
                .ok_or_else(|| EngineError::MissingModelPath {
                    model: model.clone(),
                })?;
        // A safetensors snapshot is loaded as a directory. Pull builds it; a model copied
        // with `cp` or restored without it gets it rebuilt here (symlinks inside the
        // Alkahest store only).
        if entry.entry.is_snapshot() && !model_path.join("config.json").exists() {
            ensure_snapshot(&entry.entry, &model_path)?;
        }
        Ok(Self {
            model,
            model_path,
            projector_path: entry.entry.projector_path().map(Path::to_path_buf),
            format: entry.info.format,
            kind: entry.info.kind,
            quantization: entry.info.quantization.clone(),
            needs_prism: entry.info.needs_prism,
            settings: effective.settings.clone(),
            accelerator,
            num_parallel: None,
            threads: None,
            builtin_template: None,
            gpu_memory_utilization: None,
            tensor_parallel_size: None,
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port,
        })
    }

    /// The same request on another port (the supervisor retries on port collisions).
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }
}

/// Rebuilds the snapshot directory of a safetensors model in its own store.
fn ensure_snapshot(entry: &alkahest_store::ModelEntry, dir: &Path) -> Result<()> {
    // `<store>/snapshots/<digest>`: the store root is two levels up.
    let root =
        dir.parent()
            .and_then(Path::parent)
            .ok_or_else(|| EngineError::MissingModelPath {
                model: entry.name.display_short(),
            })?;
    let store = alkahest_store::AlkahestStore::open(root).map_err(|e| {
        EngineError::SnapshotUnavailable {
            model: entry.name.display_short(),
            reason: e.to_string(),
        }
    })?;
    store
        .materialise_snapshot(&entry.manifest_digest, &entry.manifest)
        .map(|_| ())
        .map_err(|e| EngineError::SnapshotUnavailable {
            model: entry.name.display_short(),
            reason: e.to_string(),
        })
}

/// When a health response means "ready to serve".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ReadyPredicate {
    /// Any HTTP response: the server only listens once the model is loaded.
    AnyResponse,
    /// A 2xx status (llama-server answers 503 while loading).
    Status2xx,
    /// A 2xx status whose body contains the given text.
    Status2xxBodyContains(String),
}

impl ReadyPredicate {
    /// Evaluates the predicate on one response.
    pub fn matches(&self, status: u16, body: &str) -> bool {
        let ok = (200..300).contains(&status);
        match self {
            ReadyPredicate::AnyResponse => true,
            ReadyPredicate::Status2xx => ok,
            ReadyPredicate::Status2xxBodyContains(needle) => ok && body.contains(needle.as_str()),
        }
    }
}

/// How the supervisor decides the engine is up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthCheck {
    /// Path polled with `GET` (`/health`).
    pub path: String,
    /// What a ready response looks like.
    pub ready: ReadyPredicate,
}

impl HealthCheck {
    /// `GET /health` answered with a 2xx.
    pub fn health_2xx() -> Self {
        Self {
            path: "/health".into(),
            ready: ReadyPredicate::Status2xx,
        }
    }
}

/// A process to spawn, fully resolved: what [`crate::Engine::prepare`] returns and the
/// supervisor consumes. Nothing in it depends on the scheduler.
#[derive(Debug, Clone, PartialEq)]
pub struct LaunchSpec {
    /// Engine that produced the spec.
    pub engine: EngineId,
    /// Executable to run.
    pub program: PathBuf,
    /// Arguments, in order.
    pub args: Vec<OsString>,
    /// Variables set on top of the inherited environment (library search path among them).
    pub env: BTreeMap<String, OsString>,
    /// Working directory; `None` keeps the server's.
    pub working_dir: Option<PathBuf>,
    /// Address the engine binds.
    pub host: IpAddr,
    /// Port the engine binds.
    pub port: u16,
    /// Readiness probe.
    pub health: HealthCheck,
    /// How long loading may take before the start counts as failed.
    pub startup_timeout: Duration,
    /// Time between SIGTERM and SIGKILL on stop.
    pub stop_grace: Duration,
    /// HTTP API the process speaks.
    pub dialect: ApiDialect,
}

impl LaunchSpec {
    /// A spec with no arguments or environment, `GET /health` 2xx readiness, 5 minute start
    /// timeout and 10 second stop grace.
    pub fn new(
        engine: EngineId,
        program: impl Into<PathBuf>,
        host: IpAddr,
        port: u16,
        dialect: ApiDialect,
    ) -> Self {
        Self {
            engine,
            program: program.into(),
            args: Vec::new(),
            env: BTreeMap::new(),
            working_dir: None,
            host,
            port,
            health: HealthCheck::health_2xx(),
            startup_timeout: Duration::from_secs(300),
            stop_grace: Duration::from_secs(10),
            dialect,
        }
    }

    /// Appends one argument.
    pub fn arg(&mut self, a: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(a.as_ref().to_os_string());
        self
    }

    /// Appends a flag followed by its value.
    pub fn flag(&mut self, flag: &str, value: impl AsRef<OsStr>) -> &mut Self {
        self.arg(flag).arg(value)
    }

    /// `http://<host>:<port>`, the base URL the API layer proxies to.
    pub fn base_url(&self) -> String {
        match self.host {
            IpAddr::V6(v6) => format!("http://[{v6}]:{}", self.port),
            IpAddr::V4(v4) => format!("http://{v4}:{}", self.port),
        }
    }

    /// The arguments as lossy UTF-8, for logs and tests.
    pub fn args_lossy(&self) -> Vec<String> {
        self.args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect()
    }

    /// The command line as one string, for logs (`program arg arg`).
    pub fn command_line(&self) -> String {
        let mut out = self.program.display().to_string();
        for a in self.args_lossy() {
            out.push(' ');
            if a.contains(' ') {
                out.push_str(&format!("{a:?}"));
            } else {
                out.push_str(&a);
            }
        }
        out
    }

    /// Prepends `dirs` to the platform library search variable (`LD_LIBRARY_PATH` on Linux,
    /// `DYLD_LIBRARY_PATH` on macOS, `PATH` on Windows), keeping the inherited value after
    /// them so the engine's bundled shared libraries win over system copies (ALK-SP-0003.R5).
    pub fn prepend_library_dirs(&mut self, dirs: &[PathBuf]) {
        let var = library_path_var(std::env::consts::OS);
        let inherited = std::env::var_os(var);
        if let Some(value) = joined_search_path(dirs, inherited.as_deref()) {
            self.env.insert(var.to_string(), value);
        }
    }
}

/// The library search variable of `os` (a `std::env::consts::OS` value).
pub fn library_path_var(os: &str) -> &'static str {
    match os {
        "macos" | "ios" => "DYLD_LIBRARY_PATH",
        "windows" => "PATH",
        _ => "LD_LIBRARY_PATH",
    }
}

/// `dirs` followed by the entries of `inherited`, joined with the platform separator, or
/// `None` when `dirs` is empty or a directory contains the separator.
pub fn joined_search_path(dirs: &[PathBuf], inherited: Option<&OsStr>) -> Option<OsString> {
    if dirs.is_empty() {
        return None;
    }
    let mut all: Vec<PathBuf> = dirs.to_vec();
    if let Some(prev) = inherited {
        all.extend(std::env::split_paths(prev).filter(|p| !p.as_os_str().is_empty()));
    }
    std::env::join_paths(all).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_predicates() {
        assert!(ReadyPredicate::AnyResponse.matches(404, ""));
        assert!(!ReadyPredicate::Status2xx.matches(503, "{\"status\":\"loading\"}"));
        assert!(ReadyPredicate::Status2xx.matches(200, ""));
        let p = ReadyPredicate::Status2xxBodyContains("ok".into());
        assert!(p.matches(200, "{\"status\":\"ok\"}"));
        assert!(!p.matches(200, "{}"));
    }

    #[test]
    fn library_var_per_os() {
        assert_eq!(library_path_var("linux"), "LD_LIBRARY_PATH");
        assert_eq!(library_path_var("macos"), "DYLD_LIBRARY_PATH");
        assert_eq!(library_path_var("windows"), "PATH");
    }

    #[cfg(unix)]
    #[test]
    fn search_path_prepends_dirs() {
        let v = joined_search_path(
            &[PathBuf::from("/e/lib"), PathBuf::from("/e")],
            Some(OsStr::new("/usr/lib:/opt/lib")),
        )
        .unwrap();
        assert_eq!(v, OsString::from("/e/lib:/e:/usr/lib:/opt/lib"));
        assert!(joined_search_path(&[], Some(OsStr::new("/usr/lib"))).is_none());
    }

    #[test]
    fn base_url_formats_ipv6() {
        let mut s = LaunchSpec::new(
            EngineId::LlamaCpp,
            "llama-server",
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            8080,
            ApiDialect::LlamaServer,
        );
        assert_eq!(s.base_url(), "http://127.0.0.1:8080");
        s.host = "::1".parse().unwrap();
        assert_eq!(s.base_url(), "http://[::1]:8080");
    }
}
