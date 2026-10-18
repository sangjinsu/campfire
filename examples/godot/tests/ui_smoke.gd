extends SceneTree

var failures: Array[String] = []

func check(condition: bool, label: String) -> void:
	if not condition:
		failures.append(label)
		push_error("UI_SMOKE_FAIL: " + label)

func _initialize() -> void:
	call_deferred("run")

func run() -> void:
	check(ResourceLoader.exists("res://scenes/main.tscn"), "main scene exists")
	if not failures.is_empty():
		quit(1)
		return
	var ui = load("res://scenes/main.tscn").instantiate()
	root.add_child(ui)
	await process_frame
	check(ui.get_node("%GuestConnect").disabled == false, "guest initially enabled")
	check(ui.get_node("%CreateRoom").disabled, "create disabled before auth")
	check(ui.get_node("%JoinRoom").disabled, "join disabled without room selection")
	check(ui.get_node("%Reconnect").disabled, "reconnect disabled without credentials")
	check(root.content_scale_mode == Window.CONTENT_SCALE_MODE_DISABLED, "small windows reflow without shrinking Korean text")
	var client = ui.client
	client.status = "lobby"
	client.is_authenticated = true
	client.status_changed.emit("lobby")
	check(ui.get_node("%ServerURL").editable == false, "server locked while authenticated")
	check(ui.get_node("%CreateRoom").disabled == false, "create enabled in lobby")
	client.rooms_received.emit([{"game_id":"arena", "room_id":"11111111-2222-3333-4444-555555555555", "room_instance_id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "members":7, "capacity":8}], "next-page")
	var rooms: ItemList = ui.get_node("%RoomList")
	rooms.select(0)
	rooms.item_selected.emit(0)
	check(ui.get_node("%JoinRoom").disabled == false, "selection enables join")
	check(ui.get_node("%NextRooms").disabled == false, "cursor enables next page")
	check(rooms.get_item_metadata(0).room_instance_id == "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee", "actual instance retained")
	client.guest_id = "self-player"
	client.room_id = "11111111-2222-3333-4444-555555555555"
	client.room_instance_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
	client.status = "in_room"
	client.is_in_room = true
	client.status_changed.emit("in_room")
	var snapshot = {"room_id":client.room_id, "room_instance_id":client.room_instance_id,"game_id":"arena", "server_tick":60,"snapshot_seq":30,"last_processed_input_seq":11,"state":{"players":[{"id":"self-player","x":2.0,"y":3.0,"connected":true},{"id":"peer-player","x":-3.0,"y":-1.0,"connected":false}], "shared_score":1,"goal":{"x":5.0,"y":0.0,"radius":0.5,"active":false}}}
	client.snapshot_received.emit(snapshot)
	check(ui.get_node("%ArenaView").snapshot.state.players.size() == 2, "arena displays authoritative peers")
	check("1" in ui.get_node("%Score").text, "shared score visible")
	check("11" in ui.get_node("%Stats").text, "processed ack visible")
	check(ui.get_node("%ArenaView").world_to_canvas(Vector2(0, 1)).y < ui.get_node("%ArenaView").world_to_canvas(Vector2.ZERO).y, "positive world y renders upwards")
	ui.get_node("%ServerURL").release_focus()
	var key = InputEventKey.new()
	key.physical_keycode = KEY_W
	key.pressed = true
	Input.parse_input_event(key)
	await process_frame
	check(ui.current_direction().y == 1.0, "W sends positive world y")
	ui.get_node("%ServerURL").editable = true
	ui.get_node("%ServerURL").grab_focus()
	check(ui.current_direction() == Vector2.ZERO, "text focus stops movement")
	ui.get_node("%ServerURL").release_focus()
	ui._notification(NOTIFICATION_APPLICATION_FOCUS_OUT)
	check(ui.current_direction() == Vector2.ZERO, "application focus loss stops movement")
	key.pressed = false
	Input.parse_input_event(key)
	ui._notification(NOTIFICATION_APPLICATION_FOCUS_IN)
	client.error_received.emit("ROOM_FULL", "Room is full", false)
	check("정원" in ui.get_node("%Notice").text, "room full explains next action")
	client.error_received.emit("RESUME_EXPIRED", "Reservation expired", false)
	check("만료" in ui.get_node("%Notice").text, "expiry explains next action")
	ui.size = Vector2(900,600)
	await process_frame
	check(ui.get_node("%ArenaView").size.x > 300, "minimum size leaves playable arena")
	check(ui.get_node("%ArenaView").get_global_rect().end.x <= 901, "minimum size no horizontal arena clipping")
	client.is_in_room = false
	client.status = "disconnected"
	client.status_changed.emit("disconnected")
	check(ui.current_direction() == Vector2.ZERO, "disconnect prevents movement")
	check(ui.get_node("%GuestConnect").disabled == false, "guest failure can retry")
	client.snapshot_received.emit(snapshot)
	client.room_id = ""
	client.room_instance_id = ""
	client.can_resume = false
	client.status = "lobby"
	client.is_authenticated = true
	client.authenticated.emit({"can_resume":false})
	check(ui.get_node("%ArenaView").snapshot.is_empty(), "expired resume clears old court")
	check(not "11111111" in ui.get_node("%RoomIdentity").text, "expired resume clears old identity")
	client.snapshot_received.emit(snapshot)
	client.status = "in_room"
	client.is_authenticated = false
	client.logout()
	check(ui.get_node("%ArenaView").snapshot.is_empty(), "logout clears old court")
	check(ui.get_node("%Score").text == "공동 점수  0", "logout resets old score")
	for code in ["RESUME_EXPIRED", "ROOM_NOT_FOUND", "STALE_INSTANCE"]:
		client.status = "resuming"
		client.is_authenticated = true
		client.is_in_room = false
		client.can_resume = true
		client.room_id = snapshot.room_id
		client.room_instance_id = snapshot.room_instance_id
		client.snapshot_received.emit(snapshot)
		client.authenticated.emit({"can_resume":true})
		client.room_id = ""
		client.room_instance_id = ""
		client.status = "lobby"
		client.status_changed.emit("lobby")
		client.error_received.emit(code, "Resume failed", false)
		check(ui.get_node("%ArenaView").snapshot.is_empty(), "resume race error clears court: " + code)
		check(ui.get_node("%Score").text == "공동 점수  0", "resume race error clears score: " + code)
		check(not "11111111" in ui.get_node("%RoomIdentity").text, "resume race error clears identity: " + code)
	client.status = "in_room"
	client.is_authenticated = true
	client.is_in_room = true
	client.room_id = snapshot.room_id
	client.room_instance_id = snapshot.room_instance_id
	client.status_changed.emit("in_room")
	ui._command("leave", true)
	client.snapshot_received.emit(snapshot)
	check(ui.get_node("%LeaveRoom").disabled, "ordinary snapshot preserves leave button lock")
	check(ui.get_node("%Logout").disabled, "ordinary snapshot preserves pending lock on logout")
	ui._command("logout", true)
	client.snapshot_received.emit(snapshot)
	check(ui.get_node("%Logout").disabled, "ordinary snapshot preserves logout button lock")
	client.room_left.emit()
	ui._command("join", true)
	client.room_entered.emit(snapshot)
	check(not ui.get_node("%LeaveRoom").disabled, "room entered full sync clears join lock")
	ui._command("leave", true)
	client.last_error_command = "game.input"
	client.error_received.emit("STALE_INPUT", "Input stale", false)
	check(ui.get_node("%LeaveRoom").disabled, "unrelated input error preserves leave lock")
	client.last_error_command = "room.leave"
	client.error_received.emit("SERVER_BUSY", "Try leave again", true)
	check(not ui.get_node("%LeaveRoom").disabled, "matching leave error releases pending lock")
	ui._command("logout", true)
	client.rooms_received.emit([], null)
	check(ui.get_node("%Logout").disabled, "unrelated list response preserves logout lock")
	client.last_error_command = "session.logout"
	client.error_received.emit("SERVER_BUSY", "Try logout again", true)
	check(not ui.get_node("%Logout").disabled, "matching logout error releases pending lock")
	ui.size = Vector2(900,600)
	await process_frame
	check(ui.get_node("%Notice").get_global_rect().position.y < 150, "notice remains visible above scroll rail")
	check(ui.get_node("%Notice").get_global_rect().end.x <= 901, "minimum viewport notice wraps inside width")
	check(ui.get_node("%ArenaView").size.y >= 260, "visible notice preserves minimum court height")
	ui.queue_free()
	await process_frame
	if failures.is_empty():
		print("UI_SMOKE_PASS")
	quit(0 if failures.is_empty() else 1)
