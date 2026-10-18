use campfire_runtime::Config;

#[test]
fn config_rejects_unbounded_or_invalid_timing() {
    let mut config = Config::default();
    assert!(config.validate().is_ok());
    config.room_input_capacity = 0;
    assert!(config.validate().is_err());
    config.room_input_capacity = 1024;
    config.snapshot_hz = config.tick_hz + 1;
    assert!(config.validate().is_err());
}

#[test]
fn config_rejects_values_that_overflow_clocks_or_channel_allocations() {
    let config = Config {
        guest_ttl: std::time::Duration::MAX,
        ..Config::default()
    };
    assert!(config.validate().is_err());
    let config = Config {
        room_input_capacity: usize::MAX,
        ..Config::default()
    };
    assert!(config.validate().is_err());
}
