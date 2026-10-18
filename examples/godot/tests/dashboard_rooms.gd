extends SceneTree

# Real production transport fixture; no administrator key is supplied to Godot.
const ClientScript = preload("res://scripts/campfire_client.gd")
var clients: Array = []
var rooms: Array = []
var failures: Array = []
var work: String
var server: String
var count := 104

func _initialize() -> void:
	var args := OS.get_cmdline_user_args()
	server = args[0]
	work = args[1]
	if args.size() > 2:
		count = int(args[2])
	call_deferred("_run")

func _run() -> void:
	for index in range(count):
		var client: Node = ClientScript.new()
		root.add_child(client)
		clients.append(client)
		client.error_received.connect(func(code: String, _message: String, _retryable: bool): failures.append(code))
		client.connect_guest(server, "arena" if index < count - 2 else "arena-b")
		if not await _until(func(): return client.status == "lobby", 8.0):
			_finish(false, "authentication timeout")
			return
		# The public adapter intentionally offers public rooms only. This fixture
		# uses its normal command transport to exercise private framework metadata.
		client._send_command("room.create", {"settings": {"capacity": 16, "public": index % 2 == 0}})
		if not await _until(func(): return client.is_in_room, 8.0):
			_finish(false, "room creation timeout")
			return
		rooms.append({"game_id": client.game_id, "room_id": client.room_id, "room_instance_id": client.room_instance_id, "public": index % 2 == 0})
		if not failures.is_empty():
			_finish(false, "unexpected transport error")
			return
	_write("ready.json", {"success": true, "room_count": rooms.size(), "rooms": rooms, "transport": "Production CampfireClient / Godot WebSocketPeer"})
	while not FileAccess.file_exists(work.path_join("stop.json")):
		await create_timer(0.1).timeout
	_finish(true, "")

func _until(predicate: Callable, seconds: float) -> bool:
	var deadline := Time.get_ticks_msec() + int(seconds * 1000)
	while Time.get_ticks_msec() < deadline:
		if predicate.call():
			return true
		if not failures.is_empty():
			return false
		await create_timer(0.02).timeout
	return false

func _write(name: String, value: Dictionary) -> void:
	var file := FileAccess.open(work.path_join(name), FileAccess.WRITE)
	file.store_string(JSON.stringify(value))
	file.close()

func _finish(success: bool, failure: String) -> void:
	_write("result.json", {"success": success, "failure": failure, "unexpected_errors": failures, "room_count": rooms.size()})
	for client in clients:
		client.disconnect_for_test()
	quit(0 if success else 1)
