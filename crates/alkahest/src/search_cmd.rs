//! `alkahest search`: git catalog, Hugging Face and Ollama library model search (ALK-US-0021,
//! ALK-US-0120).
//!
//! The remote sources are queried concurrently; catalog entries come from the local
//! snapshots (refreshed with the policy of `catalog_cmd::prepare`) and are listed first, one
//! row per tag, named `alkahest/<namespace>/<model>:<tag>` so the name can be pulled as is.
//! With `--source all` a failing source is reported on stderr and the others still print; the
//! command fails only when every queried source failed. `ALKAHEST_OLLAMA_LIBRARY` points the
//! Ollama side at another site (mirror, tests).

use std::io::Write;
use std::process::ExitCode;

use alkahest_catalog::{CatalogModel, search_models};
use alkahest_core::config::{Config, Loaded};
use alkahest_core::paths::process_env;
use alkahest_hf::{HfClient, KeyringStore, ModelSummary, SearchQuery, SortKey};
use alkahest_ollama::{DEFAULT_LIBRARY_URL, LibraryModel, OllamaLibrary};
use alkahest_store::CATALOG_HOST;
use anyhow::{Context, Result, bail};

use crate::cli::{SearchArgs, SearchSort, SearchSource};
use crate::output::{Format, Listing};

/// Column headers; the JSON keys are the lower-case forms. `size` and `license` are filled
/// for catalog rows only (the remote listings do not carry them).
const HEADERS: [&str; 8] = [
    "source",
    "name",
    "downloads",
    "updated",
    "kind",
    "size",
    "license",
    "description",
];

/// Longest description cell of the human table.
const DESCRIPTION_WIDTH: usize = 60;

/// Runs the command.
pub fn run(args: &SearchArgs, loaded: &Loaded, json: bool) -> Result<ExitCode> {
    let want_catalog = matches!(args.source, SearchSource::All | SearchSource::Catalog);
    if let Some(c) = &args.catalog {
        if !want_catalog {
            bail!("--catalog applies to catalog results (--source catalog or all)");
        }
        if !loaded.config.catalogs.iter().any(|s| &s.name == c) {
            bail!("no catalog named {c} is configured (see `alkahest catalog ls`)");
        }
    }
    let catalog_rows = if want_catalog {
        let states = crate::catalog_cmd::prepare(loaded);
        let mut rows = search_models(&states, &args.query.join(" "), args.catalog.as_deref());
        rows.truncate(args.limit as usize);
        Some(rows)
    } else {
        None
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the async runtime")?;
    runtime.block_on(run_async(args, &loaded.config, catalog_rows, json))
}

async fn run_async(
    args: &SearchArgs,
    config: &Config,
    catalog: Option<Vec<CatalogModel>>,
    json: bool,
) -> Result<ExitCode> {
    let text = args.query.join(" ");
    let limit = args.limit as usize;
    // `--catalog` narrows the search to catalog results.
    let remote = args.catalog.is_none();
    let want_hf = remote && matches!(args.source, SearchSource::All | SearchSource::Hf);
    let want_ollama = remote && matches!(args.source, SearchSource::All | SearchSource::Ollama);

    let hf_fut = async {
        if !want_hf {
            return None;
        }
        Some(search_hf(args, config, &text).await)
    };
    let ollama_fut = async {
        if !want_ollama {
            return None;
        }
        Some(search_ollama(&text, limit).await)
    };
    let (hf, ollama) = tokio::join!(hf_fut, ollama_fut);

    let mut failures = Vec::new();
    let mut queried = usize::from(catalog.is_some());
    let catalog_rows = catalog.unwrap_or_default();
    let hf_rows = settle("Hugging Face", hf, &mut queried, &mut failures);
    let ollama_rows = settle("Ollama library", ollama, &mut queried, &mut failures);
    for f in &failures {
        eprintln!("warning: {f}");
    }
    if queried > 0 && failures.len() == queried {
        bail!("every search source failed");
    }

    let listing = build_listing(&catalog_rows, &hf_rows, &ollama_rows, !json);
    let mut out = std::io::stdout().lock();
    listing.render(Format::from_json_flag(json), &mut out)?;
    out.flush()?;
    Ok(ExitCode::SUCCESS)
}

/// Unwraps one source result, recording whether it ran and how it failed.
fn settle<T>(
    label: &str,
    result: Option<Result<Vec<T>>>,
    queried: &mut usize,
    failures: &mut Vec<String>,
) -> Vec<T> {
    match result {
        None => Vec::new(),
        Some(Ok(rows)) => {
            *queried += 1;
            rows
        }
        Some(Err(e)) => {
            *queried += 1;
            failures.push(format!("{label}: {e:#}"));
            Vec::new()
        }
    }
}

async fn search_hf(args: &SearchArgs, config: &Config, text: &str) -> Result<Vec<ModelSummary>> {
    let client = HfClient::from_config(&config.hf, &process_env, &KeyringStore)?;
    Ok(client.search_models(&hf_query(args, text)).await?)
}

async fn search_ollama(text: &str, limit: usize) -> Result<Vec<LibraryModel>> {
    let base =
        process_env("ALKAHEST_OLLAMA_LIBRARY").unwrap_or_else(|| DEFAULT_LIBRARY_URL.to_string());
    Ok(OllamaLibrary::new(&base)?.search(text, limit).await?)
}

/// Maps the flags to a Hub query: GGUF unless `--safetensors` or `--all` says otherwise.
pub fn hf_query(args: &SearchArgs, text: &str) -> SearchQuery {
    let filter = if args.all_formats {
        None
    } else if args.safetensors {
        Some("safetensors".to_string())
    } else {
        Some("gguf".to_string())
    };
    SearchQuery {
        search: text.to_string(),
        filter,
        sort: match args.sort {
            SearchSort::Downloads => SortKey::Downloads,
            SearchSort::Likes => SortKey::Likes,
            SearchSort::Recent => SortKey::LastModified,
        },
        limit: args.limit,
        author: None,
        pipeline_tag: args.task.clone(),
    }
}

/// One listing for every source, catalog entries first; `human` abbreviates counts and sizes
/// and truncates descriptions.
pub fn build_listing(
    catalog: &[CatalogModel],
    hf: &[ModelSummary],
    ollama: &[LibraryModel],
    human: bool,
) -> Listing {
    let mut listing = Listing::new(HEADERS);
    for m in catalog {
        let mut kind: Vec<&str> = vec![match m.variant.format {
            alkahest_catalog::ModelFormat::Gguf => "gguf",
            alkahest_catalog::ModelFormat::Safetensors => "safetensors",
        }];
        if let Some(q) = &m.variant.quant {
            kind.push(q);
        }
        kind.extend(m.entry.tags.iter().map(String::as_str));
        let size = m.variant.total_size();
        listing.push_row([
            format!("catalog:{}", m.catalog),
            format!("{CATALOG_HOST}/{}:{}", m.entry.name, m.variant.tag),
            String::new(),
            String::new(),
            kind.join(" "),
            if human {
                crate::models_cmd::human_size(size)
            } else {
                size.to_string()
            },
            m.entry.license.clone(),
            if human {
                truncate(&m.entry.description, DESCRIPTION_WIDTH)
            } else {
                m.entry.description.clone()
            },
        ]);
    }
    for m in hf {
        let mut kind: Vec<&str> = Vec::new();
        if let Some(t) = &m.pipeline_tag {
            kind.push(t);
        }
        for tag in ["gguf", "safetensors"] {
            if m.tags.iter().any(|t| t == tag) {
                kind.push(tag);
            }
        }
        listing.push_row([
            "hf".to_string(),
            m.id.clone(),
            count(m.downloads, human),
            m.last_modified
                .as_deref()
                .map(|d| d.chars().take(10).collect())
                .unwrap_or_default(),
            kind.join(" "),
            String::new(),
            String::new(),
            String::new(),
        ]);
    }
    for m in ollama {
        let mut kind: Vec<&str> = m.capabilities.iter().map(String::as_str).collect();
        kind.extend(m.sizes.iter().map(String::as_str));
        listing.push_row([
            "ollama".to_string(),
            m.name.clone(),
            m.pulls.map(|p| count(p, human)).unwrap_or_default(),
            m.updated.clone().unwrap_or_default(),
            kind.join(" "),
            String::new(),
            String::new(),
            if human {
                truncate(&m.description, DESCRIPTION_WIDTH)
            } else {
                m.description.clone()
            },
        ]);
    }
    listing
}

/// `1234567` as `1.2M` for tables, plain digits for JSON.
fn count(n: u64, human: bool) -> String {
    if !human {
        return n.to_string();
    }
    // Display approximation only; precision loss above 2^52 is irrelevant.
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}K", f / 1e3),
        1_000_000..=999_999_999 => format!("{:.1}M", f / 1e6),
        _ => format!("{:.1}B", f / 1e9),
    }
}

/// Cuts `s` to `max` characters with an ellipsis.
fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Command};
    use clap::Parser;

    fn args(line: &str) -> SearchArgs {
        let cli = Cli::parse_from(line.split_whitespace());
        match cli.command {
            Command::Search(a) => a,
            other => panic!("not a search: {other:?}"),
        }
    }

    #[test]
    fn gguf_is_the_default_filter() {
        let q = hf_query(&args("alkahest search qwen"), "qwen");
        assert_eq!(q.filter.as_deref(), Some("gguf"));
        assert_eq!(q.sort, SortKey::Downloads);
    }

    #[test]
    fn format_flags_change_the_filter() {
        let q = hf_query(&args("alkahest search qwen --safetensors"), "qwen");
        assert_eq!(q.filter.as_deref(), Some("safetensors"));
        let q = hf_query(
            &args("alkahest search qwen --all --task text-generation"),
            "qwen",
        );
        assert_eq!(q.filter, None);
        assert_eq!(q.pipeline_tag.as_deref(), Some("text-generation"));
        assert!(Cli::try_parse_from(["alkahest", "search", "x", "--gguf", "--all"]).is_err());
    }

    #[test]
    fn counts_and_truncation() {
        assert_eq!(count(999, true), "999");
        assert_eq!(count(7_140_134, true), "7.1M");
        assert_eq!(count(7_140_134, false), "7140134");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }
}
