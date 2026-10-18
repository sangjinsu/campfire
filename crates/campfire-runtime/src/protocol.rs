use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;

#[derive(Clone, Debug, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum Command {
    #[serde(rename = "session.authenticate")]
    Authenticate { token: String },
    #[serde(rename = "session.logout")]
    Logout,
    #[serde(rename = "room.create")]
    Create { settings: Value },
    #[serde(rename = "room.list")]
    List { cursor: Option<String>, limit: u32 },
    #[serde(rename = "room.join")]
    Join {
        room_id: String,
        room_instance_id: String,
    },
    #[serde(rename = "room.leave")]
    Leave {
        room_id: String,
        room_instance_id: String,
    },
    #[serde(rename = "resume")]
    Resume {
        room_id: String,
        room_instance_id: String,
    },
    #[serde(rename = "game.input")]
    Input {
        room_id: String,
        room_instance_id: String,
        input_seq: u64,
        payload: Value,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub id: String,
    pub game_id: Option<String>,
    pub command: Command,
    pub body: Value,
}

pub(crate) fn parse(text: &str) -> Result<Request, &'static str> {
    let body: Value = serde_json::from_str(text).map_err(|_| "INVALID_MESSAGE")?;
    let mut object = body.as_object().cloned().ok_or("INVALID_MESSAGE")?;
    if object.remove("v").and_then(|v| v.as_u64()) != Some(1) {
        return Err("UNSUPPORTED_VERSION");
    }
    let id = object
        .remove("request_id")
        .and_then(|v| v.as_str().map(str::to_owned))
        .ok_or("INVALID_MESSAGE")?;
    if id.is_empty() || id.len() > 128 {
        return Err("INVALID_MESSAGE");
    }
    let game_id = match object.remove("game_id") {
        None => None,
        Some(Value::String(id)) => Some(id),
        Some(_) => return Err("INVALID_MESSAGE"),
    };
    // Option fields are otherwise implicitly optional in Serde. Require cursor explicitly.
    if object.get("type").and_then(Value::as_str) == Some("room.list")
        && !object.contains_key("cursor")
    {
        return Err("INVALID_MESSAGE");
    }
    let command = serde_json::from_value(Value::Object(object)).map_err(|_| "INVALID_MESSAGE")?;
    Ok(Request {
        id,
        game_id,
        command,
        body,
    })
}

#[derive(Clone, Debug)]
pub(crate) struct Error {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
impl Error {
    pub fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: code == "SERVER_BUSY",
        }
    }
    pub fn busy() -> Self {
        Self::new("SERVER_BUSY", "server capacity is temporarily exhausted")
    }
    pub fn value(&self, request_id: Option<&str>) -> Value {
        json!({"v":1,"type":"error","request_id":request_id,"code":self.code,"message":self.message,"retryable":self.retryable})
    }
}
impl From<crate::GameError> for Error {
    fn from(e: crate::GameError) -> Self {
        Self {
            code: e.code,
            message: e.message,
            retryable: false,
        }
    }
}

pub(crate) struct ResponseCache {
    capacity: usize,
    entries: VecDeque<(String, [u8; 32], Value)>,
}
impl ResponseCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            entries: VecDeque::new(),
        }
    }
    pub fn get(&self, request: &Request) -> Result<Option<Value>, Error> {
        match self.entries.iter().find(|(id, _, _)| id == &request.id) {
            Some((_, body, response)) if body == &fingerprint(&request.body) => {
                Ok(Some(response.clone()))
            }
            Some(_) => Err(Error::new(
                "REQUEST_ID_CONFLICT",
                "request_id already identifies a different command",
            )),
            None => Ok(None),
        }
    }
    pub fn insert(&mut self, request: &Request, response: Value) {
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries
            .push_back((request.id.clone(), fingerprint(&request.body), response));
    }
}

fn fingerprint(body: &Value) -> [u8; 32] {
    Sha256::digest(body.to_string().as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn requires_explicit_version_type_request_id_and_fields() {
        assert!(
            parse(r#"{"v":1,"type":"room.list","request_id":"1","cursor":null,"limit":10}"#)
                .is_ok()
        );
        for text in [
            r#"{"type":"room.list","request_id":"1"}"#,
            r#"{"v":2,"type":"room.list","request_id":"1"}"#,
            r#"{"v":1,"type":"bogus","request_id":"1"}"#,
            r#"{"v":1,"type":"room.join","request_id":"1","room_id":"a"}"#,
            r#"{"v":1,"type":"room.list","request_id":"1","cursor":null,"limit":"10"}"#,
        ] {
            assert!(parse(text).is_err(), "accepted invalid schema: {text}");
        }
    }
    #[test]
    fn response_cache_is_bounded_and_conflicting_request_body_is_rejected() {
        let first =
            parse(r#"{"v":1,"type":"room.list","request_id":"1","cursor":null,"limit":10}"#)
                .unwrap();
        let second =
            parse(r#"{"v":1,"type":"room.list","request_id":"2","cursor":null,"limit":10}"#)
                .unwrap();
        let conflict =
            parse(r#"{"v":1,"type":"room.list","request_id":"1","cursor":null,"limit":20}"#)
                .unwrap();
        let mut cache = ResponseCache::new(1);
        cache.insert(&first, json!({"response":1}));
        assert_eq!(cache.get(&first).unwrap(), Some(json!({"response":1})));
        assert_eq!(
            cache.get(&conflict).unwrap_err().code,
            "REQUEST_ID_CONFLICT"
        );
        cache.insert(&second, json!({"response":2}));
        assert!(cache.get(&first).unwrap().is_none());
        assert_eq!(cache.entries.len(), 1);
    }
}
