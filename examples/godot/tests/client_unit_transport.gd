extends SceneTree

# This regression uses the actual Rust server and actual WebSocketPeer queues.
# Usage: godot --headless --path examples/godot --script tests/client_unit_transport.gd -- http://127.0.0.1:3000
const Client = preload("res://scripts/campfire_client.gd")
var failures: Array[String] = []
var server_url: String = "http://127.0.0.1:3000"


func _initialize() -> void:
	var args: PackedStringArray = OS.get_cmdline_user_args()
	if not args.is_empty():
		server_url = args[0]
	call_deferred("_run")


func _run() -> void:
	await _test_queued_logout_before_transport_close()
	await _test_unexpected_close_retains_credentials()
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
