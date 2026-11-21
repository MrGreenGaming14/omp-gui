//! [`JsonRpc2`]: the JSON-RPC 2.0 envelope (acp, codex app-server).

use super::rpc::{Framing, Incoming, RpcError, id_key};
use serde_json::{Value as Json, json};

/// JSON-RPC 2.0 framing. `header` writes `"jsonrpc":"2.0"`; codex omits it.
#[derive(Clone, Copy, Debug)]
pub struct JsonRpc2 {
    pub header: bool,
}

/// JSON-RPC "internal error", for failures that carry no code.
const INTERNAL: i64 = -32603;

impl JsonRpc2 {
    fn frame(&self, mut msg: Json) -> Json {
        if self.header {
            msg["jsonrpc"] = "2.0".into();
        }
        msg
    }
}

impl Framing for JsonRpc2 {
    fn request(&self, id: &Json, method: &str, params: Json) -> Json {
        self.frame(json!({ "id": id, "method": method, "params": params }))
    }

    fn notification(&self, method: &str, params: Json) -> Json {
        match params {
            Json::Null => self.frame(json!({ "method": method })),
            params => self.frame(json!({ "method": method, "params": params })),
        }
    }

    fn response(&self, id: Json, result: Result<Json, RpcError>) -> Json {
        let error = |code: i64, message: String| self.frame(json!({ "id": id, "error": { "code": code, "message": message } }));
        match result {
            Ok(result) => self.frame(json!({ "id": id, "result": result })),
            Err(RpcError::Command { message, code, .. }) => error(code.and_then(|c| c.parse().ok()).unwrap_or(INTERNAL), message),
            Err(err) => error(INTERNAL, err.to_string()),
        }
    }

    fn classify(&self, mut msg: Json) -> Option<Incoming> {
        let params = msg.get_mut("params").map_or(Json::Null, Json::take);
        let id = msg.get_mut("id").map(Json::take).filter(|id| !id.is_null());
        match (msg.get("method").and_then(Json::as_str), id) {
            (Some(method), Some(id)) => Some(Incoming::Request { id, method: method.to_owned(), params }),
            (Some(method), None) => Some(Incoming::Notification { method: method.to_owned(), params }),
            (None, Some(id)) => {
                let result = match msg.get_mut("error").map(Json::take) {
                    Some(error) => Err(RpcError::Command {
                        command: "request".to_owned(),
                        message: error.get("message").and_then(Json::as_str).unwrap_or("error").to_owned(),
                        code: error.get("code").map(Json::to_string),
                    }),
                    None => Ok(msg.get_mut("result").map_or(Json::Null, Json::take)),
                };
                Some(Incoming::Response { id: id_key(&id), result })
            }
            (None, None) => None,
        }
    }
}
