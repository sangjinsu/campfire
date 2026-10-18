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
	_test_account_session_identity_and_alias()
	_test_inventory_private_projection_and_profile_boundary()
	_test_command_retry_and_terminal_result()
	_test_full_sync_command_recovery_and_membership_reset()
	_test_command_resume_retries_uncertain_payload_and_deduplicates_results()
	_test_missing_retained_result_blocks_unsafe_progress()
	_test_authentication_requires_both_runtime_aliases()
	_test_absent_private_projection_clears_previous_inventory()
	_test_account_http_errors_keep_codes_without_sensitive_echo()
	_test_account_http_binding_cannot_change_midflight()
	_test_live_context_change_discards_recipient_data()
	_test_authentication_new_membership_clears_old_command_history()
	_test_inventory_requires_command_sync_metadata()
	for failure in failures:
		print("FAIL: ", failure)
	if failures.is_empty():
		print("PASS: CampfireClient unit invariants")
	quit(0 if failures.is_empty() else 1)


func _client() -> Node:
	var client: Node = Client.new()
	client.game_id = "arena"
	client.guest_id = "guest-a"
	client.player_id = "guest-a"
	client._credential_player_id = "guest-a"
	client._credential_profile = "arena"
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


class RecordingClient:
	extends "res://scripts/campfire_client.gd"
	var sent: Array[Dictionary] = []
	func _send_command(kind: String, fields: Dictionary) -> bool:
		sent.append({"type": kind, "fields": fields.duplicate(true)})
		var request: String = "test-%d" % sent.size()
		var entry: Dictionary = fields.duplicate(true)
		entry.merge({"type": kind, "deadline": 100})
		_pending[request] = entry
		return true
	func _open_socket() -> bool:
		return true


func _inventory_client() -> Node:
	var client: Node = RecordingClient.new()
	client.game_id = "inventory"
	client.guest_id = "account-a"
	client.set("player_id", "account-a")
	client.set("game_profile", "inventory")
	client.base_url = "http://localhost:3001"
	client._credential_base_url = client.base_url
	client._credential_game_id = "inventory"
	client._credential_profile = "inventory"
	client._credential_player_id = "account-a"
	client._token = "memory-token"
	client.is_authenticated = true
	return client


func _inventory_snapshot(kind: String, sequence: int, ack: int = 0) -> Dictionary:
	return {
		"v": 1, "type": kind, "game_id": "inventory", "room_id": "room-a",
		"room_instance_id": "instance-a", "server_tick": 12, "snapshot_seq": sequence,
		"last_processed_input_seq": 0, "last_processed_command_seq": ack,
		"state": {"total_collected": ack, "players": 2},
		"private_state": {"inventory": ["alice-starter-item"]}, "command_results": []
	}


func _command_result(sequence: int, request: String, successful: bool = true) -> Dictionary:
	var result: Dictionary = {"v": 1, "type": "game.command.result", "request_id": request,
		"game_id": "inventory", "room_id": "room-a", "room_instance_id": "instance-a",
		"command_seq": sequence, "ok": successful}
	if successful:
		result["result"] = {"inventory_count": sequence + 1}
	else:
		result["error"] = {"code": "INVALID_ITEM", "message": "rejected item", "retryable": false}
	return result


func _test_account_session_identity_and_alias() -> void:
	var client: Node = RecordingClient.new()
	_check(client.has_method("connect_account"), "account credentials must be exchangeable without guest issuance")
	if not client.has_method("connect_account"):
		client.free()
		return
	client.game_id = "inventory"
	client.game_profile = "inventory"
	client.base_url = "http://localhost:3001"
	client.status = "requesting_account"
	client._connection_epoch = 5
	var issued: PackedByteArray = JSON.stringify({"player_id": "account-a", "token": "runtime-token", "game_id": "inventory", "expires_at": 9000000}).to_utf8_buffer()
	client._on_account_completed(HTTPRequest.RESULT_SUCCESS, 201, PackedStringArray(), issued, 4)
	_check(client.player_id.is_empty(), "stale account HTTP callback must be ignored")
	client._on_account_completed(HTTPRequest.RESULT_SUCCESS, 201, PackedStringArray(), issued, 5)
	_check(client.player_id == "account-a" and client.guest_id == "account-a", "account player ID must become the runtime identity and compatible alias")
	var auth: Dictionary = {"game_id": "inventory", "player_id": "account-a", "guest_id": "account-a", "expires_at": 9000000,
		"can_resume": false, "room_id": null, "room_instance_id": null, "reservation_expires_at": null}
	_check(client._valid_authentication(auth), "authenticated response must match both identity fields")
	auth["player_id"] = "other-player"
	_check(not client._valid_authentication(auth), "conflicting authenticated player ID must be rejected")
	client.free()


func _test_inventory_private_projection_and_profile_boundary() -> void:
	var probe: Node = _client()
	_check(probe.has_method("set_game_profile"), "explicit game profile selection must be supported")
	if not probe.has_method("set_game_profile"):
		probe.free()
		return
	_check(not probe.set_game_profile("unknown"), "unknown game profile must fail closed")
	_check(probe.set_game_profile("inventory"), "disconnected profile can switch to inventory")
	_check(not probe.has_credentials(), "profile changes must discard identity binding")
	probe.free()
	var client: Node = _inventory_client()
	_check(client._apply_snapshot(_inventory_snapshot("room.entered", 1), true), "inventory public counters must be accepted without arena shape")
	_check(client.private_state == {"inventory": ["alice-starter-item"]}, "recipient inventory must have its separate projection")
	_check(not client.snapshot.has("private_state") and not client.snapshot.has("command_results"), "public snapshot must never merge private state or command history")
	_check(not client.send_direction(Vector2.RIGHT), "inventory profile must not send unsupported movement inputs")
	var malformed: Dictionary = _inventory_snapshot("snapshot", 2)
	malformed["private_state"]["inventory"] = [17]
	_check(not client._apply_snapshot(malformed, false), "malformed private inventory must be rejected atomically")
	_check(client.snapshot_seq == 1, "malformed private data must not advance public snapshot")
	client.disconnect_for_test()
	_check(client.private_state.is_empty(), "disconnect must hide the previous recipient's private state immediately")
	client.free()


func _test_command_retry_and_terminal_result() -> void:
	var probe: Node = _client()
	_check(probe.has_method("send_game_command"), "single actions must have a game.command API")
	if not probe.has_method("send_game_command"):
		probe.free()
		return
	probe.free()
	var client: Node = _inventory_client()
	client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
	var payload: Dictionary = {"action": "collect", "item": "wood"}
	_check(client.send_game_command(payload), "first command must send")
	payload["item"] = "stone"
	_check(client.sent[0]["fields"]["command_seq"] == 1, "membership first command sequence must be one")
	_check(not client.send_game_command({"action": "collect", "item": "key"}), "unconfirmed command must block a new action")
	client._receive_message({"v": 1, "type": "error", "request_id": "test-1", "code": "SERVER_BUSY", "message": "busy", "retryable": true})
	_check(client.has_pending_game_command() and client.can_retry_pending_command(), "nonterminal busy must preserve a retryable same-sequence action")
	_check(client.retry_pending_command(), "busy command must allow explicit retry")
	_check(client.sent[1]["fields"]["command_seq"] == 1 and client.sent[1]["fields"]["payload"]["item"] == "wood", "retry must preserve deep-copied sequence and payload")
	var events: Array = []
	client.command_result_received.connect(func(value: Dictionary) -> void: events.append(value))
	_check(client._receive_message(_command_result(1, "test-2", false)), "terminal game failure must resolve the matching action")
	_check(not client.has_pending_game_command() and client.last_processed_command_seq == 1, "terminal failure consumes sequence and clears pending action")
	_check(client.send_game_command({"action": "collect", "item": "key"}), "a new action may follow a terminal game failure")
	_check(client.sent[2]["fields"]["command_seq"] == 2, "next command must be contiguous after terminal game failure")
	_check(not client._receive_message(_command_result(1, "unknown")), "unsolicited command result must not mutate another pending request")
	_check(events.size() == 1, "terminal result must emit once")
	client._next_command_seq = Client.MAX_SAFE_INTEGER + 1
	client._clear_game_commands()
	client._next_command_seq = Client.MAX_SAFE_INTEGER + 1
	_check(not client.send_game_command({"action": "collect", "item": "wood"}), "unsafe JSON command sequence must never be sent")
	client.free()


func _test_full_sync_command_recovery_and_membership_reset() -> void:
	var probe: Node = _client()
	if not probe.has_method("send_game_command"):
		probe.free()
		return
	probe.free()
	var client: Node = _inventory_client()
	client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
	client.send_game_command({"action": "collect", "item": "wood"})
	client._check_timeouts(101)
	_check(client.has_pending_game_command() and client.status == "disconnected", "timeout must preserve uncertain action until authoritative resume")
	client.is_authenticated = true
	var resumed: Dictionary = _inventory_snapshot("room.resumed", 2, 1)
	resumed["command_results"] = [_command_result(1, "original-request")]
	_check(client._apply_snapshot(resumed, true), "resume must reconcile retained terminal results")
	_check(not client.has_pending_game_command() and client.command_results.size() == 1, "retained result must resolve pending action without replaying mutation")
	_check(client.send_game_command({"action": "collect", "item": "stone"}), "next action must unlock after retained result")
	_check(client.sent[-1]["fields"]["command_seq"] == 2, "resume must derive command sequence from authoritative command acknowledgement")
	client._clear_room()
	_check(client.private_state.is_empty() and client.command_results.is_empty() and not client.has_pending_game_command(), "membership teardown must clear all recipient-local data")
	client.free()


func _test_command_resume_retries_uncertain_payload_and_deduplicates_results() -> void:
	var client: Node = _inventory_client()
	client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
	client.send_game_command({"action": "collect", "item": "wood"})
	client.disconnect_for_test()
	client.is_authenticated = true
	_check(client._apply_snapshot(_inventory_snapshot("room.resumed", 2), true), "unprocessed action may resume at the same membership")
	_check(client.can_retry_pending_command(), "unprocessed action must remain retryable after authoritative resume")
	_check(client.retry_pending_command(), "resume retry must use pending payload")
	_check(client.sent[-1]["fields"]["command_seq"] == 1 and client.sent[-1]["fields"]["payload"] == {"action": "collect", "item": "wood"}, "resume must retry same sequence and payload")
	var events: Array = []
	client.command_result_received.connect(func(value: Dictionary) -> void: events.append(value))
	var foreign: Dictionary = _command_result(1, "test-2")
	foreign["room_instance_id"] = "old-instance"
	_check(not client._receive_message(foreign) and client.has_pending_game_command(), "foreign epoch result must not resolve current command")
	client._receive_message(_command_result(1, "test-2"))
	var resumed: Dictionary = _inventory_snapshot("room.resumed", 3, 1)
	resumed["command_results"] = [_command_result(1, "old-request-id")]
	client._apply_snapshot(resumed, true)
	client._publish_recipient_state()
	_check(events.size() == 1 and client.command_results.size() == 1, "full sync replay must not emit a terminal result twice")
	client._clear_room()
	client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
	client.send_game_command({"action": "collect", "item": "key"})
	_check(client.sent[-1]["fields"]["command_seq"] == 1, "new membership must restart command sequence at one")
	client.free()


func _test_missing_retained_result_blocks_unsafe_progress() -> void:
	var client: Node = _inventory_client()
	var errors: Array[String] = []
	client.error_received.connect(func(code: String, _message: String, _retryable: bool) -> void: errors.append(code))
	client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
	client.send_game_command({"action": "collect", "item": "wood"})
	client.disconnect_for_test()
	client.is_authenticated = true
	client._apply_snapshot(_inventory_snapshot("room.resumed", 2, 2), true)
	_check(client.has_pending_game_command(), "ack without retained terminal result must not fabricate a successful action")
	_check(errors.has("COMMAND_RESULT_UNAVAILABLE"), "missing retained result must explain required membership recovery")
	_check(not client.can_retry_pending_command(), "evicted result must not invite a doomed stale-command retry")
	_check(not client.send_game_command({"action": "collect", "item": "stone"}), "unknown outcome must block a new action even if server ack advances")
	client.free()


func _test_authentication_requires_both_runtime_aliases() -> void:
	var client: Node = _client()
	var auth: Dictionary = {"game_id": "arena", "guest_id": "guest-a", "expires_at": 9000000,
		"can_resume": false, "room_id": null, "room_instance_id": null, "reservation_expires_at": null}
	_check(not client._valid_authentication(auth), "authentication must not accept a missing player_id alias")
	auth["player_id"] = "guest-a"
	_check(client._valid_authentication(auth), "matching guest and player aliases must authenticate")
	client.free()


func _test_absent_private_projection_clears_previous_inventory() -> void:
	var client: Node = _inventory_client()
	client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
	var message: Dictionary = _inventory_snapshot("snapshot", 2)
	message.erase("private_state")
	_check(client._apply_snapshot(message, false), "private snapshot omission must be accepted")
	_check(client.private_state.is_empty(), "omitted private snapshot must clear previous inventory rather than reuse it")
	client.free()


func _test_account_http_errors_keep_codes_without_sensitive_echo() -> void:
	for code: String in ["ACCOUNT_AUTH_DISABLED", "UNAUTHENTICATED", "AUTH_TIMEOUT"]:
		var client: Node = RecordingClient.new()
		client.game_id = "inventory"
		client.game_profile = "inventory"
		client.base_url = "http://localhost:3001"
		client.status = "requesting_account"
		var seen: Array = []
		client.error_received.connect(func(kind: String, text: String, _retryable: bool) -> void: seen.append([kind, text, client.last_error_command]))
		var body: PackedByteArray = JSON.stringify({"v": 1, "type": "error", "request_id": null, "code": code, "message": "sensitive-credential-echo", "retryable": false}).to_utf8_buffer()
		client._on_account_completed(HTTPRequest.RESULT_SUCCESS, 501 if code == "ACCOUNT_AUTH_DISABLED" else 401, PackedStringArray(), body, 0)
		_check(seen.size() == 1 and seen[0][0] == code, "HTTP account failure must expose the server's bounded error code: " + code)
		_check(not str(seen).contains("sensitive-credential-echo"), "HTTP failure must not echo untrusted body diagnostics")
		_check(seen.size() == 1 and seen[0][2] == "session.account", "HTTP account error must identify its operation")
		client.free()


func _test_account_http_binding_cannot_change_midflight() -> void:
	var client: Node = RecordingClient.new()
	client.game_id = "inventory"
	client.game_profile = "inventory"
	client.base_url = "http://different-server:3001"
	client.status = "requesting_account"
	# Only the immutable issue origin may bind the returned token.
	client.set("_http_request_binding", {"base_url": "http://localhost:3001", "game_id": "inventory", "game_profile": "inventory"})
	var body: PackedByteArray = JSON.stringify({"player_id": "account-a", "token": "runtime-token", "game_id": "inventory", "expires_at": 9000000}).to_utf8_buffer()
	client._on_account_completed(HTTPRequest.RESULT_SUCCESS, 201, PackedStringArray(), body, 0)
	_check(not client.has_credentials() and client.status == "disconnected", "changing endpoint while issuing HTTP must not relabel a token's origin")
	client.free()


func _test_live_context_change_discards_recipient_data() -> void:
	for field: String in ["base_url", "game_id", "game_profile", "player_id"]:
		var client: Node = _inventory_client()
		client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
		client.send_game_command({"action": "collect", "item": "wood"})
		client.set(field, "changed-context")
		_check(not client._receive_message(_inventory_snapshot("snapshot", 2)), "live changed binding must refuse incoming state: " + field)
		_check(client.private_state.is_empty() and client.command_results.is_empty() and not client.has_pending_game_command() and not client.has_credentials(), "changed recipient context must discard previous state and credentials: " + field)
		client.free()


func _test_authentication_new_membership_clears_old_command_history() -> void:
	var client: Node = _inventory_client()
	client._apply_snapshot(_inventory_snapshot("room.entered", 1), true)
	client.send_game_command({"action": "collect", "item": "wood"})
	client._receive_message(_command_result(1, "test-1"))
	client.disconnect_for_test()
	client._pending["auth-new"] = {"type": "session.authenticate"}
	var auth: Dictionary = {"v": 1, "type": "session.authenticated", "request_id": "auth-new", "game_id": "inventory", "guest_id": "account-a", "player_id": "account-a", "expires_at": 9000000,
		"can_resume": true, "room_id": "room-a", "room_instance_id": "new-instance", "reservation_expires_at": 8000000}
	_check(client._receive_message(auth), "valid authentication may point to a different authoritative membership epoch")
	_check(client.command_results.is_empty() and client.last_processed_command_seq == 0, "different authenticated membership must not retain the old room's terminal history")
	client.free()


func _test_inventory_requires_command_sync_metadata() -> void:
	var client: Node = _inventory_client()
	var message: Dictionary = _inventory_snapshot("room.entered", 1)
	message.erase("last_processed_command_seq")
	_check(not client._apply_snapshot(message, true), "inventory full sync must provide its authoritative command high-water")
	message = _inventory_snapshot("room.entered", 1)
	message.erase("command_results")
	_check(not client._apply_snapshot(message, true), "inventory full sync must explicitly provide retained command results")
	client.free()
