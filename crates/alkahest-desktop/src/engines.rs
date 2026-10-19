//! Engines screen logic (screen 15, ALK-US-0072, ALK-SP-0003.R4), free of GPUI.
//!
//! Data: `GET /alkahest/v1/engines` (installed builds and recipe variants with their state
//! for the server's hardware), engine install jobs (`GET /alkahest/v1/jobs` plus the SSE
//! `job` events), `GET /alkahest/v1/info` for the "This machine" strip and the server
//! settings for `engines.auto_install`.

use std::collections::BTreeMap;

use alkahest_client::EngineOverride;
use alkahest_client::admin::{
    EngineInstall, EngineInstallOptions, EngineListing, EngineVariant, Job,
};
use serde_json::Value;

use crate::present::format_bytes;

/// What an engine id serves, for the card header.
pub fn engine_title(id: &str) -> (&str, &'static str) {
    match id {
        "llama-cpp" => (
            "llama.cpp",
            "GGUF chat, embeddings, rerank, vision \u{b7} mainline ggml-org",
        ),
        "prism-llama-cpp" => (
            "prism llama.cpp",
            "Ternary GGUF (PQ2_0, PTQ1_0) \u{b7} Bonsai models \u{b7} PrismML fork",
        ),
        "vllm" => (
            "vLLM",
            "safetensors chat and embeddings \u{b7} Linux + NVIDIA \u{b7} needs uv or python3",
        ),
        "whisper-cpp" => ("whisper.cpp", "speech to text \u{b7} GGML whisper models"),
        "stable-diffusion-cpp" => ("stable-diffusion.cpp", "image generation \u{b7} GGUF"),
        other => (other, ""),
    }
}

/// State of a variant chip (screen 15, by style).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChipState {
    /// On disk (verdigris border).
    Installed,
    /// Picked for this machine and not installed (brass border).
    Recommended,
    /// Installable (neutral).
    Available,
    /// An install job runs for it (signal blue).
    Building,
    /// No prebuilt archive for this host; a click builds from source (dashed).
    SourceOnly,
    /// Cannot run on this host (dashed, faint).
    Unsupported,
    /// An installed native build made for another CPU (dashed brass border).
    NativeStale,
}

/// One variant chip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VariantChip {
    /// Engine id.
    pub engine: String,
    /// Recipe version.
    pub version: String,
    /// Variant (`cuda12.8`).
    pub variant: String,
    /// Style.
    pub state: ChipState,
    /// Short note after the name (`Installed`, `recommended`, the reason it is unsupported),
    /// with the download size of installable variants (`recommended \u{b7} 142 MB`).
    pub note: String,
    /// The native variant (`cpu-native`): installed with `native: true`, never recommended.
    pub native: bool,
}

impl VariantChip {
    /// Whether a click installs it (and how).
    pub fn install_request(&self) -> Option<EngineInstall> {
        match self.state {
            ChipState::Recommended | ChipState::Available | ChipState::SourceOnly
                if self.native =>
            {
                // The server picks the native variant of its CPU (`variant` must stay empty).
                Some(EngineInstall {
                    engine: self.engine.clone(),
                    version: Some(self.version.clone()),
                    variant: None,
                    from_source: false,
                    force: false,
                })
            }
            ChipState::Recommended | ChipState::Available | ChipState::SourceOnly => {
                Some(EngineInstall {
                    engine: self.engine.clone(),
                    version: Some(self.version.clone()),
                    variant: Some(self.variant.clone()),
                    from_source: self.state == ChipState::SourceOnly,
                    force: false,
                })
            }
            ChipState::Installed
            | ChipState::Building
            | ChipState::Unsupported
            | ChipState::NativeStale => None,
        }
    }

    /// The install options that go with [`Self::install_request`].
    pub fn install_options(&self) -> EngineInstallOptions {
        EngineInstallOptions {
            with: Vec::new(),
            native: self.native,
            cuda_arch: None,
        }
    }
}

/// One installed build row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledBuild {
    /// Engine id.
    pub engine: String,
    /// Version.
    pub version: String,
    /// Variant.
    pub variant: String,
    /// `b11669 · cuda12.8`.
    pub label: String,
    /// A loaded model runs on this engine (newest build of the id).
    pub in_use: bool,
    /// `built from source` / `prebuilt`.
    pub origin: String,
    /// Shipped with the binary: it cannot be removed.
    pub bundled: bool,
    /// Executable.
    pub path: String,
    /// Built from source (dependency installs keep it that way).
    pub from_source: bool,
    /// A native build (built for a CPU).
    pub native: bool,
    /// A native build made for another CPU ("Rebuild for this CPU").
    pub native_stale: bool,
}

impl InstalledBuild {
    /// The native tag of the row: `native · built for this CPU` (neutral) or
    /// `built for another CPU` (brass-subtle warning, `true`).
    pub fn native_tag(&self) -> Option<(&'static str, bool)> {
        match (self.native, self.native_stale) {
            (_, true) => Some(("built for another CPU", true)),
            (true, false) => Some(("native \u{b7} built for this CPU", false)),
            _ => None,
        }
    }

    /// "Rebuild for this CPU": the native variant of this version, built again
    /// (`engines install --native --force`).
    pub fn rebuild_request(&self) -> Option<(EngineInstall, EngineInstallOptions)> {
        self.native_stale.then(|| {
            (
                EngineInstall {
                    engine: self.engine.clone(),
                    version: Some(self.version.clone()),
                    variant: None,
                    from_source: false,
                    force: true,
                },
                EngineInstallOptions {
                    with: Vec::new(),
                    native: true,
                    cuda_arch: None,
                },
            )
        })
    }
}

/// One dependency chip of an engine card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DependencyChip {
    /// Dependency id (`cuda-runtime`).
    pub id: String,
    /// The installed variant it belongs to (`cuda12.8`).
    pub variant: String,
    /// `optional · for cuda12.8 · 594 MB`, `with cuda12.8`.
    pub note: String,
    /// Present in the installed build (verdigris).
    pub installed: bool,
    /// "Install" for a missing optional dependency (`engines install <id> --with <dep>`).
    pub install: Option<(EngineInstall, EngineInstallOptions)>,
}

/// The recipe origin chip: `built-in` (dashed), `catalog official @ c1c1877b`, `recipes/`,
/// `user dir`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OriginChip {
    /// Text.
    pub text: String,
    /// The built-in layer (dashed border).
    pub builtin: bool,
}

/// The origin chip of a recipe layer (`built-in`, `catalog:<name>`, `dir:<path>`).
pub fn origin_chip(origin: &str, commit: Option<&str>) -> OriginChip {
    let text = if origin == "built-in" {
        "built-in".to_string()
    } else if let Some(name) = origin.strip_prefix("catalog:") {
        match commit {
            Some(c) => format!(
                "catalog {name} @ {}",
                crate::catalog_models::short_commit(c)
            ),
            None => format!("catalog {name}"),
        }
    } else if let Some(path) = origin.strip_prefix("dir:") {
        if path.trim_end_matches('/').ends_with("recipes") && !path.contains(".local") {
            "recipes/".to_string()
        } else {
            "user dir".to_string()
        }
    } else {
        origin.to_string()
    };
    OriginChip {
        builtin: origin == "built-in",
        text,
    }
}

/// One engine card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineCard {
    /// Engine id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// What it serves.
    pub serves: String,
    /// Newest recipe version (`recipe b11669`).
    pub recipe: Option<String>,
    /// Installed builds, newest first.
    pub installed: Vec<InstalledBuild>,
    /// Variant chips of the newest recipe version.
    pub chips: Vec<VariantChip>,
    /// The layer the newest recipe was read from.
    pub origin: Option<OriginChip>,
    /// Dependencies of the installed variants (row shown only when not empty).
    pub dependencies: Vec<DependencyChip>,
    /// A hint from the notes of a variant with an optional dependency.
    pub dependency_hint: Option<String>,
}

/// `id version variant`, the job target format of engine installs.
pub fn job_target(engine: &str, version: &str, variant: &str) -> String {
    format!("{engine} {version} {variant}")
}

fn version_key(v: &str) -> (u64, String) {
    // `b11669` and `prism-b10777-...` sort by their build number; others by text.
    let digits: String = v
        .split(|c: char| !c.is_ascii_digit())
        .find(|s| !s.is_empty())
        .unwrap_or_default()
        .to_string();
    (digits.parse().unwrap_or(0), v.to_string())
}

/// The cards, one per engine id in listing order. `running_jobs` are the targets of install
/// jobs in flight; `loaded_engines` the engine ids of loaded models.
pub fn cards(
    listing: &EngineListing,
    running_jobs: &[String],
    loaded_engines: &[String],
) -> Vec<EngineCard> {
    let mut order: Vec<String> = Vec::new();
    let mut by_id: BTreeMap<String, Vec<&EngineVariant>> = BTreeMap::new();
    for v in &listing.engines {
        if !by_id.contains_key(&v.id) {
            order.push(v.id.clone());
        }
        by_id.entry(v.id.clone()).or_default().push(v);
    }
    order
        .into_iter()
        .map(|id| {
            let rows = by_id.remove(&id).unwrap_or_default();
            let (name, serves) = engine_title(&id);
            let newest = rows
                .iter()
                .map(|r| r.version.clone())
                .max_by_key(|v| version_key(v));
            let mut installed: Vec<InstalledBuild> = rows
                .iter()
                .filter(|r| r.installed)
                .map(|r| InstalledBuild {
                    engine: id.clone(),
                    version: r.version.clone(),
                    variant: r.variant.clone(),
                    label: format!("{} \u{b7} {}", r.version, r.variant),
                    in_use: false,
                    origin: if r.from_source {
                        "built from source".into()
                    } else {
                        "prebuilt".into()
                    },
                    bundled: r.bundled,
                    path: r.path.clone().unwrap_or_default(),
                    from_source: r.from_source,
                    native: r.native,
                    native_stale: r.native_stale,
                })
                .collect();
            installed.sort_by_key(|b| std::cmp::Reverse(version_key(&b.version)));
            if loaded_engines.iter().any(|e| e == &id)
                && let Some(first) = installed.first_mut()
            {
                first.in_use = true;
            }
            let chips = newest
                .as_ref()
                .map(|ver| {
                    rows.iter()
                        .filter(|r| &r.version == ver)
                        .map(|r| chip(r, running_jobs))
                        .collect()
                })
                .unwrap_or_default();
            let origin = newest.as_ref().and_then(|ver| {
                rows.iter().filter(|r| &r.version == ver).find_map(|r| {
                    r.origin
                        .as_deref()
                        .map(|o| origin_chip(o, r.origin_commit.as_deref()))
                })
            });
            let (dependencies, dependency_hint) = dependency_chips(&rows);
            // A fork engine (ALK-US-0132) says what it forks and how a model opts in.
            let serves = match rows.iter().find_map(|r| r.base.as_deref()) {
                Some(base) => format!(
                    "fork of {} \u{b7} opt-in per model: engine={id}",
                    engine_title(base).0
                ),
                None => serves.to_string(),
            };
            EngineCard {
                name: name.to_string(),
                serves,
                recipe: newest.map(|v| format!("recipe {v}")),
                id,
                installed,
                chips,
                origin,
                dependencies,
                dependency_hint,
            }
        })
        .collect()
}

/// The dependency chips of the installed variants and a hint line.
fn dependency_chips(rows: &[&EngineVariant]) -> (Vec<DependencyChip>, Option<String>) {
    let mut out: Vec<DependencyChip> = Vec::new();
    let mut hint = None;
    for r in rows.iter().filter(|r| r.installed) {
        for d in &r.dependencies {
            if out.iter().any(|c| c.id == d.id && c.variant == r.variant) {
                continue;
            }
            let mut note = if d.optional {
                format!("optional \u{b7} for {}", r.variant)
            } else {
                format!("with {}", r.variant)
            };
            if let Some(size) = d.size.filter(|s| *s > 0) {
                note.push_str(&format!(" \u{b7} {}", format_bytes(size)));
            }
            let install = (d.optional && !d.installed).then(|| {
                (
                    EngineInstall {
                        engine: r.id.clone(),
                        version: Some(r.version.clone()),
                        variant: Some(r.variant.clone()),
                        from_source: r.from_source,
                        force: false,
                    },
                    EngineInstallOptions {
                        with: vec![d.id.clone()],
                        native: false,
                        cuda_arch: None,
                    },
                )
            });
            if d.optional && hint.is_none() {
                let first = r.notes.split(['\n']).next().unwrap_or_default().trim();
                if !first.is_empty() {
                    hint = Some(first.to_string());
                }
            }
            out.push(DependencyChip {
                id: d.id.clone(),
                variant: r.variant.clone(),
                note,
                installed: d.installed,
                install,
            });
        }
    }
    (out, hint)
}

fn chip(r: &EngineVariant, running_jobs: &[String]) -> VariantChip {
    let building = running_jobs
        .iter()
        .any(|t| t == &job_target(&r.id, &r.version, &r.variant));
    let (state, note) = if building {
        (ChipState::Building, "building\u{2026}".to_string())
    } else if r.installed && r.native_stale {
        (ChipState::NativeStale, "built for another CPU".to_string())
    } else if r.installed && r.native {
        (ChipState::Installed, "native".to_string())
    } else if r.native && r.state != "unsupported" {
        // Never recommended and never picked automatically: built from source on request.
        (
            ChipState::SourceOnly,
            "native \u{b7} on request".to_string(),
        )
    } else if r.installed {
        (ChipState::Installed, "Installed".to_string())
    } else if r.state == "unsupported" {
        (
            ChipState::Unsupported,
            short_note(&r.notes, "not for this host"),
        )
    } else if !r.prebuilt && r.source_build {
        (ChipState::SourceOnly, "from source".to_string())
    } else if r.recommended {
        (ChipState::Recommended, with_size("recommended", r))
    } else {
        (ChipState::Available, with_size("", r))
    };
    VariantChip {
        engine: r.id.clone(),
        version: r.version.clone(),
        variant: r.variant.clone(),
        state,
        note,
        native: r.native,
    }
}

/// `note` followed by the download size of the prebuilt archive, when the recipe states it.
fn with_size(note: &str, r: &EngineVariant) -> String {
    match (note.is_empty(), r.download_bytes.filter(|b| *b > 0)) {
        (_, None) => note.to_string(),
        (true, Some(b)) => format_bytes(b),
        (false, Some(b)) => format!("{note} \u{b7} {}", format_bytes(b)),
    }
}

fn short_note(notes: &str, fallback: &str) -> String {
    let first = notes
        .split(['.', ';', '\n'])
        .next()
        .unwrap_or_default()
        .trim();
    if first.is_empty() || first.len() > 40 {
        fallback.to_string()
    } else {
        first.to_string()
    }
}

/// "Install recommended builds": the recommended variant of every engine that has no build
/// installed yet.
pub fn recommended_installs(listing: &EngineListing) -> Vec<EngineInstall> {
    let mut out = Vec::new();
    let mut seen = Vec::new();
    for v in &listing.engines {
        if seen.contains(&v.id) {
            continue;
        }
        let rows: Vec<&EngineVariant> = listing.engines.iter().filter(|r| r.id == v.id).collect();
        seen.push(v.id.clone());
        if rows.iter().any(|r| r.installed) {
            continue;
        }
        if let Some(r) = rows
            .iter()
            .find(|r| r.recommended && r.prebuilt && !r.native)
        {
            out.push(EngineInstall {
                engine: r.id.clone(),
                version: Some(r.version.clone()),
                variant: Some(r.variant.clone()),
                from_source: false,
                force: false,
            });
        }
    }
    out
}

/// An install job card.
#[derive(Debug, Clone, PartialEq)]
pub struct JobCard {
    /// Job id (cancel target).
    pub id: String,
    /// `llama-cpp b11669 cuda12.8`.
    pub target: String,
    /// `Downloading`, `Building from source`, `Verifying sha256`, `Installed`, `Failed`.
    pub pill: String,
    /// Progress `0..=1`, when known.
    pub fraction: Option<f64>,
    /// `412 MB / 612 MB`.
    pub bytes: String,
    /// Phase lines seen so far (the log tail of the card).
    pub log: Vec<String>,
    /// Still running.
    pub running: bool,
    /// Failure text.
    pub error: Option<String>,
    /// Failed job: the same install again (a build from source stays a build from source).
    pub retry: Option<EngineInstall>,
    /// Failed source build: the prebuilt archive instead (design 17.8).
    pub prebuilt: Option<EngineInstall>,
}

/// The install request that repeats a job, from its target (`llama-cpp b11669 cuda12.8`).
/// `from_source` keeps a source build a source build; `force` replaces a half-installed
/// build.
pub fn reinstall_request(target: &str, from_source: bool) -> Option<EngineInstall> {
    let mut parts = target.split_whitespace();
    let engine = parts.next()?.to_string();
    Some(EngineInstall {
        engine,
        version: parts.next().map(str::to_string),
        variant: parts.next().map(str::to_string),
        from_source,
        force: true,
    })
}

/// Phase history per job id, kept while the screen lives (the server reports the current
/// phase only).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct JobLog {
    lines: BTreeMap<String, Vec<String>>,
}

impl JobLog {
    /// Records the phase of `job` if it changed.
    pub fn record(&mut self, job: &Job) {
        let lines = self.lines.entry(job.id.clone()).or_default();
        let line = match (&job.error, job.state.as_str()) {
            (Some(e), _) => format!("failed: {e}"),
            (None, "completed") => "installed".to_string(),
            (None, "cancelled") => "cancelled".to_string(),
            _ => job.phase.clone(),
        };
        if !line.is_empty() && lines.last() != Some(&line) {
            lines.push(line);
            if lines.len() > 8 {
                lines.remove(0);
            }
        }
    }

    /// Lines of job `id`.
    pub fn lines(&self, id: &str) -> &[String] {
        self.lines.get(id).map_or(&[], Vec::as_slice)
    }
}

fn pill_label(job: &Job) -> String {
    match job.state.as_str() {
        "completed" => "Installed".into(),
        "failed" => "Failed".into(),
        "cancelled" => "Cancelled".into(),
        _ => match job.phase.as_str() {
            "building" => "Building from source".into(),
            "verifying" => "Verifying sha256".into(),
            "extracting" => "Extracting".into(),
            "downloading" | "" => "Downloading".into(),
            other => {
                let mut c = other.chars();
                c.next().map_or_else(String::new, |f| {
                    f.to_uppercase().collect::<String>() + c.as_str()
                })
            }
        },
    }
}

/// Whether the phase log shows a build from source (the server reports `building`).
fn built_from_source(lines: &[String]) -> bool {
    lines.iter().any(|l| l == "building")
}

/// Cards for the engine install jobs: running ones, plus failures not dismissed yet.
pub fn job_cards(jobs: &[Job], log: &JobLog) -> Vec<JobCard> {
    jobs.iter()
        .filter(|j| j.kind == "engine_install" && (j.state == "running" || j.state == "failed"))
        .map(|j| JobCard {
            id: j.id.clone(),
            target: j.target.clone(),
            pill: pill_label(j),
            fraction: j.fraction(),
            bytes: match j.total {
                Some(t) if t > 0 => format!("{} / {}", format_bytes(j.completed), format_bytes(t)),
                _ if j.completed > 0 => format_bytes(j.completed),
                _ => String::new(),
            },
            log: log.lines(&j.id).to_vec(),
            running: j.state == "running",
            error: j.error.clone(),
            retry: (j.state == "failed")
                .then(|| reinstall_request(&j.target, built_from_source(log.lines(&j.id))))
                .flatten(),
            prebuilt: (j.state == "failed" && built_from_source(log.lines(&j.id)))
                .then(|| reinstall_request(&j.target, false))
                .flatten(),
        })
        .collect()
}

/// The "This machine" strip: one entry per detected device, from `/alkahest/v1/info`
/// (`system.hardware`).
pub fn machine_facts(system: &Value) -> Vec<String> {
    let hw = &system["hardware"];
    let mut out = Vec::new();
    for g in hw["gpus"].as_array().into_iter().flatten() {
        let mut parts = vec![g["name"].as_str().unwrap_or("GPU").to_string()];
        if let Some(v) = g["vram_total_bytes"].as_u64().filter(|v| *v > 0) {
            parts.push(format_bytes(v));
        }
        if let Some(d) = g["driver_version"].as_str() {
            parts.push(format!("driver {d}"));
        }
        if let Some(c) = g["cuda_version"].as_array().filter(|c| c.len() == 2) {
            parts.push(format!(
                "CUDA \u{2264} {}.{}",
                c[0].as_u64().unwrap_or(0),
                c[1].as_u64().unwrap_or(0)
            ));
        }
        out.push(parts.join(" \u{b7} "));
    }
    let cpu = &hw["cpu"];
    if cpu.is_object() {
        let mut parts = vec![format!("CPU {}", cpu["arch"].as_str().unwrap_or_default())];
        if let Some(n) = cpu["logical_cores"].as_u64() {
            parts.push(format!("{n} threads"));
        }
        out.push(parts.join(" \u{b7} "));
    }
    out
}

/// Splits an extra-arguments line into arguments: whitespace separates them, single or double
/// quotes keep spaces inside one (`--chat-template "my template"`).
pub fn split_args(line: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    for ch in line.chars() {
        match (quote, ch) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '"' | '\'') => {
                quote = Some(ch);
                started = true;
            }
            (None, c) if c.is_whitespace() => {
                if started || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    started = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if quote.is_some() {
        return Err("A quote is not closed.".into());
    }
    if started || !cur.is_empty() {
        out.push(cur);
    }
    Ok(out)
}

/// The inverse of [`split_args`]: arguments with spaces or quotes are double-quoted.
pub fn join_args(args: &[String]) -> String {
    args.iter()
        .map(|a| {
            if a.is_empty() || a.contains(char::is_whitespace) || a.contains(['"', '\'']) {
                if a.contains('"') {
                    format!("'{a}'")
                } else {
                    format!("\"{a}\"")
                }
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `path` is absolute on Linux, macOS or Windows (the server may be any of them).
fn absolute(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with("\\\\")
        || path
            .as_bytes()
            .get(1..3)
            .is_some_and(|b| b == b":\\" || b == b":/")
}

/// The override sheet's form: what the two fields hold.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OverrideForm {
    /// Executable path (empty = the installed build).
    pub path: String,
    /// Extra arguments, one line (see [`split_args`]).
    pub args: String,
}

impl OverrideForm {
    /// The form showing `o`.
    pub fn from_override(o: &EngineOverride) -> Self {
        Self {
            path: o.path.clone().unwrap_or_default(),
            args: join_args(&o.extra_args),
        }
    }

    /// The override the form describes, or why it cannot be saved.
    pub fn to_override(&self) -> Result<EngineOverride, String> {
        let path = self.path.trim();
        if !path.is_empty() && !absolute(path) {
            return Err("The executable path must be absolute.".into());
        }
        Ok(EngineOverride {
            path: (!path.is_empty()).then(|| path.to_string()),
            extra_args: split_args(&self.args)?,
        })
    }
}

/// The line after "Reload recipes": counts, and the recipe files the server ignored with the
/// reason. The flag is `true` when something was ignored (shown as a warning).
pub fn reload_summary(doc: &Value) -> (String, bool) {
    let engines = doc["engines"].as_array().map_or(0, Vec::len);
    let variants = doc["variants"].as_u64().unwrap_or(0);
    let mut line = format!("Recipes reloaded: {engines} engines, {variants} variants.");
    let issues = doc["issues"].as_array().cloned().unwrap_or_default();
    if issues.is_empty() {
        return (line, false);
    }
    let what: Vec<String> = issues
        .iter()
        .map(|i| {
            format!(
                "{} ({})",
                i["path"].as_str().unwrap_or("?"),
                i["reason"].as_str().unwrap_or("ignored")
            )
        })
        .collect();
    line.push_str(&format!(
        " {} recipe file{} ignored: {}.",
        issues.len(),
        if issues.len() == 1 { "" } else { "s" },
        what.join("; ")
    ));
    (line, true)
}

/// Accelerator preference order shown in the strip.
pub const PREFERENCE: &str = "Metal \u{203a} CUDA \u{203a} ROCm \u{203a} Vulkan \u{203a} CPU";

/// The notice for a refused or failed removal of `build`. `error` is the client error text
/// (`<message> (HTTP <status>)`): the server answers 409 while loaded models still run on the
/// build, and its message names them.
pub fn remove_failure(engine_name: &str, build: &str, error: &str) -> String {
    match error.strip_suffix(" (HTTP 409)") {
        Some(message) => format!(
            "{engine_name} {build} was not removed: {}. Stop those models from the Running screen \
             first.",
            message.trim_end_matches('.')
        ),
        None => format!("Cannot remove {engine_name} {build}: {error}"),
    }
}

/// A pending remove confirmation (screen 17.13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveAsk {
    /// The build.
    pub build: InstalledBuild,
    /// Display name of the engine.
    pub engine_name: String,
    /// What models fall back to (another installed build of the same engine), if any.
    pub fallback: Option<String>,
}

impl RemoveAsk {
    /// Asks to remove `build` of `card`.
    pub fn new(card: &EngineCard, build: &InstalledBuild) -> Self {
        let fallback = card
            .installed
            .iter()
            .find(|b| b.label != build.label)
            .map(|b| b.label.clone());
        Self {
            build: build.clone(),
            engine_name: card.name.clone(),
            fallback,
        }
    }

    /// The consequence sentence.
    pub fn consequence(&self) -> String {
        match &self.fallback {
            Some(f) => format!(
                "Models on this engine fall back to {f}. Running instances keep going until \
                 they unload."
            ),
            None => format!(
                "No other {} build stays installed: models that need it fail until one is \
                 installed again. Running instances keep going until they unload.",
                self.engine_name
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use serde_json::json;

    use super::*;

    fn v(id: &str, version: &str, variant: &str, state: &str) -> EngineVariant {
        EngineVariant {
            id: id.into(),
            version: version.into(),
            variant: variant.into(),
            state: state.into(),
            installed: state == "installed",
            recommended: state == "recommended",
            prebuilt: state != "source",
            source_build: true,
            path: (state == "installed").then(|| format!("/e/{id}/{variant}/llama-server")),
            ..EngineVariant::default()
        }
    }

    #[test]
    fn a_fork_engine_gets_its_own_card_naming_its_family() {
        let mut fork = v("llama-cpp-turboquant", "b1", "cpu", "source");
        fork.base = Some("llama-cpp".into());
        let l = EngineListing {
            accelerators: vec!["cpu".into()],
            engines: vec![v("llama-cpp", "b11669", "cpu", "installed"), fork],
        };
        let c = cards(&l, &[], &[]);
        assert_eq!(c.len(), 2);
        assert_eq!(c[1].id, "llama-cpp-turboquant");
        assert_eq!(c[1].name, "llama-cpp-turboquant");
        assert_eq!(
            c[1].serves,
            "fork of llama.cpp \u{b7} opt-in per model: engine=llama-cpp-turboquant"
        );
        assert!(!c[0].serves.contains("fork of"));
    }

    fn listing() -> EngineListing {
        let mut source = v(
            "prism-llama-cpp",
            "prism-b10777-e3de715",
            "cuda13.3",
            "available",
        );
        source.prebuilt = false;
        let mut unsupported = v("llama-cpp", "b11669", "rocm", "unsupported");
        unsupported.notes = "no AMD GPU".into();
        EngineListing {
            accelerators: vec!["cuda12.8".into(), "vulkan".into(), "cpu".into()],
            engines: vec![
                v("llama-cpp", "b11669", "cpu", "installed"),
                v("llama-cpp", "b11669", "cuda12.8", "recommended"),
                v("llama-cpp", "b11669", "vulkan", "available"),
                unsupported,
                source,
                v(
                    "prism-llama-cpp",
                    "prism-b10777-e3de715",
                    "cuda12.8",
                    "recommended",
                ),
                v("vllm", "0.11.0", "cuda12.8", "recommended"),
            ],
        }
    }

    #[test]
    fn bundled_builds_and_download_sizes_show_in_the_cards() {
        let mut l = listing();
        l.engines[0].bundled = true;
        l.engines[1].download_bytes = Some(142_000_000);
        l.engines[2].download_bytes = Some(31_500_000);
        let c = cards(&l, &[], &[]);
        assert!(c[0].installed[0].bundled);
        let chip = |variant: &str| c[0].chips.iter().find(|x| x.variant == variant).unwrap();
        assert_eq!(chip("cuda12.8").note, "recommended \u{b7} 142 MB");
        assert_eq!(chip("vulkan").note, "32 MB");
        assert_eq!(
            chip("rocm").note,
            "no AMD GPU",
            "unsupported keeps its reason"
        );
        assert!(!c[1].installed.iter().any(|b| b.bundled));
    }

    #[test]
    fn override_forms_round_trip_and_validate() {
        assert_eq!(
            split_args(r#"--mlock --chat-template "my template" -c '4 096'"#).unwrap(),
            ["--mlock", "--chat-template", "my template", "-c", "4 096"]
        );
        assert_eq!(split_args("  ").unwrap(), Vec::<String>::new());
        assert_eq!(split_args(r#"a "" b"#).unwrap(), ["a", "", "b"]);
        assert!(split_args("--x \"open").is_err());
        let args = vec!["--mlock".to_string(), "a b".to_string(), "it's".to_string()];
        assert_eq!(split_args(&join_args(&args)).unwrap(), args);

        let form = OverrideForm {
            path: " /opt/llama/llama-server ".into(),
            args: "--mlock".into(),
        };
        let o = form.to_override().unwrap();
        assert_eq!(o.path.as_deref(), Some("/opt/llama/llama-server"));
        assert_eq!(OverrideForm::from_override(&o).args, "--mlock");
        assert!(OverrideForm::default().to_override().unwrap() == EngineOverride::default());
        let bad = OverrideForm {
            path: "llama-server".into(),
            args: String::new(),
        };
        assert!(bad.to_override().unwrap_err().contains("absolute"));
        assert!(
            OverrideForm {
                path: r"C:\llama\llama-server.exe".into(),
                args: String::new()
            }
            .to_override()
            .is_ok()
        );
    }

    #[test]
    fn reload_reports_ignored_recipes() {
        let (line, warn) =
            reload_summary(&json!({"engines": ["a", "b"], "variants": 9, "issues": []}));
        assert_eq!(line, "Recipes reloaded: 2 engines, 9 variants.");
        assert!(!warn);
        let (line, warn) = reload_summary(&json!({"engines": ["a"], "variants": 3,
            "issues": [{"path": "/r/bad.toml", "reason": "unknown field"}]}));
        assert!(warn);
        assert!(
            line.contains("1 recipe file ignored: /r/bad.toml (unknown field)"),
            "{line}"
        );
    }

    #[test]
    fn cuda_pick() {
        // Scenario ALK-SP-0003.R4 "cuda pick" as the desktop shows it: on an NVIDIA host the
        // server marks the CUDA variant recommended; the chip carries the brass state and a
        // click installs exactly that variant (prebuilt, not from source).
        let c = cards(&listing(), &[], &[]);
        let llama = &c[0];
        assert_eq!(llama.name, "llama.cpp");
        assert_eq!(llama.recipe.as_deref(), Some("recipe b11669"));
        let cuda = llama
            .chips
            .iter()
            .find(|ch| ch.variant == "cuda12.8")
            .unwrap();
        assert_eq!(cuda.state, ChipState::Recommended);
        let req = cuda.install_request().unwrap();
        assert_eq!(
            (req.engine.as_str(), req.variant.as_deref(), req.from_source),
            ("llama-cpp", Some("cuda12.8"), false)
        );
        let states: Vec<_> = llama.chips.iter().map(|ch| ch.state).collect();
        assert_eq!(
            states,
            [
                ChipState::Installed,
                ChipState::Recommended,
                ChipState::Available,
                ChipState::Unsupported
            ]
        );
        assert_eq!(llama.chips[3].note, "no AMD GPU");
        assert!(llama.chips[3].install_request().is_none());
    }

    #[test]
    fn source_only_and_building_chips() {
        let running = vec![job_target(
            "prism-llama-cpp",
            "prism-b10777-e3de715",
            "cuda12.8",
        )];
        let c = cards(&listing(), &running, &["llama-cpp".into()]);
        let prism = &c[1];
        assert_eq!(prism.chips[0].state, ChipState::SourceOnly);
        assert!(prism.chips[0].install_request().unwrap().from_source);
        assert_eq!(prism.chips[1].state, ChipState::Building);
        assert!(c[0].installed[0].in_use, "a loaded model runs on llama-cpp");
        assert!(prism.installed.is_empty());
    }

    #[test]
    fn install_recommended_covers_engines_without_a_build() {
        let reqs = recommended_installs(&listing());
        let ids: Vec<_> = reqs.iter().map(|r| r.engine.as_str()).collect();
        assert_eq!(
            ids,
            ["prism-llama-cpp", "vllm"],
            "llama-cpp already has a build"
        );
    }

    #[test]
    fn job_cards_keep_a_phase_log() {
        let mut job = Job {
            id: "j1".into(),
            kind: "engine_install".into(),
            target: "llama-cpp b11669 cuda12.8".into(),
            state: "running".into(),
            phase: "downloading".into(),
            completed: 50 << 20,
            total: Some(100 << 20),
            ..Job::default()
        };
        let mut log = JobLog::default();
        log.record(&job);
        log.record(&job);
        job.phase = "verifying".into();
        log.record(&job);
        let cards = job_cards(std::slice::from_ref(&job), &log);
        assert_eq!(cards[0].pill, "Verifying sha256");
        assert_eq!(cards[0].log, ["downloading", "verifying"]);
        assert_eq!(cards[0].fraction, Some(0.5));
        job.state = "completed".into();
        assert!(
            job_cards(&[job], &log).is_empty(),
            "finished jobs leave the list"
        );
    }

    #[test]
    fn a_failed_source_build_offers_retry_and_prebuilt() {
        let mut job = Job {
            id: "j2".into(),
            kind: "engine_install".into(),
            target: "llama-cpp b11669 cuda12.8".into(),
            state: "running".into(),
            phase: "building".into(),
            ..Job::default()
        };
        let mut log = JobLog::default();
        log.record(&job);
        assert!(
            job_cards(std::slice::from_ref(&job), &log)[0]
                .retry
                .is_none()
        );
        job.state = "failed".into();
        job.error = Some("cmake exited with status 1".into());
        log.record(&job);
        let card = &job_cards(std::slice::from_ref(&job), &log)[0];
        let retry = card.retry.as_ref().unwrap();
        assert_eq!(
            (
                retry.engine.as_str(),
                retry.version.as_deref(),
                retry.variant.as_deref()
            ),
            ("llama-cpp", Some("b11669"), Some("cuda12.8"))
        );
        assert!(retry.from_source && retry.force, "same build again");
        let prebuilt = card.prebuilt.as_ref().unwrap();
        assert!(!prebuilt.from_source, "install prebuilt instead");
        // A failed download has nothing to switch to.
        let mut log = JobLog::default();
        job.phase = "downloading".into();
        log.record(&job);
        let card = &job_cards(&[job], &log)[0];
        assert!(card.prebuilt.is_none());
        assert!(!card.retry.as_ref().unwrap().from_source);
    }

    #[test]
    fn machine_strip_and_remove_fallback() {
        let sys = json!({"hardware": {
            "gpus": [{"name": "NVIDIA RTX 4000 SFF Ada", "vram_total_bytes": 21_474_836_480u64,
                      "driver_version": "580.173", "cuda_version": [13, 0]}],
            "cpu": {"arch": "x86_64", "logical_cores": 16}
        }});
        let facts = machine_facts(&sys);
        assert!(
            facts[0].contains("NVIDIA RTX 4000 SFF Ada") && facts[0].contains("CUDA \u{2264} 13.0")
        );
        assert_eq!(facts[1], "CPU x86_64 \u{b7} 16 threads");
        let mut l = listing();
        l.engines
            .push(v("llama-cpp", "b11669", "cuda12.8", "installed"));
        let c = cards(&l, &[], &[]);
        let ask = RemoveAsk::new(&c[0], &c[0].installed[0]);
        assert!(ask.consequence().contains("fall back to"));
    }

    #[test]
    fn removing_a_build_in_use_is_explained() {
        let busy = alkahest_client::ClientError::Api {
            status: 409,
            message: "engine llama-cpp b11669 is used by the loaded models qwen3:8b".into(),
        }
        .to_string();
        let m = remove_failure("llama.cpp", "b11669 \u{b7} cpu", &busy);
        assert!(
            m.contains("was not removed") && m.contains("qwen3:8b") && m.contains("Running"),
            "{m}"
        );
        assert!(!m.contains("HTTP"), "{m}");
        let other = alkahest_client::ClientError::Api {
            status: 500,
            message: "boom".into(),
        }
        .to_string();
        assert!(
            remove_failure("llama.cpp", "b1", &other).starts_with("Cannot remove llama.cpp b1")
        );
    }

    #[test]
    fn origin_native_builds_and_dependencies() {
        use alkahest_client::admin::EngineDependency;
        let base = |variant: &str| EngineVariant {
            id: "llama-cpp".into(),
            version: "b11669".into(),
            variant: variant.into(),
            state: "available".into(),
            prebuilt: true,
            origin: Some("catalog:official".into()),
            origin_commit: Some("c1c1877b99".into()),
            ..EngineVariant::default()
        };
        let cuda = EngineVariant {
            installed: true,
            state: "installed".into(),
            notes: "Not needed when the CUDA 12 runtime is installed on the host.".into(),
            dependencies: vec![EngineDependency {
                id: "cuda-runtime".into(),
                optional: true,
                size: Some(594_000_000),
                installed: false,
            }],
            ..base("cuda12.8")
        };
        let stale = EngineVariant {
            installed: true,
            state: "installed".into(),
            native: true,
            native_stale: true,
            from_source: true,
            prebuilt: false,
            source_build: true,
            ..base("cpu-native")
        };
        let listing = EngineListing {
            accelerators: vec!["cuda".into()],
            engines: vec![cuda, stale],
        };
        let c = &cards(&listing, &[], &[])[0];
        assert_eq!(
            c.origin,
            Some(OriginChip {
                text: "catalog official @ c1c1877b".into(),
                builtin: false
            })
        );
        let native = c
            .installed
            .iter()
            .find(|b| b.variant == "cpu-native")
            .unwrap();
        assert_eq!(native.native_tag(), Some(("built for another CPU", true)));
        let (req, opts) = native.rebuild_request().unwrap();
        assert!(opts.native && req.force && req.variant.is_none());
        let plain = c
            .installed
            .iter()
            .find(|b| b.variant == "cuda12.8")
            .unwrap();
        assert!(plain.native_tag().is_none() && plain.rebuild_request().is_none());
        assert_eq!(c.dependencies.len(), 1);
        let dep = &c.dependencies[0];
        assert_eq!(dep.note, "optional \u{b7} for cuda12.8 \u{b7} 594 MB");
        let (req, opts) = dep.install.clone().unwrap();
        assert_eq!(req.variant.as_deref(), Some("cuda12.8"));
        assert_eq!(opts.with, ["cuda-runtime"]);
        assert!(
            c.dependency_hint
                .as_deref()
                .unwrap()
                .starts_with("Not needed")
        );
        let chip = c
            .chips
            .iter()
            .find(|ch| ch.variant == "cpu-native")
            .unwrap();
        assert_eq!(chip.state, ChipState::NativeStale);
        assert!(chip.install_request().is_none());
        // A native variant not installed is source-only on request, never recommended.
        let fresh = EngineVariant {
            native: true,
            recommended: true,
            source_build: true,
            prebuilt: false,
            ..base("cpu-native")
        };
        let ch = super::chip(&fresh, &[]);
        assert_eq!(ch.state, ChipState::SourceOnly);
        let req = ch.install_request().unwrap();
        assert!(req.variant.is_none() && ch.install_options().native);
        assert!(
            recommended_installs(&EngineListing {
                accelerators: vec![],
                engines: vec![EngineVariant {
                    prebuilt: true,
                    ..fresh
                }],
            })
            .is_empty()
        );
        assert_eq!(origin_chip("built-in", None).text, "built-in");
        assert!(origin_chip("built-in", None).builtin);
        assert_eq!(
            origin_chip("dir:/home/u/.local/share/alkahest/recipes", None).text,
            "user dir"
        );
    }
}
