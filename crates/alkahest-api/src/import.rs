//! Importing models from local files and folders, shared by `alkahest create`,
//! `alkahest import`, `alkahest run <path>` and `POST /alkahest/v1/models/import`
//! (ALK-US-0127, ALK-SP-0002.R7 to R10).
//!
//! What can be imported:
//!
//! * a `.gguf` file: one model, named after the file stem; a shard of a split model
//!   (`x-00001-of-00003.gguf`) brings its sibling shards;
//! * a folder holding one GGUF model (single file or every shard of a split one) and
//!   optionally an `mmproj*.gguf` projector, which becomes the vision layer;
//! * a Hugging Face safetensors folder (`config.json` and `*.safetensors`), stored as a
//!   snapshot model for vLLM, the same layout `pull --safetensors` produces;
//! * a Modelfile (`alkahest create <name> -f Modelfile`, or a `Modelfile` inside the imported
//!   folder): `FROM ./file.gguf` or `FROM ./folder` resolved against the Modelfile's folder,
//!   or `FROM <model>` for a plain create from an existing model; `TEMPLATE`, `PARAMETER`,
//!   `SYSTEM` and `LICENSE` become layers. `ADAPTER` is refused: no engine applies LoRA
//!   adapters yet.
//!
//! Files are copied by default or linked with [`ImportMode::Link`]; see
//! [`alkahest_store::import`]. Sources are only ever read, and nothing is written under the
//! Ollama store (a source inside it is linked symbolically, never hard linked).
//!
//! The work is blocking file I/O: async callers run [`import_model`] on the blocking pool.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alkahest_hf::RepoFile;
use alkahest_hf::safetensors::{SnapshotFacts, chat_template_of, select_snapshot_files};
use alkahest_store::{
    Catalog, CreateLayers, ImportHow, ImportMode, ImportedBlob, ImportedModel, ImportedWeights,
    ModelConfig, ModelSource, StoreError, destination_name, format_parameters,
};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::ollama::create::{Modelfile, parameters_json, parse_modelfile};

/// Largest side file (config, tokenizer, Modelfile) read whole while planning an import.
const MAX_SIDE_FILE: u64 = 64 * 1024 * 1024;

/// The Modelfile a `create -f` passes, with the folder its relative paths resolve against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelfileInput {
    /// The Modelfile text.
    pub text: String,
    /// Folder of the Modelfile (`FROM ./x.gguf` is relative to it).
    pub dir: PathBuf,
}

/// One import.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportRequest {
    /// File or folder to import; `None` when a Modelfile's `FROM` names it.
    pub path: Option<PathBuf>,
    /// Model name; defaults to the file stem or folder name.
    pub name: Option<String>,
    /// Copy (default) or link.
    pub mode: ImportMode,
    /// A Modelfile given explicitly (`-f`); it wins over a `Modelfile` inside the folder.
    pub modelfile: Option<ModelfileInput>,
}

/// One file of an import plan, announced before any byte is read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlannedFile {
    /// Display label (file name, or path inside the folder).
    pub label: String,
    /// Absolute or user-given path of the source.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
}

/// Progress of an import.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImportEvent {
    /// The files that will be read, in order, and how.
    Plan {
        /// The files.
        files: Vec<PlannedFile>,
        /// Copy or link.
        mode: ImportMode,
    },
    /// File `index` started (copying, or hashing in link mode).
    Started {
        /// Index into the plan.
        index: usize,
    },
    /// Bytes of file `index` processed so far.
    Progress {
        /// Index into the plan.
        index: usize,
        /// Bytes processed.
        done: u64,
        /// File size.
        total: u64,
    },
    /// File `index` is in the store.
    Finished {
        /// Index into the plan.
        index: usize,
        /// What happened to it.
        how: ImportHow,
    },
    /// Every file is in; the manifest is being written.
    Installing,
    /// The model is registered.
    Done,
}

/// Callback receiving [`ImportEvent`]s.
pub type ImportProgress = Arc<dyn Fn(ImportEvent) + Send + Sync>;

/// One imported file in an [`ImportSummary`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportedFile {
    /// Source path.
    pub path: String,
    /// Blob digest (the sha256 of the source).
    pub digest: String,
    /// Bytes.
    pub size: u64,
    /// `copied`, `present`, `hardlinked` or `symlinked`.
    pub how: ImportHow,
}

/// How an import ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ImportSummary {
    /// Short display name (`tiny`, `hf.co/org/repo:Q4_K_M`).
    pub name: String,
    /// Fully qualified stored name.
    pub reference: String,
    /// Manifest digest.
    pub digest: String,
    /// `gguf`, `safetensors`, or `model` for a Modelfile `FROM <model>`.
    pub format: String,
    /// Copy or link.
    pub mode: ImportMode,
    /// Files brought in, in layer order.
    pub files: Vec<ImportedFile>,
    /// Bytes copied into the store by this run.
    pub copied: u64,
}

/// Why an import failed.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The request cannot be served as given (no such file, nothing importable, bad
    /// Modelfile, ambiguous folder, invalid name).
    #[error("{0}")]
    Invalid(String),
    /// The store refused or failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// Cancelled by the caller; nothing was registered.
    #[error("the import was cancelled")]
    Cancelled,
}

impl ImportError {
    fn invalid(msg: impl Into<String>) -> Self {
        Self::Invalid(msg.into())
    }
}

/// What a request resolved to.
#[derive(Debug, Clone, PartialEq)]
enum Plan {
    /// GGUF shards in order and an optional projector.
    Gguf {
        shards: Vec<PathBuf>,
        projector: Option<PathBuf>,
    },
    /// A safetensors folder and its snapshot files (relative names).
    Safetensors { dir: PathBuf, files: Vec<String> },
    /// `FROM <model>` of a Modelfile: an ordinary create from an existing model.
    FromModel(String),
}

/// A resolved request.
#[derive(Debug, Clone, PartialEq)]
struct Resolved {
    plan: Plan,
    layers: CreateLayers,
    default_name: Option<String>,
    reference: String,
}

/// Imports a local file or folder into the Alkahest store of `catalog` and registers it.
///
/// Blocking. `progress` receives the plan, per-file byte progress and the end; `cancel`
/// stops the import between chunks, leaving no blob or manifest behind.
pub fn import_model(
    catalog: &Catalog,
    req: &ImportRequest,
    progress: Option<&ImportProgress>,
    cancel: &CancellationToken,
) -> Result<ImportSummary, ImportError> {
    let emit = |e: ImportEvent| {
        if let Some(p) = progress {
            p(e);
        }
    };
    let resolved = resolve(req)?;
    let name_text = match (&req.name, &resolved.default_name) {
        (Some(n), _) if !n.trim().is_empty() => n.trim().to_string(),
        (_, Some(d)) => d.clone(),
        _ => {
            return Err(ImportError::invalid(
                "a model name is required (alkahest create <name> ...)",
            ));
        }
    };
    let name = destination_name(&name_text).map_err(|e| {
        ImportError::invalid(format!(
            "{name_text:?} is not a valid model name ({e}); pass another one with --name"
        ))
    })?;

    if let Plan::FromModel(from) = &resolved.plan {
        let (stored, report) =
            catalog.create(&name.to_string(), Some(from), None, &resolved.layers)?;
        emit(ImportEvent::Done);
        return Ok(ImportSummary {
            name: stored.display_short(),
            reference: stored.to_string(),
            digest: report.manifest_digest.to_string(),
            format: "model".into(),
            mode: req.mode,
            files: Vec::new(),
            copied: 0,
        });
    }

    // Sources, in layer order, with their display labels.
    let sources: Vec<(String, PathBuf)> = match &resolved.plan {
        Plan::Gguf { shards, projector } => shards
            .iter()
            .chain(projector)
            .map(|p| (file_label(p), p.clone()))
            .collect(),
        Plan::Safetensors { dir, files } => {
            files.iter().map(|f| (f.clone(), dir.join(f))).collect()
        }
        Plan::FromModel(_) => Vec::new(),
    };
    let mut planned = Vec::with_capacity(sources.len());
    for (label, path) in &sources {
        let size = fs::metadata(path)
            .map_err(|e| ImportError::invalid(format!("cannot read {}: {e}", path.display())))?
            .len();
        planned.push(PlannedFile {
            label: label.clone(),
            path: path.display().to_string(),
            size,
        });
    }
    emit(ImportEvent::Plan {
        files: planned.clone(),
        mode: req.mode,
    });

    let store = catalog.alkahest();
    let ollama_root = catalog
        .ollama()
        .and_then(|o| fs::canonicalize(o.root()).ok());
    // Held until the manifest is written, so GC cannot collect the new blobs in between.
    let _lock = store.lock_shared()?;
    let mut blobs: Vec<ImportedBlob> = Vec::with_capacity(sources.len());
    for (index, (_, path)) in sources.iter().enumerate() {
        emit(ImportEvent::Started { index });
        let total = planned[index].size;
        let in_ollama = ollama_root
            .as_ref()
            .is_some_and(|root| fs::canonicalize(path).is_ok_and(|p| p.starts_with(root)));
        let mut tick = |done: u64| {
            emit(ImportEvent::Progress { index, done, total });
            !cancel.is_cancelled()
        };
        let blob = store
            .import_file(path, req.mode, in_ollama, &mut tick)
            .map_err(|e| match e {
                StoreError::Io { ref source, .. }
                    if source.kind() == std::io::ErrorKind::Interrupted =>
                {
                    ImportError::Cancelled
                }
                other => ImportError::Store(other),
            })?;
        emit(ImportEvent::Finished {
            index,
            how: blob.how,
        });
        blobs.push(blob);
    }
    if cancel.is_cancelled() {
        return Err(ImportError::Cancelled);
    }

    emit(ImportEvent::Installing);
    let named = |i: usize| (sources[i].0.clone(), blobs[i].clone());
    let (weights, format) = match &resolved.plan {
        Plan::Gguf { shards, projector } => (
            ImportedWeights::Gguf {
                shards: (0..shards.len()).map(named).collect(),
                projector: projector.as_ref().map(|_| named(shards.len())),
            },
            "gguf",
        ),
        Plan::Safetensors { dir, files } => {
            let config = safetensors_config(dir, files, &blobs)?;
            (
                ImportedWeights::Safetensors {
                    files: (0..files.len()).map(named).collect(),
                    config: Box::new(config),
                },
                "safetensors",
            )
        }
        Plan::FromModel(_) => unreachable_plan()?,
    };
    let digest = store.write_imported_model(
        &name,
        &ImportedModel {
            weights,
            layers: resolved.layers.clone(),
            reference: resolved.reference.clone(),
        },
    )?;
    emit(ImportEvent::Done);
    let copied = blobs
        .iter()
        .filter(|b| b.how == ImportHow::Copied)
        .map(|b| b.size)
        .sum();
    Ok(ImportSummary {
        name: name.display_short(),
        reference: name.to_string(),
        digest: digest.to_string(),
        format: format.into(),
        mode: req.mode,
        files: sources
            .iter()
            .zip(&blobs)
            .map(|((_, p), b)| ImportedFile {
                path: p.display().to_string(),
                digest: b.digest.to_string(),
                size: b.size,
                how: b.how,
            })
            .collect(),
        copied,
    })
}

/// `FromModel` never reaches the file-import half (it returns early); this keeps the match
/// exhaustive without a panic.
fn unreachable_plan<T>() -> Result<T, ImportError> {
    Err(ImportError::invalid("internal error: no files to import"))
}

/// The default model name of a local path, as [`import_model`] would choose it (file stem
/// without the shard suffix, or folder name); `None` when nothing usable can be derived.
pub fn default_model_name(path: &Path) -> Option<String> {
    let base = if path.is_dir() {
        path.canonicalize()
            .ok()?
            .file_name()?
            .to_string_lossy()
            .into_owned()
    } else {
        let stem = path.file_stem()?.to_string_lossy().into_owned();
        match shard_of(&format!("{stem}.gguf")) {
            Some((prefix, _, _)) => prefix,
            None => stem,
        }
    };
    let cleaned: String = base
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let cleaned = cleaned
        .trim_start_matches(['-', '.', '_'])
        .trim_end_matches(['-', '.'])
        .to_string();
    (!cleaned.is_empty()).then_some(cleaned)
}

/// Whether `arg` (a `run` argument) names a local model file or folder rather than a model:
/// an existing path that is a `.gguf` file or a folder, written as a path (`./x`, `/x`, `~/x`)
/// or ending in `.gguf`.
pub fn looks_like_local_model(arg: &str) -> bool {
    let p = expand_home(arg);
    let pathish = arg.starts_with(['.', '/', '~', '\\'])
        || arg.to_ascii_lowercase().ends_with(".gguf")
        || (arg.len() > 2 && arg.as_bytes()[1] == b':' && arg.as_bytes()[0].is_ascii_alphabetic());
    pathish
        && (p.is_dir()
            || (p.is_file()
                && p.extension()
                    .is_some_and(|e| e.eq_ignore_ascii_case("gguf"))))
}

/// `~/x` -> `$HOME/x`.
fn expand_home(p: &str) -> PathBuf {
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\"))
        && let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
    {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(p)
}

/// The file name of a path, for labels and layer names.
fn file_label(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.display().to_string())
}

/// `(prefix, index, count)` of a split GGUF shard name (`x-00002-of-00003.gguf`).
fn shard_of(name: &str) -> Option<(String, u32, u32)> {
    let lower = name.to_ascii_lowercase();
    let stem = lower.strip_suffix(".gguf")?;
    let (head, count) = stem.rsplit_once("-of-")?;
    let (prefix, index) = head.rsplit_once('-')?;
    let ok = |s: &str| s.len() == 5 && s.bytes().all(|b| b.is_ascii_digit());
    if !ok(index) || !ok(count) {
        return None;
    }
    Some((
        name[..prefix.len()].to_string(),
        index.parse().ok()?,
        count.parse().ok()?,
    ))
}

/// A Modelfile `FROM` that names a local path, resolved against `dir`; `None` for a model
/// name.
fn local_from(from: &str, dir: &Path) -> Option<PathBuf> {
    let lower = from.to_ascii_lowercase();
    let pathish = from.starts_with(['.', '/', '~', '\\'])
        || lower.ends_with(".gguf")
        || lower.ends_with(".safetensors")
        || dir.join(from).exists();
    if !pathish {
        return None;
    }
    let p = expand_home(from);
    Some(if p.is_absolute() { p } else { dir.join(p) })
}

/// Reads a small text file whole.
fn read_text(path: &Path) -> Result<String, ImportError> {
    let meta = fs::metadata(path)
        .map_err(|e| ImportError::invalid(format!("cannot read {}: {e}", path.display())))?;
    if meta.len() > MAX_SIDE_FILE {
        return Err(ImportError::invalid(format!(
            "{} is too large to be a Modelfile or config file",
            path.display()
        )));
    }
    fs::read_to_string(path)
        .map_err(|e| ImportError::invalid(format!("cannot read {}: {e}", path.display())))
}

fn parse_mf(text: &str, origin: &Path) -> Result<Modelfile, ImportError> {
    parse_modelfile(text)
        .map_err(|e| ImportError::invalid(format!("{}: {}", origin.display(), e.message)))
}

/// The Modelfile layers.
fn layers_of(mf: &Modelfile) -> CreateLayers {
    CreateLayers {
        template: mf.template.clone(),
        system: mf.system.clone(),
        parameters: (!mf.parameters.is_empty()).then(|| parameters_json(&mf.parameters)),
        license: (!mf.license.is_empty()).then(|| mf.license.clone()),
        components: Vec::new(),
    }
}

/// Turns a request into a plan: which files, which Modelfile layers, which default name.
fn resolve(req: &ImportRequest) -> Result<Resolved, ImportError> {
    let explicit = match &req.modelfile {
        Some(m) => Some((parse_mf(&m.text, &m.dir.join("Modelfile"))?, m.dir.clone())),
        None => None,
    };
    let mut layers = explicit
        .as_ref()
        .map(|(mf, _)| layers_of(mf))
        .unwrap_or_default();
    let path = match (&req.path, &explicit) {
        (Some(p), _) => expand_home(&p.to_string_lossy()),
        (None, Some((mf, dir))) => {
            let from = mf
                .from
                .as_deref()
                .ok_or_else(|| ImportError::invalid("the Modelfile has no FROM"))?;
            match local_from(from, dir) {
                Some(p) => p,
                None => {
                    return Ok(Resolved {
                        plan: Plan::FromModel(from.to_string()),
                        layers,
                        default_name: None,
                        reference: from.to_string(),
                    });
                }
            }
        }
        (None, None) => {
            return Err(ImportError::invalid(
                "nothing to import: give a file or folder, or a Modelfile with FROM",
            ));
        }
    };
    let meta = fs::metadata(&path)
        .map_err(|e| ImportError::invalid(format!("cannot read {}: {e}", path.display())))?;
    let reference = fs::canonicalize(&path)
        .unwrap_or_else(|_| path.clone())
        .display()
        .to_string();
    let default_name = default_model_name(&path);

    if meta.is_dir() {
        // A Modelfile inside the folder is honoured unless one was given explicitly.
        let mut target = path.clone();
        let folder_mf = path.join("Modelfile");
        if explicit.is_none() && folder_mf.is_file() {
            let mf = parse_mf(&read_text(&folder_mf)?, &folder_mf)?;
            layers = layers_of(&mf);
            if let Some(from) = mf.from.as_deref() {
                match local_from(from, &path) {
                    Some(p) => target = p,
                    None => {
                        return Err(ImportError::invalid(format!(
                            "{}: FROM {from} names a model, not a file of this folder; use \
                             `alkahest create <name> -f {}` to build on an existing model",
                            folder_mf.display(),
                            folder_mf.display()
                        )));
                    }
                }
            }
        }
        let same = fs::canonicalize(&target).ok() == fs::canonicalize(&path).ok();
        let plan = if !same && target.is_file() {
            plan_file(&target)?
        } else {
            plan_folder(&target)?
        };
        return Ok(Resolved {
            plan,
            layers,
            default_name,
            reference,
        });
    }
    Ok(Resolved {
        plan: plan_file(&path)?,
        layers,
        default_name,
        reference,
    })
}

/// Regular files directly in `dir` (symbolic links to files count), as repository files.
fn folder_files(dir: &Path) -> Result<Vec<RepoFile>, ImportError> {
    let rd = fs::read_dir(dir)
        .map_err(|e| ImportError::invalid(format!("cannot list {}: {e}", dir.display())))?;
    let mut out = Vec::new();
    for entry in rd.flatten() {
        let p = entry.path();
        let Ok(meta) = fs::metadata(&p) else { continue };
        if !meta.is_file() {
            continue;
        }
        if let Some(name) = entry.file_name().to_str() {
            out.push(RepoFile {
                path: name.to_string(),
                size: Some(meta.len()),
                sha256: None,
            });
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}

/// Checks that `files` (in shard order) is a complete split set, or a single unsplit file.
fn check_shards(dir: &Path, files: &[RepoFile]) -> Result<(), ImportError> {
    let Some(first) = files.first() else {
        return Err(ImportError::invalid(format!(
            "no GGUF model in {}",
            dir.display()
        )));
    };
    let Some((prefix, _, count)) = shard_of(&first.path) else {
        return Ok(());
    };
    let indices: Vec<u32> = files
        .iter()
        .filter_map(|f| shard_of(&f.path).map(|(_, i, _)| i))
        .collect();
    let expected: Vec<u32> = (1..=count).collect();
    if indices != expected {
        return Err(ImportError::invalid(format!(
            "{} has {} of the {count} shards of {prefix}; every -0000N-of-{count:05}.gguf file \
             must be present",
            dir.display(),
            indices.len()
        )));
    }
    Ok(())
}

/// A GGUF file: one model, or (a shard) every shard of its split model.
fn plan_file(path: &Path) -> Result<Plan, ImportError> {
    let name = file_label(path);
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".safetensors") {
        return Err(ImportError::invalid(format!(
            "{name}: a safetensors model is imported as its folder (config.json and \
             *.safetensors); pass the folder"
        )));
    }
    if alkahest_hf::is_mmproj(&name) {
        return Err(ImportError::invalid(format!(
            "{name} is a multimodal projector; import the folder holding the model and its \
             projector"
        )));
    }
    if let Err(e) = alkahest_gguf::Gguf::read_file(path) {
        return Err(ImportError::invalid(format!(
            "{} is not a GGUF model file: {e}",
            path.display()
        )));
    }
    if shard_of(&name).is_some() {
        let dir = path.parent().unwrap_or(Path::new("."));
        let files = folder_files(dir)?;
        let variant = alkahest_hf::gguf_variants(&files)
            .into_iter()
            .find(|v| v.files.iter().any(|f| f.path == name))
            .ok_or_else(|| ImportError::invalid(format!("no shard set for {name}")))?;
        check_shards(dir, &variant.files)?;
        return Ok(Plan::Gguf {
            shards: variant.files.iter().map(|f| dir.join(&f.path)).collect(),
            projector: None,
        });
    }
    Ok(Plan::Gguf {
        shards: vec![path.to_path_buf()],
        projector: None,
    })
}

/// A folder: one GGUF model (with its projector), or a safetensors snapshot.
fn plan_folder(dir: &Path) -> Result<Plan, ImportError> {
    let files = folder_files(dir)?;
    let variants = alkahest_hf::gguf_variants(&files);
    match variants.as_slice() {
        [] => {}
        [v] => {
            check_shards(dir, &v.files)?;
            let first = dir.join(&v.files[0].path);
            if let Err(e) = alkahest_gguf::Gguf::read_file(&first) {
                return Err(ImportError::invalid(format!(
                    "{} is not a GGUF model file: {e}",
                    first.display()
                )));
            }
            let projectors = alkahest_hf::mmproj_files(&files);
            let projector = alkahest_hf::pick_mmproj(&projectors).map(|f| dir.join(&f.path));
            return Ok(Plan::Gguf {
                shards: v.files.iter().map(|f| dir.join(&f.path)).collect(),
                projector,
            });
        }
        many => {
            let names: Vec<&str> = many.iter().map(|v| v.primary().path.as_str()).collect();
            return Err(ImportError::invalid(format!(
                "{} holds several GGUF models ({}); import one file, or move it to its own folder",
                dir.display(),
                names.join(", ")
            )));
        }
    }
    let has_weights = files
        .iter()
        .any(|f| f.path.to_ascii_lowercase().ends_with(".safetensors"));
    if has_weights {
        let label = dir.display().to_string();
        let picked = select_snapshot_files(&label, &files)
            .map_err(|e| ImportError::invalid(format!("{e}")))?;
        return Ok(Plan::Safetensors {
            dir: dir.to_path_buf(),
            files: picked.into_iter().map(|f| f.path).collect(),
        });
    }
    Err(ImportError::invalid(format!(
        "no GGUF file and no safetensors model (config.json and *.safetensors) in {}",
        dir.display()
    )))
}

/// The manifest config of a safetensors folder, read from its `config.json` and chat
/// template like `pull --safetensors` does (there is no Hub pipeline tag locally).
fn safetensors_config(
    dir: &Path,
    files: &[String],
    blobs: &[ImportedBlob],
) -> Result<ModelConfig, ImportError> {
    let read = |name: &str| -> Option<Vec<u8>> {
        files
            .iter()
            .any(|f| f == name)
            .then(|| dir.join(name))
            .filter(|p| fs::metadata(p).is_ok_and(|m| m.len() <= MAX_SIDE_FILE))
            .and_then(|p| fs::read(p).ok())
    };
    let config_json: serde_json::Value = read("config.json")
        .and_then(|b| serde_json::from_slice(&b).ok())
        .ok_or_else(|| {
            ImportError::invalid(format!("{}: config.json is not valid JSON", dir.display()))
        })?;
    let template = chat_template_of(read);
    let facts = SnapshotFacts::from_config(&config_json, None, template.as_deref());
    let weight_bytes: u64 = files
        .iter()
        .zip(blobs)
        .filter(|(f, _)| f.to_ascii_lowercase().ends_with(".safetensors"))
        .map(|(_, b)| b.size)
        .sum();
    let mut config = ModelConfig {
        model_format: "safetensors".into(),
        model_family: facts.family.clone(),
        file_type: facts.precision.clone(),
        capabilities: facts.capabilities.clone(),
        context_length: facts.context_length,
        embedding_length: facts.embedding_length,
        ..ModelConfig::default()
    };
    if let Some(b) = facts.bytes_per_param.filter(|b| *b > 0) {
        config.model_type = format_parameters(weight_bytes / b);
    }
    Ok(config)
}

/// Whether the model `name` exists in the Alkahest store as a linked import of `path` whose
/// files are unchanged, so `alkahest run <path>` can skip importing it again.
pub fn current_linked_import(catalog: &Catalog, name: &str, path: &Path) -> bool {
    let Ok(model) = destination_name(name) else {
        return false;
    };
    let Ok(Some(entry)) = catalog.alkahest().get(&model) else {
        return false;
    };
    let first = if path.is_dir() {
        match plan_folder(path) {
            Ok(Plan::Gguf { shards, .. }) => shards.into_iter().next(),
            Ok(Plan::Safetensors { dir, files }) => files.first().map(|f| dir.join(f)),
            _ => None,
        }
    } else {
        Some(path.to_path_buf())
    };
    first.is_some_and(|f| alkahest_store::import::is_current_link_of(&entry, &f))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_names_parse() {
        assert_eq!(
            shard_of("Big-Q4_K_M-00002-of-00003.gguf"),
            Some(("Big-Q4_K_M".to_string(), 2, 3))
        );
        assert_eq!(shard_of("big-Q4_K_M.gguf"), None);
        assert_eq!(shard_of("big-1-of-2.gguf"), None);
    }

    #[test]
    fn default_names_come_from_stems_and_folders() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("Qwen3 4B-Q4_K_M-00001-of-00002.gguf");
        assert_eq!(default_model_name(&f).as_deref(), Some("Qwen3-4B-Q4_K_M"));
        let d = tmp.path().join("my-model");
        fs::create_dir(&d).unwrap();
        assert_eq!(default_model_name(&d).as_deref(), Some("my-model"));
    }

    #[test]
    fn modelfile_from_paths_resolve_against_the_modelfile_folder() {
        let dir = Path::new("/models/x");
        assert_eq!(
            local_from("./w/tiny.gguf", dir),
            Some(PathBuf::from("/models/x/./w/tiny.gguf"))
        );
        assert_eq!(local_from("llama3.2", dir), None);
        assert_eq!(
            local_from("/abs/m.gguf", dir),
            Some(PathBuf::from("/abs/m.gguf"))
        );
    }
}
