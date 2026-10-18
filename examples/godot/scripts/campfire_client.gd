class_name CampfireClient
extends Node

signal status_changed(value: String)
signal authenticated(info: Dictionary)
signal rooms_received(rooms: Array, next_cursor: Variant)
signal room_entered(full_sync: Dictionary)
signal room_resumed(full_sync: Dictionary)
signal snapshot_received(value: Dictionary)
signal room_left()
signal room_closed(reason: String)
signal error_received(code: String, message: String, retryable: bool)

const MAX_SAFE_INTEGER: int = 9007199254740991
const MAX_PENDING: int = 128
const MAX_PACKETS_PER_FRAME: int = 64
const MAX_MESSAGE_BYTES: int = 65536
const REQUEST_TIMEOUT_MS: int = 6000
const INPUT_INTERVAL: float = 0.05

var status: String = "disconnected"
var is_authenticated: bool = false
var is_in_room: bool = false
var is_connecting: bool = false
var can_resume: bool = false
var base_url: String = ""
var game_id: String = ""
var guest_id: String = ""
var room_id: String = ""
var room_instance_id: String = ""
var expires_at: int = 0
var reservation_expires_at: int = 0
var snapshot: Dictionary = {}
var snapshot_seq: int = -1
var server_tick: int = 0
var last_processed_input_seq: int = 0
var last_error_command: String = ""

# Credentials stay only in this Node's memory and are bound to their issuing origin/game.
var _token: String = ""
var _credential_base_url: String = ""
var _credential_game_id: String = ""
var _peer: WebSocketPeer = null
var _http: HTTPRequest = null
var _connection_epoch: int = 0
var _request_counter: int = 0
var _pending: Dictionary = {}
var _connection_deadline: int = 0
var _auth_sent: bool = false
var _logout_requested: bool = false
var _direction: Vector2 = Vector2.ZERO
var _input_accumulator: float = 0.0
var _next_input_seq: int = 1


func connect_guest(url: String, game: String) -> bool:
	if status != "disconnected":
		return _client_error("CLIENT_BUSY", "연결을 끊은 뒤 새 게스트로 접속하세요.")
	var normalized: String = url.strip_edges().trim_suffix("/")
	if not _valid_endpoint(normalized) or not _valid_game_id(game):
		return _client_error("INVALID_ENDPOINT", "HTTP(S) 서버 주소와 1~64자의 영문 게임 ID를 확인하세요.")
	_stop_transport()
	_clear_credentials()
	_clear_room()
	base_url = normalized
	game_id = game
	_connection_epoch += 1
	_http = HTTPRequest.new()
	_http.timeout = 10.0
	_http.body_size_limit = MAX_MESSAGE_BYTES
	_http.max_redirects = 0
	add_child(_http)
	_http.request_completed.connect(_on_guest_completed.bind(_connection_epoch))
	_set_status("requesting_guest")
	var result: Error = _http.request(base_url + "/v1/games/" + game_id + "/guests", PackedStringArray(), HTTPClient.METHOD_POST)
	if result != OK:
		_lose_connection()
		return _client_error("HTTP_START_FAILED", "게스트 발급 요청을 시작하지 못했습니다: " + error_string(result))
	return true


func reconnect() -> bool:
	if status != "disconnected":
		return _client_error("CLIENT_BUSY", "현재 연결을 먼저 끊으세요.")
	if not has_credentials():
		return _client_error("NO_SESSION", "재접속할 게스트가 없습니다. 새 게스트로 접속하세요.")
	if base_url != _credential_base_url or game_id != _credential_game_id:
		_clear_credentials()
		_clear_room()
		return _client_error("SESSION_BINDING_CHANGED", "서버나 게임이 바뀌었습니다. 새 게스트로 접속하세요.")
	_connection_epoch += 1
	return _open_socket()


func has_credentials() -> bool:
	return not _token.is_empty()


func request_rooms(cursor: Variant = null) -> bool:
	if cursor != null and (typeof(cursor) != TYPE_STRING or cursor.length() > 4096):
		return _client_error("INVALID_CURSOR", "서버가 준 목록 cursor를 사용하세요.")
	if _has_pending("room.list"):
		return false
	return _send_command("room.list", {"cursor": cursor, "limit": 100})


func create_room(capacity: int = 16) -> bool:
	if capacity < 1 or capacity > 4096:
		return _client_error("INVALID_CAPACITY", "방 정원은 1~4096명 범위여야 합니다.")
	if not room_id.is_empty() or _room_request_pending():
		return _client_error("ALREADY_IN_ROOM", "현재 방에서 나간 뒤 새 방을 만드세요.")
	return _send_command("room.create", {"settings": {"capacity": capacity, "public": true}})


func join_room(id: String, instance: String) -> bool:
	if not _identifier(id) or not _identifier(instance):
		return _client_error("INVALID_ROOM", "방 목록에서 입장할 방을 선택하세요.")
	if not room_id.is_empty() or _room_request_pending():
		return _client_error("ALREADY_IN_ROOM", "현재 방에서 나간 뒤 다른 방에 입장하세요.")
	return _send_command("room.join", {"room_id": id, "room_instance_id": instance})


func leave_room() -> bool:
	if room_id.is_empty() or _room_request_pending():
		return false
	_direction = Vector2.ZERO
	return _send_command("room.leave", {"room_id": room_id, "room_instance_id": room_instance_id})


func logout() -> bool:
	if not is_authenticated:
		_stop_transport()
		_clear_credentials()
		_clear_room()
		_set_status("disconnected")
		return true
	if _has_pending("session.logout"):
		return false
	_direction = Vector2.ZERO
	return _send_command("session.logout", {})


func disconnect_for_test() -> bool:
	if status == "disconnected":
		return false
	_lose_connection()
	return true


func send_direction(direction: Vector2) -> bool:
	if not is_finite(direction.x) or not is_finite(direction.y):
		return _client_error("INVALID_DIRECTION", "이동 방향은 유한한 좌표여야 합니다.")
	if not is_in_room or _has_pending("room.leave") or _has_pending("session.logout"):
		_direction = Vector2.ZERO
		return false
	_direction = direction.limit_length(1.0)
	return true


func _process(delta: float) -> void:
	if _peer == null:
		return
	_peer.poll()
	var ready_state: int = _peer.get_ready_state()
	if ready_state == WebSocketPeer.STATE_OPEN:
		if not _auth_sent:
			_auth_sent = true
			_set_status("authenticating")
			if not _send_command("session.authenticate", {"token": _token}):
				return
		var remaining: int = MAX_PACKETS_PER_FRAME
		while _peer != null and _peer.get_available_packet_count() > 0 and remaining > 0:
			var packet: PackedByteArray = _peer.get_packet()
			remaining -= 1
			if not _peer.was_string_packet() or packet.size() > MAX_MESSAGE_BYTES:
				_protocol_failure()
				return
			_receive_text(packet.get_string_from_utf8())
	elif ready_state == WebSocketPeer.STATE_CLOSED:
		var explicit_logout: bool = _logout_requested
		_lose_connection()
		if not explicit_logout:
			_client_error("CONNECTION_CLOSED", "서버 연결이 끊겼습니다. 같은 게스트로 재접속할 수 있습니다.", true)
		return
	var now: int = Time.get_ticks_msec()
	if status == "connecting" and now > _connection_deadline:
		_lose_connection()
		_client_error("CONNECTION_TIMEOUT", "서버 연결 시간이 초과되었습니다.", true)
		return
	_check_timeouts(now)
	if not is_in_room or _peer == null:
		_input_accumulator = 0.0
		return
	# A stalled frame sends at most one input; old directions are never replayed in a burst.
	_input_accumulator += minf(delta, INPUT_INTERVAL)
	if _input_accumulator >= INPUT_INTERVAL:
		_input_accumulator = 0.0
		_send_input()


func _exit_tree() -> void:
	_stop_transport()


func _on_guest_completed(result: int, response_code: int, _headers: PackedStringArray, body: PackedByteArray, epoch: int) -> void:
	if epoch != _connection_epoch or status != "requesting_guest":
		return
	if _http != null:
		_http.queue_free()
		_http = null
	if result != HTTPRequest.RESULT_SUCCESS or response_code != 201:
		_lose_connection()
		_client_error("GUEST_REQUEST_FAILED", "게스트 발급 실패 (HTTP %d, result %d). 서버 주소와 발급 제한을 확인하세요." % [response_code, result], response_code == 429 or response_code == 503)
		return
	var parsed: Variant = _parse_json(body.get_string_from_utf8())
	if typeof(parsed) != TYPE_DICTIONARY:
		_protocol_failure()
		return
	var data: Dictionary = parsed
	if not _identifier(data.get("guest_id")) or not _identifier(data.get("token"), 4096) or data.get("game_id") != game_id or not _uint(data.get("expires_at")):
		_protocol_failure()
		return
	guest_id = data["guest_id"]
	_token = data["token"]
	expires_at = int(data["expires_at"])
	_credential_base_url = base_url
	_credential_game_id = game_id
	_open_socket()


func _open_socket() -> bool:
	_pending.clear()
	_direction = Vector2.ZERO
	_input_accumulator = 0.0
	_auth_sent = false
	_peer = WebSocketPeer.new()
	_peer.inbound_buffer_size = MAX_MESSAGE_BYTES
	_peer.outbound_buffer_size = MAX_MESSAGE_BYTES
	_peer.max_queued_packets = 256
	var ws_url: String = ("wss://" + base_url.substr(8)) if base_url.begins_with("https://") else ("ws://" + base_url.substr(7))
	_set_status("connecting")
	_connection_deadline = Time.get_ticks_msec() + 10000
	var result: Error = _peer.connect_to_url(ws_url + "/v1/games/" + game_id + "/ws")
	if result != OK:
		_lose_connection()
		return _client_error("CONNECTION_FAILED", "WebSocket 연결을 시작하지 못했습니다: " + error_string(result), true)
	return true


func _send_command(kind: String, fields: Dictionary) -> bool:
	if _peer == null or _peer.get_ready_state() != WebSocketPeer.STATE_OPEN:
		return _client_error("NOT_CONNECTED", "서버에 먼저 연결하세요.")
	if kind != "session.authenticate" and not is_authenticated:
		return _client_error("UNAUTHENTICATED", "게스트 인증을 먼저 완료하세요.")
	if _pending.size() >= MAX_PENDING:
		return _client_error("CLIENT_BUSY", "서버 응답을 기다리는 중입니다.", true)
	_request_counter += 1
	var request_id: String = "%d-%d" % [_connection_epoch, _request_counter]
	var command: Dictionary = fields.duplicate(true)
	command.merge({"v": 1, "type": kind, "request_id": request_id, "game_id": game_id})
	var result: Error = _peer.send_text(JSON.stringify(command))
	if result != OK:
		_lose_connection()
		return _client_error("SEND_FAILED", "서버로 요청을 보내지 못했습니다: " + error_string(result), true)
	# Never put the authentication token in pending metadata or emitted diagnostics.
	_pending[request_id] = {
		"type": kind, "deadline": Time.get_ticks_msec() + (5000 if kind == "session.authenticate" else REQUEST_TIMEOUT_MS),
		"room_id": fields.get("room_id", ""), "room_instance_id": fields.get("room_instance_id", ""),
		"input_seq": fields.get("input_seq", 0)
	}
	if kind == "session.logout":
		_logout_requested = true
	return true


func _send_input() -> void:
	if not is_in_room or _has_pending("room.leave") or _has_pending("session.logout"):
		return
	if _next_input_seq > MAX_SAFE_INTEGER:
		_lose_connection()
		_client_error("SEQUENCE_LIMIT", "안전한 입력 번호 범위를 넘었습니다. 새 게스트로 접속하세요.")
		return
	if _send_command("game.input", {
		"room_id": room_id, "room_instance_id": room_instance_id, "input_seq": _next_input_seq,
		"payload": {"move_x": _direction.x, "move_y": _direction.y}
	}):
		_next_input_seq += 1


func _receive_text(raw: String) -> bool:
	var parsed: Variant = _parse_json(raw)
	if typeof(parsed) != TYPE_DICTIONARY:
		_protocol_failure()
		return false
	return _receive_message(parsed)


func _receive_message(message: Dictionary) -> bool:
	if not _uint(message.get("v")) or int(message["v"]) != 1 or typeof(message.get("type")) != TYPE_STRING:
		_protocol_failure()
		return false
	var kind: String = message["type"]
	if kind == "snapshot":
		return _apply_snapshot(message, false)
	if kind == "room.closed":
		if message.get("room_id") != room_id or message.get("room_instance_id") != room_instance_id or typeof(message.get("reason")) != TYPE_STRING:
			return false
		var reason: String = message["reason"]
		_clear_room()
		_set_status("lobby" if is_authenticated else "disconnected")
		room_closed.emit(reason)
		return true
	var request_id_value: Variant = message.get("request_id")
	if kind == "error":
		return _receive_error(message)
	if typeof(request_id_value) != TYPE_STRING or not _pending.has(request_id_value):
		return false
	var request_id: String = request_id_value
	var pending: Dictionary = _pending[request_id]
	match kind:
		"session.authenticated":
			if pending["type"] != "session.authenticate" or not _valid_authentication(message):
				_protocol_failure()
				return false
			_pending.erase(request_id)
			is_authenticated = true
			expires_at = int(message["expires_at"])
			can_resume = message["can_resume"]
			if can_resume:
				room_id = message["room_id"]
				room_instance_id = message["room_instance_id"]
				reservation_expires_at = int(message["reservation_expires_at"])
			else:
				_clear_room()
			_set_status("lobby")
			authenticated.emit(message.duplicate(true))
			if can_resume:
				_set_status("resuming")
				_send_command("resume", {"room_id": room_id, "room_instance_id": room_instance_id})
			return true
		"room.listed":
			if pending["type"] != "room.list" or not _valid_rooms(message):
				_protocol_failure()
				return false
			_pending.erase(request_id)
			rooms_received.emit(message["rooms"].duplicate(true), message["next_cursor"])
			return true
		"room.entered", "room.resumed":
			var expected: String = "resume" if kind == "room.resumed" else pending["type"]
			if (kind == "room.entered" and expected != "room.create" and expected != "room.join") or (kind == "room.resumed" and pending["type"] != "resume"):
				return false
			if expected != "room.create" and (message.get("room_id") != pending["room_id"] or message.get("room_instance_id") != pending["room_instance_id"]):
				return false
			if not _apply_snapshot(message, true):
				return false
			_pending.erase(request_id)
			if kind == "room.entered":
				room_entered.emit(snapshot.duplicate(true))
			else:
				room_resumed.emit(snapshot.duplicate(true))
			snapshot_received.emit(snapshot.duplicate(true))
			return true
		"room.left":
			if pending["type"] != "room.leave" or message.get("room_id") != pending["room_id"] or message.get("room_instance_id") != pending["room_instance_id"]:
				return false
			_pending.erase(request_id)
			_clear_room()
			_set_status("lobby")
			room_left.emit()
			return true
		"session.logged_out":
			if pending["type"] != "session.logout":
				return false
			_stop_transport()
			_clear_credentials()
			_clear_room()
			_set_status("disconnected")
			return true
		"game.input.accepted":
			if pending["type"] != "game.input" or not _uint(message.get("input_seq")) or int(message["input_seq"]) != pending["input_seq"]:
				return false
			_pending.erase(request_id)
			# Receive acceptance does not advance the actual processed-input acknowledgement.
			return true
	return false


func _apply_snapshot(message: Dictionary, full_sync: bool) -> bool:
	if message.get("game_id") != game_id or not _identifier(message.get("room_id")) or not _identifier(message.get("room_instance_id")):
		return false
	if not _uint(message.get("snapshot_seq")) or not _uint(message.get("server_tick")) or not _uint(message.get("last_processed_input_seq"), MAX_SAFE_INTEGER - 1) or not _valid_arena_state(message.get("state")):
		return false
	if not room_id.is_empty() and (message["room_id"] != room_id or message["room_instance_id"] != room_instance_id):
		return false
	var sequence: int = int(message["snapshot_seq"])
	var tick: int = int(message["server_tick"])
	var ack: int = int(message["last_processed_input_seq"])
	if not full_sync and (not is_in_room or sequence <= snapshot_seq or tick < server_tick or ack < last_processed_input_seq):
		return false
	if full_sync:
		room_id = message["room_id"]
		room_instance_id = message["room_instance_id"]
		is_in_room = true
		can_resume = false
		reservation_expires_at = 0
		_next_input_seq = ack + 1
		_direction = Vector2.ZERO
		_input_accumulator = 0.0
		_clear_pending_inputs()
	snapshot_seq = sequence
	server_tick = tick
	last_processed_input_seq = ack
	snapshot = message.duplicate(true)
	if full_sync:
		_set_status("in_room")
	else:
		snapshot_received.emit(snapshot.duplicate(true))
	return true


func _valid_authentication(message: Dictionary) -> bool:
	if message.get("game_id") != game_id or message.get("guest_id") != guest_id or not _uint(message.get("expires_at")) or typeof(message.get("can_resume")) != TYPE_BOOL:
		return false
	if message["can_resume"]:
		return _identifier(message.get("room_id")) and _identifier(message.get("room_instance_id")) and _uint(message.get("reservation_expires_at"))
	return message.has("room_id") and message["room_id"] == null and message.has("room_instance_id") and message["room_instance_id"] == null and message.has("reservation_expires_at") and message["reservation_expires_at"] == null


func _valid_rooms(message: Dictionary) -> bool:
	if typeof(message.get("rooms")) != TYPE_ARRAY or message["rooms"].size() > 100 or not message.has("next_cursor"):
		return false
	var cursor: Variant = message["next_cursor"]
	if cursor != null and not _identifier(cursor, 4096):
		return false
	for item: Variant in message["rooms"]:
		if typeof(item) != TYPE_DICTIONARY:
			return false
		if item.get("game_id") != game_id or not _identifier(item.get("room_id")) or not _identifier(item.get("room_instance_id")) or not _uint(item.get("members"), 4096) or not _uint(item.get("capacity"), 4096):
			return false
		if int(item["capacity"]) < 1 or int(item["members"]) > int(item["capacity"]):
			return false
	return true


func _valid_arena_state(state: Variant) -> bool:
	if typeof(state) != TYPE_DICTIONARY or typeof(state.get("players")) != TYPE_ARRAY or state["players"].size() > 4096 or not _uint(state.get("shared_score")):
		return false
	var goal: Variant = state.get("goal")
	if typeof(goal) != TYPE_DICTIONARY or not _arena_coordinate(goal.get("x")) or not _arena_coordinate(goal.get("y")) or not _arena_coordinate(goal.get("radius")) or float(goal["radius"]) <= 0.0 or typeof(goal.get("active")) != TYPE_BOOL:
		return false
	var players: Dictionary = {}
	for player: Variant in state["players"]:
		if typeof(player) != TYPE_DICTIONARY or not _identifier(player.get("id")) or not _arena_coordinate(player.get("x")) or not _arena_coordinate(player.get("y")) or typeof(player.get("connected")) != TYPE_BOOL:
			return false
		if players.has(player["id"]):
			return false
		players[player["id"]] = true
	return players.has(guest_id)


func _receive_error(message: Dictionary) -> bool:
	if not _identifier(message.get("code")) or typeof(message.get("message")) != TYPE_STRING or typeof(message.get("retryable")) != TYPE_BOOL or not message.has("request_id"):
		_protocol_failure()
		return false
	var request_id: Variant = message["request_id"]
	var pending_type: String = ""
	if request_id != null:
		if typeof(request_id) != TYPE_STRING or not _pending.has(request_id):
			return false
		pending_type = _pending[request_id]["type"]
		_pending.erase(request_id)
	var code: String = message["code"]
	if code == "UNAUTHENTICATED" or (code == "GAME_MISMATCH" and pending_type == "session.authenticate"):
		_clear_credentials()
		_clear_room()
		_lose_connection()
	elif pending_type == "session.logout":
		_lose_connection()
	elif pending_type == "session.authenticate":
		_lose_connection()
	elif pending_type == "resume" and code not in ["ROOM_NOT_FOUND", "STALE_INSTANCE", "RESUME_EXPIRED"]:
		_lose_connection()
	elif pending_type == "resume" or (pending_type == "game.input" and code in ["ROOM_NOT_FOUND", "STALE_INSTANCE", "RESUME_EXPIRED"]):
		_clear_room()
		_set_status("lobby" if is_authenticated else "disconnected")
	last_error_command = pending_type
	error_received.emit(code, message["message"], message["retryable"])
	return true


func _check_timeouts(now: int) -> void:
	for request_id: Variant in _pending.keys():
		var entry: Dictionary = _pending[request_id]
		if now <= entry["deadline"]:
			continue
		_pending.erase(request_id)
		if entry["type"] != "room.list":
			_lose_connection()
		if entry["type"] == "session.logout":
			_client_error("LOGOUT_UNCONFIRMED", "서버의 로그아웃 응답을 확인하지 못했지만 이 기기의 게스트 정보는 지웠습니다.", false, entry["type"])
		else:
			_client_error("REQUEST_TIMEOUT", "서버 응답 시간이 초과되었습니다. 다시 연결해 상태를 확인하세요.", true, entry["type"])
		return


func _has_pending(kind: String) -> bool:
	for entry: Dictionary in _pending.values():
		if entry["type"] == kind:
			return true
	return false


func _room_request_pending() -> bool:
	return _has_pending("room.create") or _has_pending("room.join") or _has_pending("room.leave") or _has_pending("resume")


func _clear_pending_inputs() -> void:
	for request_id: Variant in _pending.keys():
		if _pending[request_id]["type"] == "game.input":
			_pending.erase(request_id)


func _lose_connection() -> void:
	var explicit_logout: bool = _logout_requested
	_stop_transport()
	# Godot clears application packets when a close frame is consumed in the same poll.
	# Preserve explicit local logout intent without treating an absent reply as server acknowledgement.
	if explicit_logout:
		_clear_credentials()
		_clear_room()
	is_authenticated = false
	is_in_room = false
	_direction = Vector2.ZERO
	_input_accumulator = 0.0
	can_resume = not room_id.is_empty() and has_credentials()
	_set_status("disconnected")


func _stop_transport() -> void:
	_connection_epoch += 1
	if _http != null:
		_http.cancel_request()
		_http.queue_free()
		_http = null
	if _peer != null:
		_peer.close(1000, "client disconnect")
		_peer = null
	_pending.clear()
	_auth_sent = false
	_logout_requested = false


func _clear_room() -> void:
	room_id = ""
	room_instance_id = ""
	is_in_room = false
	can_resume = false
	reservation_expires_at = 0
	snapshot.clear()
	snapshot_seq = -1
	server_tick = 0
	last_processed_input_seq = 0
	_next_input_seq = 1
	_direction = Vector2.ZERO
	_input_accumulator = 0.0
	_clear_pending_inputs()


func _clear_credentials() -> void:
	_token = ""
	_credential_base_url = ""
	_credential_game_id = ""
	guest_id = ""
	expires_at = 0
	is_authenticated = false


func _set_status(value: String) -> void:
	status = value
	is_connecting = value in ["requesting_guest", "connecting", "authenticating", "resuming"]
	status_changed.emit(value)


func _protocol_failure() -> void:
	_lose_connection()
	_client_error("INVALID_SERVER_MESSAGE", "서버 응답 형식을 확인할 수 없어 연결을 끊었습니다.")


func _client_error(code: String, message: String, retryable: bool = false, command_type: String = "") -> bool:
	last_error_command = command_type
	error_received.emit(code, message, retryable)
	return false


static func _parse_json(raw: String) -> Variant:
	var parser: JSON = JSON.new()
	if parser.parse(raw) != OK:
		return null
	return parser.data


static func _uint(value: Variant, maximum: int = MAX_SAFE_INTEGER) -> bool:
	if typeof(value) == TYPE_INT:
		return value >= 0 and value <= maximum
	if typeof(value) == TYPE_FLOAT:
		return is_finite(value) and value >= 0.0 and value <= float(maximum) and floor(value) == value
	return false


static func _finite_number(value: Variant) -> bool:
	return (typeof(value) == TYPE_FLOAT or typeof(value) == TYPE_INT) and is_finite(float(value))


static func _arena_coordinate(value: Variant) -> bool:
	return _finite_number(value) and absf(float(value)) <= 1000.0


static func _identifier(value: Variant, maximum: int = 128) -> bool:
	return typeof(value) == TYPE_STRING and not value.is_empty() and value.to_utf8_buffer().size() <= maximum


static func _valid_game_id(value: String) -> bool:
	if value.is_empty() or value.length() > 64:
		return false
	for character: String in value:
		if not character in "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_-":
			return false
	return true


static func _valid_endpoint(value: String) -> bool:
	var prefix: int = 8 if value.begins_with("https://") else 7
	if not value.begins_with("http://") and not value.begins_with("https://"):
		return false
	var authority: String = value.substr(prefix)
	if authority.is_empty():
		return false
	for forbidden: String in ["/", "@", "?", "#", " ", "\t", "\r", "\n", "\\"]:
		if authority.contains(forbidden):
			return false
	return true
