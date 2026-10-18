extends SceneTree

const ClientScript = preload("res://scripts/campfire_client.gd")
const MainScene = preload("res://scenes/main.tscn")
var server: String
var report_path: String
var capture_path: String
var main: Control
var peer: Node
var other_game: Node
var checks: Array = []
var failures: Array = []
var expected_errors: Array = []
var unexpected_errors: Array = []
var auth_info: Dictionary = {}
var peer_rooms: Array = []
var other_rooms: Array = []
var other_list_received := false
var peer_list_received := false
var expected_join_error := ""
var finished := false
var started_ms := 0

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	server = args[0]
	report_path = args[1]
	capture_path = args[2]
	started_ms = Time.get_ticks_msec()
	call_deferred("_run")

func _run() -> void:
	main = MainScene.instantiate()
	root.add_child(main)
	await process_frame
	main.get_node("%ServerURL").text = server
	main.get_node("%GameID").text = "arena"
	var client: Node = main.client
	client.error_received.connect(_unexpected_error)
	client.authenticated.connect(func(value: Dictionary): auth_info = value)
	await _click(main.get_node("%GuestConnect"))
	if not await _wait(func(): return client.status == "lobby", 10, "Native guest connect button authenticates"):
		return
	var guest: String = client.guest_id
	await _click(main.get_node("%CreateRoom"))
	if not await _wait(func(): return client.is_in_room, 6, "Native create button enters room"):
		return
	var room: String = client.room_id
	var instance: String = client.room_instance_id
	peer = ClientScript.new()
	root.add_child(peer)
	peer.error_received.connect(_unexpected_error)
	peer.rooms_received.connect(func(rooms: Array, _cursor: Variant): peer_rooms = rooms; peer_list_received = true)
	peer.connect_guest(server, "arena")
	if not await _wait(func(): return peer.status == "lobby", 8, "Second production adapter authenticates"):
		return
	peer.request_rooms()
	if not await _wait(func(): return peer_list_received, 6, "Production adapter lists rooms"):
		return
	var listed := false
	for item in peer_rooms:
		if item["room_id"] == room and item["room_instance_id"] == instance:
			listed = true
	if not _check(listed, "List includes exact created room instance"):
		return
	peer.join_room(room, instance)
	if not await _wait(func(): return peer.is_in_room and client.snapshot.get("state", {}).get("players", []).size() == 2, 6, "Same room remote membership reaches visible UI"):
		return
	client.set_process(false)
	await _click(main.get_node("%LeaveRoom"))
	if not _check(main.get_node("%LeaveRoom").disabled and main.get_node("%Logout").disabled, "Native leave request remains locked until server response is applied"):
		return
	client.set_process(true)
	if not await _wait(func(): return not client.is_in_room, 6, "Native leave button releases room"):
		return
	await _click(main.get_node("%RefreshRooms"))
	var room_list: ItemList = main.get_node("%RoomList")
	if not await _wait(func(): return _list_contains(room_list, room), 6, "Native refresh button populates ItemList with requested room"):
		return
	var selection := -1
	for index in range(room_list.item_count):
		var item: Dictionary = room_list.get_item_metadata(index)
		if item.get("room_id") == room:
			selection = index
	if not _check(selection >= 0, "UI room list preserves room identity metadata"):
		return
	room_list.select(selection)
	room_list.item_selected.emit(selection)
	await _click(main.get_node("%JoinRoom"))
	if not await _wait(func(): return client.is_in_room, 6, "Native join button enters selected room"):
		return
	var before := _position(client.snapshot, guest)
	main.get_node("%ArenaView").grab_focus()
	await process_frame
	var key := InputEventKey.new()
	key.physical_keycode = KEY_D
	key.keycode = KEY_D
	key.pressed = true
	Input.parse_input_event(key)
	await create_timer(0.8).timeout
	key = InputEventKey.new()
	key.physical_keycode = KEY_D
	key.keycode = KEY_D
	key.pressed = false
	Input.parse_input_event(key)
	await create_timer(0.2).timeout
	var after := _position(client.snapshot, guest)
	if not _check(after.x - before.x > 1.0 and client.last_processed_input_seq > 0, "Real D key event moves server-authoritative visible player with processed ack"):
		return
	if not _check(main.get_node("%Participants").text.contains("2"), "Native UI participant count reflects shared snapshot"):
		return
	if DisplayServer.get_name() != "headless":
		await RenderingServer.frame_post_draw
		var image := root.get_texture().get_image()
		if not _check(not image.is_empty() and image.save_png(capture_path) == OK, "Actual renderer saves live-server UI screenshot"):
			return
	await _click(main.get_node("%ForceDisconnect"))
	if not await _wait(func(): return client.status == "disconnected", 3, "Native force disconnect button closes transport"):
		return
	await create_timer(2.8).timeout
	await _click(main.get_node("%Reconnect"))
	if not await _wait(func(): return client.status == "lobby", 8, "Grace-expired same-token reconnect returns to lobby"):
		return
	if not _check(client.guest_id == guest and not auth_info.get("can_resume", true) and client.room_id.is_empty(), "Expired reservation cannot resume previous membership"):
		return
	if not _check(main.get_node("%ArenaView").snapshot.is_empty(), "Expired membership clears the visible stale room state"):
		return
	other_game = ClientScript.new()
	root.add_child(other_game)
	other_game.error_received.connect(func(code: String, message: String, retryable: bool):
		if code in ["ROOM_NOT_FOUND", "GAME_MISMATCH"]:
			expected_join_error = code
			expected_errors.append({"code": code, "message": message, "retryable": retryable})
		else:
			_unexpected_error(code, message, retryable))
	other_game.rooms_received.connect(func(rooms: Array, _cursor: Variant): other_rooms = rooms; other_list_received = true)
	other_game.connect_guest(server, "arena-b")
	if not await _wait(func(): return other_game.status == "lobby", 8, "Other game adapter authenticates"):
		return
	other_game.request_rooms()
	if not await _wait(func(): return other_list_received, 6, "Other game lists its namespace"):
		return
	for item in other_rooms:
		if not _check(item["room_id"] != room, "Other game list hides arena room"):
			return
	other_game.join_room(room, instance)
	if not await _wait(func(): return not expected_join_error.is_empty(), 6, "Cross-game join is rejected by server"):
		return
	if not _check(not other_game.is_in_room and peer.is_in_room, "Cross-game failure preserves original game membership"):
		return
	if not _check(other_game.is_authenticated and other_game.has_credentials(), "Cross-game join error preserves authenticated guest credentials"):
		return
	other_list_received = false
	other_game.request_rooms()
	if not await _wait(func(): return other_list_received, 6, "Other-game room list still succeeds after rejected cross-game join"):
		return
	client.join_room(room, instance)
	if not await _wait(func(): return client.is_in_room and not main.get_node("%ArenaView").snapshot.is_empty(), 6, "Rejoin restores live scene before logout"):
		return
	client.set_process(false)
	await _click(main.get_node("%Logout"))
	if not _check(main.get_node("%Logout").disabled and main.get_node("%LeaveRoom").disabled, "Native logout request remains locked until connection result is applied"):
		return
	client.set_process(true)
	if not await _wait(func(): return client.status == "disconnected" and not client.has_credentials(), 6, "Native logout button clears local guest credentials"):
		return
	if not _check(main.get_node("%ArenaView").snapshot.is_empty(), "Logout clears visible room state without room.left"):
		return
	_finish()

func _click(button: Button) -> void:
	await process_frame
	if button.disabled:
		failures.append("Button disabled: " + str(button.name))
		_finish()
		return
	# Native focus and ui_accept input exercise the button's GUI event path.
	button.grab_focus()
	await process_frame
	var event := InputEventAction.new()
	event.action = "ui_accept"
	event.pressed = true
	Input.parse_input_event(event)
	await process_frame
	event = InputEventAction.new()
	event.action = "ui_accept"
	event.pressed = false
	Input.parse_input_event(event)
	await process_frame

func _wait(predicate: Callable, timeout: float, label: String) -> bool:
	var deadline := Time.get_ticks_msec() + int(timeout * 1000)
	while Time.get_ticks_msec() < deadline and not finished:
		if predicate.call():
			return _check(true, label)
		await process_frame
	return _check(false, label + " (timeout)")

func _list_contains(items: ItemList, room: String) -> bool:
	for index in range(items.item_count):
		var data: Dictionary = items.get_item_metadata(index)
		if data.get("room_id") == room:
			return true
	return false

func _position(value: Dictionary, id: String) -> Vector2:
	for player in value.get("state", {}).get("players", []):
		if player["id"] == id:
			return Vector2(float(player["x"]), float(player["y"]))
	return Vector2(INF, INF)

func _check(condition: bool, label: String) -> bool:
	if condition:
		checks.append(label)
		return true
	failures.append(label)
	_finish()
	return false

func _unexpected_error(code: String, message: String, retryable: bool) -> void:
	unexpected_errors.append({"code": code, "message": message, "retryable": retryable})

func _finish() -> void:
	if finished:
		return
	finished = true
	var success := failures.is_empty() and unexpected_errors.is_empty()
	var report := {"success": success, "checks": checks, "failures": failures, "unexpected_errors": unexpected_errors, "expected_errors": expected_errors, "elapsed_seconds": float(Time.get_ticks_msec() - started_ms) / 1000.0, "display_driver": DisplayServer.get_name(), "capture": capture_path if DisplayServer.get_name() != "headless" else "", "grace_override_ms": 2000}
	var file := FileAccess.open(report_path, FileAccess.WRITE)
	file.store_string(JSON.stringify(report))
	for value in [peer, other_game]:
		if is_instance_valid(value):
			value.disconnect_for_test()
	if is_instance_valid(main):
		main.client.disconnect_for_test()
	quit(0 if success else 1)
