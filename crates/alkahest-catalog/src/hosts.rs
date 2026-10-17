//! Host-specific URL shapes for raw files and commit archives (ALK-SP-0011.R11): GitHub,
//! GitLab and Gitea/Forgejo built in, `raw_url` / `archive_url` templates for the rest.

use alkahest_core::config::CatalogSource;

use crate::layout::repo_path;

/// The kind of git host behind a source URL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostKind {
    /// github.com: raw files on raw.githubusercontent.com, archives on codeload.github.com.
    GitHub {
        /// Owner.
        owner: String,
        /// Repository (without `.git`).
        repo: String,
    },
    /// gitlab.com or a host named like a GitLab instance.
    GitLab {
        /// `https://host`.
        base: String,
        /// `group/subgroup/project`.
        project: String,
    },
    /// codeberg.org or a host named like a Gitea/Forgejo instance.
    Gitea {
        /// `https://host`.
        base: String,
        /// `owner/repo`.
        project: String,
    },
    /// Anything else: only the configured templates.
    Other,
}

/// Scheme + authority (`https://host:port`) and the path of `url`, without query or fragment.
pub(crate) fn split_url(url: &str) -> Option<(String, String)> {
    let (scheme, rest) = url.split_once("://")?;
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = rest.split_once('/').unwrap_or((rest, ""));
    Some((
        format!("{scheme}://{authority}"),
        path.trim_matches('/').to_string(),
    ))
}

/// The host (with port) of `url`, lower-cased, without user info.
pub fn host_of(url: &str) -> Option<String> {
    let (base, _) = split_url(url)?;
    let authority = base.split_once("://")?.1;
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    Some(authority.to_ascii_lowercase())
}

impl HostKind {
    /// Detects the host kind of a repository URL.
    pub fn detect(url: &str) -> Self {
        let Some((base, path)) = split_url(url) else {
            return Self::Other;
        };
        let path = path.trim_end_matches(".git").to_string();
        let host = host_of(url).unwrap_or_default();
        let name = host.split(':').next().unwrap_or_default();
        if name == "github.com" {
            let mut parts = path.splitn(2, '/');
            if let (Some(owner), Some(repo)) = (parts.next(), parts.next())
                && !repo.contains('/')
            {
                return Self::GitHub {
                    owner: owner.to_string(),
                    repo: repo.to_string(),
                };
            }
            return Self::Other;
        }
        if path.is_empty() {
            return Self::Other;
        }
        if name == "gitlab.com" || name.split('.').any(|l| l == "gitlab") {
            return Self::GitLab {
                base,
                project: path,
            };
        }
        if name == "codeberg.org" || name.split('.').any(|l| l == "gitea" || l == "forgejo") {
            return Self::Gitea {
                base,
                project: path,
            };
        }
        Self::Other
    }

    /// Hosts that belong to the repository: the only ones a token is sent to.
    pub fn credential_hosts(&self, url: &str) -> Vec<String> {
        match self {
            Self::GitHub { .. } => vec![
                "github.com".into(),
                "raw.githubusercontent.com".into(),
                "codeload.github.com".into(),
            ],
            _ => host_of(url).into_iter().collect(),
        }
    }

    /// Basic-auth user name for the git smart HTTP endpoints.
    pub fn git_user(&self) -> &'static str {
        match self {
            Self::GitHub { .. } => "x-access-token",
            Self::GitLab { .. } => "oauth2",
            _ => "alkahest",
        }
    }
}

fn fill(template: &str, commit: &str, path: &str) -> String {
    template.replace("{commit}", commit).replace("{path}", path)
}

/// URL of `path` (relative to the catalog root) at `commit`, or `None` when the host has no
/// raw endpoint and no `raw_url` template is configured.
pub fn raw_url(
    source: &CatalogSource,
    kind: &HostKind,
    commit: &str,
    path: &str,
) -> Option<String> {
    let full = repo_path(&source.subdir, path);
    if let Some(t) = &source.raw_url {
        return Some(fill(t, commit, &full));
    }
    match kind {
        HostKind::GitHub { owner, repo } => Some(format!(
            "https://raw.githubusercontent.com/{owner}/{repo}/{commit}/{full}"
        )),
        HostKind::GitLab { base, project } => {
            Some(format!("{base}/{project}/-/raw/{commit}/{full}"))
        }
        HostKind::Gitea { base, project } => {
            Some(format!("{base}/{project}/raw/commit/{commit}/{full}"))
        }
        HostKind::Other => None,
    }
}

/// URL of the commit archive (`tar.gz`), or `None` when unknown.
pub fn archive_url(source: &CatalogSource, kind: &HostKind, commit: &str) -> Option<String> {
    if let Some(t) = &source.archive_url {
        return Some(fill(t, commit, &source.subdir));
    }
    match kind {
        HostKind::GitHub { owner, repo } => Some(format!(
            "https://codeload.github.com/{owner}/{repo}/tar.gz/{commit}"
        )),
        HostKind::GitLab { base, project } => {
            let name = project.rsplit('/').next().unwrap_or("archive");
            let sub = source.subdir.trim_matches('/');
            let query = if sub.is_empty() || sub == "." {
                String::new()
            } else {
                format!("?path={sub}")
            };
            Some(format!(
                "{base}/{project}/-/archive/{commit}/{name}-{commit}.tar.gz{query}"
            ))
        }
        HostKind::Gitea { base, project } => {
            Some(format!("{base}/{project}/archive/{commit}.tar.gz"))
        }
        HostKind::Other => None,
    }
}

/// `url` with user info, query and fragment removed: safe for messages and logs.
pub fn redact_url(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.split(['?', '#']).next().unwrap_or_default().to_string();
    };
    let rest = rest.split(['?', '#']).next().unwrap_or_default();
    let (authority, path) = rest.split_once('/').map_or((rest, ""), |(a, p)| (a, p));
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    if path.is_empty() {
        format!("{scheme}://{authority}")
    } else {
        format!("{scheme}://{authority}/{path}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const C: &str = "2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29";

    #[test]
    fn github_urls() {
        let s = CatalogSource::official();
        let k = HostKind::detect(&s.url);
        assert_eq!(
            raw_url(&s, &k, C, "index.toml").as_deref(),
            Some(format!("https://raw.githubusercontent.com/digiogithub/alchemai-alkahest/{C}/catalog/index.toml").as_str())
        );
        assert_eq!(
            archive_url(&s, &k, C).as_deref(),
            Some(
                format!("https://codeload.github.com/digiogithub/alchemai-alkahest/tar.gz/{C}")
                    .as_str()
            )
        );
        assert!(
            k.credential_hosts(&s.url)
                .contains(&"raw.githubusercontent.com".to_string())
        );
    }

    #[test]
    fn gitlab_and_gitea_urls() {
        let s = CatalogSource::new("a", "https://gitlab.com/g/sub/cat.git");
        let k = HostKind::detect(&s.url);
        assert_eq!(
            raw_url(&s, &k, C, "index.toml").as_deref(),
            Some(format!("https://gitlab.com/g/sub/cat/-/raw/{C}/index.toml").as_str())
        );
        let s = CatalogSource::new("b", "https://codeberg.org/o/r");
        let k = HostKind::detect(&s.url);
        assert_eq!(
            raw_url(&s, &k, C, "catalog.toml").as_deref(),
            Some(format!("https://codeberg.org/o/r/raw/commit/{C}/catalog.toml").as_str())
        );
    }

    #[test]
    fn templates_win_and_unknown_hosts_have_none() {
        let mut s = CatalogSource::new("c", "https://git.example.org/acme/catalog");
        let k = HostKind::detect(&s.url);
        assert_eq!(k, HostKind::Other);
        assert_eq!(raw_url(&s, &k, C, "index.toml"), None);
        s.raw_url = Some("https://files.example.org/{commit}/{path}".into());
        assert_eq!(
            raw_url(&s, &k, C, "index.toml").as_deref(),
            Some(format!("https://files.example.org/{C}/index.toml").as_str())
        );
    }

    #[test]
    fn urls_are_redacted() {
        assert_eq!(
            redact_url("https://user:secret@host/p?token=x"),
            "https://host/p"
        );
    }
}
