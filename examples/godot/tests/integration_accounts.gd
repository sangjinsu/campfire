extends SceneTree

# Production scene controls are driven through signals, not OS mouse clicks.
const MainScene = preload("res://scenes/main.tscn")
var main: Control
var client: Node
var role := ""
var work := ""
var out := ""
var server := ""
var disabled_server := ""
var checks: Array = []
var failures: Array = []
var expected_errors: Array = []
var unexpected_errors: Array = []
var observations: Array = []
var expected_codes: Array[String] = []
var finished := false
var started_ms := 0
var identity: Dictionary = {}
var dropped_messages: Dictionary = {}
var retry_evidence: Dictionary = {}
var sensitive_values: Array[String] = []

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	if args.size() != 5:
		push_error("Expected role, work, output, account URL, disabled-auth URL")
		quit(2)
		return
	role = args[0]
	work = args[1]
	out = args[2]
	server = args[3]
	disabled_server = args[4]
	started_ms = Time.get_ticks_msec()
	call_deferred("_run")

func _run() -> void:
	main = MainScene.instantiate()
	root.add_child(main)
	await process_frame
	client = main.client
	client.error_received.connect(_error)
	client.authenticated.connect(func(_info: Dictionary): sensitive_values.append(client._token))
	_check(DisplayServer.get_name() != "headless", "Native display driver is active")
	_select_mode(1)
	_set_field("ServerURL", server)
	_set_field("GameID", "inventory")
	if not _check(main.get_node("%Credential").secret, "Credential input is masked"):
		return
	_login(OS.get_environment("CAMPFIRE_DEMO_" + role.to_upper() + "_CREDENTIAL"))
	if not await _wait(func(): return client.status == "lobby" and not main.get_node("%CreateRoom").disabled, 10.0, "Account login reaches lobby through production button"):
		return
	identity = {"player_id": client.player_id}
	if role == "alice":
		await _alice()
	else:
		await _bob()
	if not finished:
		_finish()

func _alice() -> void:
	if not _press("CreateRoom"):
		return
	if not await _wait(func(): return client.is_in_room, 6.0, "CreateRoom enters inventory room"):
		return
	identity["room_id"] = client.room_id
	identity["room_instance_id"] = client.room_instance_id
	_mark("room", identity)
	if not await _wait(func(): return _total_players() == 2, 10.0, "Two independent native accounts share room"):
		return
	if not _press("CollectWood"):
		return
	if not await _wait(func(): return client.last_processed_command_seq == 1 and not client.has_pending_game_command(), 5.0, "Alice wood receives terminal result"):
		return
	_mark("alice-collected", {})
	if not await _wait(func(): return _exists("bob-collected") and _total() == 2, 8.0, "Alice sees Bob collection in public count"):
		return
	if not _check_isolation(["alice-starter-item", "wood"], "bob-starter-item", 2, 1):
		return
	_observe("two-account-collections")
	# Drop actual socket replies while leaving the membership command ledger intact.
	client.set_process(false)
	if not _press("CollectKey"):
		return
	var retained: Dictionary = client._pending_game_command.duplicate(true)
	var original_requests := _command_request_ids()
	if not _check(client.has_pending_game_command() and client.pending_command_seq == 2 and _collect_disabled(), "One pending command disables all new collect buttons"):
		return
	expected_codes = ["COMMAND_PENDING"]
	if not _check(not client.send_game_command({"action":"collect", "item":"wood"}), "Adapter rejects a second command while pending"):
		return
	expected_codes.clear()
	await _drain_until("bob-observed-key", 6.0)
	if finished:
		return
	# This fixture only releases the transport request lock. It does not alter seq,
	# payload, membership, server state, or the retained pending game command.
	for request: Variant in client._pending.keys():
		if client._pending[request]["type"] == "game.command":
			client._pending.erase(request)
	client.command_pending_changed.emit(true)
	if not _check(client.can_retry_pending_command() and not main.get_node("%RetryCommand").disabled and client._pending_game_command == retained, "Test hook exposes RetryCommand without changing retained seq or payload"):
		return
	if not _press("RetryCommand"):
		return
	var retry_requests := _command_request_ids()
	retry_evidence = {"command_seq": client.pending_command_seq, "payload": retained["payload"], "original_request_ids": original_requests, "retry_request_ids": retry_requests, "same_retained_command": client._pending_game_command == retained, "transport_lock_release_fixture": true}
	if not _check(original_requests != retry_requests and client._pending_game_command == retained, "UI retry uses a new request ID with exactly the same sequence and payload"):
		return
	client.set_process(true)
	if not await _wait(func(): return client.last_processed_command_seq == 2 and not client.has_pending_game_command() and _total() == 3, 6.0, "Retry receives retained terminal result without another collection"):
		return
	if not _check_isolation(["alice-starter-item", "wood", "key"], "bob-starter-item", 3, 2):
		return
	_mark("alice-retried", {})
	# Exercise the real six-second production request timeout separately.
	client.set_process(false)
	if not _press("CollectStone"):
		return
	await _drain_until("bob-observed-stone", 6.0)
	if finished:
		return
	var timeout_deadline := Time.get_ticks_msec() + 6300
	while Time.get_ticks_msec() < timeout_deadline:
		_drain_socket()
		await process_frame
	_drain_socket()
	expected_codes = ["REQUEST_TIMEOUT"]
	client.set_process(true)
	if not await _wait(func(): return client.status == "disconnected", 2.0, "Real request timeout disconnects after deliberately lost reply"):
		return
	expected_codes.clear()
	if not _check(client.has_pending_game_command() and client.pending_command_seq == 3 and _private_hidden(), "Timeout preserves pending metadata and clears private UI"):
		return
	if not _press("Reconnect"):
		return
	if not await _wait(func(): return client.is_in_room and not client.has_pending_game_command(), 8.0, "Reconnect full sync resolves lost reply from retained result"):
		return
	if not _check_identity() or not _check_isolation(["alice-starter-item", "wood", "key", "stone"], "bob-starter-item", 4, 3):
		return
	if not _check(main.get_node("%CommandResult").text.contains("명령 3") and main.get_node("%CommandResult").text.contains("성공"), "Full sync restores recent own terminal result in UI"):
		return
	_observe("lost-reply-timeout-resumed")
	_mark("alice-resumed", {})
	if not await _wait(func(): return _exists("bob-final-ready"), 6.0, "Bob independently observes count four without duplicate"):
		return
	await _capture("alice-shared")
	_mark("alice-captured", {})
	if not await _wait(func(): return _exists("bob-captured"), 6.0, "Both native renderer captures exist"):
		return
	var token_before: String = client._token
	if not _press("ForceDisconnect") or not _check(_private_hidden(), "Manual disconnect immediately hides private state and results"):
		return
	_login(OS.get_environment("CAMPFIRE_DEMO_ALICE_CREDENTIAL"))
	if not await _wait(func(): return client.is_in_room, 8.0, "Same-account credential login rotates token and resumes membership"):
		return
	if not _check(token_before != client._token, "Account session token rotated in memory"):
		return
	token_before = ""
	if not _check_identity() or not _check_isolation(["alice-starter-item", "wood", "key", "stone"], "bob-starter-item", 4, 3):
		return
	_observe("same-account-token-rotation")
	if not _press("Logout"):
		return
	if not await _wait(func(): return client.status == "disconnected" and not client.has_credentials(), 6.0, "Logout clears local identity and credentials"):
		return
	if not _check(_private_hidden() and client.private_state.is_empty() and client.command_results.is_empty() and not client.has_pending_game_command(), "Logout removes all stale personal state and results"):
		return
	_mark("alice-logged-out", {})
	if not await _wait(func(): return _exists("bob-finished"), 16.0, "Bob completes account switch and error UI scenarios"):
		return

func _bob() -> void:
	if not await _wait(func(): return _exists("room"), 10.0, "Alice created room coordination barrier"):
		return
	var room: Dictionary = _read("room")
	if not _press("RefreshRooms"):
		return
	if not await _wait(func(): return _find_room(room) >= 0, 6.0, "RefreshRooms lists exact Alice room instance"):
		return
	var list: ItemList = main.get_node("%RoomList")
	var index := _find_room(room)
	list.select(index)
	list.item_selected.emit(index)
	if not _press("JoinRoom"):
		return
	if not await _wait(func(): return client.is_in_room and client.room_id == room["room_id"] and client.room_instance_id == room["room_instance_id"], 6.0, "JoinRoom enters selected exact room instance"):
		return
	identity.merge({"room_id":client.room_id, "room_instance_id":client.room_instance_id})
	if not await _wait(func(): return _exists("alice-collected"), 6.0, "Alice first collection observed"):
		return
	if not _press("CollectStone"):
		return
	if not await _wait(func(): return client.last_processed_command_seq == 1 and _total() == 2, 6.0, "Bob stone receives own terminal result and shared count two"):
		return
	if not _check_isolation(["bob-starter-item", "stone"], "alice-starter-item", 2, 1):
		return
	_observe("two-account-collections")
	_mark("bob-collected", {})
	if not await _wait(func(): return _total() == 3, 8.0, "Bob observes Alice key executed while Alice discards reply"):
		return
	_mark("bob-observed-key", {})
	if not await _wait(func(): return _exists("alice-retried"), 8.0, "Alice UI retry completed"):
		return
	if not _check(_total() == 3 and client.last_processed_command_seq == 1 and client.command_results.size() == 1, "Alice retry does not repeat mutation or leak terminal result to Bob"):
		return
	if not await _wait(func(): return _total() == 4, 8.0, "Bob observes Alice stone executed before genuine timeout"):
		return
	_mark("bob-observed-stone", {})
	if not await _wait(func(): return _exists("alice-resumed"), 12.0, "Alice genuine timeout and resume completed"):
		return
	if not _check_isolation(["bob-starter-item", "stone"], "alice-starter-item", 4, 1):
		return
	_observe("lost-reply-timeout-resumed")
	_mark("bob-final-ready", {})
	await _capture("bob-shared")
	_mark("bob-captured", {})
	if not await _wait(func(): return _exists("alice-logged-out"), 12.0, "Alice rotates and logs out before account switch"):
		return
	if not _press("ForceDisconnect"):
		return
	_login(OS.get_environment("CAMPFIRE_DEMO_ALICE_CREDENTIAL"))
	if not _check(_private_hidden(), "Different-account submission clears old Bob UI immediately"):
		return
	if not await _wait(func(): return client.status == "lobby" and not main.get_node("%CreateRoom").disabled, 8.0, "Different-account credential establishes fresh lobby identity"):
		return
	if not _check(client.player_id != identity["player_id"] and client.room_id.is_empty() and client.private_state.is_empty() and client.command_results.is_empty(), "Different account cannot inherit prior membership or personal state"):
		return
	if not _press("CreateRoom"):
		return
	if not await _wait(func(): return client.is_in_room, 6.0, "Switched Alice enters fresh membership"):
		return
	if not _check_isolation(["alice-starter-item"], "bob-starter-item", 0, 0):
		return
	if not _press("ForceDisconnect"):
		return
	_set_field("GameID", "inventory-edited")
	if not _check(not client.has_credentials() and client.room_id.is_empty() and main.get_node("%Reconnect").disabled and _private_hidden(), "Disconnected context edit clears old credentials and blocks old-context reconnect"):
		return
	_select_mode(0)
	if not _check(client.game_profile == "arena" and not client.has_credentials() and _private_hidden() and client.command_results.is_empty(), "Profile switch clears private state, results and account credentials"):
		return
	_select_mode(1)
	_set_field("ServerURL", server)
	_set_field("GameID", "inventory")
	expected_codes = ["UNAUTHENTICATED"]
	_login(OS.get_environment("CAMPFIRE_TEST_INVALID_CREDENTIAL"))
	if not await _wait(func(): return main.get_node("%Notice").text.contains("UNAUTHENTICATED") and client.status == "disconnected", 6.0, "Invalid credential UNAUTHENTICATED code is visible in production UI"):
		return
	expected_codes.clear()
	_set_field("ServerURL", disabled_server)
	_set_field("GameID", "arena")
	expected_codes = ["ACCOUNT_AUTH_DISABLED"]
	_login(OS.get_environment("CAMPFIRE_TEST_INVALID_CREDENTIAL"))
	if not await _wait(func(): return main.get_node("%Notice").text.contains("ACCOUNT_AUTH_DISABLED") and client.status == "disconnected", 6.0, "Server without verifier exposes ACCOUNT_AUTH_DISABLED in UI"):
		return
	expected_codes.clear()
	if not _check(_private_hidden() and client.private_state.is_empty() and client.command_results.is_empty(), "Failed login never restores previous private data"):
		return
	_mark("bob-finished", {})

func _select_mode(index: int) -> void:
	var mode: OptionButton = main.get_node("%Mode")
	mode.select(index)
	mode.item_selected.emit(index)

func _login(credential: String) -> void:
	_set_field("Credential", credential)
	_press("AccountConnect")
	_check(main.get_node("%Credential").text.is_empty(), "Credential field clears synchronously on submission")

func _set_field(name: String, value: String) -> void:
	var field: LineEdit = main.get_node("%" + name)
	field.text = value
	field.text_changed.emit(value)

func _press(name: String) -> bool:
	var button: Button = main.get_node("%" + name)
	if not _check(not button.disabled, name + " control is enabled"):
		return false
	button.pressed.emit()
	return true

func _total() -> int:
	return int(client.snapshot.get("state", {}).get("total_collected", -1))

func _total_players() -> int:
	return int(client.snapshot.get("state", {}).get("players", -1))

func _collect_disabled() -> bool:
	return main.get_node("%CollectWood").disabled and main.get_node("%CollectStone").disabled and main.get_node("%CollectKey").disabled

func _private_hidden() -> bool:
	var text: String = main.get_node("%PrivateState").text + main.get_node("%CommandResult").text
	return not text.contains("starter-item") and not text.contains("최종 결과")

func _check_identity() -> bool:
	return _check(client.player_id == identity["player_id"] and client.room_id == identity["room_id"] and client.room_instance_id == identity["room_instance_id"], "Resume keeps exact player, room and instance")

func _check_isolation(inventory: Array, other_secret: String, total: int, results: int) -> bool:
	var visible: String = main.get_node("%PrivateState").text
	var public_text: String = main.get_node("%PublicState").text
	var serialized := JSON.stringify({"snapshot":client.snapshot, "private":client.private_state, "results":client.command_results, "visible":visible, "result_ui":main.get_node("%CommandResult").text})
	return _check(client.private_state.get("inventory", []) == inventory and not serialized.contains(other_secret) and _total() == total and client.command_results.size() == results and public_text.contains("수집 횟수  %d" % total) and visible.contains(str(inventory[0])) and not JSON.stringify(client.snapshot.get("state", {})).contains("starter-item"), "Own inventory and own results are isolated; public count and visible UI agree")

func _command_request_ids() -> Array:
	var ids: Array = []
	for id: Variant in client._pending:
		if client._pending[id]["type"] == "game.command":
			ids.append(id)
	return ids

func _drain_socket() -> void:
	if client._peer == null:
		return
	client._peer.poll()
	while client._peer.get_available_packet_count() > 0:
		var data: Variant = JSON.parse_string(client._peer.get_packet().get_string_from_utf8())
		if typeof(data) == TYPE_DICTIONARY:
			var kind: String = str(data.get("type", "unknown"))
			dropped_messages[kind] = int(dropped_messages.get(kind, 0)) + 1

func _drain_until(marker: String, seconds: float) -> void:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000.0)
	while Time.get_ticks_msec() < deadline and not finished:
		_drain_socket()
		if _exists(marker):
			_drain_socket()
			_check(true, "Peer independently confirms server mutation before deliberately lost reply")
			return
		await process_frame
	_check(false, "Peer did not observe mutation while reply was discarded")

func _find_room(room: Dictionary) -> int:
	var list: ItemList = main.get_node("%RoomList")
	for i in range(list.item_count):
		var value: Dictionary = list.get_item_metadata(i)
		if value.get("room_id") == room["room_id"] and value.get("room_instance_id") == room["room_instance_id"]:
			return i
	return -1

func _wait(predicate: Callable, seconds: float, label: String) -> bool:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000.0)
	while Time.get_ticks_msec() < deadline and not finished:
		if predicate.call():
			return _check(true, label)
		await process_frame
	return _check(false, label + " (timeout)")

func _observe(label: String) -> void:
	observations.append({"label":label, "player_id":client.player_id, "room_id":client.room_id, "room_instance_id":client.room_instance_id, "snapshot_seq":client.snapshot_seq, "public_state":client.snapshot.get("state", {}).duplicate(true), "private_state":client.private_state.duplicate(true), "command_results":client.command_results.duplicate(true), "last_processed_command_seq":client.last_processed_command_seq, "public_ui":main.get_node("%PublicState").text, "private_ui":main.get_node("%PrivateState").text, "result_ui":main.get_node("%CommandResult").text})

func _capture(name: String) -> void:
	await RenderingServer.frame_post_draw
	var image := root.get_texture().get_image()
	_check(not image.is_empty() and image.save_png(out.path_join(name + ".png")) == OK, "Actual renderer saves " + name + " PNG after frame_post_draw")

func _error(code: String, _message: String, retryable: bool) -> void:
	var value := {"code":code, "retryable":retryable, "command_type":client.last_error_command}
	if expected_codes.has(code):
		expected_errors.append(value)
	else:
		unexpected_errors.append(value)

func _check(condition: bool, label: String) -> bool:
	if condition:
		checks.append(label)
		return true
	failures.append(label)
	_finish()
	return false

func _mark(name: String, data: Dictionary) -> void:
	var path := work.path_join(name + ".json")
	_write(path + ".tmp", data)
	DirAccess.rename_absolute(path + ".tmp", path)

func _exists(name: String) -> bool:
	return FileAccess.file_exists(work.path_join(name + ".json"))

func _read(name: String) -> Dictionary:
	return JSON.parse_string(FileAccess.get_file_as_string(work.path_join(name + ".json")))

func _write(path: String, data: Dictionary) -> void:
	var file := FileAccess.open(path, FileAccess.WRITE)
	file.store_string(JSON.stringify(data, "\t"))

func _finish() -> void:
	if finished:
		return
	finished = true
	var report := {"role":role, "pid":OS.get_process_id(), "display_driver":DisplayServer.get_name(), "rendering_method":RenderingServer.get_current_rendering_method(), "checks":checks, "failures":failures, "expected_errors":expected_errors, "unexpected_errors":unexpected_errors, "observations":observations, "retry_evidence":retry_evidence, "deliberately_discarded_socket_messages":dropped_messages, "interaction_method":"production Control signals; lost-reply socket drain and explicit transport-lock-release test fixture; no OS click claimed", "elapsed_seconds":float(Time.get_ticks_msec() - started_ms) / 1000.0}
	var token_leaks := _scan_tokens(report)
	if not token_leaks.is_empty():
		failures.append("Runtime token leaked in " + ", ".join(token_leaks))
	var success := failures.is_empty() and unexpected_errors.is_empty()
	report["success"] = success
	report["known_runtime_token_scan_passed"] = token_leaks.is_empty()
	sensitive_values.clear()
	_write(out.path_join(role + ".json"), report)
	if is_instance_valid(client):
		client.set_process(true)
		client.disconnect_for_test()
	print("ACCOUNT_NATIVE_%s: %s (%d checks)" % [role.to_upper(), "PASS" if success else "FAIL", checks.size()])
	quit(0 if success else 1)

func _scan_tokens(report: Dictionary) -> Array[String]:
	var texts := {"current-report":JSON.stringify(report)}
	for name: String in DirAccess.get_files_at(out):
		if name.ends_with(".json") or name.ends_with(".log"):
			texts[name] = FileAccess.get_file_as_string(out.path_join(name))
	var leaked: Array[String] = []
	for name: String in texts:
		for value: String in sensitive_values:
			if not value.is_empty() and texts[name].contains(value):
				leaked.append(name)
				break
	return leaked
