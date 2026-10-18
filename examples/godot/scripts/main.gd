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
var _inventory_mode := false
var _command_waiting := false
var _command_result_unavailable := false
const COMMAND_RECOVERY_NOTICE := "[COMMAND_RESULT_UNAVAILABLE] 명령 처리 번호는 확인했지만 최종 결과 이력이 없습니다. 방에서 나간 뒤 다시 입장하세요."

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
	client.private_state_changed.connect(_on_private_state)
	client.command_result_received.connect(_on_command_result)
	client.command_pending_changed.connect(_on_command_pending)
	%Mode.item_selected.connect(_select_mode)
	%Credential.text_changed.connect(func(_text): _refresh_controls())
	%ServerURL.text_changed.connect(func(_text): _context_edited())
	%GameID.text_changed.connect(func(_text): _context_edited())
	%AccountConnect.pressed.connect(_connect_account)
	%CollectWood.pressed.connect(func(): _collect("wood"))
	%CollectStone.pressed.connect(func(): _collect("stone"))
	%CollectKey.pressed.connect(func(): _collect("key"))
	%RetryCommand.pressed.connect(_retry_command)
	%GuestConnect.pressed.connect(func(): client.connect_guest(%ServerURL.text.strip_edges(), %GameID.text.strip_edges()))
	%Logout.pressed.connect(func(): _clear_private_ui(); %Credential.clear(); _command("logout", client.logout()))
	%ForceDisconnect.pressed.connect(func(): client.disconnect_for_test())
	%Reconnect.pressed.connect(func(): client.reconnect())
	%CreateRoom.pressed.connect(_create_room)
	%RefreshRooms.pressed.connect(func(): _command("list", client.request_rooms()))
	%NextRooms.pressed.connect(func(): _command("list", client.request_rooms(_next_cursor)))
	%JoinRoom.pressed.connect(_join_room)
	%LeaveRoom.pressed.connect(func(): _clear_private_ui(); client.send_direction(Vector2.ZERO); _command("leave", client.leave_room()))
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
	_label("파티 테스트 코트   /   같은 방에서 함께 움직이기", title, 14, "Subtitle")
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
	var mode := OptionButton.new()
	mode.add_item("Guest Arena · 함께 이동", 0)
	mode.add_item("Account Inventory · 내 아이템", 1)
	_named(mode, "Mode", rail)
	_label("Server URL", rail, 13)
	var url := LineEdit.new()
	url.text = "http://127.0.0.1:3000"
	_named(url, "ServerURL", rail)
	_label("Game ID", rail, 13)
	var game := LineEdit.new()
	game.text = "arena"
	_named(game, "GameID", rail)
	_button("게스트로 연결", "GuestConnect", rail)
	var account_box := VBoxContainer.new()
	_named(account_box, "AccountFields", rail)
	account_box.visible = false
	_label("개발용 계정 credential", account_box, 13)
	var credential := LineEdit.new()
	credential.secret = true
	credential.placeholder_text = "서버에 설정한 계정 credential"
	_named(credential, "Credential", account_box)
	_button("계정으로 연결", "AccountConnect", account_box)
	_label("개발용 계정 서버입니다. credential은 저장하지 않습니다.", account_box, 12).autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
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
	_build_inventory_ui(right)
	_label("WASD / 방향키 이동     ·     금빛 목표에 먼저 닿으면 공동 점수 +1", right, 14, "MovementHint").autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	_label("입력 처리 확인  —    서버 tick  —    snapshot  —", right, 13, "Stats")
	_label("불빛 링: 나    ·    색 점: 다른 참가자    ·    회색: 재접속 예약", right, 12, "ArenaLegend").autowrap_mode = TextServer.AUTOWRAP_WORD_SMART

func _process(_delta: float) -> void:
	if is_instance_valid(client) and not _inventory_mode:
		client.send_direction(current_direction())

func current_direction() -> Vector2:
	if _inventory_mode or not is_instance_valid(client) or not client.is_in_room or not _window_focused:
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
	var labels := {"disconnected":"연결 전 / 끊김", "requesting_account":"계정 session 발급 중", "requesting_guest":"게스트 발급 중", "connecting":"서버 연결 중", "authenticating":"게스트 인증 중", "lobby":"로비 연결됨", "resuming":"같은 자리 복구 중", "in_room":"코트 입장 · 실시간 연결"}
	%Status.text = labels.get(status, status)
	if _inventory_mode:
		%Status.text = {"authenticating":"계정 인증 중", "in_room":"인벤토리 입장 · 실시간 연결"}.get(status, %Status.text)
	if status != "in_room":
		_clear_private_ui()
	if status in ["requesting_guest", "requesting_account"] or (status == "disconnected" and not client.has_credentials()):
		_on_left()
		_lost_reservation = false
	if status == "disconnected":
		_pending = ""
		%Notice.text = "같은 계정 재접속으로 예약된 자리를 복구하세요." if _inventory_mode and client.has_credentials() else ("같은 게스트 재접속으로 예약된 자리를 복구하거나 새 게스트로 연결하세요." if client.has_credentials() else ("계정 서버를 실행하고 credential을 입력하세요." if _inventory_mode else "서버를 실행한 뒤 게스트로 연결하세요."))
	if not _last_snapshot.is_empty():
		%ArenaView.set_snapshot(_last_snapshot, client.guest_id, client.is_in_room)
	_refresh_controls()

func _refresh_controls() -> void:
	var disconnected: bool = client.status == "disconnected"
	var lobby: bool = client.status == "lobby" and _pending.is_empty()
	var in_room: bool = client.is_in_room and client.status == "in_room" and _pending.is_empty()
	%ServerURL.editable = disconnected
	%GameID.editable = disconnected
	%Mode.disabled = not disconnected
	%Credential.editable = disconnected
	%GuestConnect.disabled = not disconnected
	%AccountConnect.disabled = not (disconnected and _inventory_mode and not %Credential.text.is_empty() and _valid_connection_fields())
	var collecting: bool = _inventory_mode and in_room and not _command_waiting and not _command_result_unavailable and not client.has_pending_game_command()
	%CollectWood.disabled = not collecting
	%CollectStone.disabled = not collecting
	%CollectKey.disabled = not collecting
	%RetryCommand.disabled = not (_inventory_mode and _pending.is_empty() and not _command_result_unavailable and client.can_retry_pending_command())
	%Reconnect.disabled = not (disconnected and client.has_credentials())
	%ForceDisconnect.disabled = disconnected or client.status in ["requesting_guest", "requesting_account"]
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
	if _inventory_mode and not _command_result_unavailable:
		for result: Dictionary in client.command_results:
			if result.get("game_id") == client.game_id and result.get("room_id") == client.room_id and result.get("room_instance_id") == client.room_instance_id:
				_on_command_result(result)
	_refresh_controls()

func _apply_snapshot(value: Dictionary) -> void:
	_lost_reservation = false
	_last_snapshot = value
	%ArenaView.set_snapshot(value if not _inventory_mode else {}, client.guest_id, client.is_in_room)
	if _inventory_mode:
		_apply_inventory_snapshot(value)
		return
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
	_command_result_unavailable = false
	_pending = ""
	_last_snapshot = {}
	_clear_private_ui()
	%ArenaView.set_snapshot({}, client.guest_id, false)
	%RoomIdentity.text = "방에서 나왔습니다. 새 방에 입장할 수 있습니다."
	%Notice.text = "목록에서 입장할 방을 선택하거나 새 방을 만드세요."
	%Participants.text = "참가자 0 / %d" % _room_capacity
	%Score.text = "공동 수집  0" if _inventory_mode else "공동 점수  0"
	%PublicState.text = "공용 상태를 기다립니다.\n같은 방의 수집 횟수와 참가 인원만 표시합니다."
	%Stats.text = "내 명령 처리 확인  —    서버 tick  —    snapshot  —" if _inventory_mode else "입력 처리 확인  —    서버 tick  —    snapshot  —"
	_refresh_controls()

func _on_closed(reason: String) -> void:
	_on_left()
	%Notice.text = "방이 닫혔습니다 (%s). 목록을 새로고침하거나 새 방을 만드세요." % reason

func _on_error(code: String, message: String, retryable: bool) -> void:
	if _inventory_mode and code == "COMMAND_RESULT_UNAVAILABLE":
		_command_result_unavailable = true
		%CommandResult.text = "최종 결과를 확인할 수 없습니다.\n방에서 나간 뒤 다시 입장하세요."
		%Notice.text = COMMAND_RECOVERY_NOTICE
		_refresh_controls()
		return
	var pending_types := {"create":"room.create", "join":"room.join", "list":"room.list", "leave":"room.leave", "logout":"session.logout", "resume":"resume"}
	if not _pending.is_empty() and client.get("last_error_command") == pending_types.get(_pending):
		_pending = ""
	if code in ["RESUME_EXPIRED", "ROOM_NOT_FOUND", "STALE_INSTANCE"] and client.status == "lobby" and client.room_id.is_empty():
		_on_left()
		_lost_reservation = code == "RESUME_EXPIRED"
	var explanations := {"ROOM_FULL":"방 정원이 찼습니다. 다른 방을 선택하거나 새 방을 만드세요.", "RESUME_EXPIRED":"자리 예약이 만료됐습니다. 목록을 새로고침해 다시 입장하세요.", "ROOM_NOT_FOUND":"방이 종료됐습니다. 목록을 새로고침하세요.", "STALE_INSTANCE":"방 정보가 바뀌었습니다. 목록을 새로고침하고 다시 선택하세요.", "UNAUTHENTICATED":"게스트 인증이 만료되거나 종료됐습니다. 새 게스트로 연결하세요.", "GAME_NOT_FOUND":"등록되지 않은 Game ID입니다. 서버의 게임 설정을 확인하세요.", "RATE_LIMITED":"요청이 너무 빠릅니다. 잠시 뒤 다시 시도하세요."}
	var fallback := "요청 실패 [%s]: %s%s" % [code, message, " 잠시 뒤 다시 시도하세요." if retryable else ""]
	if _inventory_mode:
		var account_explanations := {"UNAUTHENTICATED":"계정 인증을 확인할 수 없습니다. credential을 확인하고 계정으로 다시 연결하세요.", "INVALID_CREDENTIAL":"계정 credential을 확인하고 다시 연결하세요.", "ACCOUNT_AUTH_DISABLED":"이 서버에서 계정 로그인을 사용할 수 없습니다. 계정 서버 주소를 확인하세요.", "AUTH_TIMEOUT":"계정 인증 시간이 초과됐습니다. 잠시 뒤 계정으로 다시 연결하세요."}
		var guidance: String = account_explanations.get(code, explanations.get(code, "미확인 명령은 재확인 버튼으로 같은 행동을 다시 확인하세요." if client.has_pending_game_command() else "연결과 선택한 방을 확인하세요."))
		%Notice.text = "요청 실패 [%s]. %s" % [code, guidance]
	else:
		%Notice.text = explanations.get(code, fallback)
	_refresh_controls()

func _configure_cli() -> void:
	var delay := 0.8
	var quit_after := 0.0
	var auto_connect := false
	for argument in OS.get_cmdline_user_args():
		if argument == "--profile=inventory":
			%Mode.select(1)
			_select_mode(1)
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
	if auto_connect and not _inventory_mode:
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

func _build_inventory_ui(parent: Control) -> void:
	var scroll := ScrollContainer.new()
	scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	_named(scroll, "InventoryView", parent)
	scroll.visible = false
	var column := VBoxContainer.new()
	column.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	column.add_theme_constant_override("separation", 14)
	scroll.add_child(column)
	_inventory_section("모두의 수집 현황", "PublicState", "공용 상태를 기다립니다.\n같은 방의 수집 횟수와 참가 인원만 표시합니다.", column)
	_inventory_section("내 비공개 인벤토리", "PrivateState", "방에 입장하면 서버에서 내 아이템을 가져옵니다.", column)
	var actions := HBoxContainer.new()
	actions.add_theme_constant_override("separation", 8)
	column.add_child(actions)
	for item in [["나무 줍기", "CollectWood"], ["돌 줍기", "CollectStone"], ["열쇠 줍기", "CollectKey"]]:
		_button(item[0], item[1], actions).size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_inventory_section("최근 내 명령 결과", "CommandResult", "아직 실행한 명령이 없습니다.", column)
	_button("미확인 명령 같은 내용으로 재확인", "RetryCommand", column)
	_label("처리 확인 번호는 내 명령 기준입니다. 재확인은 같은 행동의 결과를 요청합니다.", column, 13).autowrap_mode = TextServer.AUTOWRAP_WORD_SMART

func _inventory_section(heading: String, node_name: String, text: String, parent: Node) -> void:
	var panel := PanelContainer.new()
	var box := StyleBoxFlat.new()
	box.bg_color = Color("112e34")
	box.set_content_margin_all(16)
	box.set_corner_radius_all(5)
	panel.add_theme_stylebox_override("panel", box)
	parent.add_child(panel)
	var content := VBoxContainer.new()
	content.add_theme_constant_override("separation", 10)
	panel.add_child(content)
	_label(heading, content, 19)
	var label := _label(text, content, 15, node_name)
	label.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	label.size_flags_horizontal = Control.SIZE_EXPAND_FILL

func _select_mode(index: int) -> void:
	if is_instance_valid(client) and client.status != "disconnected":
		%Mode.select(1 if _inventory_mode else 0)
		return
	_inventory_mode = index == 1
	if is_instance_valid(client):
		client.set_game_profile("inventory" if _inventory_mode else "arena")
	%Credential.clear()
	_clear_private_ui()
	%RoomList.clear()
	_next_cursor = null
	%ServerURL.text = "http://127.0.0.1:3001" if _inventory_mode else "http://127.0.0.1:3000"
	%GameID.text = "inventory" if _inventory_mode else "arena"
	%AccountFields.visible = _inventory_mode
	%GuestConnect.visible = not _inventory_mode
	%ArenaView.visible = not _inventory_mode
	%MovementHint.visible = not _inventory_mode
	%ArenaLegend.visible = not _inventory_mode
	%InventoryView.visible = _inventory_mode
	%Reconnect.text = "같은 계정 재접속" if _inventory_mode else "같은 게스트 재접속"
	%Logout.text = "계정 종료" if _inventory_mode else "게스트 종료"
	%Subtitle.text = "계정 인벤토리   /   같은 방의 공용 상태와 내 아이템 확인" if _inventory_mode else "파티 테스트 코트   /   같은 방에서 함께 움직이기"
	_on_left()
	_on_status("disconnected")

func _valid_connection_fields() -> bool:
	var url: String = %ServerURL.text.strip_edges()
	return (url.begins_with("http://") or url.begins_with("https://")) and not %GameID.text.strip_edges().is_empty()

func _context_edited() -> void:
	if is_instance_valid(client) and client.status == "disconnected":
		client.logout()
	_clear_private_ui()
	_refresh_controls()

func _connect_account() -> void:
	var credential: String = %Credential.text
	%Credential.clear()
	_clear_private_ui()
	client.connect_account(%ServerURL.text.strip_edges(), %GameID.text.strip_edges(), credential)
	_refresh_controls()

func _clear_private_ui() -> void:
	_command_waiting = false
	%PrivateState.text = "방에 입장하면 서버에서 내 아이템을 가져옵니다."
	%CommandResult.text = "아직 실행한 명령이 없습니다."

func _apply_inventory_snapshot(value: Dictionary) -> void:
	var state: Dictionary = value.get("state", {})
	var count := int(state.get("total_collected", 0))
	var players := int(state.get("players", 0))
	%Score.text = "공동 수집  %d" % count
	%Participants.text = "참가자 %d / %d" % [players, _room_capacity]
	%PublicState.text = "수집 횟수  %d\n참가 인원  %d" % [count, players]
	%Stats.text = "내 명령 처리 확인  %d    서버 tick  %d    snapshot  %d" % [int(value.get("last_processed_command_seq", 0)), int(value.get("server_tick", 0)), int(value.get("snapshot_seq", 0))]
	%RoomIdentity.text = "게임  %s\n방  %s\ninstance  %s\n나  %s" % [value.get("game_id", ""), value.get("room_id", ""), value.get("room_instance_id", ""), client.player_id]
	if _command_result_unavailable:
		%Notice.text = COMMAND_RECOVERY_NOTICE
	elif _pending.is_empty():
		%Notice.text = "아이템을 골라 한 번씩 수집하세요. 내 비공개 인벤토리는 이 화면에만 표시합니다."
	_refresh_controls()

func _on_private_state(value: Dictionary) -> void:
	if not _inventory_mode or not client.is_in_room or client.status != "in_room" or _pending in ["leave", "logout"]:
		_clear_private_ui()
		return
	var items: Array = value.get("inventory", [])
	if value.is_empty():
		%PrivateState.text = "서버의 내 비공개 상태를 기다립니다."
	elif items.is_empty():
		%PrivateState.text = "내 아이템이 없습니다. 아래에서 아이템을 수집하세요."
	else:
		var lines := PackedStringArray()
		for item in items:
			lines.append("• " + str(item))
		%PrivateState.text = "내 아이템 %d개\n%s" % [items.size(), "\n".join(lines)]

func _collect(item: String) -> void:
	if not _inventory_mode or client.has_pending_game_command():
		return
	if client.send_game_command({"action":"collect", "item":item}):
		%CommandResult.text = "명령 %d · 서버 결과 대기 중" % client.pending_command_seq
	_refresh_controls()

func _retry_command() -> void:
	if client.retry_pending_command():
		%CommandResult.text = "명령 %d · 같은 행동의 결과 재확인 중" % client.pending_command_seq
	_refresh_controls()

func _on_command_pending(pending: bool) -> void:
	_command_waiting = pending
	_refresh_controls()

func _on_command_result(result: Dictionary) -> void:
	if _command_result_unavailable:
		return
	if not _inventory_mode or not client.is_in_room or client.status != "in_room" or _pending in ["leave", "logout"]:
		return
	var seq := int(result.get("command_seq", 0))
	if result.get("ok", false):
		var detail: Dictionary = result.get("result", {})
		%CommandResult.text = "명령 %d · 성공 (최종 결과)\n내 인벤토리 %d개" % [seq, int(detail.get("inventory_count", 0))]
	else:
		var error: Dictionary = result.get("error", {})
		%CommandResult.text = "명령 %d · 실패 (최종 결과)\n오류: %s\n이 번호는 처리됐습니다. 다음 행동을 선택하세요." % [seq, str(error.get("code", "COMMAND_FAILED"))]
	_refresh_controls()
