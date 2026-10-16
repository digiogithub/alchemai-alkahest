//! `cargo xtask check-deps`: keeps GPUI confined to the desktop crate.
//!
//! Two rules from AGENTS.md section 4:
//! 1. No workspace crate other than `alkahest-desktop` depends on `gpui-kit` or `gpui`,
//!    directly or through another crate (paths through `alkahest-desktop` itself, such as the
//!    optional `desktop` feature of the binary, are allowed).
//! 2. No Rust source outside `crates/alkahest-desktop/src/ui/` names the gpui-kit facade.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo_metadata::{CargoOpt, DependencyKind, MetadataCommand};

/// The only crate allowed to depend on GPUI.
pub const DESKTOP_CRATE: &str = "alkahest-desktop";

/// Directory (relative to the workspace root) whose sources may name the facade.
pub const UI_DIR: &str = "crates/alkahest-desktop/src/ui";

/// The facade path prefix, assembled so that this file does not match itself.
pub const FACADE_PATH: &str = concat!("gpui", "_kit::");

/// A package in the dependency graph, reduced to what the checks need.
#[derive(Debug, Clone)]
pub struct Pkg {
    /// Package name.
    pub name: String,
    /// Whether the package is a workspace member.
    pub is_member: bool,
    /// Resolved non-dev dependencies (package names), all features enabled.
    pub deps: BTreeSet<String>,
    /// Declared dependencies of any kind, including optional and dev ones.
    pub declared: BTreeSet<String>,
}

/// Run both checks against the real workspace. Returns `Ok(true)` when clean.
pub fn run() -> Result<bool> {
    let metadata = MetadataCommand::new()
        .features(CargoOpt::AllFeatures)
        .exec()
        .context("running `cargo metadata`")?;
    let resolve = metadata.resolve.context("metadata has no resolve graph")?;
    let by_id: BTreeMap<_, _> = metadata.packages.iter().map(|p| (&p.id, p)).collect();
    let members: BTreeSet<_> = metadata.workspace_members.iter().collect();

    let mut pkgs = Vec::new();
    for node in &resolve.nodes {
        let Some(pkg) = by_id.get(&node.id) else {
            continue;
        };
        let deps = node
            .deps
            .iter()
            .filter(|d| {
                d.dep_kinds
                    .iter()
                    .any(|k| k.kind != DependencyKind::Development)
            })
            .filter_map(|d| by_id.get(&d.pkg).map(|p| p.name.to_string()))
            .collect();
        let declared = pkg.dependencies.iter().map(|d| d.name.clone()).collect();
        pkgs.push(Pkg {
            name: pkg.name.to_string(),
            is_member: members.contains(&node.id),
            deps,
            declared,
        });
    }

    let root = metadata.workspace_root.as_std_path();
    let mut errors = check_graph(&pkgs);
    errors.extend(scan_sources(root)?);

    if errors.is_empty() {
        let n = pkgs.iter().filter(|p| p.is_member).count();
        println!("check-deps: OK ({n} workspace crates; GPUI confined to {DESKTOP_CRATE})");
        Ok(true)
    } else {
        for e in &errors {
            eprintln!("check-deps: ERROR: {e}");
        }
        eprintln!("check-deps: {} problem(s) found", errors.len());
        Ok(false)
    }
}

/// True for the GPUI crates and the gpui-kit facade, however the name is spelled.
fn is_gpui(name: &str) -> bool {
    let n = name.replace('_', "-");
    n == "gpui" || n.starts_with("gpui-")
}

/// Pure graph check: returns one message per workspace crate that reaches GPUI.
pub fn check_graph(pkgs: &[Pkg]) -> Vec<String> {
    let by_name: BTreeMap<&str, &Pkg> = pkgs.iter().map(|p| (p.name.as_str(), p)).collect();
    let mut errors = Vec::new();

    for p in pkgs
        .iter()
        .filter(|p| p.is_member && p.name != DESKTOP_CRATE)
    {
        if let Some(dep) = p.declared.iter().find(|d| is_gpui(d)) {
            errors.push(format!(
                "`{}` declares a dependency on `{dep}`; only `{DESKTOP_CRATE}` may use GPUI",
                p.name
            ));
            continue;
        }
        // Transitive closure that does not walk through the desktop crate.
        let mut seen = BTreeSet::new();
        let mut stack: Vec<&str> = vec![p.name.as_str()];
        while let Some(n) = stack.pop() {
            let Some(pkg) = by_name.get(n) else {
                continue;
            };
            for d in &pkg.deps {
                if d != DESKTOP_CRATE && seen.insert(d.as_str()) {
                    stack.push(d.as_str());
                }
            }
        }
        if let Some(dep) = seen.iter().find(|d| is_gpui(d)) {
            errors.push(format!(
                "`{}` depends transitively on `{dep}`; only `{DESKTOP_CRATE}` may use GPUI",
                p.name
            ));
        }
    }
    errors
}

/// Scan every `.rs` file under `root` (skipping `target/` and hidden directories) and report
/// those outside [`UI_DIR`] that contain [`FACADE_PATH`].
pub fn scan_sources(root: &Path) -> Result<Vec<String>> {
    let ui_dir: PathBuf = root.join(UI_DIR);
    let mut errors = Vec::new();
    let walker = walkdir::WalkDir::new(root).into_iter().filter_entry(|e| {
        let name = e.file_name().to_string_lossy();
        e.depth() == 0 || !(name.starts_with('.') || name == "target")
    });
    for entry in walker {
        let entry = entry.context("walking the workspace")?;
        let path = entry.path();
        if !entry.file_type().is_file()
            || path.extension().is_none_or(|ext| ext != "rs")
            || path.starts_with(&ui_dir)
        {
            continue;
        }
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        if let Some(line) = text.lines().position(|l| l.contains(FACADE_PATH)) {
            let rel = path.strip_prefix(root).unwrap_or(path);
            errors.push(format!(
                "{}:{} names `{FACADE_PATH}`; only {UI_DIR}/ may use the gpui-kit facade",
                rel.display(),
                line + 1
            ));
        }
    }
    Ok(errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str, member: bool, deps: &[&str], declared: &[&str]) -> Pkg {
        Pkg {
            name: name.to_string(),
            is_member: member,
            deps: deps.iter().map(|s| s.to_string()).collect(),
            declared: declared.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn graph(extra: Vec<Pkg>) -> Vec<Pkg> {
        let mut pkgs = vec![
            pkg(
                "alkahest",
                true,
                &["alkahest-desktop", "clap"],
                &["alkahest-desktop", "clap"],
            ),
            pkg("alkahest-desktop", true, &["gpui-kit"], &["gpui-kit"]),
            pkg("gpui-kit", false, &["gpui"], &["gpui"]),
            pkg("gpui", false, &[], &[]),
            pkg("clap", false, &[], &[]),
        ];
        pkgs.extend(extra);
        pkgs
    }

    #[test]
    fn desktop_crate_and_binary_feature_are_allowed() {
        assert!(check_graph(&graph(vec![])).is_empty());
    }

    #[test]
    fn direct_gpui_kit_dependency_outside_desktop_fails() {
        let errors = check_graph(&graph(vec![pkg(
            "alkahest-api",
            true,
            &["gpui-kit"],
            &["gpui-kit"],
        )]));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("alkahest-api"));
    }

    #[test]
    fn dev_dependency_on_gpui_also_fails() {
        let errors = check_graph(&graph(vec![pkg("alkahest-core", true, &[], &["gpui"])]));
        assert_eq!(errors.len(), 1);
    }

    #[test]
    fn transitive_gpui_through_another_crate_fails() {
        let errors = check_graph(&graph(vec![
            pkg("some-ui-lib", false, &["gpui"], &["gpui"]),
            pkg("alkahest-client", true, &["some-ui-lib"], &["some-ui-lib"]),
        ]));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("transitively"));
    }

    fn write(root: &Path, rel: &str, body: &str) {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    #[test]
    fn facade_use_inside_ui_dir_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let use_line = format!("use {FACADE_PATH}Button;\n");
        write(dir.path(), &format!("{UI_DIR}/library.rs"), &use_line);
        write(dir.path(), "crates/alkahest-core/src/lib.rs", "//! core\n");
        // Build output and hidden directories are ignored.
        write(dir.path(), "target/debug/build/x.rs", &use_line);
        write(dir.path(), ".claude/worktrees/x/y.rs", &use_line);
        assert!(scan_sources(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn facade_use_outside_ui_dir_fails() {
        let dir = tempfile::tempdir().unwrap();
        let use_line = format!("//! x\nuse {FACADE_PATH}Button;\n");
        write(dir.path(), "crates/alkahest-desktop/src/app.rs", &use_line);
        write(dir.path(), "crates/alkahest-api/src/lib.rs", &use_line);
        let errors = scan_sources(dir.path()).unwrap();
        assert_eq!(errors.len(), 2);
        assert!(errors.iter().all(|e| e.contains(":2 ")));
    }

    #[test]
    fn real_workspace_is_clean() {
        assert!(run().unwrap());
    }
}
