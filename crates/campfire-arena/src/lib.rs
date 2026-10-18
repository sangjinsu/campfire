//! Deterministic server-authoritative sample arena.
use campfire_runtime::{Game, GameContext, GameError, GameModule};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Duration};

#[derive(Clone, Copy, Default)]
pub struct ArenaModule;

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Settings {
    speed: f64,
    boundary: f64,
    goal_radius: f64,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            speed: 5.0,
            boundary: 10.0,
            goal_radius: 0.5,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Direction {
    move_x: f64,
    move_y: f64,
}

#[derive(Serialize)]
struct Player {
    id: String,
    x: f64,
    y: f64,
    connected: bool,
    #[serde(skip)]
    direction: (f64, f64),
}

struct Arena {
    settings: Settings,
    players: BTreeMap<String, Player>,
    shared_score: u64,
    goal_active: bool,
}

fn invalid(message: &str) -> GameError {
    GameError::new("INVALID_INPUT", message)
}
fn direction(payload: &Value) -> Result<Direction, GameError> {
    let value: Direction = serde_json::from_value(payload.clone())
        .map_err(|_| invalid("Expected only numeric move_x and move_y"))?;
    if !value.move_x.is_finite()
        || !value.move_y.is_finite()
        || value.move_x.abs() > 1.0
        || value.move_y.abs() > 1.0
    {
        return Err(invalid(
            "Direction components must be finite and in [-1, 1]",
        ));
    }
    Ok(value)
}

impl GameModule for ArenaModule {
    fn create(&self, settings: &Value, _context: &GameContext) -> Result<Box<dyn Game>, GameError> {
        let settings: Settings = serde_json::from_value(settings.clone()).map_err(|_| {
            GameError::new("INVALID_SETTINGS", "Unknown or malformed arena settings")
        })?;
        if !settings.speed.is_finite()
            || !(0.1..=100.0).contains(&settings.speed)
            || !settings.boundary.is_finite()
            || !(1.0..=1000.0).contains(&settings.boundary)
            || !settings.goal_radius.is_finite()
            || !(0.1..=settings.boundary).contains(&settings.goal_radius)
        {
            return Err(GameError::new(
                "INVALID_SETTINGS",
                "speed: 0.1..100; boundary: 1..1000; goal_radius: 0.1..boundary",
            ));
        }
        Ok(Box::new(Arena {
            settings,
            players: BTreeMap::new(),
            shared_score: 0,
            goal_active: true,
        }))
    }
}

impl Game for Arena {
    fn player_joined(&mut self, player_id: &str) {
        self.players
            .entry(player_id.into())
            .or_insert_with(|| Player {
                id: player_id.into(),
                x: 0.0,
                y: 0.0,
                connected: true,
                direction: (0.0, 0.0),
            });
    }
    fn player_left(&mut self, player_id: &str) {
        self.players.remove(player_id);
    }
    fn player_connection_changed(&mut self, player_id: &str, connected: bool) {
        if let Some(player) = self.players.get_mut(player_id) {
            player.connected = connected;
            if !connected {
                player.direction = (0.0, 0.0);
            }
        }
    }
    fn validate_input(&self, player_id: &str, payload: &Value) -> Result<(), GameError> {
        let Some(player) = self.players.get(player_id) else {
            return Err(invalid("Unknown player"));
        };
        if !player.connected {
            return Err(invalid("Player is disconnected"));
        }
        direction(payload).map(|_| ())
    }
    fn apply_input(&mut self, player_id: &str, payload: &Value) {
        if self.validate_input(player_id, payload).is_err() {
            return;
        }
        let input = direction(payload).expect("validated direction");
        let length = input.move_x.hypot(input.move_y).max(1.0);
        if let Some(player) = self.players.get_mut(player_id) {
            player.direction = (input.move_x / length, input.move_y / length);
        }
    }
    fn tick(&mut self, dt: Duration) {
        let movement = self.settings.speed * dt.as_secs_f64();
        let goal_x = self.settings.boundary / 2.0;
        for player in self.players.values_mut() {
            if !player.connected {
                continue;
            }
            player.x = (player.x + player.direction.0 * movement)
                .clamp(-self.settings.boundary, self.settings.boundary);
            player.y = (player.y + player.direction.1 * movement)
                .clamp(-self.settings.boundary, self.settings.boundary);
            if self.goal_active && (player.x - goal_x).hypot(player.y) <= self.settings.goal_radius
            {
                self.goal_active = false;
                self.shared_score += 1;
            }
        }
    }
    fn snapshot(&self) -> Value {
        json!({"players": self.players.values().collect::<Vec<_>>(), "shared_score": self.shared_score,
            "goal": {"x": self.settings.boundary / 2.0, "y": 0.0, "radius": self.settings.goal_radius, "active": self.goal_active}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn arena() -> Box<dyn Game> {
        ArenaModule
            .create(
                &json!({}),
                &GameContext {
                    capacity: 16,
                    tick_interval: Duration::from_secs_f64(1.0 / 30.0),
                },
            )
            .unwrap()
    }

    fn position(game: &dyn Game, id: &str) -> (f64, f64) {
        let state = game.snapshot();
        let player = state["players"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap();
        (player["x"].as_f64().unwrap(), player["y"].as_f64().unwrap())
    }

    #[test]
    fn input_messages_do_not_move_until_tick_and_speed_is_fixed() {
        let mut game = arena();
        game.player_joined("p1");
        for _ in 0..20 {
            game.apply_input("p1", &json!({"move_x":1,"move_y":0}));
        }
        assert_eq!(position(&*game, "p1"), (0.0, 0.0));
        game.tick(Duration::from_millis(100));
        assert_eq!(position(&*game, "p1"), (0.5, 0.0));
    }

    #[test]
    fn diagonals_are_normalized_and_boundaries_clamped() {
        let mut game = arena();
        game.player_joined("p1");
        game.apply_input("p1", &json!({"move_x":1,"move_y":1}));
        game.tick(Duration::from_secs(1));
        let (x, y) = position(&*game, "p1");
        assert!((x.hypot(y) - 5.0).abs() < 1e-10);
        game.tick(Duration::from_secs(30));
        assert_eq!(position(&*game, "p1"), (10.0, 10.0));
    }

    #[test]
    fn disconnect_stops_direction_and_reconnect_does_not_replay_it() {
        let mut game = arena();
        game.player_joined("p1");
        game.apply_input("p1", &json!({"move_x":1,"move_y":0}));
        game.player_connection_changed("p1", false);
        game.tick(Duration::from_secs(1));
        assert_eq!(position(&*game, "p1"), (0.0, 0.0));
        assert_eq!(game.snapshot()["players"][0]["connected"], false);
        game.player_connection_changed("p1", true);
        game.tick(Duration::from_secs(1));
        assert_eq!(position(&*game, "p1"), (0.0, 0.0));
    }

    #[test]
    fn simultaneous_goal_acquisition_scores_exactly_once() {
        let mut game = arena();
        for id in ["a", "b"] {
            game.player_joined(id);
            game.apply_input(id, &json!({"move_x":1,"move_y":0}));
        }
        game.tick(Duration::from_secs(1));
        assert_eq!(game.snapshot()["shared_score"], 1);
        assert_eq!(game.snapshot()["goal"]["active"], false);
        game.tick(Duration::from_secs(1));
        assert_eq!(game.snapshot()["shared_score"], 1);
    }

    #[test]
    fn rejects_client_authority_invalid_components_and_unknown_fields() {
        let mut game = arena();
        game.player_joined("p1");
        for payload in [
            json!({"move_x":2,"move_y":0}),
            json!({"move_x":0}),
            json!({"move_x":"1","move_y":0}),
            json!({"move_x":0,"move_y":0,"x":99}),
            json!({"move_x":0,"move_y":0,"shared_score":999}),
        ] {
            assert!(game.validate_input("p1", &payload).is_err(), "{payload}");
        }
        assert!(
            game.validate_input("p1", &json!({"move_x":-1,"move_y":0.5}))
                .is_ok()
        );
        assert!(
            game.validate_input("missing", &json!({"move_x":0,"move_y":0}))
                .is_err()
        );
    }

    #[test]
    fn invalid_settings_are_rejected() {
        let context = GameContext {
            capacity: 16,
            tick_interval: Duration::from_millis(33),
        };
        for settings in [
            json!({"speed":0}),
            json!({"boundary":0}),
            json!({"goal_radius":100}),
            json!({"x":10}),
            json!({"speed":"5"}),
        ] {
            assert!(
                ArenaModule.create(&settings, &context).is_err(),
                "{settings}"
            );
        }
    }

    #[test]
    fn replay_is_deterministic_and_departure_removes_player() {
        let mut a = arena();
        let mut b = arena();
        for game in [&mut a, &mut b] {
            game.player_joined("p1");
            game.player_joined("p2");
            game.apply_input("p1", &json!({"move_x":1,"move_y":-1}));
            for _ in 0..120 {
                game.tick(Duration::from_secs_f64(1.0 / 30.0));
            }
            game.player_left("p2");
        }
        assert_eq!(a.snapshot(), b.snapshot());
        assert_eq!(a.snapshot()["players"].as_array().unwrap().len(), 1);
    }
}
