use campfire_runtime::ConnectionLimits;

/// Read only the transport connection limits from the server environment.
pub fn connection_limits_from_env(
    get: impl Fn(&str) -> Option<String>,
) -> Result<ConnectionLimits, String> {
    fn parse(
        get: &impl Fn(&str) -> Option<String>,
        name: &str,
        default: u32,
    ) -> Result<u32, String> {
        match get(name) {
            Some(value) => value
                .parse::<u32>()
                .map_err(|_| format!("{name} must be a positive u32 within runtime bounds")),
            None => Ok(default),
        }
    }

    let defaults = ConnectionLimits::default();
    let limits = ConnectionLimits {
        control_rate_per_second: parse(
            &get,
            "CAMPFIRE_CONTROL_RATE_PER_SECOND",
            defaults.control_rate_per_second,
        )?,
        control_burst: parse(&get, "CAMPFIRE_CONTROL_BURST", defaults.control_burst)?,
        invalid_rate_per_second: parse(
            &get,
            "CAMPFIRE_INVALID_RATE_PER_SECOND",
            defaults.invalid_rate_per_second,
        )?,
        invalid_burst: parse(&get, "CAMPFIRE_INVALID_BURST", defaults.invalid_burst)?,
    };
    limits.validate().map_err(|error| error.to_string())?;
    Ok(limits)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn defaults_and_exact_environment_keys() {
        let observed = RefCell::new(Vec::new());
        let limits = connection_limits_from_env(|name| {
            observed.borrow_mut().push(name.to_string());
            None
        })
        .unwrap();
        assert_eq!(limits, ConnectionLimits::default());
        assert_eq!(
            observed.into_inner(),
            [
                "CAMPFIRE_CONTROL_RATE_PER_SECOND",
                "CAMPFIRE_CONTROL_BURST",
                "CAMPFIRE_INVALID_RATE_PER_SECOND",
                "CAMPFIRE_INVALID_BURST",
            ]
        );
    }

    #[test]
    fn overrides_and_rejects_invalid_values() {
        let limits = connection_limits_from_env(|name| match name {
            "CAMPFIRE_CONTROL_BURST" => Some("17".into()),
            "CAMPFIRE_INVALID_RATE_PER_SECOND" => Some("3".into()),
            _ => None,
        })
        .unwrap();
        assert_eq!(limits.control_burst, 17);
        assert_eq!(limits.invalid_rate_per_second, 3);
        for value in ["0", "-1", "4294967296", "1000001", "not-a-number", ""] {
            assert!(
                connection_limits_from_env(|name| {
                    (name == "CAMPFIRE_CONTROL_BURST").then(|| value.into())
                })
                .is_err(),
                "accepted {value:?}"
            );
        }
    }
}
