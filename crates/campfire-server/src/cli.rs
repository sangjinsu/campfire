use campfire_runtime::Config;
use std::{collections::HashSet, net::SocketAddr, time::Duration};

#[derive(Debug)]
pub struct Options {
    pub bind: SocketAddr,
    pub games: Vec<String>,
    pub config: Config,
}
impl Options {
    pub fn parse(
        args: impl IntoIterator<Item = String>,
        env: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        let mut options = Self {
            bind: "127.0.0.1:3000".parse().unwrap(),
            games: vec!["arena".into()],
            config: Config::default(),
        };
        for flag in FLAGS {
            let key = format!("CAMPFIRE_{}", flag.replace('-', "_").to_uppercase());
            if let Some(value) = env(&key) {
                options.set(flag, &value)?;
            }
        }
        if let Some(games) = env("CAMPFIRE_GAMES") {
            options.games = games.split(',').map(str::to_owned).collect();
        }
        let mut args = args.into_iter();
        let mut explicit_games = false;
        while let Some(flag) = args.next() {
            let flag = flag
                .strip_prefix("--")
                .ok_or_else(|| format!("Expected --option, got {flag}"))?;
            let value = args
                .next()
                .ok_or_else(|| format!("Missing value for --{flag}"))?;
            if flag == "game" {
                if !explicit_games {
                    options.games.clear();
                    explicit_games = true;
                }
                options.games.push(value);
            } else {
                options.set(flag, &value)?;
            }
        }
        let mut seen = HashSet::new();
        if options.games.is_empty()
            || options.games.iter().any(|id| {
                id.is_empty()
                    || id.len() > 64
                    || !id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                    || !seen.insert(id)
            })
        {
            return Err("Game identifiers must be unique, 1..64 ASCII letters, digits, underscore or hyphen".into());
        }
        options.config.validate().map_err(|e| e.to_string())?;
        Ok(options)
    }

    fn set(&mut self, flag: &str, value: &str) -> Result<(), String> {
        macro_rules! number {
            ($field:ident) => {
                self.config.$field = value
                    .parse()
                    .map_err(|_| format!("Invalid value for --{flag}"))?
            };
        }
        macro_rules! milliseconds {
            ($field:ident) => {
                self.config.$field = Duration::from_millis(
                    value
                        .parse()
                        .map_err(|_| format!("Invalid milliseconds for --{flag}"))?,
                )
            };
        }
        match flag {
            "bind" => {
                self.bind = value
                    .parse()
                    .map_err(|_| "--bind requires an IP:port socket address")?
            }
            "capacity" => number!(room_capacity),
            "tick-hz" => number!(tick_hz),
            "snapshot-hz" => number!(snapshot_hz),
            "guest-ttl-ms" => milliseconds!(guest_ttl),
            "reconnect-grace-ms" => milliseconds!(reconnect_grace),
            "auth-timeout-ms" => milliseconds!(auth_timeout),
            "cleanup-interval-ms" => milliseconds!(cleanup_interval),
            "max-frame-bytes" => number!(max_frame_bytes),
            "input-rate-per-second" => number!(input_rate_per_second),
            "input-burst" => number!(input_burst),
            "room-input-capacity" => number!(room_input_capacity),
            "reliable-capacity" => number!(reliable_capacity),
            "request-cache-capacity" => number!(request_cache_capacity),
            "max-input-seq-jump" => number!(max_input_seq_jump),
            "max-sessions" => number!(max_sessions),
            "max-connections" => number!(max_connections),
            "max-unauthenticated-connections" => number!(max_unauthenticated_connections),
            "max-rooms" => number!(max_rooms),
            "max-rate-keys" => number!(max_rate_keys),
            "guest-rate-per-second" => number!(guest_rate_per_second),
            "guest-burst" => number!(guest_burst),
            "global-guest-rate-per-second" => number!(global_guest_rate_per_second),
            "global-guest-burst" => number!(global_guest_burst),
            "rate-key-ttl-ms" => milliseconds!(rate_key_ttl),
            "write-timeout-ms" => milliseconds!(write_timeout),
            "max-write-timeouts" => number!(max_write_timeouts),
            "max-abuse-violations" => number!(max_abuse_violations),
            _ => return Err(format!("Unknown option --{flag}")),
        }
        Ok(())
    }
}

pub const FLAGS: &[&str] = &[
    "bind",
    "capacity",
    "tick-hz",
    "snapshot-hz",
    "guest-ttl-ms",
    "reconnect-grace-ms",
    "auth-timeout-ms",
    "cleanup-interval-ms",
    "max-frame-bytes",
    "input-rate-per-second",
    "input-burst",
    "room-input-capacity",
    "reliable-capacity",
    "request-cache-capacity",
    "max-input-seq-jump",
    "max-sessions",
    "max-connections",
    "max-unauthenticated-connections",
    "max-rooms",
    "max-rate-keys",
    "guest-rate-per-second",
    "guest-burst",
    "global-guest-rate-per-second",
    "global-guest-burst",
    "rate-key-ttl-ms",
    "write-timeout-ms",
    "max-write-timeouts",
    "max-abuse-violations",
];

pub fn help() -> String {
    format!(
        "Campfire server\nUsage: campfire-server [--game ID]... [--OPTION VALUE]\n\nDefault: --bind 127.0.0.1:3000 --game arena --capacity 16 --tick-hz 30 --snapshot-hz 15\n\nOptions:\n  --help\n  --game ID (repeat to register the compiled arena module under distinct game IDs)\n{}\nEnvironment: CAMPFIRE_<OPTION_IN_UPPER_SNAKE_CASE>; CAMPFIRE_GAMES=arena,arena-b. CLI overrides environment. Duration options use integer milliseconds. See docs/configuration.md.\n",
        FLAGS
            .iter()
            .map(|s| format!("  --{s} VALUE\n"))
            .collect::<String>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parse(args: &[&str]) -> Result<Options, String> {
        Options::parse(args.iter().map(|s| s.to_string()), |_| None)
    }
    #[test]
    fn defaults_and_explicit_module_selection() {
        let options = parse(&[]).unwrap();
        assert_eq!(options.bind.to_string(), "127.0.0.1:3000");
        assert_eq!(options.games, vec!["arena"]);
        assert_eq!(options.config.room_capacity, 16);
        let options = parse(&[
            "--game",
            "arena",
            "--game",
            "arena-b",
            "--capacity",
            "8",
            "--bind",
            "0.0.0.0:4040",
        ])
        .unwrap();
        assert_eq!(options.games, vec!["arena", "arena-b"]);
        assert_eq!(options.config.room_capacity, 8);
        assert_eq!(options.bind.port(), 4040);
    }
    #[test]
    fn cli_overrides_environment_without_global_env_mutation() {
        let options = Options::parse(["--capacity".into(), "32".into()], |key| match key {
            "CAMPFIRE_CAPACITY" => Some("8".into()),
            "CAMPFIRE_GAMES" => Some("arena,arena-b".into()),
            "CAMPFIRE_RECONNECT_GRACE_MS" => Some("250".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(options.config.room_capacity, 32);
        assert_eq!(options.config.reconnect_grace.as_millis(), 250);
        assert_eq!(options.games.len(), 2);
    }
    #[test]
    fn rejects_invalid_unknown_duplicate_and_overflow_values() {
        for args in [
            vec!["--capacity", "0"],
            vec!["--tick-hz", "0"],
            vec!["--snapshot-hz", "31"],
            vec!["--reconnect-grace-ms", "0"],
            vec!["--max-sessions", "-1"],
            vec!["--bind", "localhost"],
            vec!["--game", "arena", "--game", "arena"],
            vec!["--unknown", "1"],
            vec!["--capacity"],
            vec!["--input-burst", "4294967296"],
        ] {
            assert!(parse(&args).is_err(), "{args:?}");
        }
    }
}
