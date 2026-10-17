//! A JSON request helper for the client modules added after the admin module (they live in
//! files of their own so stories touching the client in parallel do not collide).

use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::{Client, ClientError, Result};

impl Client {
    /// Sends `method path?query` with an optional JSON body and decodes the JSON answer.
    pub(crate) async fn call_json<T: DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, String)],
        body: Option<&Value>,
    ) -> Result<T> {
        let mut req = self.request(method, path);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let resp = self.send(req, path).await?;
        resp.json()
            .await
            .map_err(|e| ClientError::Decode(e.to_string()))
    }
}

/// Takes `key` out of a JSON object and decodes it.
pub(crate) fn take<T: DeserializeOwned>(mut v: Value, key: &str) -> Result<T> {
    serde_json::from_value(v.get_mut(key).map(Value::take).unwrap_or(Value::Null))
        .map_err(|e| ClientError::Decode(format!("`{key}`: {e}")))
}
