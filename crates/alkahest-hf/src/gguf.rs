//! Choosing GGUF files in a repository: quant detection, shard grouping, `mmproj` detection.

use std::collections::BTreeMap;
use std::sync::LazyLock;

use regex::Regex;

use crate::types::RepoFile;

/// Quantisations tried in order when the user names none: balanced quality and size first.
pub const DEFAULT_QUANT_PREFERENCE: &[&str] = &[
    "Q4_K_M", "Q4_K_S", "Q4_0", "Q5_K_M", "Q5_K_S", "Q6_K", "Q8_0", "IQ4_XS", "Q3_K_M", "Q4_1",
    "Q5_0", "Q3_K_L", "Q3_K_S", "Q2_K", "MXFP4", "F16", "BF16",
];

/// Preference for the multimodal projector: half precision first.
const MMPROJ_PREFERENCE: &[&str] = &["F16", "BF16", "Q8_0", "F32"];

#[allow(clippy::expect_used)] // constant pattern, covered by tests
static QUANT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)(?:^|[^a-z0-9])((?:IQ|Q)\d(?:_[A-Z0-9]+)*|BF16|F16|F32|MXFP4(?:_MOE)?|PQ2_0|PTQ1_0|TQ[12]_0)(?:[^a-z0-9]|$)",
    )
    .expect("quant regex is valid")
});

#[allow(clippy::expect_used)] // constant pattern, covered by tests
static SHARD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(.*)-(\d{5})-of-(\d{5})\.gguf$").expect("shard regex is valid")
});

/// The quantisation named in a file path (`Q4_K_M`, `IQ3_XS`, `F16`, ...), upper-cased. The
/// last match wins because the quant sits at the end of the usual names.
pub fn detect_quant(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let name = name
        .strip_suffix(".gguf")
        .or_else(|| name.strip_suffix(".GGUF"))
        .unwrap_or(name);
    QUANT_RE
        .captures_iter(name)
        .last()
        .map(|c| c[1].to_ascii_uppercase())
}

/// Whether the file is a multimodal projector (`mmproj-*.gguf`).
pub fn is_mmproj(path: &str) -> bool {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase()
        .contains("mmproj")
}

/// A loadable model: one GGUF file or all shards of a split model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GgufVariant {
    /// Detected quantisation, if the name carries one.
    pub quant: Option<String>,
    /// Files in shard order (one entry for unsplit models).
    pub files: Vec<RepoFile>,
}

impl GgufVariant {
    /// Total size of all shards; `None` if any size is unknown.
    pub fn size(&self) -> Option<u64> {
        self.files.iter().map(|f| f.size).sum()
    }

    /// The file to hand to the engine (first shard).
    pub fn primary(&self) -> &RepoFile {
        &self.files[0]
    }
}

/// Groups the GGUF files of a repository into variants, excluding `mmproj` files. Sorted by
/// path so results are stable.
pub fn gguf_variants(files: &[RepoFile]) -> Vec<GgufVariant> {
    let mut groups: BTreeMap<String, Vec<(u32, &RepoFile)>> = BTreeMap::new();
    for f in files {
        if !f.path.to_ascii_lowercase().ends_with(".gguf") || is_mmproj(&f.path) {
            continue;
        }
        match SHARD_RE.captures(&f.path) {
            Some(c) => {
                let index = c[2].parse().unwrap_or(0);
                groups.entry(c[1].to_string()).or_default().push((index, f));
            }
            None => groups.entry(f.path.clone()).or_default().push((0, f)),
        }
    }
    groups
        .into_values()
        .map(|mut shards| {
            shards.sort_by_key(|(i, _)| *i);
            let files: Vec<RepoFile> = shards.into_iter().map(|(_, f)| f.clone()).collect();
            GgufVariant {
                quant: detect_quant(&files[0].path),
                files,
            }
        })
        .collect()
}

/// The `mmproj` files of a repository.
pub fn mmproj_files(files: &[RepoFile]) -> Vec<RepoFile> {
    files
        .iter()
        .filter(|f| f.path.to_ascii_lowercase().ends_with(".gguf") && is_mmproj(&f.path))
        .cloned()
        .collect()
}

/// Picks the projector matching the main model: half precision first, then the rest of
/// [`MMPROJ_PREFERENCE`], else the first one.
pub fn pick_mmproj(projectors: &[RepoFile]) -> Option<&RepoFile> {
    MMPROJ_PREFERENCE
        .iter()
        .find_map(|q| {
            projectors
                .iter()
                .find(|f| detect_quant(&f.path).as_deref() == Some(q))
        })
        .or_else(|| projectors.first())
}

/// Picks a variant. With `requested` (case-insensitive, e.g. `q4_k_m`) only an exact quant
/// match qualifies. Without it the first hit of [`DEFAULT_QUANT_PREFERENCE`] wins, then the
/// smallest variant, so a repository with unusual names still resolves.
pub fn pick_variant<'a>(
    variants: &'a [GgufVariant],
    requested: Option<&str>,
) -> Option<&'a GgufVariant> {
    if let Some(want) = requested {
        let want = want.to_ascii_uppercase();
        return variants
            .iter()
            .find(|v| v.quant.as_deref() == Some(want.as_str()));
    }
    DEFAULT_QUANT_PREFERENCE
        .iter()
        .find_map(|q| variants.iter().find(|v| v.quant.as_deref() == Some(*q)))
        .or_else(|| variants.iter().min_by_key(|v| v.size().unwrap_or(u64::MAX)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, size: u64) -> RepoFile {
        RepoFile {
            path: path.into(),
            size: Some(size),
            sha256: None,
        }
    }

    #[test]
    fn detects_common_quant_spellings() {
        for (name, quant) in [
            ("Llama-3.2-3B-Instruct-Q4_K_M.gguf", "Q4_K_M"),
            ("mistral-7b-v0.1.Q5_K_S.gguf", "Q5_K_S"),
            ("model_Q8_0.gguf", "Q8_0"),
            ("gemma-2-9b-it-IQ4_XS.gguf", "IQ4_XS"),
            ("Qwen3-4B-f16.gguf", "F16"),
            ("sub/dir/Qwen3-4B-BF16.gguf", "BF16"),
            ("Bonsai-8B-PQ2_0.gguf", "PQ2_0"),
            ("x-Q4_K_M-00001-of-00002.gguf", "Q4_K_M"),
        ] {
            assert_eq!(detect_quant(name).as_deref(), Some(quant), "{name}");
        }
        assert_eq!(detect_quant("Qwen3-4B.gguf"), None);
        assert_eq!(detect_quant("README.md"), None);
    }

    #[test]
    fn mmproj_is_detected_and_excluded_from_variants() {
        let files = vec![
            file("llava-v1.6-Q4_K_M.gguf", 4),
            file("mmproj-model-f16.gguf", 1),
            file("config.json", 1),
        ];
        assert!(is_mmproj("mmproj-model-f16.gguf"));
        let v = gguf_variants(&files);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].primary().path, "llava-v1.6-Q4_K_M.gguf");
        let mm = mmproj_files(&files);
        assert_eq!(mm.len(), 1);
        assert_eq!(pick_mmproj(&mm).unwrap().path, "mmproj-model-f16.gguf");
    }

    #[test]
    fn shards_are_grouped_in_order() {
        let files = vec![
            file("big-Q4_K_M-00002-of-00002.gguf", 10),
            file("big-Q4_K_M-00001-of-00002.gguf", 20),
            file("big-Q8_0.gguf", 50),
        ];
        let v = gguf_variants(&files);
        assert_eq!(v.len(), 2);
        let split = v.iter().find(|v| v.files.len() == 2).unwrap();
        assert_eq!(split.primary().path, "big-Q4_K_M-00001-of-00002.gguf");
        assert_eq!(split.size(), Some(30));
    }

    #[test]
    fn default_preference_prefers_q4_k_m() {
        let files = vec![
            file("m-Q8_0.gguf", 8),
            file("m-Q4_K_M.gguf", 4),
            file("m-Q2_K.gguf", 2),
        ];
        let v = gguf_variants(&files);
        assert_eq!(
            pick_variant(&v, None).unwrap().quant.as_deref(),
            Some("Q4_K_M")
        );
        assert_eq!(
            pick_variant(&v, Some("q8_0")).unwrap().quant.as_deref(),
            Some("Q8_0")
        );
        assert!(pick_variant(&v, Some("Q6_K")).is_none());
    }

    #[test]
    fn unusual_names_fall_back_to_the_smallest_file() {
        let files = vec![file("weird-a.gguf", 9), file("weird-b.gguf", 3)];
        let v = gguf_variants(&files);
        assert_eq!(
            pick_variant(&v, None).unwrap().primary().path,
            "weird-b.gguf"
        );
    }
}
