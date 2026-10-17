//! Model actions the Library and Running screens send to the server (load, unload, delete,
//! unload all), free of GPUI.
//!
//! [`ModelOps`] is what the screens need; [`alkahest_client::Client`] implements it over HTTP
//! and the tests use a fake. [`run`] turns a client error into the one-line message the
//! screen shows ([`OpFailure`]).

use std::future::Future;

use alkahest_client::admin::CatalogModel;
use alkahest_client::{Client, ClientError, LibraryModel};

use crate::library::read_only_hint;

/// A model action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelOp {
    /// Load (warm) a model with the server's default keep-alive.
    Load(String),
    /// Unload a model now.
    Unload(String),
    /// Delete a model from the Alkahest store.
    Delete(String),
    /// Kill the model's engine at once (`POST /alkahest/v1/models/{name}/kill`).
    Kill(String),
    /// Unload every model.
    UnloadAll,
    /// Unload every model, then load this one (retry of a load that did not fit in memory).
    UnloadAllThenLoad(String),
}

/// Why an action failed, when the screen can offer a way out (design 17.8).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FailureCause {
    /// Nothing specific.
    #[default]
    Other,
    /// The model does not fit in the free memory.
    Memory,
    /// The engine that serves the model is not installed (the id).
    EngineMissing(String),
}

/// A failed action, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpFailure {
    /// One-line explanation.
    pub message: String,
    /// What went wrong, for the recovery actions.
    pub cause: FailureCause,
}

/// Classifies a failed load from the server's error text (`ErrorKind` has no code for these
/// yet; the sentences are stable, see `alkahest-sched` `InsufficientMemory` and
/// `alkahest-engine` `NotInstalled`).
pub fn classify_load_failure(err: &ClientError) -> FailureCause {
    let ClientError::Api { message, .. } = err else {
        return FailureCause::Other;
    };
    if message.contains("does not fit in memory") {
        return FailureCause::Memory;
    }
    if message.contains("is not installed")
        && let Some(rest) = message.split("engine `").nth(1)
        && let Some(id) = rest.split('`').next().filter(|id| !id.is_empty())
    {
        return FailureCause::EngineMissing(id.to_string());
    }
    FailureCause::Other
}

/// What the screens need from a server.
pub trait ModelOps: Send + Sync + 'static {
    /// Loads `model` (`/api/generate` without a prompt); resolves once it is ready.
    fn load(&self, model: &str) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// Unloads `model` (`keep_alive: 0`).
    fn unload(&self, model: &str) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// Deletes `model` (`DELETE /api/delete`).
    fn delete(&self, model: &str) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// Kills the engine of `model` at once (`POST /alkahest/v1/models/{name}/kill`).
    fn kill(&self, model: &str) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// Unloads everything (`POST /alkahest/v1/unload`).
    fn unload_all(&self) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// The model list: the admin catalog (`GET /alkahest/v1/models`, one call), else the
    /// list composed from the Ollama-compatible routes ([`Client::library`]).
    fn library(&self) -> impl Future<Output = Result<Vec<LibraryModel>, ClientError>> + Send;
}

impl ModelOps for Client {
    async fn load(&self, model: &str) -> Result<(), ClientError> {
        Client::load(self, model, None).await
    }

    async fn unload(&self, model: &str) -> Result<(), ClientError> {
        self.stop(model).await
    }

    async fn delete(&self, model: &str) -> Result<(), ClientError> {
        Client::delete(self, model).await
    }

    async fn kill(&self, model: &str) -> Result<(), ClientError> {
        self.kill_model(model).await
    }

    async fn unload_all(&self) -> Result<(), ClientError> {
        Client::unload_all(self).await
    }

    async fn library(&self) -> Result<Vec<LibraryModel>, ClientError> {
        match self.models().await {
            Ok(catalog) => Ok(catalog.models.iter().map(library_model).collect()),
            // A server without the admin catalog route (older build) or a key without the
            // admin scope: compose the list from the public routes instead.
            Err(ClientError::Api {
                status: 401 | 403 | 404 | 405,
                ..
            }) => Client::library(self).await,
            Err(e) => Err(e),
        }
    }
}

/// The Library row of an admin catalog entry. The catalog knows source, kind, engine and
/// loadability for every model, so nothing stays unknown.
pub fn library_model(m: &CatalogModel) -> LibraryModel {
    let opt = |s: &str| (!s.is_empty()).then(|| s.to_string());
    LibraryModel {
        name: m.name.clone(),
        size: m.size,
        digest: m.digest.clone(),
        modified: m.modified.clone(),
        format: m.format.clone(),
        quantization: m.quantization.clone(),
        family: m.family.clone(),
        parameters: m.parameters.clone(),
        capabilities: m.capabilities.clone(),
        source: opt(&m.source),
        kind: opt(&m.kind),
        engine: opt(&m.engine),
        context_length: m.context_length,
        loadable: Some(m.loadable),
    }
}

/// Runs `op` and explains a failure.
pub async fn run<O: ModelOps>(ops: &O, op: &ModelOp) -> Result<(), OpFailure> {
    let result = match op {
        ModelOp::Load(m) => ops.load(m).await,
        ModelOp::Unload(m) => ops.unload(m).await,
        ModelOp::Delete(m) => ops.delete(m).await,
        ModelOp::Kill(m) => ops.kill(m).await,
        ModelOp::UnloadAll => ops.unload_all().await,
        ModelOp::UnloadAllThenLoad(m) => match ops.unload_all().await {
            Ok(()) => ops.load(m).await,
            Err(e) => Err(e),
        },
    };
    result.map_err(|e| failure(op, &e))
}

/// The message for `op` failing with `err`.
pub fn failure(op: &ModelOp, err: &ClientError) -> OpFailure {
    let message = match (op, err) {
        // The server refuses to touch the Ollama store (the UI normally refuses first, but the
        // source may have been unknown).
        (ModelOp::Delete(m), ClientError::Api { status: 403, .. }) => read_only_hint(m),
        (
            ModelOp::Kill(_),
            ClientError::Api {
                status: 401 | 403, ..
            },
        ) => "Killing an engine needs an API key with the admin scope.".to_string(),
        (
            ModelOp::UnloadAll,
            ClientError::Api {
                status: 401 | 403, ..
            },
        ) => "Unloading everything needs an API key with the admin scope.".to_string(),
        (op, err) => format!("{} failed: {err}", describe(op)),
    };
    let cause = match op {
        ModelOp::Load(_) | ModelOp::UnloadAllThenLoad(_) => classify_load_failure(err),
        _ => FailureCause::Other,
    };
    OpFailure { message, cause }
}

fn describe(op: &ModelOp) -> String {
    match op {
        ModelOp::Load(m) => format!("Loading {m}"),
        ModelOp::Unload(m) => format!("Unloading {m}"),
        ModelOp::Delete(m) => format!("Deleting {m}"),
        ModelOp::Kill(m) => format!("Killing {m}"),
        ModelOp::UnloadAll => "Unloading all models".to_string(),
        ModelOp::UnloadAllThenLoad(m) => format!("Unloading all models and loading {m}"),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default, Clone)]
    struct Fake {
        calls: Arc<Mutex<Vec<String>>>,
        fail_with: Option<u16>,
    }

    impl Fake {
        fn answer(&self, call: String) -> Result<(), ClientError> {
            self.calls.lock().unwrap().push(call);
            match self.fail_with {
                Some(status) => Err(ClientError::Api {
                    status,
                    message: "refused".into(),
                }),
                None => Ok(()),
            }
        }
    }

    impl ModelOps for Fake {
        async fn load(&self, model: &str) -> Result<(), ClientError> {
            self.answer(format!("load {model}"))
        }
        async fn unload(&self, model: &str) -> Result<(), ClientError> {
            self.answer(format!("unload {model}"))
        }
        async fn delete(&self, model: &str) -> Result<(), ClientError> {
            self.answer(format!("delete {model}"))
        }
        async fn kill(&self, model: &str) -> Result<(), ClientError> {
            self.answer(format!("kill {model}"))
        }
        async fn unload_all(&self) -> Result<(), ClientError> {
            self.answer("unload all".into())
        }
        async fn library(&self) -> Result<Vec<LibraryModel>, ClientError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn ops_reach_the_server() {
        let fake = Fake::default();
        run(&fake, &ModelOp::Load("a".into())).await.unwrap();
        run(&fake, &ModelOp::Unload("a".into())).await.unwrap();
        run(&fake, &ModelOp::Delete("a".into())).await.unwrap();
        run(&fake, &ModelOp::Kill("a".into())).await.unwrap();
        run(&fake, &ModelOp::UnloadAll).await.unwrap();
        assert_eq!(
            *fake.calls.lock().unwrap(),
            ["load a", "unload a", "delete a", "kill a", "unload all"]
        );
    }

    #[test]
    fn catalog_entries_become_library_rows() {
        let m = CatalogModel {
            name: "qwen3:8b".into(),
            size: 5,
            source: "ollama".into(),
            kind: "chat".into(),
            format: "gguf".into(),
            quantization: Some("Q4_K_M".into()),
            engine: "llama-cpp".into(),
            context_length: Some(40_960),
            loadable: true,
            ..CatalogModel::default()
        };
        let row = library_model(&m);
        assert_eq!(row.source.as_deref(), Some("ollama"));
        assert_eq!(row.kind.as_deref(), Some("chat"));
        assert_eq!(row.engine.as_deref(), Some("llama-cpp"));
        assert_eq!(row.loadable, Some(true));
        assert_eq!(row.context_length, Some(40_960));
        let bare = library_model(&CatalogModel::default());
        assert_eq!(bare.source, None);
        assert_eq!(bare.kind, None);
    }

    #[tokio::test]
    async fn failures_are_explained() {
        let fake = Fake {
            fail_with: Some(403),
            ..Fake::default()
        };
        let e = run(&fake, &ModelOp::Delete("qwen3:8b".into()))
            .await
            .unwrap_err();
        assert!(e.message.contains("ollama rm qwen3:8b"), "{}", e.message);
        let e = run(&fake, &ModelOp::UnloadAll).await.unwrap_err();
        assert!(e.message.contains("admin scope"));
        let fake = Fake {
            fail_with: Some(500),
            ..Fake::default()
        };
        let e = run(&fake, &ModelOp::Load("m".into())).await.unwrap_err();
        assert!(e.message.starts_with("Loading m failed"), "{}", e.message);
        assert_eq!(e.cause, FailureCause::Other);
    }

    #[test]
    fn load_failures_are_classified_for_the_recovery_actions() {
        let api = |status, message: &str| ClientError::Api {
            status,
            message: message.into(),
        };
        let memory = api(
            503,
            "`qwen3:30b` does not fit in memory: needs 20 GB RAM and 0 B VRAM, 8 GB RAM usable",
        );
        assert_eq!(classify_load_failure(&memory), FailureCause::Memory);
        let missing = api(
            503,
            "engine `llama-cpp` is not installed (needed by qwen3:8b); install it with `alkahest engines install llama-cpp`",
        );
        assert_eq!(
            classify_load_failure(&missing),
            FailureCause::EngineMissing("llama-cpp".into())
        );
        assert_eq!(
            classify_load_failure(&api(500, "boom")),
            FailureCause::Other
        );
        let f = failure(&ModelOp::Load("qwen3:30b".into()), &memory);
        assert_eq!(f.cause, FailureCause::Memory);
        let f = failure(&ModelOp::Delete("x".into()), &memory);
        assert_eq!(f.cause, FailureCause::Other, "only loads have a way out");
    }

    #[tokio::test]
    async fn unload_all_then_load_runs_in_that_order() {
        let fake = Fake::default();
        run(&fake, &ModelOp::UnloadAllThenLoad("m".into()))
            .await
            .unwrap();
        assert_eq!(*fake.calls.lock().unwrap(), ["unload all", "load m"]);
    }
}
