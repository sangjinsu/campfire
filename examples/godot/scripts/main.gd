extends Control

const ArenaScript = preload("res://scripts/arena_view.gd")
const ClientScript = preload("res://scripts/campfire_client.gd")
const FOREST := Color("19383e")
const CANVAS := Color("eaf1ea")
const FIRE := Color("e8a75e")
var client: Node
var _pending := ""
var _next_cursor: Variant = null
var _room_capacity := 16
var _window_focused := true
var _last_snapshot: Dictionary = {}
var _auto_create := 0
var _capture_path := ""
var _lost_reservation := false

func _ready() -> void:
	_build_theme()
	_build_ui()
	client = ClientScript.new()
	client.name = "CampfireClient"
	add_child(client)
	client.status_changed.connect(_on_status)
	client.authenticated.connect(_on_authenticated)
	client.rooms_received.connect(_on_rooms)
	client.snapshot_received.connect(_apply_snapshot)
	client.room_entered.connect(_on_room_full_sync)
	client.room_resumed.connect(_on_room_full_sync)
	client.room_left.connect(_on_left)
	client.room_closed.connect(_on_closed)
	client.error_received.connect(_on_error)
	%GuestConnect.pressed.connect(func(): client.connect_guest(%ServerURL.text.strip_edges(), %GameID.text.strip_edges()))
	%Logout.pressed.connect(func(): _command("logout", client.logout()))
	%ForceDisconnect.pressed.connect(func(): client.disconnect_for_test())
	%Reconnect.pressed.connect(func(): client.reconnect())
	%CreateRoom.pressed.connect(_create_room)
	%RefreshRooms.pressed.connect(func(): _command("list", client.request_rooms()))
	%NextRooms.pressed.connect(func(): _command("list", client.request_rooms(_next_cursor)))
	%JoinRoom.pressed.connect(_join_room)
	%LeaveRoom.pressed.connect(func(): client.send_direction(Vector2.ZERO); _command("leave", client.leave_room()))
	%RoomList.item_selected.connect(func(_index): _refresh_controls())
	%ArenaView.gui_input.connect(func(event):
		if event is InputEventMouseButton and event.pressed:
			%ArenaView.grab_focus())
	_on_status("disconnected")
	_configure_cli()

func _build_theme() -> void:
	var palette := Theme.new()
	var font := SystemFont.new()
	font.font_names = PackedStringArray(["Apple SD Gothic Neo", "Noto Sans CJK KR", "Malgun Gothic", "sans-serif"])
	font.allow_system_fallback = true
	palette.default_font = font
	palette.default_font_size = 15
	palette.set_color("font_color", "Label", CANVAS)
	palette.set_color("font_color", "Button", CANVAS)
	palette.set_color("font_disabled_color", "Button", Color("8ba09e"))
	palette.set_color("font_focus_color", "Button", CANVAS)
	palette.set_color("font_color", "LineEdit", CANVAS)
	palette.set_color("font_uneditable_color", "LineEdit", Color("a7bcb9"))
	palette.set_color("font_color", "ItemList", CANVAS)
	palette.set_color("font_selected_color", "ItemList", FOREST)
	for kind in ["Button", "LineEdit", "ItemList", "OptionButton"]:
		for state in ["normal", "hover", "pressed", "focus", "disabled", "read_only"]:
			var box := StyleBoxFlat.new()
			box.bg_color = Color("284a50") if state != "pressed" else Color("3f666c")
			if kind == "LineEdit" or kind == "ItemList":
				box.bg_color = Color("112e34")
			box.set_content_margin_all(9)
			box.set_corner_radius_all(5)
			if state == "focus":
				box.bg_color = Color(0,0,0,0)
				box.set_border_width_all(2)
				box.border_color = FIRE
			palette.set_stylebox(state, kind, box)
	var selected := StyleBoxFlat.new()
	selected.bg_color = FIRE
	selected.set_corner_radius_all(3)
	palette.set_stylebox("selected", "ItemList", selected)
	palette.set_stylebox("selected_focus", "ItemList", selected)
	theme = palette

func _named(node: Control, node_name: String, parent: Node) -> Control:
	node.name = node_name
	parent.add_child(node)
	node.owner = self
	node.unique_name_in_owner = true
	return node

func _label(text: String, parent: Node, font_size := 15, node_name := "") -> Label:
	var label := Label.new()
	label.text = text
	label.add_theme_font_size_override("font_size", font_size)
	if node_name.is_empty():
		parent.add_child(label)
	else:
		_named(label, node_name, parent)
	return label

func _button(text: String, node_name: String, parent: Node) -> Button:
	var button := Button.new()
	button.text = text
	button.custom_minimum_size.y = 39
	_named(button, node_name, parent)
	return button

func _section(text: String, parent: Node) -> void:
	var spacer := Control.new()
	spacer.custom_minimum_size.y = 10
	parent.add_child(spacer)
	_label(text, parent, 19)

func _build_ui() -> void:
	var bg := ColorRect.new()
	bg.color = FOREST
	bg.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	bg.mouse_filter = Control.MOUSE_FILTER_IGNORE
	add_child(bg)
	var margin := MarginContainer.new()
	margin.set_anchors_and_offsets_preset(Control.PRESET_FULL_RECT)
	for side in ["left", "right", "top", "bottom"]:
		margin.add_theme_constant_override("margin_" + side, 20)
	add_child(margin)
	var page := VBoxContainer.new()
	page.add_theme_constant_override("separation", 14)
	margin.add_child(page)
	var header := HBoxContainer.new()
	page.add_child(header)
	var title := VBoxContainer.new()
	title.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	header.add_child(title)
	_label("Campfire", title, 30)
	_label("파티 테스트 코트   /   같은 방에서 함께 움직이기", title, 14)
	var status := _label("연결 전", header, 16, "Status")
	status.vertical_alignment = VERTICAL_ALIGNMENT_CENTER
	var notice := _label("서버를 실행한 뒤 게스트로 연결하세요.", page, 14, "Notice")
	notice.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	notice.add_theme_color_override("font_color", Color("ecd77e"))
	var body := HBoxContainer.new()
	body.size_flags_vertical = Control.SIZE_EXPAND_FILL
	body.add_theme_constant_override("separation", 22)
	page.add_child(body)
	var scroll := ScrollContainer.new()
	scroll.custom_minimum_size.x = 320
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	body.add_child(scroll)
	var rail := VBoxContainer.new()
	rail.custom_minimum_size.x = 304
	rail.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	rail.add_theme_constant_override("separation", 8)
	scroll.add_child(rail)
	_label("서버 연결", rail, 20)
	_label("Server URL", rail, 13)
	var url := LineEdit.new()
	url.text = "http://127.0.0.1:3000"
	_named(url, "ServerURL", rail)
	_label("Game ID", rail, 13)
	var game := LineEdit.new()
	game.text = "arena"
	_named(game, "GameID", rail)
	_button("게스트로 연결", "GuestConnect", rail)
	_button("같은 게스트 재접속", "Reconnect", rail)
	var reconnect_hint := _label("예약 시간 안에 재접속하면 자동으로 복구합니다.", rail, 12)
	reconnect_hint.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	var pair := HBoxContainer.new()
	rail.add_child(pair)
	_button("연결 끊기 (시험)", "ForceDisconnect", pair).size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_button("게스트 종료", "Logout", pair)
	_section("공개 방 만들기", rail)
	var capacity_row := HBoxContainer.new()
	rail.add_child(capacity_row)
	_label("최대 인원", capacity_row)
	var capacity := OptionButton.new()
	capacity.add_item("8명", 8)
	capacity.add_item("16명", 16)
	capacity.select(1)
	capacity.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_named(capacity, "Capacity", capacity_row)
	_button("방 만들고 입장", "CreateRoom", rail)
	_section("공개 방 목록", rail)
	var room_actions := HBoxContainer.new()
	rail.add_child(room_actions)
	_button("새로고침", "RefreshRooms", room_actions).size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_button("다음 목록", "NextRooms", room_actions).size_flags_horizontal = Control.SIZE_EXPAND_FILL
	var rooms := ItemList.new()
	rooms.custom_minimum_size.y = 126
	rooms.max_text_lines = 2
	_named(rooms, "RoomList", rail)
	_label("선택한 방의 실제 UUID / instance로 입장합니다.", rail, 12).autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_button("선택한 방 입장", "JoinRoom", rail)
	_button("방 나가기", "LeaveRoom", rail)
	var identity := _label("아직 입장한 방이 없습니다.", rail, 12, "RoomIdentity")
	identity.autowrap_mode = TextServer.AUTOWRAP_ARBITRARY
	identity.text = "아직 입장한 방이 없습니다."
	var right := VBoxContainer.new()
	right.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	right.add_theme_constant_override("separation", 10)
	body.add_child(right)
	var court_header := HBoxContainer.new()
	right.add_child(court_header)
	_label("공동 점수  0", court_header, 22, "Score").size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_label("참가자 0 / 16", court_header, 15, "Participants")
	var arena := ArenaScript.new()
	arena.custom_minimum_size = Vector2(320, 260)
	arena.size_flags_vertical = Control.SIZE_EXPAND_FILL
	arena.focus_mode = Control.FOCUS_ALL
	_named(arena, "ArenaView", right)
	_label("WASD / 방향키 이동     ·     금빛 목표에 먼저 닿으면 공동 점수 +1", right, 14).autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_label("입력 처리 확인  —    서버 tick  —    snapshot  —", right, 13, "Stats")
	_label("불빛 링: 나    ·    색 점: 다른 참가자    ·    회색: 재접속 예약", right, 12).autowrap_mode = TextServer.AUTOWRAP_WORD_SMART

func _process(_delta: float) -> void:
	if is_instance_valid(client):
		client.send_direction(current_direction())

func current_direction() -> Vector2:
	if not is_instance_valid(client) or not client.is_in_room or not _window_focused:
		return Vector2.ZERO
	var focus := get_viewport().gui_get_focus_owner()
	if focus is LineEdit or focus is TextEdit:
		return Vector2.ZERO
	var x := float(Input.is_physical_key_pressed(KEY_D) or Input.is_physical_key_pressed(KEY_RIGHT)) - float(Input.is_physical_key_pressed(KEY_A) or Input.is_physical_key_pressed(KEY_LEFT))
	var y := float(Input.is_physical_key_pressed(KEY_W) or Input.is_physical_key_pressed(KEY_UP)) - float(Input.is_physical_key_pressed(KEY_S) or Input.is_physical_key_pressed(KEY_DOWN))
	return Vector2(x, y).limit_length()

func _notification(what: int) -> void:
	if what == NOTIFICATION_APPLICATION_FOCUS_OUT:
		_window_focused = false
		if is_instance_valid(client):
			client.send_direction(Vector2.ZERO)
	elif what == NOTIFICATION_APPLICATION_FOCUS_IN:
		_window_focused = true

func _command(action: String, accepted: bool) -> void:
	if accepted:
		_pending = action
		%Notice.text = "요청 처리 중…"
	_refresh_controls()

func _create_room() -> void:
	_room_capacity = %Capacity.get_selected_id()
	_command("create", client.create_room(_room_capacity))

func _join_room() -> void:
	var selected: PackedInt32Array = %RoomList.get_selected_items()
	if selected.is_empty():
		return
	var room: Dictionary = %RoomList.get_item_metadata(selected[0])
	_room_capacity = int(room.capacity)
	_command("join", client.join_room(room.room_id, room.room_instance_id))

func _on_authenticated(_info: Dictionary) -> void:
	if not client.can_resume and client.room_id.is_empty() and not _last_snapshot.is_empty():
		_on_left()
		_lost_reservation = true
		%Notice.text = "이전 자리 예약이 만료됐습니다. 목록에서 다시 입장하거나 새 방을 만드세요."
	if _auto_create > 0 and not client.can_resume:
		call_deferred("_auto_create_room")
	elif not client.can_resume:
		call_deferred("_refresh_rooms")

func _auto_create_room() -> void:
	_room_capacity = _auto_create
	_command("create", client.create_room(_auto_create))

func _refresh_rooms() -> void:
	_command("list", client.request_rooms())

func _on_status(status: String) -> void:
	var labels := {"disconnected":"연결 전 / 끊김", "requesting_guest":"게스트 발급 중", "connecting":"서버 연결 중", "authenticating":"게스트 인증 중", "lobby":"로비 연결됨", "resuming":"같은 자리 복구 중", "in_room":"코트 입장 · 실시간 연결"}
	%Status.text = labels.get(status, status)
	if status == "requesting_guest" or (status == "disconnected" and not client.has_credentials()):
		_on_left()
		_lost_reservation = false
	if status == "disconnected":
		_pending = ""
		%Notice.text = "같은 게스트 재접속으로 예약된 자리를 복구하거나 새 게스트로 연결하세요." if client.has_credentials() else "서버를 실행한 뒤 게스트로 연결하세요."
	if not _last_snapshot.is_empty():
		%ArenaView.set_snapshot(_last_snapshot, client.guest_id, client.is_in_room)
	_refresh_controls()

func _refresh_controls() -> void:
	var disconnected: bool = client.status == "disconnected"
	var lobby: bool = client.status == "lobby" and _pending.is_empty()
	var in_room: bool = client.is_in_room and _pending.is_empty()
	%ServerURL.editable = disconnected
	%GameID.editable = disconnected
	%GuestConnect.disabled = not disconnected
	%Reconnect.disabled = not (disconnected and client.has_credentials())
	%ForceDisconnect.disabled = disconnected or client.status == "requesting_guest"
	%Logout.disabled = not client.is_authenticated or not _pending.is_empty()
	%CreateRoom.disabled = not lobby
	%Capacity.disabled = not lobby
	%RefreshRooms.disabled = not lobby
	%NextRooms.disabled = not lobby or _next_cursor == null
	%JoinRoom.disabled = not lobby or %RoomList.get_selected_items().is_empty()
	%LeaveRoom.disabled = not in_room

func _on_rooms(rooms: Array, next_cursor: Variant) -> void:
	if _pending == "list":
		_pending = ""
	_next_cursor = next_cursor
	%RoomList.clear()
	for room in rooms:
		var index: int = %RoomList.add_item("%d / %d명   %s" % [int(room.members), int(room.capacity), str(room.room_id).left(13)])
		%RoomList.set_item_metadata(index, room)
		%RoomList.set_item_tooltip(index, "방: %s\ninstance: %s" % [room.room_id, room.room_instance_id])
	%Notice.text = "공개 방이 없습니다. 새 방을 만들어 시작하세요." if rooms.is_empty() else "함께할 방을 선택하고 입장하세요."
	if _lost_reservation:
		%Notice.text = "이전 자리 예약이 만료됐습니다. " + %Notice.text
	_refresh_controls()

func _on_room_full_sync(_value: Dictionary) -> void:
	if _pending in ["create", "join", "resume"]:
		_pending = ""
	_refresh_controls()

func _apply_snapshot(value: Dictionary) -> void:
	_lost_reservation = false
	_last_snapshot = value
	%ArenaView.set_snapshot(value, client.guest_id, client.is_in_room)
	var state: Dictionary = value.get("state", {})
	var players: Array = state.get("players", [])
	var reserved := 0
	for player in players:
		if not player.get("connected", false):
			reserved += 1
	%Score.text = "공동 점수  %d" % int(state.get("shared_score", 0))
	%Participants.text = "참가자 %d / %d%s" % [players.size(), _room_capacity, " (예약 %d)" % reserved if reserved > 0 else ""]
	%Stats.text = "입력 처리 확인  %d    서버 tick  %d    snapshot  %d" % [int(value.get("last_processed_input_seq", 0)), int(value.get("server_tick", 0)), int(value.get("snapshot_seq", 0))]
	%RoomIdentity.text = "게임  %s\n방  %s\ninstance  %s\n나  %s" % [value.get("game_id", ""), value.get("room_id", ""), value.get("room_instance_id", ""), client.guest_id]
	if _pending.is_empty():
		%Notice.text = "코트를 클릭한 뒤 WASD / 방향키로 움직이세요. 위치와 점수는 서버가 계산합니다."
	_refresh_controls()

func _on_left() -> void:
	_pending = ""
	_last_snapshot = {}
	%ArenaView.set_snapshot({}, client.guest_id, false)
	%RoomIdentity.text = "방에서 나왔습니다. 새 방에 입장할 수 있습니다."
	%Notice.text = "목록에서 입장할 방을 선택하거나 새 방을 만드세요."
	%Participants.text = "참가자 0 / %d" % _room_capacity
	%Score.text = "공동 점수  0"
	%Stats.text = "입력 처리 확인  —    서버 tick  —    snapshot  —"
	_refresh_controls()

func _on_closed(reason: String) -> void:
	_on_left()
	%Notice.text = "방이 닫혔습니다 (%s). 목록을 새로고침하거나 새 방을 만드세요." % reason

func _on_error(code: String, message: String, retryable: bool) -> void:
	var pending_types := {"create":"room.create", "join":"room.join", "list":"room.list", "leave":"room.leave", "logout":"session.logout", "resume":"resume"}
	if not _pending.is_empty() and client.get("last_error_command") == pending_types.get(_pending):
		_pending = ""
	if code in ["RESUME_EXPIRED", "ROOM_NOT_FOUND", "STALE_INSTANCE"] and client.status == "lobby" and client.room_id.is_empty():
		_on_left()
		_lost_reservation = code == "RESUME_EXPIRED"
	var explanations := {"ROOM_FULL":"방 정원이 찼습니다. 다른 방을 선택하거나 새 방을 만드세요.", "RESUME_EXPIRED":"자리 예약이 만료됐습니다. 목록을 새로고침해 다시 입장하세요.", "ROOM_NOT_FOUND":"방이 종료됐습니다. 목록을 새로고침하세요.", "STALE_INSTANCE":"방 정보가 바뀌었습니다. 목록을 새로고침하고 다시 선택하세요.", "UNAUTHENTICATED":"게스트 인증이 만료되거나 종료됐습니다. 새 게스트로 연결하세요.", "GAME_NOT_FOUND":"등록되지 않은 Game ID입니다. 서버의 게임 설정을 확인하세요.", "RATE_LIMITED":"요청이 너무 빠릅니다. 잠시 뒤 다시 시도하세요."}
	%Notice.text = explanations.get(code, "요청 실패 [%s]: %s%s" % [code, message, " 잠시 뒤 다시 시도하세요." if retryable else ""])
	_refresh_controls()

func _configure_cli() -> void:
	var delay := 0.8
	var quit_after := 0.0
	var auto_connect := false
	for argument in OS.get_cmdline_user_args():
		if argument.begins_with("--server="):
			%ServerURL.text = argument.trim_prefix("--server=")
			auto_connect = true
		elif argument.begins_with("--game="):
			%GameID.text = argument.trim_prefix("--game=")
		elif argument.begins_with("--auto-create="):
			_auto_create = int(argument.trim_prefix("--auto-create="))
		elif argument.begins_with("--capture="):
			_capture_path = argument.trim_prefix("--capture=")
		elif argument.begins_with("--capture-delay="):
			delay = float(argument.trim_prefix("--capture-delay="))
		elif argument.begins_with("--quit-after-seconds="):
			quit_after = float(argument.trim_prefix("--quit-after-seconds="))
	if auto_connect:
		%GuestConnect.pressed.emit()
	if not _capture_path.is_empty():
		get_tree().create_timer(maxf(delay, 0.1)).timeout.connect(_capture)
	if quit_after > 0.0:
		get_tree().create_timer(quit_after).timeout.connect(func(): get_tree().quit())

func _capture() -> void:
	await RenderingServer.frame_post_draw
	var image := get_viewport().get_texture().get_image()
	var result := image.save_png(_capture_path)
	print("UI_CAPTURE: %s (%s)" % [_capture_path, error_string(result)])
