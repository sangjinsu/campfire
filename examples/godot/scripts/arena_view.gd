extends Control

const CANVAS := Color("eaf1ea")
const INK := Color("19383e")
const FIRE := Color("e8a75e")
const GOLD := Color("ecd77e")
const PEERS := [Color("4f8795"), Color("799c74"), Color("ad788d"), Color("7d85b1")]
var snapshot: Dictionary = {}
var own_id := ""
var live := false

func set_snapshot(value: Dictionary, guest_id: String, connected: bool) -> void:
	snapshot = value
	own_id = guest_id
	live = connected
	queue_redraw()

func court_rect() -> Rect2:
	var side: float = maxf(20.0, minf(size.x - 64.0, size.y - 64.0))
	return Rect2((size - Vector2.ONE * side) / 2.0, Vector2.ONE * side)

func world_to_canvas(point: Vector2) -> Vector2:
	var court := court_rect()
	return court.get_center() + Vector2(point.x, -point.y) * court.size.x / 20.0

func _draw() -> void:
	draw_rect(Rect2(Vector2.ZERO, size), CANVAS)
	var court := court_rect()
	for line in range(-10, 11, 2):
		var tone := Color(INK, 0.18 if line == 0 else 0.07)
		draw_line(world_to_canvas(Vector2(line, -10)), world_to_canvas(Vector2(line, 10)), tone, 1.0)
		draw_line(world_to_canvas(Vector2(-10, line)), world_to_canvas(Vector2(10, line)), tone, 1.0)
	draw_rect(court, Color(INK, 0.5), false, 2.0)
	var font := get_theme_default_font()
	draw_string(font, court.position + Vector2(0, -12), "(-10, +10)", HORIZONTAL_ALIGNMENT_LEFT, -1, 12, Color(INK, 0.7))
	draw_string(font, court.end + Vector2(-68, 20), "(+10, -10)", HORIZONTAL_ALIGNMENT_LEFT, -1, 12, Color(INK, 0.7))
	if snapshot.is_empty():
		var center := court.get_center()
		draw_circle(world_to_canvas(Vector2(5, 0)), court.size.x * 0.025, GOLD)
		draw_string(font, center + Vector2(-104, -12), "함께 움직일 코트", HORIZONTAL_ALIGNMENT_LEFT, -1, 24, INK)
		draw_string(font, center + Vector2(-114, 21), "게스트로 연결하고 방을 만드세요", HORIZONTAL_ALIGNMENT_LEFT, -1, 15, Color(INK, 0.7))
		return
	var state: Dictionary = snapshot.get("state", {})
	var goal: Dictionary = state.get("goal", {})
	var goal_pos := world_to_canvas(Vector2(float(goal.get("x", 5)), float(goal.get("y", 0))))
	var radius := maxf(7.0, float(goal.get("radius", 0.5)) * court.size.x / 20.0)
	if goal.get("active", false):
		draw_circle(goal_pos, radius + 6.0, Color(GOLD, 0.24))
		draw_circle(goal_pos, radius, GOLD)
		draw_arc(goal_pos, radius + 4.0, 0, TAU, 40, INK, 1.5, true)
	else:
		draw_arc(goal_pos, radius, 0, TAU, 40, Color(INK, 0.3), 1.5, true)
	draw_string(font, goal_pos + Vector2(-25, -radius - 12), "목표" if goal.get("active", false) else "달성", HORIZONTAL_ALIGNMENT_LEFT, -1, 13, INK)
	var index := 0
	for value in state.get("players", []):
		var player: Dictionary = value
		var id: String = player.get("id", "")
		var own := id == own_id
		var connected: bool = player.get("connected", false)
		var pos := world_to_canvas(Vector2(float(player.get("x", 0)), float(player.get("y", 0))))
		var color: Color = FIRE if own else PEERS[index % PEERS.size()]
		if not connected:
			color = Color("9da9a5")
		if own:
			draw_arc(pos, 14.0, 0, TAU, 40, INK, 2.5, true)
		draw_circle(pos, 9.0, color)
		draw_arc(pos, 9.0, 0, TAU, 30, CANVAS, 1.5, true)
		if not connected:
			draw_line(pos - Vector2(4, 0), pos + Vector2(4, 0), INK, 2)
		var label := ("나 " if own else "") + id.left(5) + (" · 예약" if not connected else "")
		var offset := Vector2(13, -13 - (index % 3) * 14)
		if pos.x > court.end.x - 90:
			offset.x = -font.get_string_size(label, HORIZONTAL_ALIGNMENT_LEFT, -1, 13).x - 13
		if pos.y < court.position.y + 50:
			offset.y = 24 + (index % 3) * 14
		draw_string(font, pos + offset, label, HORIZONTAL_ALIGNMENT_LEFT, -1, 13, INK)
		index += 1
	if not live:
		draw_string(font, court.position + Vector2(16, 30), "연결 끊김 · 마지막 서버 상태", HORIZONTAL_ALIGNMENT_LEFT, -1, 17, INK)
