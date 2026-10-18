extends SceneTree

var failures: Array[String] = []
const Client = preload("res://scripts/campfire_client.gd")


func _initialize() -> void:
	call_deferred("_run")


func _run() -> void:
	_test_resume_uses_processed_ack()
	_test_snapshot_filters_foreign_and_stale_messages()
	_test_acceptance_is_not_processed_ack()
	_test_full_sync_signal_order()
	_test_retryable_authentication_and_resume_errors()
	_test_expired_reservation_and_token()
	_test_untrusted_numbers_and_arena_state()
	_test_session_binding_and_old_http_callback()
	_test_unknown_response_cannot_leave_room()
	_test_error_exposes_its_matching_command()
	_test_cross_game_command_rejection_preserves_valid_session()
	_test_confirmed_logout_clears_session()
	for failure in failures:
		print("FAIL: ", failure)
	if failures.is_empty():
		print("PASS: CampfireClient unit invariants")
	quit(0 if failures.is_empty() else 1)


func _client() -> Node:
	var client: Node = Client.new()
	client.game_id = "arena"
	client.guest_id = "guest-a"
	client.base_url = "http://localhost:3000"
	client._credential_base_url = client.base_url
	client._credential_game_id = "arena"
	client._token = "unit-memory-only"
	return client


func _test_resume_uses_processed_ack() -> void:
	var client: Node = _client()
	client.room_id = "room-a"
	client.room_instance_id = "instance-a"
	client._next_input_seq = 99
	client._direction = Vector2.RIGHT
	client._pending["old-input"] = {"type": "game.input"}
	_check(client._apply_snapshot(_snapshot("room.resumed", 7, 3), true), "resume full sync must be accepted")
	_check(client._next_input_seq == 4, "resume must use processed ack + 1, not old sent sequence")
	_check(client._direction == Vector2.ZERO, "resume must clear old movement direction")
	_check(client._pending.is_empty(), "resume must discard pending old inputs")
	client.free()


func _test_snapshot_filters_foreign_and_stale_messages() -> void:
	var client: Node = _client()
	client._apply_snapshot(_snapshot("room.entered", 7, 3), true)
	var foreign: Dictionary = _snapshot("snapshot", 8, 4)
	foreign["game_id"] = "another-game"
	_check(not client._apply_snapshot(foreign, false), "foreign game snapshots must be ignored")
	foreign["game_id"] = "arena"
	foreign["room_instance_id"] = "old-instance"
	_check(not client._apply_snapshot(foreign, false), "old room instances must be ignored")
	_check(not client._apply_snapshot(_snapshot("snapshot", 7, 4), false), "equal snapshot sequence must be ignored")
	_check(not client._apply_snapshot(_snapshot("snapshot", 6, 4), false), "older snapshot sequence must be ignored")
	_check(not client._apply_snapshot(_snapshot("snapshot", 8, 2), false), "processed ack cannot regress")
	_check(client._apply_snapshot(_snapshot("snapshot", 8, 4), false), "new snapshot must advance own processed ack")
	_check(client.last_processed_input_seq == 4 and client.snapshot_seq == 8, "applied snapshot must expose own ack and sequence")
	client.disconnect_for_test()
	_check(not client._apply_snapshot(_snapshot("snapshot", 9, 4), false), "disconnected client must not accept regular snapshots")
	_check(client.room_id == "room-a" and client.has_credentials(), "disconnect must retain in-memory reconnect identity")
	_check(client._direction == Vector2.ZERO and not client.is_in_room, "disconnect must stop control")
	client.free()


func _test_acceptance_is_not_processed_ack() -> void:
	var client: Node = _client()
	client._apply_snapshot(_snapshot("room.entered", 7, 3), true)
	client._pending["input-4"] = {"type": "game.input", "input_seq": 4}
	_check(client._receive_message({"v": 1, "type": "game.input.accepted", "request_id": "input-4", "input_seq": 4}), "matching input acceptance must be consumed")
	_check(client.last_processed_input_seq == 3, "receive acceptance must not advance processed ack")
	client.free()


func _test_full_sync_signal_order() -> void:
	var client: Node = _client()
	var events: Array[String] = []
	client.room_entered.connect(func(_value: Dictionary) -> void: events.append("entered"))
	client.snapshot_received.connect(func(_value: Dictionary) -> void: events.append("snapshot"))
	client._pending["create-1"] = {"type": "room.create"}
	var message: Dictionary = _snapshot("room.entered", 1, 0)
	message["request_id"] = "create-1"
	_check(client._receive_message(message), "matching room creation full sync must apply")
	_check(events == ["entered", "snapshot"], "room entry must be announced before its initial snapshot")
	client.free()


func _test_retryable_authentication_and_resume_errors() -> void:
	for kind: String in ["session.authenticate", "resume"]:
		var client: Node = _client()
		client.status = "authenticating" if kind == "session.authenticate" else "resuming"
		client.room_id = "room-a"
		client.room_instance_id = "instance-a"
		client._pending["busy-1"] = {"type": kind}
		_check(client._receive_message({"v": 1, "type": "error", "request_id": "busy-1", "code": "SERVER_BUSY", "message": "server capacity is temporarily exhausted", "retryable": true}), "retryable server error must be consumed")
		_check(client.status == "disconnected", "retryable auth/resume failure must allow an explicit reconnect")
		_check(client.has_credentials() and client.room_id == "room-a", "retryable error must retain token and reservation identity")
		client.free()


func _test_expired_reservation_and_token() -> void:
	var client: Node = _client()
	client.is_authenticated = true
	client._apply_snapshot(_snapshot("room.entered", 7, 3), true)
	client._pending["resume-1"] = {"type": "resume"}
	client._receive_message({"v": 1, "type": "error", "request_id": "resume-1", "code": "RESUME_EXPIRED", "message": "reservation expired", "retryable": false})
	_check(client.room_id.is_empty() and client.status == "lobby" and not client.is_in_room, "expired reservation must clear room and permit another join")
	_check(client.has_credentials(), "expired room reservation must preserve a still-valid guest")
	client._pending["auth-2"] = {"type": "session.authenticate"}
	client._receive_message({"v": 1, "type": "error", "request_id": "auth-2", "code": "UNAUTHENTICATED", "message": "invalid or expired token", "retryable": false})
	_check(not client.has_credentials() and client.guest_id.is_empty() and client.status == "disconnected", "expired token must be discarded without automatically issuing another guest")
	client.free()


func _test_untrusted_numbers_and_arena_state() -> void:
	var client: Node = _client()
	for invalid: Variant in ["1", true, -1, 1.25, INF, NAN, 9007199254740992.0]:
		var message: Dictionary = _snapshot("room.entered", 1, 0)
		message["snapshot_seq"] = invalid
		_check(not client._apply_snapshot(message, true), "invalid or unsafe sequence must be rejected: " + str(invalid))
	var malformed: Dictionary = _snapshot("room.entered", 1, 0)
	malformed["state"]["players"][0]["x"] = 1.0e30
	_check(not client._apply_snapshot(malformed, true), "finite but impossible arena coordinates must be rejected before rendering")
	malformed["state"]["players"][0]["x"] = 0
	malformed["state"]["players"].append(malformed["state"]["players"][0].duplicate(true))
	_check(not client._apply_snapshot(malformed, true), "duplicate player identities must be rejected")
	_check(not client._receive_text("[]"), "non-object server messages must be safely rejected")
	client.free()


func _test_session_binding_and_old_http_callback() -> void:
	var client: Node = _client()
	client._connection_epoch = 4
	client.status = "requesting_guest"
	client._on_guest_completed(HTTPRequest.RESULT_SUCCESS, 201, PackedStringArray(), JSON.stringify({"guest_id": "stale-guest", "token": "stale-token", "game_id": "arena", "expires_at": 9000000}).to_utf8_buffer(), 3)
	_check(client.guest_id == "guest-a", "cancelled old HTTP generation must not replace the current guest")
	client.status = "disconnected"
	client.base_url = "http://different-server:3000"
	_check(not client.reconnect(), "changing endpoint must prevent reuse of a token from another server")
	_check(not client.has_credentials(), "changed endpoint must purge the old token")
	client.free()


func _test_unknown_response_cannot_leave_room() -> void:
	var client: Node = _client()
	client._apply_snapshot(_snapshot("room.entered", 7, 3), true)
	_check(not client._receive_message({"v": 1, "type": "room.left", "request_id": "old-connection", "room_id": "room-a", "room_instance_id": "instance-a"}), "old or unknown request ID must not mutate membership")
	_check(client.is_in_room, "unknown room.left must preserve the active room")
	client._pending["leave-1"] = {"type": "room.leave", "room_id": "room-a", "room_instance_id": "instance-a"}
	_check(client._receive_message({"v": 1, "type": "room.left", "request_id": "leave-1", "room_id": "room-a", "room_instance_id": "instance-a"}), "matching leave acknowledgement must apply")
	_check(not client.is_in_room and client.room_id.is_empty() and client.snapshot.is_empty(), "explicit leave acknowledgement must immediately clear room state")
	client.free()


func _test_error_exposes_its_matching_command() -> void:
	var client: Node = _client()
	var observed_commands: Array = []
	client.error_received.connect(func(_code: String, _message: String, _retryable: bool) -> void: observed_commands.append(client.get("last_error_command")))
	client._pending["input-1"] = {"type": "game.input"}
	client._receive_message({"v": 1, "type": "error", "request_id": "input-1", "code": "STALE_INPUT", "message": "stale input", "retryable": false})
	client.send_direction(Vector2(INF, 0.0))
	client._pending["leave-1"] = {"type": "room.leave"}
	client._receive_message({"v": 1, "type": "error", "request_id": "leave-1", "code": "SERVER_BUSY", "message": "server busy", "retryable": true})
	client._pending["list-timeout"] = {"type": "room.list", "deadline": 0}
	client._check_timeouts(1)
	_check(observed_commands == ["game.input", "", "room.leave", "room.list"], "each emitted error must identify its own matched command before UI handlers run")
	client.free()


func _test_cross_game_command_rejection_preserves_valid_session() -> void:
	var client: Node = _client()
	client.is_authenticated = true
	client._apply_snapshot(_snapshot("room.entered", 7, 3), true)
	client._pending["cross-game-join"] = {"type": "room.join"}
	client._receive_message({"v": 1, "type": "error", "request_id": "cross-game-join", "code": "GAME_MISMATCH", "message": "room belongs to another game", "retryable": false})
	_check(client.is_authenticated and client.has_credentials(), "cross-game command rejection must preserve the valid original guest")
	_check(client.is_in_room and client.room_id == "room-a", "rejected cross-game command must not mutate an existing membership")
	client.free()
	client = _client()
	client._pending["wrong-game-auth"] = {"type": "session.authenticate"}
	client._receive_message({"v": 1, "type": "error", "request_id": "wrong-game-auth", "code": "GAME_MISMATCH", "message": "token belongs to another game", "retryable": false})
	_check(not client.has_credentials() and client.status == "disconnected", "authentication game mismatch must discard the incorrectly bound credential")
	client.free()


func _test_confirmed_logout_clears_session() -> void:
	var client: Node = _client()
	client.is_authenticated = true
	client._apply_snapshot(_snapshot("room.entered", 7, 3), true)
	client._pending["logout-1"] = {"type": "session.logout"}
	_check(client._receive_message({"v": 1, "type": "session.logged_out", "request_id": "logout-1"}), "matching logout acknowledgement must be consumed")
	_check(client.status == "disconnected" and not client.has_credentials() and client.room_id.is_empty() and client.snapshot.is_empty(), "confirmed logout must clear both credential and room state")
	client.free()


func _snapshot(kind: String, sequence: int, ack: int) -> Dictionary:
	return {
		"v": 1, "type": kind, "game_id": "arena", "room_id": "room-a",
		"room_instance_id": "instance-a", "server_tick": 12, "snapshot_seq": sequence,
		"last_processed_input_seq": ack,
		"state": {
			"players": [{"id": "guest-a", "x": 0.0, "y": 0.0, "connected": true}],
			"shared_score": 0,
			"goal": {"x": 5.0, "y": 0.0, "radius": 0.5, "active": true}
		}
	}


func _check(condition: bool, message: String) -> void:
	if not condition:
		failures.append(message)
