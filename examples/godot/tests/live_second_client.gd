extends SceneTree

# Visible QA companion: the production scene and adapter remain unchanged.
const MainScene = preload("res://scenes/main.tscn")
var main: Control
var client: Node
var config: Dictionary = {}
var work: String = ""
var ready := false
var busy := false
var frozen := false
var next_poll_ms := 0
var next_live_ms := 0
var command_ids: Array[String] = []
var history: Array = []
var errors: Array = []
var started_ms := 0

func _initialize() -> void:
	started_ms = Time.get_ticks_msec()
	var args := OS.get_cmdline_user_args()
	if args.size() != 1:
		push_error("Expected one live companion config path")
		return
	var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(args[0]))
	if typeof(parsed) != TYPE_DICTIONARY:
		push_error("Invalid live companion config")
		return
	config = parsed
	work = str(config["work"])
	call_deferred("_start")

func _start() -> void:
	main = MainScene.instantiate()
	root.add_child(main)
	await process_frame
	client = main.client
	client.error_received.connect(_on_error)
	client.rooms_received.connect(_on_rooms)
	client.snapshot_received.connect(_on_snapshot)
	main.get_node("%ServerURL").text = str(config["server"])
	main.get_node("%GameID").text = str(config["game"])
	main.get_node("%GuestConnect").pressed.emit()
	if not await _wait(func(): return client.status == "lobby", 10.0, "Guest authentication"):
		return
	main.get_node("%RefreshRooms").pressed.emit()
	var room_list: ItemList = main.get_node("%RoomList")
	if not await _wait(func(): return _find_room(room_list) >= 0, 8.0, "Exact room instance listed"):
		return
	var index := _find_room(room_list)
	room_list.select(index)
	room_list.item_selected.emit(index)
	main.get_node("%JoinRoom").pressed.emit()
	if not await _wait(func(): return client.is_in_room and client.room_id == str(config["room_id"]) and client.room_instance_id == str(config["room_instance_id"]), 8.0, "Exact room instance joined"):
		return
	if not await _wait(func(): return client.snapshot.get("state", {}).get("players", []).size() == 2, 5.0, "Two real GUI guests in room"):
		return
	if not _has_expected_remote():
		_on_error("LIVE_QA_IDENTITY", "Expected first GUI guest missing", false)
		return
	ready = true
	_write("joined.json", _report())
	await _capture("b-joined")
	_write("live.json", _report())
	print("LIVE_SECOND_CLIENT_JOINED: two real GUI guests; production scene handlers")

func _has_expected_remote() -> bool:
	for player in client.snapshot.get("state", {}).get("players", []):
		if player["id"] == str(config["expected_remote_id"]) and player["connected"]:
			return true
	return false

func _find_room(items: ItemList) -> int:
	for index in range(items.item_count):
		var item: Dictionary = items.get_item_metadata(index)
		if item.get("room_id") == config["room_id"] and item.get("room_instance_id") == config["room_instance_id"]:
			return index
	return -1

func _wait(predicate: Callable, seconds: float, label: String) -> bool:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000.0)
	while Time.get_ticks_msec() < deadline:
		if predicate.call():
			return true
		await process_frame
	_on_error("LIVE_QA_TIMEOUT", label, false)
	_write("b-startup-failure.json", _report())
	await _capture("b-startup-failed")
	return false

func _process(_delta: float) -> bool:
	if not ready or frozen:
		return false
	var now := Time.get_ticks_msec()
	if now >= next_live_ms:
		next_live_ms = now + 250
		_write("live.json", _report())
	if not busy and now >= next_poll_ms:
		next_poll_ms = now + 100
		var path := work.path_join("command.json")
		if FileAccess.file_exists(path):
			var parsed: Variant = JSON.parse_string(FileAccess.get_file_as_string(path))
			if typeof(parsed) == TYPE_DICTIONARY:
				var command: Dictionary = parsed
				var id: String = str(command.get("id", ""))
				if _safe_id(id) and not command_ids.has(id):
					command_ids.append(id)
					if command_ids.size() > 128:
						command_ids.pop_front()
					busy = true
					_execute(command)
	return false

func _execute(command: Dictionary) -> void:
	var id := "b-" + str(command["id"])
	var kind := str(command.get("command", ""))
	match kind:
		"capture":
			await _capture(id)
		"move_b":
			main.set_process(false)
			client.send_direction(Vector2.ZERO)
			await create_timer(0.15).timeout
			var before: Dictionary = _report()
			await _capture(id + "-before")
			var deadline := Time.get_ticks_msec() + 800
			while Time.get_ticks_msec() < deadline:
				# The adapter transmits at its existing 20Hz; no OS key event is claimed.
				client.send_direction(Vector2.RIGHT)
				await create_timer(0.05).timeout
			client.send_direction(Vector2.ZERO)
			await create_timer(0.25).timeout
			main.set_process(true)
			var after: Dictionary = _report()
			var own_before := _own_position(before)
			var own_after := _own_position(after)
			var moved: bool = own_after.x - own_before.x > 1.0 and int(after["last_processed_input_seq"]) > int(before["last_processed_input_seq"])
			await _capture(id + "-after")
			_write(id + "-movement.json", {"success": moved, "input_method": "background production adapter direction; main process paused for 800ms movement, then zero and restored", "direction": {"x": 1, "y": 0}, "before": before, "after": after, "main_process_restored": main.is_processing()})
		"handoff", "freeze":
			client.send_direction(Vector2.ZERO)
			main.set_process(true)
			await create_timer(0.25).timeout
			await _capture(id)
			frozen = true
			var final_report: Dictionary = _report()
			final_report["handoff"] = "helper file writes and command polling stopped; production scene/client/window remain running"
			_write("live.json", final_report)
			_write("handoff.json", final_report)
		_:
			_write(id + "-error.json", {"error": "Unknown live QA command", "command": kind})
	busy = false

func _safe_id(value: String) -> bool:
	if value.is_empty() or value.length() > 64:
		return false
	for letter in value:
		if not (letter.is_valid_identifier() or letter.is_valid_int() or letter == "-"):
			return false
	return true

func _own_position(report: Dictionary) -> Vector2:
	for player in report.get("players", []):
		if player["id"] == report["guest_id"]:
			return Vector2(float(player["x"]), float(player["y"]))
	return Vector2(INF, INF)

func _on_snapshot(value: Dictionary) -> void:
	if frozen:
		return
	if history.size() >= 32:
		history.pop_front()
	history.append({"snapshot_seq": value["snapshot_seq"], "server_tick": value["server_tick"], "last_processed_input_seq": value["last_processed_input_seq"], "players": value["state"]["players"].duplicate(true)})

func _on_rooms(rooms: Array, next_cursor: Variant) -> void:
	if not frozen:
		_write("b-room-list.json", {"rooms": rooms, "next_cursor": next_cursor})

func _on_error(code: String, message: String, retryable: bool) -> void:
	if frozen:
		return
	if errors.size() >= 32:
		errors.pop_front()
	errors.append({"code": code, "message": message, "retryable": retryable})
	if not work.is_empty():
		_write("b-errors.json", {"errors": errors})

func _report() -> Dictionary:
	return {"ready": ready, "frozen": frozen, "pid": OS.get_process_id(), "utc": Time.get_datetime_string_from_system(true), "elapsed_ms": Time.get_ticks_msec() - started_ms, "display_driver": DisplayServer.get_name(), "status": client.status, "guest_id": client.guest_id, "game_id": client.game_id, "room_id": client.room_id, "room_instance_id": client.room_instance_id, "last_processed_input_seq": client.last_processed_input_seq, "server_tick": client.server_tick, "snapshot_seq": client.snapshot_seq, "players": client.snapshot.get("state", {}).get("players", []).duplicate(true), "snapshot": client.snapshot.duplicate(true), "recent_snapshots": history.duplicate(true), "ui_participants": main.get_node("%Participants").text, "ui_stats": main.get_node("%Stats").text, "main_process_enabled": main.is_processing(), "errors": errors.duplicate(true)}

func _capture(label: String) -> void:
	await RenderingServer.frame_post_draw
	var result: Error = root.get_texture().get_image().save_png(work.path_join(label + ".png"))
	var report: Dictionary = _report()
	report["capture_error"] = int(result)
	report["capture"] = label + ".png"
	_write(label + ".json", report)

func _write(name: String, data: Dictionary) -> void:
	var file := FileAccess.open(work.path_join(name), FileAccess.WRITE)
	if file != null:
		file.store_string(JSON.stringify(data, "\t"))
		file.close()
