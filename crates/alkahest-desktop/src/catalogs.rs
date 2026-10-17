//! Git catalog logic of Settings › Catalogs (screen 16b), the sync-failed banner (17.17), the
//! offline notice (17.18) and the add / edit / remove dialogs (17.15, 17.16), free of GPUI
//! (ALK-US-0122, ALK-SP-0011).
//!
//! Data: `GET /alkahest/v1/catalogs` ([`CatalogListing`]), catalog sync jobs (`catalog_sync`,
//! target `catalogs` or `catalog:<name>`, the step as phase `<name>: <step>`) and the
//! `catalog` server event. Catalog actions apply at once (no Save). Tokens are typed into a
//! password field and sent once with `PUT .../token`; nothing here keeps or formats one.

use std::collections::HashSet;

use alkahest_client::admin::Job;
use alkahest_client::{
    CatalogEvent, CatalogInfo, CatalogLimits, CatalogListing, CatalogPatch, NewCatalog,
};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

use crate::catalog_models::short_commit;
use crate::keys::ago;

/// The retry delay of the background refresh after a failed sync (`design/README.md` 17.17).
pub const RETRY_AFTER_ERROR_SECS: i64 = 15 * 60;

/// Job kind of a catalog sync.
pub const SYNC_JOB_KIND: &str = "catalog_sync";

fn parse_time(s: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(s, &Rfc3339).ok()
}

/// `12 min ago` for an RFC 3339 time, `never` without one.
pub fn relative(t: Option<&str>, now: OffsetDateTime) -> String {
    t.and_then(parse_time)
        .map_or_else(|| "never".to_string(), |t| ago(t, now))
}

/// Status pill of a catalog card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    /// Snapshot current (verdigris subtle).
    UpToDate,
    /// A sync job runs (signal blue, progress bar).
    Syncing,
    /// The last sync failed; the previous snapshot stays in use (cinnabar).
    Failed,
    /// Turned off (neutral, card at 72 %).
    Disabled,
    /// Enabled but never fetched (neutral).
    NotSynced,
}

impl Status {
    /// Pill text.
    pub fn label(self) -> &'static str {
        match self {
            Self::UpToDate => "Up to date",
            Self::Syncing => "Syncing\u{2026}",
            Self::Failed => "Sync failed",
            Self::Disabled => "Disabled",
            Self::NotSynced => "Not synced yet",
        }
    }
}

/// The status of `info`, given whether a sync job covers it now.
pub fn status(info: &CatalogInfo, syncing: bool) -> Status {
    if !info.enabled {
        Status::Disabled
    } else if syncing || info.syncing {
        Status::Syncing
    } else if info.last_error.is_some() {
        Status::Failed
    } else if info.commit.is_none() {
        Status::NotSynced
    } else {
        Status::UpToDate
    }
}

/// What a catalog sync job covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SyncScope {
    /// Every enabled catalog ("Sync all").
    All,
    /// One catalog.
    One(String),
}

/// The scope of a job, `None` for other job kinds.
pub fn sync_scope(job: &Job) -> Option<SyncScope> {
    if job.kind != SYNC_JOB_KIND {
        return None;
    }
    match job.target.strip_prefix("catalog:") {
        Some(name) => Some(SyncScope::One(name.to_string())),
        None => Some(SyncScope::All),
    }
}

/// Progress of the sync of one catalog: bar fraction and the step in mono.
#[derive(Debug, Clone, PartialEq)]
pub struct SyncProgress {
    /// `0..=1` while files download.
    pub fraction: Option<f64>,
    /// `downloading changed files 7 / 12`, `waiting`.
    pub step: String,
}

/// The running catalog sync jobs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SyncJobs {
    jobs: Vec<Job>,
}

impl SyncJobs {
    /// Replaces the jobs with the catalog syncs of a job listing.
    pub fn replace_all(&mut self, jobs: &[Job]) {
        self.jobs = jobs
            .iter()
            .filter(|j| sync_scope(j).is_some())
            .cloned()
            .collect();
    }

    /// Records a job event; returns true when a catalog sync just finished (the listing and
    /// the searches should be read again).
    pub fn upsert(&mut self, job: &Job) -> bool {
        if sync_scope(job).is_none() {
            return false;
        }
        match self.jobs.iter_mut().find(|j| j.id == job.id) {
            Some(j) => *j = job.clone(),
            None => self.jobs.push(job.clone()),
        }
        job.is_finished()
    }

    /// Whether any sync runs.
    pub fn any_running(&self) -> bool {
        self.jobs.iter().any(|j| !j.is_finished())
    }

    /// Whether a "Sync all" job runs.
    pub fn all_running(&self) -> bool {
        self.jobs
            .iter()
            .any(|j| !j.is_finished() && sync_scope(j) == Some(SyncScope::All))
    }

    /// The progress of `name` when a running job covers it.
    pub fn progress(&self, name: &str, enabled: bool) -> Option<SyncProgress> {
        let prefix = format!("{name}: ");
        for j in self.jobs.iter().filter(|j| !j.is_finished()) {
            let covers = match sync_scope(j) {
                Some(SyncScope::One(n)) => n == name,
                Some(SyncScope::All) => enabled,
                None => false,
            };
            if !covers {
                continue;
            }
            return Some(match j.phase.strip_prefix(&prefix) {
                Some(step) => SyncProgress {
                    fraction: j.fraction(),
                    step: step.to_string(),
                },
                None if sync_scope(j) == Some(SyncScope::All) && !j.phase.is_empty() => {
                    SyncProgress {
                        fraction: None,
                        step: "waiting for the other catalogs".into(),
                    }
                }
                None => SyncProgress {
                    fraction: None,
                    step: "starting".into(),
                },
            });
        }
        None
    }
}

/// The source line of a card: url, `ref main`, `subdir catalog`, `commit c1c1877b · pinned`.
pub fn source_parts(info: &CatalogInfo) -> Vec<String> {
    let mut out = vec![info.url.clone(), format!("ref {}", info.r#ref)];
    out.push(format!("subdir {}", info.subdir));
    if let Some(pin) = &info.pin {
        out.push(format!("commit {} \u{b7} pinned", short_commit(pin)));
    } else if let Some(c) = &info.commit {
        out.push(format!("commit {}", short_commit(c)));
    }
    out
}

fn counts(info: &CatalogInfo) -> String {
    let n = |v: Option<usize>, one: &str, many: &str| {
        let v = v.unwrap_or(0);
        format!("{v} {}", if v == 1 { one } else { many })
    };
    format!(
        "{} \u{b7} {}",
        n(info.engines, "engine recipe", "engine recipes"),
        n(info.models, "model", "models")
    )
}

/// The facts line: fetched / checked times, counts and the refresh interval.
pub fn facts_line(info: &CatalogInfo, now: OffsetDateTime) -> String {
    let mut times = format!("fetched {}", relative(info.fetched_at.as_deref(), now));
    if info.pin.is_some() {
        times.push_str(" \u{b7} pinned, never refreshed");
    } else if info.last_error.is_some() {
        times.push_str(&format!(
            " \u{b7} last try {}",
            relative(info.error_at.as_deref(), now)
        ));
    } else if info.checked_at.is_some() {
        times.push_str(&format!(
            " \u{b7} checked {}",
            relative(info.checked_at.as_deref(), now)
        ));
    }
    let refresh = if info.pin.is_some() || info.refresh_interval.is_empty() {
        "\u{2013}".to_string()
    } else {
        info.refresh_interval.clone()
    };
    format!("{times}    {}    refresh {refresh}", counts(info))
}

/// Minutes until the next automatic try after a failure (`None` when unknown or due).
pub fn next_try_minutes(
    error_at: Option<&str>,
    retry_after_secs: Option<u64>,
    now: OffsetDateTime,
) -> Option<i64> {
    let at = error_at.and_then(parse_time)?;
    let wait = retry_after_secs.map_or(RETRY_AFTER_ERROR_SECS, |s| s as i64);
    let left = (at.unix_timestamp() + wait) - now.unix_timestamp();
    (left > 0).then(|| left.div_euclid(60).max(1))
}

/// The note under a failed sync: the snapshot still in use and the next try.
pub fn failure_note(info: &CatalogInfo, now: OffsetDateTime) -> String {
    let snapshot = match &info.commit {
        Some(c) => format!("The previous snapshot {} stays in use.", short_commit(c)),
        None => "There is no snapshot yet.".to_string(),
    };
    if info.pin.is_some() || !info.enabled {
        return snapshot;
    }
    match next_try_minutes(info.error_at.as_deref(), None, now) {
        Some(m) => format!("{snapshot} Next try in {m} min."),
        None => format!("{snapshot} The next background refresh tries again."),
    }
}

/// The token chip of a card, when the catalog has (or had) a token.
pub fn token_chip(info: &CatalogInfo) -> Option<&'static str> {
    match info.token_source.as_str() {
        "keyring" => Some("private \u{b7} token in keyring"),
        "env" => Some("private \u{b7} token from environment"),
        "mismatch" => Some("token belongs to another host \u{b7} not used"),
        _ if info.token_present => Some("private \u{b7} token set"),
        _ => None,
    }
}

/// Enabled catalogs whose last sync failed (the Settings tab dot, the Discover pill).
pub fn failed(listing: &CatalogListing) -> Vec<&CatalogInfo> {
    listing
        .catalogs
        .iter()
        .filter(|c| c.enabled && c.last_error.is_some())
        .collect()
}

/// Enabled catalogs.
pub fn enabled_count(listing: &CatalogListing) -> usize {
    listing.catalogs.iter().filter(|c| c.enabled).count()
}

/// The Discover header pill: `3 catalogs` and, when one failed, `lab sync failed`.
pub fn discover_pill(listing: &CatalogListing) -> (String, Option<String>) {
    let n = enabled_count(listing);
    let count = format!("{n} catalog{}", if n == 1 { "" } else { "s" });
    let bad = failed(listing);
    let failure = match bad.as_slice() {
        [] => None,
        [one] => Some(format!("{} sync failed", one.name)),
        many => Some(format!("{} syncs failed", many.len())),
    };
    (count, failure)
}

/// The recipe layers from lowest to highest, each with "disabled" when it contributes nothing.
pub fn recipe_layers(listing: &CatalogListing) -> Vec<(String, bool)> {
    let mut out = vec![("built-in".to_string(), false)];
    for c in &listing.catalogs {
        out.push((format!("catalog {}", c.name), !c.enabled));
    }
    out.push(("recipes/ beside the binary".into(), false));
    out.push(("user recipes directory".into(), false));
    out
}

/// Enabled catalogs with no snapshot yet (17.18: offline or never fetched).
pub fn never_fetched(listing: &CatalogListing) -> Vec<&CatalogInfo> {
    listing
        .catalogs
        .iter()
        .filter(|c| c.enabled && c.commit.is_none())
        .collect()
}

/// The Engines info strip of 17.18, when an enabled catalog has no snapshot yet.
pub fn engines_offline_note(listing: &CatalogListing) -> Option<String> {
    let missing = never_fetched(listing);
    let names: Vec<&str> = missing.iter().map(|c| c.name.as_str()).collect();
    match names.as_slice() {
        [] => None,
        [one] => Some(format!(
            "Engines use the built-in recipes. Catalog {one} has not been fetched yet; newer \
             builds may be missing."
        )),
        many => Some(format!(
            "Engines use the built-in recipes. Catalogs {} have not been fetched yet; newer \
             builds may be missing.",
            many.join(", ")
        )),
    }
}

/// The sync-failed banner (17.17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Banner {
    /// Catalog name.
    pub catalog: String,
    /// The error, in cinnabar mono.
    pub error: String,
    /// When it failed.
    pub error_at: Option<String>,
    /// Seconds to the next automatic try.
    pub retry_after_secs: Option<u64>,
    /// Whether the error is an authentication failure ("Set token…").
    pub auth: bool,
}

impl Banner {
    /// `Catalog acme could not be synced`.
    pub fn title(&self) -> String {
        format!("Catalog {} could not be synced", self.catalog)
    }

    /// `Showing the last good snapshot 3f9a02e1 from 2 days ago. Next try in 15 min.`
    pub fn detail(&self, info: Option<&CatalogInfo>, now: OffsetDateTime) -> String {
        let snapshot = match info.and_then(|i| i.commit.as_ref()) {
            Some(c) => format!(
                "Showing the last good snapshot {} from {}.",
                short_commit(c),
                relative(info.and_then(|i| i.fetched_at.as_deref()), now)
            ),
            None => "No snapshot of it is available yet.".to_string(),
        };
        match next_try_minutes(self.error_at.as_deref(), self.retry_after_secs, now) {
            Some(m) => format!("{snapshot} Next try in {m} min."),
            None => snapshot,
        }
    }
}

/// Whether an error reads like a refused authentication (401 / 403).
pub fn is_auth_error(error: &str) -> bool {
    let e = error.to_ascii_lowercase();
    e.contains("401") || e.contains("403") || e.contains("unauthorized") || e.contains("forbidden")
}

/// Shows each new sync failure once (17.17): from a `catalog` event, or from a listing for an
/// error no command reported yet. Dismissed errors stay dismissed for the session.
#[derive(Debug, Clone, Default)]
pub struct BannerState {
    seen: HashSet<(String, String)>,
    /// The banner on screen.
    pub current: Option<Banner>,
}

impl BannerState {
    fn offer(&mut self, b: Banner) {
        let key = (
            b.catalog.clone(),
            b.error_at.clone().unwrap_or_else(|| b.error.clone()),
        );
        if self.seen.insert(key) {
            self.current = Some(b);
        }
    }

    /// A `catalog` event (`sync_failed`).
    pub fn on_event(&mut self, ev: &CatalogEvent) {
        if ev.kind != "sync_failed" {
            return;
        }
        self.offer(Banner {
            catalog: ev.catalog.clone(),
            error: ev.error.clone(),
            error_at: ev.error_at.clone(),
            retry_after_secs: Some(ev.retry_after_secs),
            auth: is_auth_error(&ev.error),
        });
    }

    /// A fresh listing: offers unreported failures and drops a banner whose catalog recovered.
    pub fn on_listing(&mut self, listing: &CatalogListing) {
        if let Some(b) = &self.current {
            let still = listing
                .catalogs
                .iter()
                .any(|c| c.name == b.catalog && c.enabled && c.last_error.is_some());
            if !still {
                self.current = None;
            }
        }
        for c in failed(listing) {
            if c.error_reported {
                continue;
            }
            if let Some(e) = &c.last_error {
                self.offer(Banner {
                    catalog: c.name.clone(),
                    error: e.clone(),
                    error_at: c.error_at.clone(),
                    retry_after_secs: None,
                    auth: is_auth_error(e),
                });
            }
        }
    }

    /// Hides the banner; the error stays on the card and the tab dot.
    pub fn dismiss(&mut self) {
        self.current = None;
    }
}

/// Fields of the Add / Edit catalog dialogs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    /// Name.
    Name,
    /// Repository URL.
    Url,
    /// Branch or tag.
    Ref,
    /// Folder inside the repository.
    Subdir,
    /// Refresh interval.
    Interval,
    /// Pin to commit.
    Pin,
    /// Access token.
    Token,
}

/// A validation error under one field (or the dialog, when `field` is `None`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldError {
    /// Field the message belongs under.
    pub field: Option<Field>,
    /// Message.
    pub message: String,
}

impl FieldError {
    fn at(field: Field, message: impl Into<String>) -> Self {
        Self {
            field: Some(field),
            message: message.into(),
        }
    }
}

/// Which field a server validation message is about (best effort; `None` = the dialog).
pub fn field_of_error(message: &str) -> Option<Field> {
    let m = message.to_ascii_lowercase();
    let has = |w: &str| m.contains(w);
    if has("token") {
        Some(Field::Token)
    } else if has("pin") || has("commit") {
        Some(Field::Pin)
    } else if has("refresh") || has("interval") {
        Some(Field::Interval)
    } else if has("subdir") {
        Some(Field::Subdir)
    } else if has("`ref`") || has(" ref ") || has("ref:") || has("branch") || has("tag ") {
        Some(Field::Ref)
    } else if has("url") || has("https") || has("file://") || has("credential") {
        Some(Field::Url)
    } else if has("name") {
        Some(Field::Name)
    } else {
        None
    }
}

/// Whether `name` is a valid catalog name (lower-case letters, digits, `-`, `_`, `.`).
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
}

fn check_url(url: &str) -> Result<(), FieldError> {
    if let Some(rest) = url.strip_prefix("https://") {
        let authority = rest.split('/').next().unwrap_or_default();
        if authority.is_empty() {
            return Err(FieldError::at(Field::Url, "The URL needs a host."));
        }
        if authority.contains('@') {
            return Err(FieldError::at(
                Field::Url,
                "No credentials in the URL: use the access token field.",
            ));
        }
        Ok(())
    } else if url.starts_with("file://") {
        Ok(())
    } else {
        Err(FieldError::at(
            Field::Url,
            "Use an https:// repository URL, or file:// for a folder on the server machine.",
        ))
    }
}

fn check_pin(pin: &str) -> Result<Option<String>, FieldError> {
    let pin = pin.trim().to_ascii_lowercase();
    if pin.is_empty() {
        return Ok(None);
    }
    if pin.len() == 40 && pin.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(Some(pin))
    } else {
        Err(FieldError::at(
            Field::Pin,
            "A pin is a full 40-character commit id.",
        ))
    }
}

/// Whether `i` is a refresh interval the server accepts: a number and `s`, `m`, `h` or `d`
/// (`24h`, `30m`, `7d`), not zero.
pub fn valid_interval(i: &str) -> bool {
    let t = i.trim();
    let Some(split) = t.find(|c: char| !c.is_ascii_digit()) else {
        return false;
    };
    let (num, unit) = t.split_at(split);
    matches!(unit, "s" | "m" | "h" | "d") && num.parse::<u64>().is_ok_and(|n| n > 0)
}

fn check_interval(i: &str) -> Result<(), FieldError> {
    if valid_interval(i) {
        Ok(())
    } else {
        Err(FieldError::at(
            Field::Interval,
            "Use a number and s, m, h or d (24h, 30m, 7d).",
        ))
    }
}

/// The Add catalog form (17.15) without the token, which lives only in its password field.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AddForm {
    /// Name.
    pub name: String,
    /// Repository URL.
    pub url: String,
    /// Branch or tag (blank = `main`).
    pub r#ref: String,
    /// Folder inside the repository (blank = `.`).
    pub subdir: String,
    /// Refresh interval (blank = `24h`).
    pub interval: String,
    /// Full commit id (optional).
    pub pin: String,
}

impl AddForm {
    /// The request, or the first field in error. `replace_token` answers a 409 about a token
    /// of another host. The sync is started by the caller once the token is stored.
    pub fn request(&self, replace_token: bool) -> Result<NewCatalog, FieldError> {
        let name = self.name.trim();
        if !valid_name(name) {
            return Err(FieldError::at(
                Field::Name,
                "Lower-case letters, digits, - _ . (starting with a letter or digit).",
            ));
        }
        let url = self.url.trim();
        check_url(url)?;
        let interval = self.interval.trim();
        if !interval.is_empty() {
            check_interval(interval)?;
        }
        let pin = check_pin(&self.pin)?;
        let opt = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
        Ok(NewCatalog {
            name: name.to_string(),
            url: url.to_string(),
            r#ref: opt(&self.r#ref),
            subdir: opt(&self.subdir),
            pin,
            refresh_interval: opt(interval),
            enabled: None,
            sync: false,
            replace_token,
        })
    }
}

/// The Edit catalog form: ref, subdir, pin and refresh interval of one catalog.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditForm {
    /// Branch or tag.
    pub r#ref: String,
    /// Folder inside the repository.
    pub subdir: String,
    /// Refresh interval.
    pub interval: String,
    /// Full commit id; blank unpins.
    pub pin: String,
}

impl EditForm {
    /// The form filled from a catalog.
    pub fn from_info(info: &CatalogInfo) -> Self {
        Self {
            r#ref: info.r#ref.clone(),
            subdir: info.subdir.clone(),
            interval: info.refresh_interval.clone(),
            pin: info.pin.clone().unwrap_or_default(),
        }
    }

    /// The patch of the changed fields (empty when nothing changed).
    pub fn patch(&self, info: &CatalogInfo) -> Result<CatalogPatch, FieldError> {
        let mut p = CatalogPatch::default();
        let r = self.r#ref.trim();
        if r.is_empty() {
            return Err(FieldError::at(Field::Ref, "A branch or tag is required."));
        }
        if r != info.r#ref {
            p.r#ref = Some(r.to_string());
        }
        let s = self.subdir.trim();
        let s = if s.is_empty() { "." } else { s };
        if s != info.subdir {
            p.subdir = Some(s.to_string());
        }
        let i = self.interval.trim();
        if i != info.refresh_interval {
            check_interval(i)?;
            p.refresh_interval = Some(i.to_string());
        }
        let pin = check_pin(&self.pin)?;
        if pin != info.pin {
            p.pin = Some(pin);
        }
        Ok(p)
    }
}

/// Whether a change needs a sync to take effect (the server does not sync after a PATCH).
pub fn patch_needs_sync(p: &CatalogPatch) -> bool {
    p.r#ref.is_some() || p.subdir.is_some() || p.pin.is_some() || p.enabled == Some(true)
}

/// The Remove catalog confirmation (17.16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoveAsk {
    /// Catalog name.
    pub name: String,
    /// The object's facts in mono: url, ref and counts.
    pub facts: String,
    /// A token is stored for it (offer "also delete the token").
    pub has_token: bool,
    /// The user ticked "also delete the token".
    pub delete_token: bool,
    /// The catalog is enabled ("Disable instead" is offered).
    pub enabled: bool,
}

impl RemoveAsk {
    /// The question for `info`.
    pub fn new(info: &CatalogInfo) -> Self {
        let url = info
            .url
            .trim_start_matches("https://")
            .trim_start_matches("file://");
        Self {
            name: info.name.clone(),
            facts: format!("{url} \u{b7} {} \u{b7} {}", info.r#ref, counts(info)),
            has_token: info.token_present && info.token_source == "keyring",
            delete_token: false,
            enabled: info.enabled,
        }
    }

    /// The consequences.
    pub fn consequence(&self) -> String {
        let token = if !self.has_token {
            ""
        } else if self.delete_token {
            " Its keyring token is deleted too."
        } else {
            " The keyring token is left in place."
        };
        format!(
            "Its entry leaves config.toml and its snapshot is deleted. Models already pulled from \
             it stay in the Library; installed engine builds stay.{token}"
        )
    }
}

const KIB: u64 = 1024;

/// `16 MiB`, `1 MiB`, `256 MiB`, `1.5 GiB`, `512 KiB`, `100 B` (binary units, as the config
/// documents the limits).
pub fn format_iec(bytes: u64) -> String {
    for (unit, scale) in [("GiB", KIB * KIB * KIB), ("MiB", KIB * KIB), ("KiB", KIB)] {
        if bytes >= scale {
            let v = bytes as f64 / scale as f64;
            return if bytes.is_multiple_of(scale) {
                format!("{} {unit}", bytes / scale)
            } else {
                format!("{v:.1} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

/// Parses `16 MiB`, `16MiB`, `16 MB`, `1.5 GiB`, `1048576` into bytes.
pub fn parse_size(text: &str) -> Result<u64, String> {
    let t = text.trim();
    let split = t
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(t.len());
    let (num, unit) = t.split_at(split);
    let n: f64 = num
        .parse()
        .map_err(|_| format!("`{t}` is not a size (16 MiB, 1048576)"))?;
    let scale = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1,
        "k" | "kb" => 1000,
        "kib" => KIB,
        "m" | "mb" => 1_000_000,
        "mib" => KIB * KIB,
        "g" | "gb" => 1_000_000_000,
        "gib" => KIB * KIB * KIB,
        other => return Err(format!("unknown unit `{other}` (B, KiB, MiB, GiB)")),
    };
    let v = n * scale as f64;
    if !(v.is_finite() && v >= 1.0) {
        return Err("a limit must be at least 1 byte".into());
    }
    Ok(v.round() as u64)
}

/// One limit change for `set_catalog_limits`: `None` = unchanged, `Some(None)` = reset to the
/// default, `Some(Some(n))` = set.
pub type LimitChange = Option<Option<u64>>;

/// The limit changes of the three fields (`max_snapshot_bytes`, `max_file_bytes`,
/// `max_archive_bytes`); a blank field resets to the default. Errors name the field.
pub fn limits_patch(
    texts: [&str; 3],
    current: &CatalogLimits,
    defaults: &CatalogLimits,
) -> Result<[LimitChange; 3], (usize, String)> {
    let cur = [
        current.max_snapshot_bytes,
        current.max_file_bytes,
        current.max_archive_bytes,
    ];
    let def = [
        defaults.max_snapshot_bytes,
        defaults.max_file_bytes,
        defaults.max_archive_bytes,
    ];
    let mut out = [None, None, None];
    for i in 0..3 {
        let t = texts[i].trim();
        if t.is_empty() {
            if cur[i] != def[i] {
                out[i] = Some(None);
            }
            continue;
        }
        let v = parse_size(t).map_err(|e| (i, e))?;
        if v != cur[i] {
            out[i] = Some(Some(v));
        }
    }
    Ok(out)
}

/// The limit field keys, in screen order.
pub const LIMIT_KEYS: [&str; 3] = ["max_snapshot_bytes", "max_file_bytes", "max_archive_bytes"];

/// The limit a server error message is about (`catalog_limits.max_file_bytes ...`).
pub fn limit_of_error(message: &str) -> Option<usize> {
    LIMIT_KEYS.iter().position(|k| message.contains(k))
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn now() -> OffsetDateTime {
        OffsetDateTime::parse("2026-10-17T12:00:00Z", &Rfc3339).unwrap()
    }

    fn info(name: &str) -> CatalogInfo {
        CatalogInfo {
            position: 1,
            name: name.into(),
            url: "https://github.com/digiogithub/alchemai-alkahest".into(),
            r#ref: "main".into(),
            subdir: "catalog".into(),
            enabled: true,
            refresh_interval: "24h".into(),
            commit: Some("c1c1877b55aa".into()),
            fetched_at: Some("2026-10-17T11:48:00Z".into()),
            checked_at: Some("2026-10-17T11:58:00Z".into()),
            engines: Some(5),
            models: Some(1),
            token_source: "none".into(),
            ..CatalogInfo::default()
        }
    }

    fn job(target: &str, state: &str, phase: &str) -> Job {
        Job {
            id: target.into(),
            kind: SYNC_JOB_KIND.into(),
            target: target.into(),
            state: state.into(),
            phase: phase.into(),
            completed: 7,
            total: Some(12),
            ..Job::default()
        }
    }

    #[test]
    fn status_pills() {
        let mut i = info("official");
        assert_eq!(status(&i, false), Status::UpToDate);
        assert_eq!(status(&i, true), Status::Syncing);
        i.last_error = Some("401".into());
        assert_eq!(status(&i, false), Status::Failed);
        i.enabled = false;
        assert_eq!(status(&i, true), Status::Disabled);
        let mut n = info("new");
        n.commit = None;
        assert_eq!(status(&n, false), Status::NotSynced);
    }

    #[test]
    fn card_lines() {
        let i = info("official");
        assert_eq!(
            source_parts(&i),
            [
                "https://github.com/digiogithub/alchemai-alkahest",
                "ref main",
                "subdir catalog",
                "commit c1c1877b"
            ]
        );
        assert_eq!(
            facts_line(&i, now()),
            "fetched 12 min ago \u{b7} checked 2 min ago    5 engine recipes \u{b7} 1 model    refresh 24h"
        );
        let mut p = i.clone();
        p.pin = Some("7b2c9d40aaaa".into());
        assert!(source_parts(&p)[3].ends_with("pinned"));
        assert!(facts_line(&p, now()).contains("pinned, never refreshed"));
        assert!(facts_line(&p, now()).ends_with("refresh \u{2013}"));
        let mut f = i.clone();
        f.last_error = Some("boom".into());
        f.error_at = Some("2026-10-17T11:50:00Z".into());
        assert_eq!(
            failure_note(&f, now()),
            "The previous snapshot c1c1877b stays in use. Next try in 5 min."
        );
    }

    #[test]
    fn sync_jobs_follow_scope_and_step() {
        let mut s = SyncJobs::default();
        assert!(!s.upsert(&Job {
            kind: "pull".into(),
            ..Job::default()
        }));
        assert!(!s.upsert(&job(
            "catalog:acme",
            "running",
            "acme: downloading changed files 7 / 12"
        )));
        let p = s.progress("acme", true).unwrap();
        assert_eq!(p.step, "downloading changed files 7 / 12");
        assert!((p.fraction.unwrap() - 7.0 / 12.0).abs() < 1e-9);
        assert!(s.progress("official", true).is_none());
        assert!(!s.upsert(&job("catalogs", "running", "official: validating")));
        assert!(s.all_running());
        assert_eq!(s.progress("official", true).unwrap().step, "validating");
        assert_eq!(
            s.progress("lab", true).unwrap().step,
            "waiting for the other catalogs"
        );
        assert!(
            s.progress("lab", false).is_none(),
            "disabled catalogs are skipped"
        );
        assert!(s.upsert(&job("catalog:acme", "completed", "")));
        assert!(s.any_running());
        assert!(s.upsert(&job("catalogs", "failed", "")));
        assert!(!s.any_running());
    }

    #[test]
    fn pills_layers_and_offline() {
        let mut lab = info("lab");
        lab.last_error = Some("sha256 mismatch".into());
        let mut mirror = info("mirror");
        mirror.enabled = false;
        let listing = CatalogListing {
            catalogs: vec![info("official"), lab, mirror],
            ..CatalogListing::default()
        };
        assert_eq!(
            discover_pill(&listing),
            ("2 catalogs".into(), Some("lab sync failed".into()))
        );
        let layers = recipe_layers(&listing);
        assert_eq!(layers[0], ("built-in".into(), false));
        assert_eq!(layers[3], ("catalog mirror".into(), true));
        assert_eq!(layers.len(), 6);
        assert!(engines_offline_note(&listing).is_none());
        let mut fresh = info("official");
        fresh.commit = None;
        let l2 = CatalogListing {
            catalogs: vec![fresh],
            ..CatalogListing::default()
        };
        assert!(
            engines_offline_note(&l2)
                .unwrap()
                .contains("Catalog official has not")
        );
    }

    #[test]
    fn banner_once_per_error() {
        let mut b = BannerState::default();
        let ev = CatalogEvent {
            kind: "sync_failed".into(),
            catalog: "acme".into(),
            error: "401 Unauthorized \u{b7} git.acme.internal ls-refs v2.3".into(),
            error_at: Some("2026-10-17T11:55:00Z".into()),
            retry_after_secs: 900,
            snapshot_in_use: true,
        };
        b.on_event(&ev);
        let shown = b.current.clone().unwrap();
        assert!(shown.auth);
        assert_eq!(shown.title(), "Catalog acme could not be synced");
        let mut i = info("acme");
        i.commit = Some("3f9a02e1ffff".into());
        i.fetched_at = Some("2026-10-15T12:00:00Z".into());
        assert_eq!(
            shown.detail(Some(&i), now()),
            "Showing the last good snapshot 3f9a02e1 from 2 d ago. Next try in 10 min."
        );
        b.dismiss();
        b.on_event(&ev);
        assert!(b.current.is_none(), "the same error is shown once");
        // The listing reports it unreported: still the same error, still dismissed.
        i.last_error = Some(ev.error.clone());
        i.error_at = ev.error_at.clone();
        let listing = CatalogListing {
            catalogs: vec![i.clone()],
            ..CatalogListing::default()
        };
        b.on_listing(&listing);
        assert!(b.current.is_none());
        // A new failure shows again; a recovery hides it.
        i.error_at = Some("2026-10-17T11:59:00Z".into());
        let listing = CatalogListing {
            catalogs: vec![i.clone()],
            ..CatalogListing::default()
        };
        b.on_listing(&listing);
        assert!(b.current.is_some());
        i.last_error = None;
        b.on_listing(&CatalogListing {
            catalogs: vec![i],
            ..CatalogListing::default()
        });
        assert!(b.current.is_none());
    }

    #[test]
    fn add_form_validates() {
        let mut f = AddForm {
            name: "acme".into(),
            url: "https://git.acme.internal/ml/alkahest-catalog".into(),
            r#ref: "v2.3".into(),
            subdir: "catalog".into(),
            interval: "24h".into(),
            pin: String::new(),
        };
        let r = f.request(false).unwrap();
        assert_eq!(r.r#ref.as_deref(), Some("v2.3"));
        assert!(r.pin.is_none());
        assert!(!r.sync, "the sync starts after the token is stored");
        assert!(f.request(true).unwrap().replace_token);
        f.name = "Acme".into();
        assert_eq!(f.request(false).unwrap_err().field, Some(Field::Name));
        f.name = "acme".into();
        f.url = "https://user:pw@git.acme.internal/x".into();
        assert_eq!(f.request(false).unwrap_err().field, Some(Field::Url));
        f.url = "git@github.com:x/y".into();
        assert_eq!(f.request(false).unwrap_err().field, Some(Field::Url));
        f.url = "file:///srv/catalog".into();
        f.pin = "abc".into();
        assert_eq!(f.request(false).unwrap_err().field, Some(Field::Pin));
        f.pin = "A".repeat(40);
        assert_eq!(f.request(false).unwrap().pin, Some("a".repeat(40)));
        f.interval = "soon".into();
        assert_eq!(f.request(false).unwrap_err().field, Some(Field::Interval));
    }

    #[test]
    fn server_errors_land_under_their_field() {
        assert_eq!(
            field_of_error("invalid catalog name `Bad`"),
            Some(Field::Name)
        );
        assert_eq!(
            field_of_error("catalog url must not carry credentials"),
            Some(Field::Url)
        );
        assert_eq!(
            field_of_error("pin must be a full commit id"),
            Some(Field::Pin)
        );
        assert_eq!(
            field_of_error("a token stored for catalog `acme` belongs to another host"),
            Some(Field::Token)
        );
        assert_eq!(field_of_error("disk full"), None);
    }

    #[test]
    fn edit_patch_only_changed_fields() {
        let i = info("acme");
        let mut f = EditForm::from_info(&i);
        assert_eq!(f.patch(&i).unwrap(), CatalogPatch::default());
        f.r#ref = "v2.4".into();
        f.pin = "b".repeat(40);
        let p = f.patch(&i).unwrap();
        assert_eq!(p.r#ref.as_deref(), Some("v2.4"));
        assert_eq!(p.pin, Some(Some("b".repeat(40))));
        assert!(p.subdir.is_none());
        assert!(patch_needs_sync(&p));
        let mut pinned = i.clone();
        pinned.pin = Some("c".repeat(40));
        let mut g = EditForm::from_info(&pinned);
        g.pin.clear();
        assert_eq!(g.patch(&pinned).unwrap().pin, Some(None), "blank unpins");
        g.r#ref.clear();
        assert_eq!(g.patch(&pinned).unwrap_err().field, Some(Field::Ref));
    }

    #[test]
    fn remove_asks_about_the_token() {
        let mut i = info("acme");
        i.url = "https://git.acme.internal/ml/alkahest-catalog".into();
        i.r#ref = "v2.3".into();
        i.engines = Some(2);
        i.models = Some(3);
        i.token_present = true;
        i.token_source = "keyring".into();
        let mut ask = RemoveAsk::new(&i);
        assert_eq!(
            ask.facts,
            "git.acme.internal/ml/alkahest-catalog \u{b7} v2.3 \u{b7} 2 engine recipes \u{b7} 3 models"
        );
        assert!(ask.has_token);
        assert!(
            ask.consequence()
                .ends_with("The keyring token is left in place.")
        );
        ask.delete_token = true;
        assert!(
            ask.consequence()
                .ends_with("Its keyring token is deleted too.")
        );
        i.token_source = "env".into();
        assert!(
            !RemoveAsk::new(&i).has_token,
            "an environment token is not ours to delete"
        );
    }

    #[test]
    fn sizes_and_limits() {
        assert_eq!(format_iec(16 * 1024 * 1024), "16 MiB");
        assert_eq!(format_iec(1536), "1.5 KiB");
        assert_eq!(format_iec(10), "10 B");
        assert_eq!(parse_size("16 MiB").unwrap(), 16 * 1024 * 1024);
        assert_eq!(parse_size("1MB").unwrap(), 1_000_000);
        assert_eq!(parse_size("1.5 GiB").unwrap(), 1_610_612_736);
        assert_eq!(parse_size("2048").unwrap(), 2048);
        assert!(parse_size("lots").is_err());
        assert!(parse_size("3 parsecs").is_err());
        let def = CatalogLimits {
            max_snapshot_bytes: 16 << 20,
            max_file_bytes: 1 << 20,
            max_archive_bytes: 256 << 20,
        };
        let cur = CatalogLimits {
            max_file_bytes: 2 << 20,
            ..def
        };
        let p = limits_patch(["16 MiB", "", "512 MiB"], &cur, &def).unwrap();
        assert_eq!(p, [None, Some(None), Some(Some(512 << 20))]);
        assert_eq!(limits_patch(["x", "", ""], &cur, &def).unwrap_err().0, 0);
        assert_eq!(
            limit_of_error("catalog_limits.max_archive_bytes exceeds 4 GiB"),
            Some(2)
        );
    }
}
