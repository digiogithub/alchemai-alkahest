//! `cargo xtask engines-check [--fail-on-update]`: compares the engine pins in `catalog/engines/*.toml`
//! (or `recipes/*.toml` when there is no catalog)
//! with the upstream releases (ALK-US-0053) and reports what is newer.
//!
//! Sources: GitHub releases for recipes whose homepage is a GitHub repository (llama.cpp, the
//! PrismML fork, whisper.cpp, stable-diffusion.cpp) and the PyPI JSON API for recipes with a
//! `venv` build (vLLM). For a newer GitHub release the report lists, for every prebuilt asset
//! of the current pin, the matching asset of the newer release with its SHA-256 digest (GitHub
//! publishes it in the `digest` field), so a maintainer can bump the recipe by copy-paste.
//!
//! The command changes nothing and opens nothing: it prints a Markdown report (also appended
//! to `$GITHUB_STEP_SUMMARY` when set). It exits non-zero only when an upstream cannot be
//! queried, or with `--fail-on-update` when something is newer. Network access happens only
//! here, through `curl`; the comparison logic is pure and unit tested with fixtures.

use std::cmp::Ordering;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;

/// Number of GitHub release pages (50 each) walked before giving up on finding the pin.
const MAX_PAGES: u32 = 4;

/// Where a recipe's upstream releases live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Upstream {
    /// A GitHub repository (`owner/repo`).
    GitHub(String),
    /// A PyPI project.
    PyPi(String),
}

/// A recipe reduced to what the check needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// Recipe id.
    pub id: String,
    /// Pinned upstream version or tag.
    pub version: String,
    /// Where to look for newer ones.
    pub upstream: Upstream,
    /// File names of the prebuilt assets of the pin (all variants, all platforms).
    pub asset_names: Vec<String>,
}

/// A newer upstream release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Newer {
    /// Tag (GitHub) or version (PyPI) of the newest release.
    pub latest: String,
    /// How many releases newer than the pin exist.
    pub count: usize,
    /// `(asset name of the new release, digest)` for the assets that replace the pin's.
    pub assets: Vec<(String, String)>,
    /// Asset names of the pin that have no counterpart in the new release.
    pub missing: Vec<String>,
}

/// HTTP GET returning the body; swapped for fixtures in tests.
pub trait Http {
    /// Fetches `url` and returns the response body.
    fn get(&self, url: &str) -> Result<String>;
}

/// [`Http`] over the `curl` binary (present on every runner and dev machine).
pub struct Curl;

impl Http for Curl {
    fn get(&self, url: &str) -> Result<String> {
        let mut cmd = Command::new("curl");
        cmd.args([
            "-fsSL",
            "--max-time",
            "30",
            "-H",
            "Accept: application/json",
        ]);
        cmd.args(["-H", "User-Agent: alkahest-engines-check"]);
        if url.starts_with("https://api.github.com/")
            && let Ok(token) = std::env::var("GITHUB_TOKEN")
            && !token.is_empty()
        {
            cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
        }
        let out = cmd
            .arg(url)
            .output()
            .context("running curl (is it installed?)")?;
        if !out.status.success() {
            bail!(
                "GET {url} failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Entry point. Returns `Ok(true)` unless an upstream failed (or `--fail-on-update` and
/// something is newer).
pub fn run(root: &Path, rest: &[String]) -> Result<bool> {
    let fail_on_update = rest.iter().any(|a| a == "--fail-on-update");
    if let Some(bad) = rest.iter().find(|a| *a != "--fail-on-update") {
        bail!("engines-check: unknown option `{bad}`");
    }
    let pins = read_pins(root)?;
    let mut report = String::from("## Engine pin check\n\n");
    let mut errors = 0;
    let mut updates = 0;
    for pin in &pins {
        match check(&Curl, pin) {
            Ok(None) => {
                let _ = writeln!(report, "- `{}` {}: up to date", pin.id, pin.version);
            }
            Ok(Some(n)) => {
                updates += 1;
                report.push_str(&render_newer(pin, &n));
            }
            Err(e) => {
                errors += 1;
                let _ = writeln!(
                    report,
                    "- `{}` {}: **check failed**: {e:#}",
                    pin.id, pin.version
                );
            }
        }
    }
    report.push_str(
        "\nTo bump a pin: edit `catalog/engines/<id>.toml` (`version`, URLs, `sha256`, `size`), run \
         `cargo xtask catalog-index --sync-recipes`, the opt-in install tests (`ALKAHEST_TEST_NET=1`, \
         `ALKAHEST_TEST_GPU=1`) and `cargo xtask notice`.\n",
    );
    println!("{report}");
    if let Ok(path) = std::env::var("GITHUB_STEP_SUMMARY")
        && !path.is_empty()
    {
        use std::io::Write as _;
        let mut f = fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(path)?;
        f.write_all(report.as_bytes())?;
    }
    Ok(errors == 0 && !(fail_on_update && updates > 0))
}

/// Queries the upstream of one pin; `None` when the pin is the newest.
pub fn check(http: &dyn Http, pin: &Pin) -> Result<Option<Newer>> {
    match &pin.upstream {
        Upstream::GitHub(repo) => {
            let mut releases = Vec::new();
            for page in 1..=MAX_PAGES {
                let url =
                    format!("https://api.github.com/repos/{repo}/releases?per_page=50&page={page}");
                let body = http.get(&url)?;
                let v: Value = serde_json::from_str(&body).context("parsing GitHub releases")?;
                let list = v
                    .as_array()
                    .ok_or_else(|| anyhow!("unexpected GitHub reply"))?;
                let found = list.iter().any(|r| r["tag_name"] == pin.version.as_str());
                releases.extend(list.iter().cloned());
                if found || list.len() < 50 {
                    break;
                }
            }
            Ok(newer_github(pin, &releases))
        }
        Upstream::PyPi(name) => {
            let body = http.get(&format!("https://pypi.org/pypi/{name}/json"))?;
            let v: Value = serde_json::from_str(&body).context("parsing PyPI reply")?;
            newer_pypi(pin, &v)
        }
    }
}

/// The numeric key of a tag or version: the integers it contains, skipping commit hashes and
/// words (`b11669` -> [11669], `v1.9.5` -> [1, 9, 5], `prism-b10777-e3de715` -> [10777],
/// `master-970-2f016e8` -> [970]).
pub fn tag_key(tag: &str) -> Vec<u64> {
    tag.split(['.', '-', '_'])
        .filter_map(|tok| {
            let digits = tok
                .strip_prefix(['b', 'v'])
                .filter(|r| !r.is_empty())
                .unwrap_or(tok);
            digits.parse::<u64>().ok()
        })
        .collect()
}

/// The non-numeric lead of a tag (`b`, `v`, `prism-b`, `master-`): releases only compare within
/// the same family, so a differently named tag series is not mistaken for a newer release.
pub fn tag_family(tag: &str) -> &str {
    let end = tag.find(|c: char| c.is_ascii_digit()).unwrap_or(tag.len());
    &tag[..end]
}

/// Newer, non-draft, non-prerelease GitHub releases than the pin, with replacement assets.
pub fn newer_github(pin: &Pin, releases: &[Value]) -> Option<Newer> {
    let family = tag_family(&pin.version);
    let current = tag_key(&pin.version);
    let mut newer: Vec<&Value> = releases
        .iter()
        .filter(|r| !r["draft"].as_bool().unwrap_or(false))
        .filter(|r| !r["prerelease"].as_bool().unwrap_or(false))
        .filter(|r| {
            r["tag_name"].as_str().is_some_and(|t| {
                tag_family(t) == family && tag_key(t).cmp(&current) == Ordering::Greater
            })
        })
        .collect();
    newer.sort_by_key(|r| tag_key(r["tag_name"].as_str().unwrap_or_default()));
    let latest = *newer.last()?;
    let assets = latest["assets"].as_array().cloned().unwrap_or_default();
    let mut found = Vec::new();
    let mut missing = Vec::new();
    for old in &pin.asset_names {
        let Some(suffix) = old.split_once("-bin-").map(|(_, s)| s) else {
            continue;
        };
        let hit = assets.iter().find(|a| {
            a["name"]
                .as_str()
                .is_some_and(|n| n.split_once("-bin-").is_some_and(|(_, s)| s == suffix))
        });
        match hit {
            Some(a) => found.push((
                a["name"].as_str().unwrap_or_default().to_owned(),
                a["digest"].as_str().unwrap_or("(no digest)").to_owned(),
            )),
            None => missing.push(old.clone()),
        }
    }
    found.sort();
    found.dedup();
    missing.sort();
    missing.dedup();
    Some(Newer {
        latest: latest["tag_name"].as_str().unwrap_or_default().to_owned(),
        count: newer.len(),
        assets: found,
        missing,
    })
}

/// Newest stable PyPI version, when newer than the pin.
pub fn newer_pypi(pin: &Pin, reply: &Value) -> Result<Option<Newer>> {
    let releases = reply["releases"]
        .as_object()
        .ok_or_else(|| anyhow!("PyPI reply has no `releases`"))?;
    let current = tag_key(&pin.version);
    let mut newer: Vec<(&String, Vec<u64>)> = releases
        .iter()
        // Pre-releases (`0.12.0rc1`, `.dev`, `.post`) and yanked-only versions are not offered.
        .filter(|(v, files)| {
            v.chars().all(|c| c.is_ascii_digit() || c == '.')
                && files
                    .as_array()
                    .is_some_and(|f| f.iter().any(|x| !x["yanked"].as_bool().unwrap_or(false)))
        })
        .map(|(v, _)| (v, tag_key(v)))
        .filter(|(_, k)| k.cmp(&current) == Ordering::Greater)
        .collect();
    newer.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(newer.last().map(|(v, _)| Newer {
        latest: (*v).clone(),
        count: newer.len(),
        assets: Vec::new(),
        missing: Vec::new(),
    }))
}

/// Markdown for one outdated pin.
fn render_newer(pin: &Pin, n: &Newer) -> String {
    let mut out = format!(
        "- `{}` {}: **{} newer release{}**, latest `{}`\n",
        pin.id,
        pin.version,
        n.count,
        if n.count == 1 { "" } else { "s" },
        n.latest
    );
    for (name, digest) in &n.assets {
        let _ = writeln!(out, "  - `{name}` {digest}");
    }
    for name in &n.missing {
        let _ = writeln!(out, "  - no counterpart for `{name}` in the new release");
    }
    out
}

/// Reads the pins from `catalog/engines/*.toml`, falling back to `recipes/*.toml`.
pub fn read_pins(root: &Path) -> Result<Vec<Pin>> {
    // `catalog/engines/` is the source of truth (ALK-US-0121); `recipes/` is its built-in copy.
    let catalog = root.join("catalog").join("engines");
    let dir = if catalog.is_dir() {
        catalog
    } else {
        root.join("recipes")
    };
    let mut files: Vec<_> = fs::read_dir(&dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut pins = Vec::new();
    for f in files {
        let text = fs::read_to_string(&f)?;
        pins.push(parse_pin(&text).with_context(|| format!("recipe {}", f.display()))?);
    }
    Ok(pins)
}

/// Extracts a [`Pin`] from recipe TOML text.
pub fn parse_pin(text: &str) -> Result<Pin> {
    let t: toml::Table = text.parse()?;
    let s = |k: &str| t.get(k).and_then(|v| v.as_str()).map(str::to_owned);
    let id = s("id").ok_or_else(|| anyhow!("recipe has no id"))?;
    let version = s("version").ok_or_else(|| anyhow!("recipe {id} has no version"))?;
    let variants = t.get("variants").and_then(|v| v.as_table());
    let mut asset_names = Vec::new();
    let mut pypi = None;
    for variant in variants.into_iter().flat_map(|m| m.values()) {
        for asset in variant
            .get("assets")
            .and_then(|a| a.as_array())
            .into_iter()
            .flatten()
        {
            if let Some(name) = asset
                .get("url")
                .and_then(|u| u.as_str())
                .and_then(|u| u.rsplit('/').next())
            {
                asset_names.push(name.to_owned());
            }
        }
        if let Some(pkg) = variant
            .get("venv")
            .and_then(|v| v.get("packages"))
            .and_then(|p| p.as_array())
            .and_then(|p| p.first())
            .and_then(|p| p.as_str())
        {
            pypi = Some(
                pkg.split(['=', '<', '>', '['])
                    .next()
                    .unwrap_or(pkg)
                    .to_owned(),
            );
        }
    }
    asset_names.sort();
    asset_names.dedup();
    let upstream = if let Some(pkg) = pypi {
        Upstream::PyPi(pkg)
    } else {
        let home = s("homepage").ok_or_else(|| anyhow!("recipe {id} has no homepage"))?;
        let repo = home
            .strip_prefix("https://github.com/")
            .ok_or_else(|| anyhow!("recipe {id}: homepage is not a GitHub repository"))?
            .trim_end_matches('/')
            .to_owned();
        Upstream::GitHub(repo)
    };
    Ok(Pin {
        id,
        version,
        upstream,
        asset_names,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    struct Fixture(BTreeMap<String, String>);
    impl Http for Fixture {
        fn get(&self, url: &str) -> Result<String> {
            self.0
                .get(url)
                .cloned()
                .ok_or_else(|| anyhow!("no fixture for {url}"))
        }
    }

    fn llama_pin() -> Pin {
        Pin {
            id: "llama-cpp".into(),
            version: "b11669".into(),
            upstream: Upstream::GitHub("ggml-org/llama.cpp".into()),
            asset_names: vec![
                "llama-b11669-bin-ubuntu-x64.tar.gz".into(),
                "llama-b11669-bin-win-cpu-x64.zip".into(),
            ],
        }
    }

    fn release(tag: &str, assets: &[(&str, &str)], prerelease: bool) -> Value {
        json!({
            "tag_name": tag, "draft": false, "prerelease": prerelease,
            "assets": assets.iter().map(|(n, d)| json!({"name": n, "digest": d})).collect::<Vec<_>>()
        })
    }

    #[test]
    fn tag_keys_ignore_hashes_and_words() {
        assert_eq!(tag_key("b11669"), vec![11669]);
        assert_eq!(tag_key("v1.9.5"), vec![1, 9, 5]);
        assert_eq!(tag_key("prism-b10777-e3de715"), vec![10777]);
        assert_eq!(tag_key("master-970-2f016e8"), vec![970]);
        assert_eq!(tag_family("prism-b10777-e3de715"), "prism-b");
        assert_eq!(tag_family("v1.9.5"), "v");
    }

    #[test]
    fn newer_release_lists_replacement_assets_with_digests() {
        let releases = vec![
            release(
                "b11700",
                &[
                    ("llama-b11700-bin-ubuntu-x64.tar.gz", "sha256:aa"),
                    ("llama-b11700-bin-win-cpu-x64.zip", "sha256:bb"),
                ],
                false,
            ),
            release("b11680", &[], false),
            release("b11669", &[], false),
            release("b11500", &[], false),
        ];
        let n = newer_github(&llama_pin(), &releases).unwrap();
        assert_eq!(n.latest, "b11700");
        assert_eq!(n.count, 2);
        assert_eq!(n.assets.len(), 2);
        assert_eq!(n.assets[0].1, "sha256:aa");
        assert!(n.missing.is_empty());
    }

    #[test]
    fn up_to_date_prerelease_and_other_families_are_ignored() {
        let releases = vec![
            release("b11800", &[], true),
            release("nightly-b99999", &[], false),
            release("b11669", &[], false),
        ];
        assert!(newer_github(&llama_pin(), &releases).is_none());
    }

    #[test]
    fn missing_counterpart_asset_is_reported() {
        let releases = vec![release(
            "b11700",
            &[("llama-b11700-bin-ubuntu-x64.tar.gz", "sha256:aa")],
            false,
        )];
        let n = newer_github(&llama_pin(), &releases).unwrap();
        assert_eq!(n.missing, vec!["llama-b11669-bin-win-cpu-x64.zip"]);
    }

    #[test]
    fn pypi_picks_the_newest_stable_version() {
        let pin = Pin {
            id: "vllm".into(),
            version: "0.11.0".into(),
            upstream: Upstream::PyPi("vllm".into()),
            asset_names: Vec::new(),
        };
        let reply = json!({"releases": {
            "0.10.2": [{"yanked": false}],
            "0.11.0": [{"yanked": false}],
            "0.11.1": [{"yanked": false}],
            "0.12.0": [{"yanked": false}],
            "0.13.0rc1": [{"yanked": false}],
            "0.14.0": [{"yanked": true}],
        }});
        let n = newer_pypi(&pin, &reply).unwrap().unwrap();
        assert_eq!((n.latest.as_str(), n.count), ("0.12.0", 2));
    }

    #[test]
    fn check_walks_a_fixture_server() {
        let body = json!([release("b11700", &[], false), release("b11669", &[], false)]);
        let url = "https://api.github.com/repos/ggml-org/llama.cpp/releases?per_page=50&page=1";
        let fx = Fixture(BTreeMap::from([(url.to_owned(), body.to_string())]));
        let n = check(&fx, &llama_pin()).unwrap().unwrap();
        assert_eq!(n.latest, "b11700");
        assert!(check(&Fixture(BTreeMap::new()), &llama_pin()).is_err());
    }

    #[test]
    fn recipes_parse_into_pins() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let pins = read_pins(&root).unwrap();
        assert_eq!(pins.len(), 5);
        let by = |id: &str| pins.iter().find(|p| p.id == id).unwrap();
        assert_eq!(by("vllm").upstream, Upstream::PyPi("vllm".into()));
        assert_eq!(
            by("prism-llama-cpp").upstream,
            Upstream::GitHub("PrismML-Eng/llama.cpp".into())
        );
        assert!(
            by("llama-cpp")
                .asset_names
                .iter()
                .any(|n| n.contains("-bin-ubuntu-x64"))
        );
    }
}
