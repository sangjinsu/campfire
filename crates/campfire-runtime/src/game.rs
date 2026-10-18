use crate::PlayerIdentity;
use serde_json::Value;
use std::{fmt, time::Duration};

pub type PlayerId = String;

#[derive(Clone, Debug)]
pub struct GameContext {
    pub capacity: usize,
    pub tick_interval: Duration,
}

#[derive(Clone, Debug)]
pub struct GameError {
    pub code: String,
    pub message: String,
}
impl GameError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
impl fmt::Display for GameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for GameError {}

/// A trusted synchronous factory. Game callbacks must not perform blocking I/O.
pub trait GameModule: Send + Sync + 'static {
    fn create(&self, settings: &Value, context: &GameContext) -> Result<Box<dyn Game>, GameError>;
}

/// Pure game state transitions; transport and session policy remain in the runtime.
pub trait Game: Send + 'static {
    fn player_joined(&mut self, player_id: &str);
    fn player_joined_with_identity(&mut self, player_id: &str, _identity: &PlayerIdentity) {
        self.player_joined(player_id);
    }
    fn player_identity_changed(&mut self, _player_id: &str, _identity: &PlayerIdentity) {}
    fn player_left(&mut self, player_id: &str);
    fn player_connection_changed(&mut self, player_id: &str, connected: bool);
    fn validate_input(&self, player_id: &str, payload: &Value) -> Result<(), GameError>;
    fn apply_input(&mut self, player_id: &str, payload: &Value);
    fn validate_command(&self, _player_id: &str, _payload: &Value) -> Result<(), GameError> {
        Err(GameError::new(
            "COMMAND_UNSUPPORTED",
            "game commands are not supported",
        ))
    }
    fn apply_command(&mut self, _player_id: &str, _payload: &Value) -> Result<Value, GameError> {
        Err(GameError::new(
            "COMMAND_UNSUPPORTED",
            "game commands are not supported",
        ))
    }
    fn private_snapshot(&self, _player_id: &str) -> Option<Value> {
        None
    }
    fn tick(&mut self, dt: Duration);
    fn snapshot(&self) -> Value;
}
