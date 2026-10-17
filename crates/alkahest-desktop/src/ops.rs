//! Model actions the Library and Running screens send to the server (load, unload, delete,
//! unload all), free of GPUI.
//!
//! [`ModelOps`] is what the screens need; [`alkahest_client::Client`] implements it over HTTP
//! and the tests use a fake. [`run`] turns a client error into the one-line message the
//! screen shows ([`OpFailure`]).

use std::future::Future;

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
    /// Unload every model.
    UnloadAll,
}

/// A failed action, as shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpFailure {
    /// One-line explanation.
    pub message: String,
}

/// What the screens need from a server.
pub trait ModelOps: Send + Sync + 'static {
    /// Loads `model` (`/api/generate` without a prompt); resolves once it is ready.
    fn load(&self, model: &str) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// Unloads `model` (`keep_alive: 0`).
    fn unload(&self, model: &str) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// Deletes `model` (`DELETE /api/delete`).
    fn delete(&self, model: &str) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// Unloads everything (`POST /alkahest/v1/unload`).
    fn unload_all(&self) -> impl Future<Output = Result<(), ClientError>> + Send;
    /// The model list (see [`Client::library`]).
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

    async fn unload_all(&self) -> Result<(), ClientError> {
        Client::unload_all(self).await
    }

    async fn library(&self) -> Result<Vec<LibraryModel>, ClientError> {
        Client::library(self).await
    }
}

/// Runs `op` and explains a failure.
pub async fn run<O: ModelOps>(ops: &O, op: &ModelOp) -> Result<(), OpFailure> {
    let result = match op {
        ModelOp::Load(m) => ops.load(m).await,
        ModelOp::Unload(m) => ops.unload(m).await,
        ModelOp::Delete(m) => ops.delete(m).await,
        ModelOp::UnloadAll => ops.unload_all().await,
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
            ModelOp::UnloadAll,
            ClientError::Api {
                status: 401 | 403, ..
            },
        ) => "Unloading everything needs an API key with the admin scope.".to_string(),
        (op, err) => format!("{} failed: {err}", describe(op)),
    };
    OpFailure { message }
}

fn describe(op: &ModelOp) -> String {
    match op {
        ModelOp::Load(m) => format!("Loading {m}"),
        ModelOp::Unload(m) => format!("Unloading {m}"),
        ModelOp::Delete(m) => format!("Deleting {m}"),
        ModelOp::UnloadAll => "Unloading all models".to_string(),
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
        run(&fake, &ModelOp::UnloadAll).await.unwrap();
        assert_eq!(
            *fake.calls.lock().unwrap(),
            ["load a", "unload a", "delete a", "unload all"]
        );
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
    }
}
