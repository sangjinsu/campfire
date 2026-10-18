extends SceneTree

# This regression uses the actual Rust server and actual WebSocketPeer queues.
# Usage: godot --headless --path examples/godot --script tests/client_unit_transport.gd -- http://127.0.0.1:3000
const Client = preload("res://scripts/campfire_client.gd")
var failures: Array[String] = []
var server_url: String = "http://127.0.0.1:3000"
var account_url: String = OS.get_environment("CAMPFIRE_TEST_ACCOUNT_URL")


func _initialize() -> void:
	var args: PackedStringArray = OS.get_cmdline_user_args()
	if not args.is_empty():
		server_url = args[0]
	call_deferred("_run")


func _run() -> void:
	await _test_queued_logout_before_transport_close()
	await _test_unexpected_close_retains_credentials()
	if not account_url.is_empty():
		await _test_account_commands_private_state_and_resume()
	for failure: String in failures:
		print("FAIL: ", failure)
	if failures.is_empty():
		print("PASS: CampfireClient real transport close invariants")
	quit(0 if failures.is_empty() else 1)


func _authenticated_client() -> Node:
	var client: Node = Client.new()
	root.add_child(client)
	client.connect_guest(server_url, "arena")
	var deadline: int = Time.get_ticks_msec() + 8000
	while client.status != "lobby" and Time.get_ticks_msec() < deadline:
		await process_frame
	if client.status != "lobby":
		failures.append("real Rust server must authenticate the production adapter")
		client.queue_free()
		return null
	return client


func _test_queued_logout_before_transport_close() -> void:
	var client: Node = await _authenticated_client()
	if client == null:
		return
	var errors: Array[String] = []
	client.error_received.connect(func(code: String, _message: String, _retryable: bool) -> void: errors.append(code))
	# Pause only automatic dispatch so one poll can contain both the reply and close frame.
	client.set_process(false)
	_check(client.logout(), "logout command must be accepted before transport close")
	var peer: WebSocketPeer = client._peer
	var observed_state: int = WebSocketPeer.STATE_OPEN
	var observed_queued: int = 0
	var prior_observation: String = ""
	var deadline: int = Time.get_ticks_msec() + 3000
	while Time.get_ticks_msec() < deadline:
		peer.poll()
		observed_state = peer.get_ready_state()
		observed_queued = peer.get_available_packet_count()
		var observation: String = "state=%d queued_packets=%d" % [observed_state, observed_queued]
		if observation != prior_observation:
			print("TRANSPORT_TRANSITION ", observation)
			prior_observation = observation
		if observed_state == WebSocketPeer.STATE_CLOSED:
			break
		await process_frame
	print("TRANSPORT_CLOSE_EVIDENCE state=", observed_state, " queued_packets=", observed_queued)
	_check(observed_state == WebSocketPeer.STATE_CLOSED, "actual server must close the transport after logout")
	client._process(0.0)
	_check(client.status == "disconnected" and not client.has_credentials(), "explicit logout must purge local credentials even when Godot discards the final reply on close")
	_check(errors.is_empty(), "explicit logout close must not be reported as an unexpected disconnect")
	client.queue_free()
	await process_frame


func _test_unexpected_close_retains_credentials() -> void:
	var client: Node = await _authenticated_client()
	if client == null:
		return
	client.set_process(false)
	var peer: WebSocketPeer = client._peer
	peer.close(1000, "transport regression")
	var deadline: int = Time.get_ticks_msec() + 3000
	while peer.get_ready_state() != WebSocketPeer.STATE_CLOSED and Time.get_ticks_msec() < deadline:
		peer.poll()
		await process_frame
	client._process(0.0)
	_check(client.status == "disconnected" and client.has_credentials(), "close without logout acknowledgement must retain the reconnect credential")
	client.logout()
	client.queue_free()
	await process_frame


func _check(condition: bool, message: String) -> void:
	if not condition:
		failures.append(message)


# Optional account transport suite uses credentials only from the process environment.
# Run against an isolated account_server before other clients rotate these accounts.
func _account_client(credential: String) -> Node:
	var client: Node = Client.new()
	root.add_child(client)
	client.set_game_profile("inventory")
	if not client.connect_account(account_url, "inventory", credential):
		failures.append("account session HTTP exchange must start")
		client.queue_free()
		return null
	await _wait_until(func() -> bool: return client.status == "lobby")
	if client.status != "lobby":
		failures.append("isolated account server must authenticate the production adapter")
		client.queue_free()
		return null
	return client


func _wait_until(condition: Callable, timeout_ms: int = 8000) -> void:
	var deadline: int = Time.get_ticks_msec() + timeout_ms
	while not condition.call() and Time.get_ticks_msec() < deadline:
		await process_frame


func _test_account_commands_private_state_and_resume() -> void:
	var alice_credential: String = OS.get_environment("CAMPFIRE_DEMO_ALICE_CREDENTIAL")
	var bob_credential: String = OS.get_environment("CAMPFIRE_DEMO_BOB_CREDENTIAL")
	if alice_credential.is_empty() or bob_credential.is_empty():
		failures.append("optional account suite requires both development credential environment variables")
		return
	var alice: Node = await _account_client(alice_credential)
	var bob: Node = await _account_client(bob_credential)
	if alice == null or bob == null:
		if alice != null:
			alice.logout()
			alice.queue_free()
		if bob != null:
			bob.logout()
			bob.queue_free()
		return
	_check(alice.player_id != bob.player_id and alice.player_id == alice.guest_id, "account identities must be distinct and preserve the guest alias")
	_check(alice.create_room(2), "account creator must create a room")
	await _wait_until(func() -> bool: return alice.is_in_room)
	if not alice.is_in_room:
		failures.append("account creator must receive an inventory full sync")
	else:
		_check(bob.join_room(alice.room_id, alice.room_instance_id), "second account must join the same membership epoch")
		await _wait_until(func() -> bool: return bob.is_in_room)
		_check(bob.is_in_room, "second account must receive its own inventory full sync")
		if bob.is_in_room:
			_check(alice.private_state.get("inventory") != bob.private_state.get("inventory"), "each account must receive a different private inventory")
			_check(not alice.snapshot.has("private_state") and not bob.snapshot.has("private_state"), "public snapshot signals must omit private projections")
			var result_events: Array = []
			alice.command_result_received.connect(func(value: Dictionary) -> void: result_events.append(value))
			# Pause delivery after send, then discard the transport to model a lost terminal reply.
			alice.set_process(false)
			_check(alice.send_game_command({"action": "collect", "item": "wood"}), "first single action must send")
			var first_request: String = ""
			for request: String in alice._pending:
				if alice._pending[request]["type"] == "game.command":
					first_request = request
			alice._lose_connection()
			_check(alice.has_pending_game_command() and alice.private_state.is_empty(), "lost reply must preserve uncertain command but hide private state")
			alice.set_process(true)
			_check(alice.reconnect(), "lost reply client must reconnect with its runtime token")
			await _wait_until(func() -> bool: return alice.is_in_room)
			_check(alice.is_in_room, "lost reply client must resume its membership")
			if alice.is_in_room and alice.has_pending_game_command():
				_check(alice.retry_pending_command(), "unprocessed lost reply action must retry after resume")
				var retry_request: String = ""
				for request: String in alice._pending:
					if alice._pending[request]["type"] == "game.command":
						retry_request = request
				_check(not first_request.is_empty() and not retry_request.is_empty() and first_request != retry_request, "retry must get a fresh request ID while retaining command sequence")
			await _wait_until(func() -> bool: return not alice.has_pending_game_command())
			_check(not alice.has_pending_game_command() and alice.last_processed_command_seq == 1, "lost terminal reply must reconcile sequence one exactly once")
			_check(result_events.size() == 1, "lost reply recovery must emit one terminal result")
			_check(bob.send_game_command({"action": "collect", "item": "stone"}), "second account must send its independent first command")
			await _wait_until(func() -> bool: return not bob.has_pending_game_command())
			await _wait_until(func() -> bool: return alice.snapshot.get("state", {}).get("total_collected") == 2 and bob.snapshot.get("state", {}).get("total_collected") == 2)
			_check(alice.snapshot.get("state", {}).get("total_collected") == 2 and bob.snapshot.get("state", {}).get("total_collected") == 2, "both public counters must include exactly two actions")
			_check(alice.private_state.get("inventory", []).has("wood") and not alice.private_state.get("inventory", []).has("stone"), "Alice's private projection must contain only Alice's action")
			_check(bob.private_state.get("inventory", []).has("stone") and not bob.private_state.get("inventory", []).has("wood"), "Bob's private projection must contain only Bob's action")
			var identity: String = alice.player_id
			var membership: String = alice.room_instance_id
			alice.disconnect_for_test()
			_check(alice.connect_account(account_url, "inventory", alice_credential), "same account may rotate its token")
			await _wait_until(func() -> bool: return alice.is_in_room)
			_check(alice.is_in_room and alice.player_id == identity and alice.room_instance_id == membership, "account rotation must retain player and membership")
			_check(alice.last_processed_command_seq == 1 and alice.private_state.get("inventory", []).has("wood"), "account rotation must recover command ledger and private inventory")
			_check(result_events.size() == 1, "account rotation full sync must deduplicate the existing terminal result")
	alice.logout()
	bob.logout()
	await _wait_until(func() -> bool: return alice.status == "disconnected" and bob.status == "disconnected")
	_check(not alice.has_credentials() and not bob.has_credentials() and alice.private_state.is_empty() and bob.private_state.is_empty(), "confirmed account logout must clear both sessions and private state")
	alice.queue_free()
	bob.queue_free()
	await process_frame
