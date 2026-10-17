//! `cargo xtask notice [--check]`: third-party licence compliance (ALK-US-0077).
//!
//! Generates, from the resolved dependency graphs of the two shipped binaries (headless and
//! desktop), the two files every package carries:
//!
//! - `THIRD_PARTY_LICENSES`: for every crate that is linked in: name, version, SPDX licence,
//!   and the licence text found in its published source under `~/.cargo/registry` (identical
//!   texts are printed once, with the list of crates that share them); then the embedded OFL
//!   fonts and the engine licences recorded in `recipes/*.toml`.
//! - the generated section of `NOTICE`: the `NOTICE` files of Apache-2.0 crates, which that
//!   licence requires us to pass on.
//!
//! `--check` regenerates both in memory and fails when the committed files differ, so CI
//! catches a dependency bump that was not followed by `cargo xtask notice`. It also fails when
//! a recipe lacks a permissive licence declaration or its text under `packaging/licenses/`.
//! No GPL-family licence can appear: `cargo deny check licenses` gates that, and this command
//! additionally refuses any crate whose SPDX expression names no permissive alternative.
//!
//! The graphs contain normal dependencies only (no dev or build-only crates) for every target
//! platform, so the output covers the Linux, macOS and Windows binaries alike.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use cargo_metadata::{CargoOpt, DependencyKind, Metadata, MetadataCommand, PackageId};

/// Generated licence bundle, relative to the workspace root.
pub const LICENSES_FILE: &str = "THIRD_PARTY_LICENSES";

/// The NOTICE file, relative to the workspace root.
pub const NOTICE_FILE: &str = "NOTICE";

/// Line that starts the generated part of `NOTICE`; everything before it is hand-written.
pub const NOTICE_MARKER: &str = "----- BEGIN GENERATED (cargo xtask notice) -----";

/// SPDX identifiers that count as permissive for the shipped binaries.
const PERMISSIVE: &[&str] = &[
    "MIT",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "Unicode-3.0",
    "MPL-2.0",
    "CC0-1.0",
    "BSL-1.0",
    "0BSD",
    "CDLA-Permissive-2.0",
    "OpenSSL",
    "bzip2-1.0.6",
    "Unlicense",
    "Apache-2.0 WITH LLVM-exception",
];

/// Licences that must never ship; checked on every SPDX token.
const FORBIDDEN_PREFIXES: &[&str] = &["GPL", "LGPL", "AGPL", "SSPL", "EUPL"];

/// One third-party crate in a shipped graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateInfo {
    /// Crate name.
    pub name: String,
    /// Crate version.
    pub version: String,
    /// SPDX licence expression from the manifest.
    pub license: String,
    /// Authors listed in the manifest (used for the fallback MIT copyright line).
    pub authors: Vec<String>,
    /// Licence texts found in the source tree, as `(file name, normalised text)`.
    pub license_files: Vec<(String, String)>,
    /// `NOTICE` files found in the source tree.
    pub notice_files: Vec<(String, String)>,
    /// In the headless graph.
    pub headless: bool,
    /// In the desktop graph.
    pub desktop: bool,
}

/// An engine recipe reduced to what the licence bundle records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineLicense {
    /// Recipe id.
    pub id: String,
    /// Pinned upstream version.
    pub version: String,
    /// SPDX licence.
    pub license: String,
    /// Upstream homepage.
    pub homepage: String,
}

/// Entry point: `rest` is `[--check]`. Returns `Ok(true)` when everything is current.
pub fn run(root: &Path, rest: &[String]) -> Result<bool> {
    let check = rest.iter().any(|a| a == "--check");
    if let Some(bad) = rest.iter().find(|a| *a != "--check") {
        bail!("notice: unknown option `{bad}`");
    }
    let crates = collect_crates()?;
    let engines = read_engines(root)?;
    let mut problems = check_engines(root, &engines);
    problems.extend(check_crates(&crates));

    let fonts = read_fonts(root)?;
    let licenses = render_licenses(&crates, &fonts, &engines, root)?;
    let notice = render_notice(&read_handwritten_notice(root)?, &crates);

    if check {
        for (file, want) in [(LICENSES_FILE, &licenses), (NOTICE_FILE, &notice)] {
            let have = fs::read_to_string(root.join(file)).unwrap_or_default();
            if &have != want {
                problems.push(format!(
                    "{file} is out of date; run `cargo xtask notice` and commit the result"
                ));
            }
        }
    } else if problems.is_empty() {
        fs::write(root.join(LICENSES_FILE), &licenses).context("writing THIRD_PARTY_LICENSES")?;
        fs::write(root.join(NOTICE_FILE), &notice).context("writing NOTICE")?;
    }
    if problems.is_empty() {
        println!(
            "notice: OK ({} third-party crates, {} engines; {})",
            crates.len(),
            engines.len(),
            if check {
                "files current"
            } else {
                "files written"
            }
        );
        Ok(true)
    } else {
        for p in &problems {
            eprintln!("notice: {p}");
        }
        Ok(false)
    }
}

/// Collects the union of the headless and desktop graphs.
fn collect_crates() -> Result<Vec<CrateInfo>> {
    let headless = MetadataCommand::new()
        .features(CargoOpt::NoDefaultFeatures)
        .exec()
        .context("cargo metadata (headless)")?;
    let desktop = MetadataCommand::new()
        .features(CargoOpt::SomeFeatures(vec!["alkahest/desktop".to_owned()]))
        .exec()
        .context("cargo metadata (desktop)")?;
    let head = reachable(&headless)?;
    let desk = reachable(&desktop)?;

    let mut map: BTreeMap<(String, String), CrateInfo> = BTreeMap::new();
    for (meta, set, is_desktop) in [(&headless, &head, false), (&desktop, &desk, true)] {
        for pkg in meta.packages.iter().filter(|p| set.contains(&p.id)) {
            if pkg.source.is_none() {
                continue; // workspace member or path dependency: our own code
            }
            let key = (pkg.name.to_string(), pkg.version.to_string());
            let dir = pkg
                .manifest_path
                .parent()
                .ok_or_else(|| anyhow!("{} has no manifest directory", pkg.name))?
                .as_std_path()
                .to_path_buf();
            let entry = match map.get_mut(&key) {
                Some(e) => e,
                None => {
                    let (license_files, notice_files) = read_legal_files(&dir)
                        .with_context(|| format!("reading {} {}", pkg.name, pkg.version))?;
                    map.entry(key.clone()).or_insert(CrateInfo {
                        name: key.0.clone(),
                        version: key.1.clone(),
                        authors: pkg.authors.clone(),
                        license: pkg
                            .license
                            .clone()
                            .unwrap_or_else(|| "(unspecified)".to_owned()),
                        license_files,
                        notice_files,
                        headless: false,
                        desktop: false,
                    })
                }
            };
            if is_desktop {
                entry.desktop = true;
            } else {
                entry.headless = true;
            }
        }
    }
    Ok(map.into_values().collect())
}

/// Package ids reachable from the `alkahest` binary through normal (non-dev, non-build)
/// dependencies.
fn reachable(meta: &Metadata) -> Result<BTreeSet<PackageId>> {
    let resolve = meta
        .resolve
        .as_ref()
        .context("metadata has no resolve graph")?;
    let root = meta
        .packages
        .iter()
        .find(|p| p.name.as_str() == "alkahest" && p.source.is_none())
        .map(|p| p.id.clone())
        .ok_or_else(|| anyhow!("package `alkahest` not found"))?;
    let nodes: BTreeMap<_, _> = resolve.nodes.iter().map(|n| (&n.id, n)).collect();
    let mut seen = BTreeSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        let Some(node) = nodes.get(&id) else { continue };
        for dep in &node.deps {
            if dep
                .dep_kinds
                .iter()
                .any(|k| k.kind == DependencyKind::Normal)
            {
                stack.push(dep.pkg.clone());
            }
        }
    }
    Ok(seen)
}

/// Normalises text for stable output: LF line ends, no trailing blanks, one final newline.
fn normalise(text: &str) -> String {
    let mut out = String::new();
    for line in text.replace("\r\n", "\n").replace('\r', "\n").lines() {
        out.push_str(line.trim_end());
        out.push('\n');
    }
    format!("{}\n", out.trim_matches('\n'))
}

/// Licence and NOTICE files at the top of a crate's source directory.
type LegalFiles = (Vec<(String, String)>, Vec<(String, String)>);

/// Reads the licence and NOTICE files that sit at the top level of a crate source tree.
fn read_legal_files(dir: &Path) -> Result<LegalFiles> {
    let mut licenses = Vec::new();
    let mut notices = Vec::new();
    let mut names: Vec<_> = fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let lower = name.to_lowercase();
        let is_license = ["license", "licence", "copying", "unlicense"]
            .iter()
            .any(|p| lower.starts_with(p))
            && !lower.ends_with(".rs");
        let is_notice = lower == "notice" || lower.starts_with("notice.");
        if !is_license && !is_notice {
            continue;
        }
        let bytes = fs::read(dir.join(&name))?;
        let text = normalise(&String::from_utf8_lossy(&bytes));
        if is_notice {
            notices.push((name, text));
        } else {
            licenses.push((name, text));
        }
    }
    Ok((licenses, notices))
}

/// Problems with the crate set: forbidden or non-permissive licences, missing texts.
fn check_crates(crates: &[CrateInfo]) -> Vec<String> {
    let mut problems = Vec::new();
    for c in crates {
        let tokens: Vec<&str> = c
            .license
            .split(|ch: char| ch.is_whitespace() || ch == '(' || ch == ')' || ch == '/')
            .filter(|t| !t.is_empty() && !matches!(*t, "OR" | "AND" | "WITH"))
            .collect();
        if let Some(bad) = tokens
            .iter()
            .find(|t| FORBIDDEN_PREFIXES.iter().any(|p| t.starts_with(p)))
        {
            // `GPL` in an `OR` expression is fine only when a permissive alternative exists.
            let has_alt =
                !c.license.contains(" AND ") && tokens.iter().any(|t| PERMISSIVE.contains(t));
            if !has_alt {
                problems.push(format!(
                    "{} {} is licensed {} ({bad} is not allowed)",
                    c.name, c.version, c.license
                ));
            }
        } else if !tokens.iter().any(|t| PERMISSIVE.contains(t)) {
            problems.push(format!(
                "{} {} has licence `{}`, which is not on the permissive list",
                c.name, c.version, c.license
            ));
        }
        if c.license_files.is_empty() && fallback_text(c).is_none() {
            problems.push(format!(
                "{} {} ships no licence file and `{}` has no standard text to fall back on",
                c.name, c.version, c.license
            ));
        }
    }
    problems
}

/// Text to print for a crate that ships no licence file in its published source: the standard
/// text of its SPDX licence (`packaging/licenses/spdx/`); MIT is reconstructed with the manifest
/// `authors` as copyright line, the best information the crate offers. Other licences (BSD
/// variants carry a copyright line) are reported instead.
fn fallback_text(c: &CrateInfo) -> Option<String> {
    let has = |id: &str| {
        c.license
            .split(|ch: char| ch.is_whitespace() || ch == '/' || ch == '(' || ch == ')')
            .any(|t| t == id)
    };
    if has("Apache-2.0") {
        Some(include_str!("../../packaging/licenses/spdx/Apache-2.0.txt").to_owned())
    } else if has("MIT") {
        let holder = if c.authors.is_empty() {
            format!("the {} authors", c.name)
        } else {
            c.authors.join(", ")
        };
        Some(format!(
            "MIT License\n\nCopyright (c) {holder}\n\n{MIT_BODY}"
        ))
    } else if has("MPL-2.0") {
        Some(include_str!("../../packaging/licenses/spdx/MPL-2.0.txt").to_owned())
    } else if has("BSL-1.0") {
        Some(include_str!("../../packaging/licenses/spdx/BSL-1.0.txt").to_owned())
    } else if has("CC0-1.0") {
        Some(include_str!("../../packaging/licenses/spdx/CC0-1.0.txt").to_owned())
    } else {
        None
    }
}

/// Body of the MIT licence after the copyright line.
const MIT_BODY: &str = "Permission is hereby granted, free of charge, to any person obtaining a copy \
of this software and associated documentation files (the \"Software\"), to deal \
in the Software without restriction, including without limitation the rights \
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell \
copies of the Software, and to permit persons to whom the Software is \
furnished to do so, subject to the following conditions:\n\n\
The above copyright notice and this permission notice shall be included in all \
copies or substantial portions of the Software.\n\n\
THE SOFTWARE IS PROVIDED \"AS IS\", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR \
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, \
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE \
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER \
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, \
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE \
SOFTWARE.\n";

/// Renders `THIRD_PARTY_LICENSES`.
fn render_licenses(
    crates: &[CrateInfo],
    fonts: &[(String, String)],
    engines: &[EngineLicense],
    root: &Path,
) -> Result<String> {
    let mut out = String::new();
    out.push_str(
        "Alkahest third-party licences\n\
         =============================\n\n\
         Alkahest itself is MIT licensed (see LICENSE). This file is generated by\n\
         `cargo xtask notice` from the dependency graphs of the headless and the desktop\n\
         binary; do not edit it by hand. Sections: 1 Rust crates, 2 embedded fonts,\n\
         3 engines downloaded or bundled next to the binary.\n\n",
    );

    out.push_str("1. Rust crates\n--------------\n\n");
    out.push_str("Flavours: H = in the headless binary, D = in the desktop binary.\n\n");
    for c in crates {
        let flavour = match (c.headless, c.desktop) {
            (true, true) => "HD",
            (true, false) => "H ",
            _ => " D",
        };
        let _ = writeln!(out, "[{flavour}] {} {}  ({})", c.name, c.version, c.license);
    }
    out.push('\n');

    // Group crates by identical licence text so shared texts are printed once.
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in crates {
        let label = format!("{} {} ({})", c.name, c.version, c.license);
        if c.license_files.is_empty() {
            if let Some(t) = fallback_text(c) {
                groups.entry(normalise(&t)).or_default().push(format!(
                    "{label} [crate ships no licence file; text rebuilt from its manifest]"
                ));
            }
            continue;
        }
        let mut seen = BTreeSet::new();
        for (_, text) in &c.license_files {
            if seen.insert(text.clone()) {
                groups.entry(text.clone()).or_default().push(label.clone());
            }
        }
    }
    let mut ordered: Vec<_> = groups.into_iter().collect();
    ordered.sort_by(|a, b| a.1.cmp(&b.1));
    for (text, users) in ordered {
        out.push_str(&"=".repeat(78));
        out.push('\n');
        out.push_str("Used by:\n");
        for u in &users {
            let _ = writeln!(out, "  - {u}");
        }
        out.push('\n');
        out.push_str(&text);
        out.push('\n');
    }

    out.push_str("\n2. Embedded fonts (desktop binary)\n----------------------------------\n\n");
    for (name, text) in fonts {
        out.push_str(&"=".repeat(78));
        let _ = write!(out, "\n{name}\n\n");
        out.push_str(&normalise(text));
        out.push('\n');
    }

    out.push_str("\n3. Engines\n----------\n\n");
    out.push_str(
        "Engines run as separate processes and are not linked into Alkahest. Each engine\n\
         keeps its own licence; the text is copied next to the installed engine as\n\
         ALKAHEST-ENGINE-LICENSE.txt and into the packages under engines/.\n\n",
    );
    for e in engines {
        out.push_str(&"=".repeat(78));
        let _ = write!(
            out,
            "\n{} {} ({}), {}\n\n",
            e.id, e.version, e.license, e.homepage
        );
        let text = fs::read_to_string(
            root.join("packaging/licenses")
                .join(format!("{}.txt", e.id)),
        )
        .with_context(|| format!("licence text of engine {}", e.id))?;
        out.push_str(&normalise(&text));
        out.push('\n');
    }
    Ok(out)
}

/// The hand-written part of `NOTICE` (everything before the generated marker).
fn read_handwritten_notice(root: &Path) -> Result<String> {
    let text = fs::read_to_string(root.join(NOTICE_FILE)).context("reading NOTICE")?;
    let head = match text.find(NOTICE_MARKER) {
        Some(i) => &text[..i],
        None => &text,
    };
    Ok(head.trim_end().to_owned())
}

/// Renders `NOTICE`: the hand-written head plus the dependency NOTICE files.
fn render_notice(head: &str, crates: &[CrateInfo]) -> String {
    let mut out = format!("{head}\n\n{NOTICE_MARKER}\n\n");
    out.push_str(
        "NOTICE files of Apache-2.0 licensed dependencies, reproduced as the licence requires.\n\
         The full licence texts of every dependency are in THIRD_PARTY_LICENSES.\n\n",
    );
    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in crates {
        for (_, text) in &c.notice_files {
            grouped
                .entry(text.clone())
                .or_default()
                .push(format!("{} {}", c.name, c.version));
        }
    }
    if grouped.is_empty() {
        out.push_str("(no dependency ships a NOTICE file)\n");
    }
    for (text, users) in grouped {
        let _ = write!(out, "--- {} ---\n{}\n", users.join(", "), text);
    }
    out
}

/// OFL font licence texts embedded in the desktop binary.
fn read_fonts(root: &Path) -> Result<Vec<(String, String)>> {
    let dir = root.join("crates/alkahest-desktop/assets/fonts");
    let mut out = Vec::new();
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("OFL-") && n.ends_with(".txt"))
        })
        .collect();
    files.sort();
    for p in files {
        let name = p
            .file_stem()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .trim_start_matches("OFL-")
            .to_owned();
        let text = fs::read_to_string(&p)?;
        out.push((format!("{name} (SIL Open Font License 1.1)"), text));
    }
    if out.is_empty() {
        bail!("no OFL font texts found in {}", dir.display());
    }
    Ok(out)
}

/// Reads `id`, `version`, `license` and `homepage` of every `recipes/*.toml`.
fn read_engines(root: &Path) -> Result<Vec<EngineLicense>> {
    let dir = root.join("recipes");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for p in files {
        let text = fs::read_to_string(&p)?;
        let value: toml::Table = text
            .parse()
            .with_context(|| format!("parsing {}", p.display()))?;
        let get = |k: &str| {
            value
                .get(k)
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        out.push(EngineLicense {
            id: get("id"),
            version: get("version"),
            license: get("license"),
            homepage: get("homepage"),
        });
    }
    Ok(out)
}

/// Engine recipes must declare a permissive licence and have their text on disk.
fn check_engines(root: &Path, engines: &[EngineLicense]) -> Vec<String> {
    let mut problems = Vec::new();
    for e in engines {
        if !matches!(e.license.as_str(), "MIT" | "Apache-2.0") {
            problems.push(format!(
                "recipe {} declares licence `{}` (expected MIT or Apache-2.0)",
                e.id, e.license
            ));
        }
        if !root
            .join("packaging/licenses")
            .join(format!("{}.txt", e.id))
            .is_file()
        {
            problems.push(format!(
                "recipe {} has no licence text at packaging/licenses/{}.txt",
                e.id, e.id
            ));
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(license: &str, with_file: bool) -> CrateInfo {
        CrateInfo {
            name: "x".into(),
            version: "1.0.0".into(),
            license: license.into(),
            authors: vec!["A. Author".into()],
            license_files: if with_file {
                vec![("LICENSE".into(), "text\n".into())]
            } else {
                Vec::new()
            },
            notice_files: Vec::new(),
            headless: true,
            desktop: false,
        }
    }

    #[test]
    fn normalise_unifies_line_ends_and_trims() {
        assert_eq!(normalise("a  \r\nb\r\n\r\n\r\n"), "a\nb\n");
    }

    #[test]
    fn permissive_dual_licence_passes() {
        assert!(check_crates(&[info("MIT OR Apache-2.0", true)]).is_empty());
        assert!(check_crates(&[info("Apache-2.0 WITH LLVM-exception", true)]).is_empty());
    }

    #[test]
    fn gpl_without_alternative_is_rejected() {
        assert_eq!(check_crates(&[info("GPL-3.0-only", true)]).len(), 1);
        assert_eq!(check_crates(&[info("LGPL-2.1 AND MIT", true)]).len(), 1);
        assert!(check_crates(&[info("GPL-2.0 OR MIT", true)]).is_empty());
    }

    #[test]
    fn missing_file_falls_back_to_a_standard_text() {
        assert!(check_crates(&[info("MIT", false)]).is_empty());
        assert_eq!(check_crates(&[info("BSD-3-Clause", false)]).len(), 1);
        assert!(check_crates(&[info("Apache-2.0", false)]).is_empty());
    }

    #[test]
    fn notice_keeps_handwritten_head_and_groups_identical_notices() {
        let mut a = info("Apache-2.0", true);
        a.name = "a".into();
        a.notice_files = vec![("NOTICE".into(), "Hello\n".into())];
        let mut b = a.clone();
        b.name = "b".into();
        let text = render_notice("HEAD", &[a, b]);
        assert!(text.starts_with("HEAD\n\n----- BEGIN GENERATED"));
        assert_eq!(text.matches("Hello").count(), 1);
        assert!(text.contains("--- a 1.0.0, b 1.0.0 ---"));
    }

    #[test]
    fn engines_need_text_and_permissive_licence() {
        let dir = tempfile::tempdir().unwrap();
        let e = EngineLicense {
            id: "eng".into(),
            version: "1".into(),
            license: "GPL-3.0".into(),
            homepage: String::new(),
        };
        assert_eq!(check_engines(dir.path(), &[e]).len(), 2);
    }
}
