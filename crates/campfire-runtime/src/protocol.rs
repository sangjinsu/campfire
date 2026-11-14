use crate::output::ReliableFrame;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    hash::{DefaultHasher, Hash, Hasher},
    io,
};

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
    #[serde(rename = "game.command")]
    Execute {
        room_id: String,
        room_instance_id: String,
        command_seq: u64,
        payload: Value,
    },
}
#[derive(Clone, Debug)]
pub(crate) struct Request {
    pub id: String,
    pub game_id: Option<String>,
    pub command: Command,
    /// Canonical hash of the whole message, used to tell a retry from a reused `request_id`.
    pub fingerprint: [u8; 32],
}

pub(crate) fn parse(text: &str) -> Result<Request, &'static str> {
    let body: Value = serde_json::from_str(text).map_err(|_| "INVALID_MESSAGE")?;
    if !body.is_object() {
        return Err("INVALID_MESSAGE");
    }
    let fingerprint = fingerprint(&body);
    let Value::Object(mut object) = body else {
        return Err("INVALID_MESSAGE");
    };
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
        fingerprint,
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

/// Per-connection responses kept so a retried `request_id` gets the same answer.
///
/// Every request looks its id up here and almost none are retries, so the lookup
/// scans a dense array of id hashes instead of comparing the stored ids. The two
/// queues stay index-aligned: `hashes[i]` belongs to `entries[i]`.
pub(crate) struct ResponseCache {
    capacity: usize,
    hashes: VecDeque<u64>,
    entries: VecDeque<CachedResponse>,
}
struct CachedResponse {
    id: Box<str>,
    fingerprint: [u8; 32],
    response: ReliableFrame,
}
impl ResponseCache {
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            hashes: VecDeque::new(),
            entries: VecDeque::new(),
        }
    }
    fn id_hash(id: &str) -> u64 {
        // A fixed key is enough: a collision only costs one extra string comparison,
        // and the ids in one cache all come from the same connection.
        let mut hasher = DefaultHasher::new();
        id.hash(&mut hasher);
        hasher.finish()
    }
    fn find(&self, id: &str) -> Option<&CachedResponse> {
        let wanted = Self::id_hash(id);
        self.hashes
            .iter()
            .enumerate()
            .filter(|(_, hash)| **hash == wanted)
            .map(|(index, _)| &self.entries[index])
            .find(|entry| &*entry.id == id)
    }
    pub fn get(&self, request: &Request) -> Result<Option<ReliableFrame>, Error> {
        match self.find(&request.id) {
            Some(entry) if entry.fingerprint == request.fingerprint => {
                Ok(Some(entry.response.clone()))
            }
            Some(_) => Err(Error::new(
                "REQUEST_ID_CONFLICT",
                "request_id already identifies a different command",
            )),
            None => Ok(None),
        }
    }
    pub fn insert(&mut self, request: &Request, response: ReliableFrame) {
        // A replayed command reaches this point again; its first response stays authoritative.
        if self.capacity == 0 || self.find(&request.id).is_some() {
            return;
        }
        if self.entries.len() == self.capacity {
            self.hashes.pop_front();
            self.entries.pop_front();
        }
        self.hashes.push_back(Self::id_hash(&request.id));
        self.entries.push_back(CachedResponse {
            id: request.id.as_str().into(),
            fingerprint: request.fingerprint,
            response,
        });
    }
}

struct HashWriter<'a>(&'a mut Sha256);
impl io::Write for HashWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// SHA-256 of the compact JSON encoding with every object's keys in sorted order.
///
/// The encoding is streamed into the hasher, so the value is neither cloned nor
/// rendered to a string first.
pub(crate) fn fingerprint(body: &Value) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hash_canonical(body, &mut hasher);
    hasher.finalize().into()
}
fn hash_canonical(value: &Value, hasher: &mut Sha256) {
    match value {
        Value::Object(map) => {
            hasher.update(b"{");
            if map.keys().is_sorted() {
                hash_entries(map.iter(), hasher);
            } else {
                let mut entries: Vec<_> = map.iter().collect();
                entries.sort_by(|left, right| left.0.cmp(right.0));
                hash_entries(entries.into_iter(), hasher);
            }
            hasher.update(b"}");
        }
        Value::Array(items) => {
            hasher.update(b"[");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    hasher.update(b",");
                }
                hash_canonical(item, hasher);
            }
            hasher.update(b"]");
        }
        // Scalars use serde_json's own number formatting and string escaping.
        scalar => serde_json::to_writer(HashWriter(hasher), scalar)
            .expect("hashing a JSON scalar cannot fail"),
    }
}
fn hash_entries<'a>(entries: impl Iterator<Item = (&'a String, &'a Value)>, hasher: &mut Sha256) {
    for (index, (key, value)) in entries.enumerate() {
        if index > 0 {
            hasher.update(b",");
        }
        serde_json::to_writer(HashWriter(hasher), key).expect("hashing a JSON key cannot fail");
        hasher.update(b":");
        hash_canonical(value, hasher);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fingerprints_canonicalize_nested_object_keys_but_preserve_array_order() {
        let first: Value =
            serde_json::from_str(r#"{"outer":{"b":2,"a":1},"array":[{"d":4,"c":3},5]}"#).unwrap();
        let reordered: Value =
            serde_json::from_str(r#"{"array":[{"c":3,"d":4},5],"outer":{"a":1,"b":2}}"#).unwrap();
        let reversed: Value =
            serde_json::from_str(r#"{"array":[5,{"c":3,"d":4}],"outer":{"a":1,"b":2}}"#).unwrap();
        assert_eq!(fingerprint(&first), fingerprint(&reordered));
        assert_ne!(fingerprint(&first), fingerprint(&reversed));
    }
    #[test]
    fn parses_ordered_game_commands() {
        assert!(parse(r#"{"v":1,"type":"game.command","request_id":"cmd","room_id":"r","room_instance_id":"i","command_seq":1,"payload":{"buy":"shield"}}"#).is_ok());
    }
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
        cache.insert(&first, (&json!({"response":1})).into());
        assert_eq!(cache.get(&first).unwrap().unwrap(), json!({"response":1}));
        assert_eq!(
            cache.get(&conflict).unwrap_err().code,
            "REQUEST_ID_CONFLICT"
        );
        cache.insert(&second, (&json!({"response":2})).into());
        assert!(cache.get(&first).unwrap().is_none());
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.hashes.len(), 1);
    }
    #[test]
    fn response_cache_keeps_the_first_response_for_a_repeated_id() {
        let request =
            parse(r#"{"v":1,"type":"room.list","request_id":"1","cursor":null,"limit":10}"#)
                .unwrap();
        let other =
            parse(r#"{"v":1,"type":"room.list","request_id":"2","cursor":null,"limit":10}"#)
                .unwrap();
        let mut cache = ResponseCache::new(2);
        cache.insert(&request, (&json!({"response":"first"})).into());
        cache.insert(&request, (&json!({"response":"again"})).into());
        assert_eq!(
            cache.get(&request).unwrap().unwrap(),
            json!({"response":"first"})
        );
        // The repeat did not take a second slot, so one more id still fits.
        cache.insert(&other, (&json!({"response":2})).into());
        assert!(cache.get(&request).unwrap().is_some());
        assert_eq!(cache.entries.len(), 2);
        // Once full, the oldest id leaves and its slot goes to the new one.
        let third =
            parse(r#"{"v":1,"type":"room.list","request_id":"3","cursor":null,"limit":10}"#)
                .unwrap();
        cache.insert(&third, (&json!({"response":3})).into());
        assert!(cache.get(&request).unwrap().is_none());
        assert_eq!(cache.get(&other).unwrap().unwrap(), json!({"response":2}));
        assert_eq!(cache.get(&third).unwrap().unwrap(), json!({"response":3}));
        assert_eq!(cache.hashes.len(), cache.entries.len());
    }
    #[test]
    fn parse_fingerprints_the_whole_message_independent_of_key_order() {
        let first = parse(r#"{"v":1,"type":"game.input","request_id":"i","room_id":"r","room_instance_id":"x","input_seq":1,"payload":{"move_x":1,"move_y":0}}"#).unwrap();
        let reordered = parse(r#"{"payload":{"move_y":0,"move_x":1},"input_seq":1,"room_instance_id":"x","room_id":"r","request_id":"i","type":"game.input","v":1}"#).unwrap();
        let different = parse(r#"{"v":1,"type":"game.input","request_id":"i","room_id":"r","room_instance_id":"x","input_seq":2,"payload":{"move_x":1,"move_y":0}}"#).unwrap();
        assert_eq!(first.fingerprint, reordered.fingerprint);
        assert_ne!(first.fingerprint, different.fingerprint);
    }
    #[test]
    fn streamed_fingerprint_equals_hashing_the_sorted_compact_text() {
        // The previous implementation, kept as the reference definition.
        fn reference(body: &Value) -> [u8; 32] {
            let mut canonical = body.clone();
            canonical.sort_all_objects();
            Sha256::digest(canonical.to_string().as_bytes()).into()
        }
        for text in [
            "null",
            "true",
            "-0.0",
            "1e300",
            "18446744073709551615",
            "-9223372036854775808",
            "0.1",
            r#""""#,
            r#""quote \" slash \\ control \u0001 tab \t unicode é 😀""#,
            "[]",
            "{}",
            r#"[1,[2,[3,{"b":[],"a":{}}]],"x",null]"#,
            r#"{"z":1,"a":{"y":[{"d":4,"c":3},5],"b":"\n"},"m":null,"":false,"key \" with quote":1.5}"#,
            r#"{"v":1,"type":"game.input","request_id":"i1","room_id":"r","room_instance_id":"i","input_seq":7,"payload":{"move_x":-1.0,"move_y":0.5}}"#,
        ] {
            let value: Value = serde_json::from_str(text).unwrap();
            assert_eq!(fingerprint(&value), reference(&value), "differs for {text}");
        }
    }
}
