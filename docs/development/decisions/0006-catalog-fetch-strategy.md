# ADR 0006: how Alkahest fetches git catalogs

Status: proposed 2026-10-17, pending product owner review (security-tagged epic). Story:
[[ALK-US-0117]]. Epic: [[ALK-EP-0015]]. Spec: [[ALK-SP-0011]] (R10, R11, R14). Plan:
[[development/plans/ALK-EP-0015-git-catalogs]].

## Context

Engine recipes and model entries move from release-only `recipes/*.toml` to catalogs kept in
git: the official one is the `catalog/` folder of the public repository
`github.com/digiogithub/alchemai-alkahest` (ref `main`), and users can add more, private ones
included. A source is `url + ref + subdir`. The fetch must:

- work on Linux, macOS and Windows with nothing but the Alkahest binary installed;
- use the proxy and TLS handling of `alkahest-fetch` (reqwest 0.13, rustls with aws-lc-rs, no
  OpenSSL / native-tls: `deny.toml` bans both);
- add little binary size and only permissive licences (`deny.toml` allow list, ADR
  [[0004-licence-policy-and-notices]]);
- support private repositories with a token from the OS keyring;
- record the commit id each snapshot came from, so the content is pinned and reportable;
- be cheap when nothing changed (refresh every 24 h on every installation).

## Measurements (2026-10-17)

All against this repository at commit `2d45ae95a500e22fa79fcd1c3c6c4ecdc4ea5a29`, whose
`recipes/` (the future `catalog/engines/`) is 48 KB.

| What | Bytes on the wire | How measured |
|---|---|---|
| v2 `ls-refs` for `refs/heads/main` (GitHub) | **65** | `curl -X POST -H 'Git-Protocol: version=2' .../git-upload-pack` with `command=ls-refs`, `ref-prefix refs/heads/main` |
| same on gitlab.com (`gitlab-runner`) | 65 | same request |
| v0 `info/refs` advertisement, this repo | < 1 KB | `curl .../info/refs?service=git-upload-pack` |
| v0 advertisement, `ggml-org/llama.cpp` (every PR ref) | 1 758 429 | same; why v2 with `ref-prefix` comes first |
| GitHub commit archive (`codeload .../tar.gz/<commit>`) | 9 951 813 | 1 408 entries: the whole repository, not just `catalog/` |
| `git clone --depth 1 --bare` (git 2.43) | 9 329 680 objects | `du -sb objects` |
| `git clone --depth 1 --filter=blob:none --sparse` + `sparse-checkout set recipes` | 287 581 objects | same |
| raw files of the catalog (index + changed files) | ~50 KB worst case, a few KB typical | sum of file sizes |

Binary size, from a probe crate built `--release`, `strip = true`, `lto = "thin"`,
`codegen-units = 1`, with the workspace pins (reqwest 0.13.5 rustls, tokio, flate2, tar):

| Probe | Stripped size | Normal dependency crates |
|---|---|---|
| baseline: reqwest + tokio + flate2 + tar (what Alkahest already links) | 6 051 200 B | 96 |
| baseline + `gix 0.88.0` (`default-features = false`, `blocking-http-transport-reqwest-rust-tls`, `worktree-mutation`, `sha1`) doing a depth-1 clone and checkout | 10 689 208 B (**+4.6 MB, +77 %**) | 193 (**+97**, 52 of them `gix-*`) |

The gix probe resolves to the same `reqwest 0.13.5` and `rustls 0.23.46` and pulls no OpenSSL;
every licence in its graph is already allowed by `deny.toml` (MIT, Apache-2.0, ISC, BSD-3-Clause,
Unicode-3.0, Zlib, Unlicense OR MIT; aws-lc-sys's MIT-0 is an OR alternative).

GitHub facts checked live: `GET api.github.com/repos/<o>/<r>/commits/main` with
`Accept: application/vnd.github.sha` returns the bare commit id, but the unauthenticated limit is
60 requests per hour per IP (`x-ratelimit-limit: 60`), which a NATed office of installations can
exhaust; `api.github.com/.../tarball/main` answers 302 to `codeload.github.com/.../legacy.tar.gz/
refs/heads/main`; the codeload archive's pax global header carries `comment = <commit id>` (what
`git archive` writes); `raw.githubusercontent.com/<o>/<r>/<commit>/<path>` serves a file pinned
to a commit (`cache-control: max-age=300`, irrelevant for commit URLs, which never change).
The git smart HTTP protocol v2 (`ls-refs`, `ref-prefix`) is documented in
https://git-scm.com/docs/protocol-v2 and the v0 advertisement in
https://git-scm.com/docs/http-protocol; both were confirmed on github.com and gitlab.com above.

## Options

### A. `git` CLI (shallow, partial, sparse clone)

`git clone --depth 1 --filter=blob:none --sparse` then `sparse-checkout set <subdir>`, later
`git fetch --depth 1` + `checkout`. Cheapest content transfer of the git options (288 KB here),
commit id for free (`git rev-parse HEAD`), signed tags verifiable with `git verify-tag`, private
repositories through the user's credential helper.

Against: Windows has no git by default and macOS ships only the Xcode stub that opens an
installer dialog, so "works without extra tools" fails; proxy, CA and TLS settings are git's,
not ours (two configurations to debug); credentials come from helpers we do not control, and
passing our keyring token means environment or `-c http.extraHeader`, which leaks into process
listings; parsing CLI output across git versions. Zero binary cost.

### B. Pure-Rust `gix` (gitoxide)

Clone/fetch in process, depth 1 via `PrepareFetch::with_shallow`, commit id from `head_id()`.
MIT OR Apache-2.0, rustls through reqwest, same reqwest major as ours.

Against: +4.6 MB stripped and +97 crates (measured above) for a feature that moves ~50 KB of
TOML; the high-level clone/fetch API of gix 0.88 exposes shallow fetches but no partial-clone
filter (only the low-level `gix-protocol` arguments know `filter`), so each changed commit costs
a depth-1 pack of the whole repository tree (~9.3 MB for this repository, growing with fonts,
design and docs) unless we drive the protocol by hand; its HTTP transport is a blocking reqwest
client with its own proxy/TLS options, so consistency with `alkahest-fetch` needs extra glue; a
fast-moving 0.x API (two minor lines, 0.84 and 0.88, already in the local registry) adds upkeep
on every bump; SHA-1 object model (verification of content against the commit is possible but
signed-tag verification still needs an OpenPGP/SSH implementation).

### C. HTTPS only: resolve the ref with git smart HTTP, fetch commit-pinned files

1. Resolve `ref` to a commit with a v2 `ls-refs` POST (`ref-prefix refs/heads/<ref>`,
   `refs/tags/<ref>`, with `peel`), 65 bytes on GitHub and GitLab; fall back to the v0
   `info/refs` advertisement on servers without v2. If the commit equals the snapshot's, stop:
   the daily refresh costs one small request.
2. Fetch `<subdir>/index.toml` at that commit through the host's raw URL
   (`raw.githubusercontent.com/<o>/<r>/<commit>/<path>`, GitLab `/-/raw/<commit>/<path>`,
   Gitea/Forgejo `/raw/commit/<commit>/<path>`, or a configured `raw_url` template), then only
   the files whose sha256 differs from the snapshot, each verified against the index.
3. Fallback when a catalog has no `index.toml` or the host has no raw endpoint: the commit
   archive (`codeload.github.com/<o>/<r>/tar.gz/<commit>`, GitLab
   `/-/archive/<commit>/x.tar.gz?path=<subdir>`, Gitea `/archive/<commit>.tar.gz`, or an
   `archive_url` template), streamed through the `flate2` + `tar` code Alkahest already has,
   extracting only `<subdir>/`, checking the pax `comment` equals the commit.

For: zero new dependencies (reqwest, rustls, flate2, tar, sha2 are already linked: ~0 binary
cost, a few hundred lines of code); identical proxy/TLS/user agent/retry behaviour to
`alkahest-fetch`; works on every OS with nothing installed; the token goes in one header we
control, to one host, and is stripped on cross-host redirects; content always addressed by
commit id; the no-change path is the cheapest of all options; the index doubles as an integrity
manifest and a size bound.

Against: host-specific raw/archive URL shapes (three built in, templates for the rest); the
index must be kept current (generated by `alkahest catalog index` / `cargo xtask catalog-check
--fix`, and CI fails on a stale index); content integrity rests on TLS to the host plus the
index digests, not on git object hashes; SSH-only git hosts are not supported.

## Decision

**Option C**, as specified in [[ALK-SP-0011]] R8, R10, R11 and R14: ref resolution over git
smart HTTP (v2 `ls-refs`, v0 fallback), content fetched by commit id as raw files listed in a
generated `index.toml`, with the commit archive as the fallback transport. No git binary and no
git library. `file://` sources (a plain directory) cover air-gapped installs and tests.

Not chosen now but kept open: an optional `transport = "git"` that shells out to a `git` found
on `PATH` for SSH-only hosts; it would be added as its own story only if a user needs it.

### Integrity

- **Commit pinning**: every content URL contains the resolved commit id; the snapshot records
  it; `pin = "<commit>"` on a source freezes it (no refresh). `catalog ls` shows the commit so an
  incident can name exactly which catalog content an installation used.
- **Per-file digests**: `index.toml` carries sha256 and size of every catalog file; downloads
  are verified, oversize is refused (16 MiB per snapshot, 1 MiB per file by default).
- **Downloads reached through the catalog** (engine assets, dependencies, model files) always
  carry a mandatory sha256; `tofu` is refused in git catalogs.
- **Signed tags / signatures**: not in v1. Verifying git tag signatures needs the git objects
  (which option C does not fetch) plus an OpenPGP implementation (`sequoia-openpgp` is
  LGPL-2.0-or-later, banned by `deny.toml`; `pgp`/rPGP is MIT OR Apache-2.0) or SSH signature
  support. If the product owner wants authenticity beyond TLS, the proposed follow-up is a
  detached signature of `index.toml` (`index.toml.sig`, Ed25519 / minisign format) with the
  public key in the source configuration (`public_key = "..."`): it covers every file through
  the index digests, needs one small permissive crate (`ed25519-dalek`, BSD-3-Clause, or
  `minisign-verify`, MIT) and works on any host.

### Private repositories

Token from the OS keyring (service `alkahest`, user `catalog:<name>`; the same `keyring` crate
as the HF token in `alkahest-hf`), or an environment variable for headless hosts. Sent as HTTP
basic auth (`x-access-token:<token>` style for GitHub, `oauth2:<token>` for GitLab) to the
`ls-refs` endpoint and as `Authorization` to raw/archive endpoints of the same host only;
redacted in every error and log line (R14).

## Consequences

- [[ALK-US-0118]] implements the transport in a new module (likely `alkahest-recipes::catalog::sync`
  or a small `alkahest-catalog` crate) on top of the `alkahest-fetch` client; tests use a
  wiremock server speaking `ls-refs` + raw + archive and `file://` fixtures; network tests
  against GitHub behind `ALKAHEST_TEST_NET=1`.
- [[ALK-US-0121]] adds `index.toml` generation and staleness checks to `cargo xtask
  catalog-check`, and CI runs it on `catalog/`.
- The fixture at `crates/alkahest-recipes/tests/fixtures/catalog/` already carries an
  `index.toml`.
- Users of hosts outside GitHub, GitLab and Gitea/Forgejo set `raw_url` / `archive_url`
  templates.
