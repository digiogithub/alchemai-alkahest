//! Engine administration added by ALK-US-0107: job output tails, recipe reload and the
//! per-engine override (executable path, extra arguments).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::admin::encode_segment;
use crate::{Client, Result};

/// The tail of a job's output (`GET /alkahest/v1/jobs/{id}/log`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobLog {
    /// Job id.
    pub id: String,
    /// Last lines, oldest first (the build log file's when the job has one).
    pub lines: Vec<String>,
    /// The build log file, when there is one.
    pub log_path: Option<String>,
}

/// One engine override (`engines.overrides.<id>`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineOverride {
    /// Executable used instead of the recipe-installed one.
    pub path: Option<String>,
    /// Extra command-line arguments.
    pub extra_args: Vec<String>,
}

/// `GET`/`PUT /alkahest/v1/engines/{id}/override`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct EngineOverrideDoc {
    /// Canonical engine id.
    pub engine: String,
    /// The override the running server uses.
    pub running: EngineOverride,
    /// The override saved in the configuration file.
    pub saved: EngineOverride,
    /// Saved differs from running: applies on restart.
    pub restart_required: bool,
}

impl Client {
    /// `GET /alkahest/v1/jobs/{id}/log?lines=`.
    pub async fn job_log(&self, id: &str, lines: Option<usize>) -> Result<JobLog> {
        let q: Vec<(&str, String)> = lines
            .map(|l| ("lines", l.to_string()))
            .into_iter()
            .collect();
        let path = format!("/alkahest/v1/jobs/{}/log", encode_segment(id));
        self.call_json(reqwest::Method::GET, &path, &q, None).await
    }

    /// `POST /alkahest/v1/engines/reload`: re-reads the recipes; the answer lists ignored
    /// recipe files (`issues`).
    pub async fn reload_recipes(&self) -> Result<Value> {
        self.call_json(
            reqwest::Method::POST,
            "/alkahest/v1/engines/reload",
            &[],
            None,
        )
        .await
    }

    /// `GET /alkahest/v1/engines/{id}/override`.
    pub async fn engine_override(&self, id: &str) -> Result<EngineOverrideDoc> {
        let path = format!("/alkahest/v1/engines/{}/override", encode_segment(id));
        self.call_json(reqwest::Method::GET, &path, &[], None).await
    }

    /// `PUT /alkahest/v1/engines/{id}/override`; an empty override removes it.
    pub async fn set_engine_override(
        &self,
        id: &str,
        o: &EngineOverride,
    ) -> Result<EngineOverrideDoc> {
        let path = format!("/alkahest/v1/engines/{}/override", encode_segment(id));
        let body = json!({"path": o.path, "extra_args": o.extra_args});
        self.call_json(reqwest::Method::PUT, &path, &[], Some(&body))
            .await
    }
}
