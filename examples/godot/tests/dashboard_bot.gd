extends "res://tests/bot.gd"

# Keep every existing soak assertion; use the same gap for OFF/ON runs so the
# one-second dashboard collector can observe the real reconnect reservation.
var dashboard_disconnected_at := -1

func _process(delta: float) -> bool:
	if phase == "disconnected":
		if dashboard_disconnected_at < 0:
			dashboard_disconnected_at = Time.get_ticks_msec()
		if Time.get_ticks_msec() - dashboard_disconnected_at < 1400:
			return false
	return super._process(delta)
