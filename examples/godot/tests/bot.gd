extends SceneTree

const ClientScript = preload("res://scripts/campfire_client.gd")
var client: Node
var config: Dictionary
var work: String
var index: int
var count: int
var phase := "connect"
var start_ms := 0
var phase_ms := 0
var started_ms := 0
var reconnect_done := false
var reconnect_pending := false
var before_reconnect: Dictionary = {}
var errors: Array = []
var observations: Array = []
var gaps: Array = []
var last_snapshot_ms := 0
var snapshot_count := 0
var max_members := 0
var min_x := INF
var max_x := -INF
var min_y := INF
var max_y := -INF
var initial_guest := ""
var initial_room := ""
var initial_instance := ""
var monotonic := true
var previous_seq := -1
var previous_tick := -1
var previous_ack := -1
var resumed: Dictionary = {}
var finished := false
var remote_disconnect: Array = []
var saw_remote_return := false
var reconnect_target := ""

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() != 3:
		push_error("Expected config path, work directory, bot index")
		quit(2)
		return
	config = JSON.parse_string(FileAccess.get_file_as_string(args[0]))
	work = args[1]
	index = int(args[2])
	count = int(config["count"])
	start_ms = Time.get_ticks_msec()
	call_deferred("_start")

func _start() -> void:
	client = ClientScript.new()
	root.add_child(client)
	client.error_received.connect(_on_error)
	client.snapshot_received.connect(_on_snapshot)
	client.room_entered.connect(_on_entered)
	client.room_resumed.connect(_on_resumed)
	client.authenticated.connect(_on_authenticated)
	client.rooms_received.connect(_on_rooms)
	client.connect_guest(str(config["server"]), "arena")

func _on_authenticated(_info: Dictionary) -> void:
	if initial_guest.is_empty():
		initial_guest = client.guest_id
		if index == 0:
			client.create_room(count)
		else:
			phase = "discover"
			phase_ms = 0

func _on_entered(value: Dictionary) -> void:
	initial_room = client.room_id
	initial_instance = client.room_instance_id
	if index == 0:
		_write("room.json", {"room_id": initial_room, "room_instance_id": initial_instance})
	phase = "ready"
	_write("ready-%d.json" % index, {"guest_id": initial_guest, "room_id": initial_room, "room_instance_id": initial_instance})
	_on_snapshot(value)

func _on_rooms(rooms: Array, _cursor: Variant) -> void:
	var room: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(work.path_join("room.json")))
	for entry in rooms:
		if entry["room_id"] == room["room_id"]:
			if entry["room_instance_id"] != room["room_instance_id"]:
				_fail("Room list instance mismatch")
				return
			phase = "joining"
			client.join_room(str(entry["room_id"]), str(entry["room_instance_id"]))
			return
	phase = "discover"

func _on_resumed(value: Dictionary) -> void:
	resumed = {"guest_id": client.guest_id, "room_id": client.room_id, "room_instance_id": client.room_instance_id, "snapshot_seq": value["snapshot_seq"], "server_tick": value["server_tick"], "ack": value["last_processed_input_seq"]}
	if client.guest_id != initial_guest or client.room_id != initial_room or client.room_instance_id != initial_instance:
		_fail("Reconnect identity changed")
		return
	if int(value["snapshot_seq"]) < int(before_reconnect["snapshot_seq"]) or int(value["server_tick"]) < int(before_reconnect["server_tick"]) or int(value["last_processed_input_seq"]) < int(before_reconnect["ack"]):
		_fail("Reconnect sequence regressed")
		return
	reconnect_done = true
	reconnect_pending = false
	phase = "running"

func _on_snapshot(value: Dictionary) -> void:
	var now := Time.get_ticks_msec()
	var seq := int(value["snapshot_seq"])
	var tick := int(value["server_tick"])
	var ack := int(value["last_processed_input_seq"])
	if seq < previous_seq or tick < previous_tick or ack < previous_ack:
		monotonic = false
	previous_seq = seq
	previous_tick = tick
	previous_ack = ack
	if last_snapshot_ms > 0 and phase == "running" and not reconnect_pending:
		gaps.append(now - last_snapshot_ms)
	last_snapshot_ms = now
	snapshot_count += 1
	var state: Dictionary = value["state"]
	if index != count - 1 and reconnect_target.is_empty() and FileAccess.file_exists(work.path_join("ready-%d.json" % (count - 1))):
		var target: Dictionary = JSON.parse_string(FileAccess.get_file_as_string(work.path_join("ready-%d.json" % (count - 1))))
		reconnect_target = str(target["guest_id"])
	for remote in state["players"]:
		if remote["id"] == reconnect_target:
			if not remote["connected"]:
				if remote_disconnect.size() < 64:
					remote_disconnect.append({"x": remote["x"], "y": remote["y"], "snapshot_seq": seq, "server_tick": tick})
			elif not remote_disconnect.is_empty():
				saw_remote_return = true
	max_members = maxi(max_members, state["players"].size())
	for player in state["players"]:
		if player["id"] == client.guest_id:
			min_x = minf(min_x, float(player["x"]))
			max_x = maxf(max_x, float(player["x"]))
			min_y = minf(min_y, float(player["y"]))
			max_y = maxf(max_y, float(player["y"]))
	if seq % 15 == 0 and phase == "running":
		if observations.size() >= 120:
			observations.pop_front()
		observations.append({"snapshot_seq": seq, "server_tick": tick, "ack": ack, "state": state.duplicate(true)})

func _on_error(code: String, message: String, retryable: bool) -> void:
	errors.append({"code": code, "message": message, "retryable": retryable})
	_fail("Protocol error: " + code)

func _process(_delta: float) -> bool:
	if finished or not is_instance_valid(client):
		return false
	var now := Time.get_ticks_msec()
	if now - start_ms > int(config["duration"] * 1000) + 40000:
		_fail("Bot timeout: " + phase)
		return false
	if phase == "discover" and now - phase_ms > 250 and FileAccess.file_exists(work.path_join("room.json")):
		phase_ms = now
		phase = "listing"
		client.request_rooms()
	if phase == "ready" and FileAccess.file_exists(work.path_join("start.json")):
		started_ms = now
		phase = "running"
	if phase == "running":
		var elapsed := float(now - started_ms) / 1000.0
		var sign_value := 1.0 if int(elapsed / 2.0) % 2 == 0 else -1.0
		var direction := Vector2(sign_value, 0.0) if index % 2 == 0 else Vector2(0.0, sign_value)
		client.send_direction(direction)
		if index == count - 1 and not reconnect_done and elapsed > 6.0:
			before_reconnect = {"snapshot_seq": client.snapshot_seq, "server_tick": client.server_tick, "ack": client.last_processed_input_seq}
			reconnect_pending = true
			phase = "disconnected"
			phase_ms = now
			client.disconnect_for_test()
		if FileAccess.file_exists(work.path_join("finish.json")):
			client.send_direction(Vector2.ZERO)
			phase = "settle"
			phase_ms = now
	if phase == "disconnected" and now - phase_ms > 700:
		phase = "reconnecting"
		client.reconnect()
	if phase == "settle" and now - phase_ms > 500:
		_finish(0, "")
	return false

func _write(name: String, data: Dictionary) -> void:
	var file := FileAccess.open(work.path_join(name), FileAccess.WRITE)
	file.store_string(JSON.stringify(data))

func _fail(reason: String) -> void:
	_finish(1, reason)

func _finish(code: int, reason: String) -> void:
	if finished:
		return
	finished = true
	var distance := maxf(max_x - min_x, max_y - min_y) if is_finite(min_x) else 0.0
	_write("report-%d.json" % index, {"index": index, "success": code == 0, "failure": reason, "guest_id": initial_guest, "room_id": initial_room, "room_instance_id": initial_instance, "duration_seconds": float(Time.get_ticks_msec() - started_ms) / 1000.0 if started_ms > 0 else 0.0, "snapshot_count": snapshot_count, "max_members": max_members, "own_position_span": distance, "last_processed_input_seq": previous_ack, "snapshot_seq": previous_seq, "server_tick": previous_tick, "monotonic": monotonic, "snapshot_gaps_ms": gaps, "reconnect_before": before_reconnect, "reconnect_resumed": resumed, "reconnect_done": reconnect_done, "errors": errors, "observations": observations, "remote_disconnect": remote_disconnect, "saw_remote_return": saw_remote_return})
	client.disconnect_for_test()
	quit(code)
