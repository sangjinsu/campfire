# Godot API 확인 기록

2026-10-18, 설치 엔진 `4.7.2.stable.official.ed1daf0bf` (`/opt/homebrew/bin/godot --version`). gstack `/browse`로 아래 공식 stable 문서를 직접 열어 계약을 확인했다. API 구현 전에 network/UI 담당자에게 확인 내용을 전달했다.

- [WebSocketPeer](https://docs.godotengine.org/en/stable/classes/class_websocketpeer.html): `connect_to_url(url)`는 non-blocking이고 반환 `OK`는 연결 완료가 아니다. 매 `_process`에서 `poll()` 후 `get_ready_state()`를 확인한다. `STATE_OPEN`에서 `get_available_packet_count()`만큼 `get_packet()`을 읽고 UTF-8로 변환한다. `STATE_CLOSING`에서도 poll을 계속한다. JSON은 `send_text(String) -> Error`를 사용한다. 입력/출력 buffer 기본값은 각 65,535 bytes, `max_queued_packets`는 4,096이며 모두 유한 값으로 조절 가능하다. TLS 인증은 기본 검증하고 토큰을 URL에 넣지 않는다.
- [HTTPRequest](https://docs.godotengine.org/en/stable/classes/class_httprequest.html): tree 안의 노드에서 `request(url, PackedStringArray(), HTTPClient.METHOD_POST)`를 호출한다. 반환 `OK`는 요청 생성 성공이다. `request_completed(result:int,response_code:int,headers:PackedStringArray,body:PackedByteArray)`에서 transport `RESULT_SUCCESS`와 HTTP 201을 각각 검증한다. 동시 요청은 `ERR_BUSY`; timeout 기본값 0은 무기한, `body_size_limit` 기본값 -1은 무제한이므로 둘 다 설정한다.
- [JSON](https://docs.godotengine.org/en/stable/classes/class_json.html): `JSON.new().parse(text)`는 Error를 반환하고 성공한 `data`는 Variant다. Objects는 Dictionary, 배열은 Array이며 각 schema를 확인한다. 숫자는 `String.to_float()`로 읽으므로 seq/tick은 유한·정수·0 이상·정확한 정수 상한(2^53-1)을 확인한 뒤 int로 변환한다. `parse_string` 실패 null과 정상 null을 구별하기 어렵기 때문에 parse Error를 확인한다.
- [GDScript](https://docs.godotengine.org/en/stable/tutorials/scripting/gdscript/gdscript_basics.html): `:=`는 정적 타입을 추론한다. Dictionary 값 반환과 Array front/back는 Variant이므로 경계 값에는 `: Variant` 및 타입 검사를 쓰고 검사 후 명시적으로 Dictionary/Array/int를 선언한다. 정적 분석과 실제 엔진 parse를 함께 실행한다.
- [Control](https://docs.godotengine.org/en/stable/classes/class_control.html): `set_anchors_and_offsets_preset`은 anchor/offset preset을 함께 적용한다. 위의 Control이 마우스를 가로챌 수 있으므로 장식 노드는 `MOUSE_FILTER_IGNORE`, 상호작용 노드는 적절한 mouse_filter를 둔다. Container가 자식 layout을 결정한다.
- [SystemFont](https://docs.godotengine.org/en/stable/classes/class_systemfont.html): `font_names:PackedStringArray`의 첫 시스템 일치 font를 사용한다. macOS 지원, `allow_system_fallback` 기본 true. 한국어 font 후보를 명시한다.
- [Input](https://docs.godotengine.org/en/stable/classes/class_input.html): `get_vector(negative_x,positive_x,negative_y,positive_y)`는 길이를 1 이하로 제한한다. `action_press`는 `_input`을 호출하지 않는다. 테스트는 `parse_input_event(InputEventKey/Action)`로 게임의 입력 이벤트 경로를 검증한다. OS 입력을 발생시키지는 않는다.
- [Viewport](https://docs.godotengine.org/en/stable/classes/class_viewport.html): 너무 이른 `get_texture`는 검은/지난 프레임일 수 있다. `await RenderingServer.frame_post_draw` 뒤 `get_viewport().get_texture().get_image().save_png(path)`로 실제 렌더러 결과를 저장한다. headless dummy renderer는 시각 품질 근거로 쓰지 않는다.
- 엔진 `--help` 직접 확인: `--headless`는 headless display + Dummy audio, `--path`는 project.godot 폴더, `--script`는 SceneTree 스크립트 실행, `--check-only`는 script parse, `--` 이후 값은 `OS.get_cmdline_user_args()`에만 전달된다. `--audio-driver Dummy`, `--single-threaded-scene`, `--max-fps`를 이용해 다중 프로세스 검증 자원을 제한한다.

문서의 stable 링크는 이후 버전으로 이동할 수 있다. 위 계약은 조회 시점 문서와 로컬 4.7.2 엔진의 실제 parse/통합 실행 결과를 함께 기준으로 삼는다.

## 마지막 응답과 close가 같은 poll에 도착하는 경계

실제 로그아웃 통합 테스트가 마지막 `session.logged_out` 처리 전에 `STATE_CLOSED`와 수신 packet 0개를 관측했다. 로컬 엔진 commit과 일치하는 [Godot 공식 native 구현](https://github.com/godotengine/godot/blob/ed1daf0bf/modules/websocket/wsl_peer.cpp)을 추가 확인했다. `poll()`은 close 송수신 완료 시 `close(-1)`을 부르고, CLOSED 상태에서는 수신 `in_buffer`와 packet buffer를 비운다. `get_available_packet_count`는 OPEN이 아니면 0, `get_packet`도 OPEN 외에는 실패한다. 따라서 CLOSED 뒤 마지막 메시지를 drain하는 방식은 해결책이 아니다. 사용자 로그아웃 의도에 따른 로컬 자격 폐기와 서버의 토큰 폐기 확인을 별도 근거로 검증해야 한다. 이는 자동 재접속을 위한 일반 단절의 token 보존 정책과 구분한다.

## Adapter 오류 문맥 계약

`CampfireClient.last_error_command:String`의 초기값은 빈 문자열이다. `error_received`를 emit하기 직전에 해당 `request_id`의 pending command type을 기록하며, request timeout도 원래 command type을 기록한다. local/no-request-id 오류는 빈 문자열이다. success/status 전환 때는 초기화하지 않으므로 각 오류 signal handler 안에서 현재 오류의 문맥을 읽는 용도다. 다음 error emit이 값을 대체한다. 예를 들어 `room.join`의 `GAME_MISMATCH`는 정상 guest 자격을 유지하고 `session.authenticate`의 game mismatch는 인증 경로의 자격 실패로 처리한다. UI는 pending command와 맞는 오류/성공만 해당 조작의 잠금을 해제한다.

## 계정 로그인·Inventory UI 추가 확인 (2026-10-18)

gstack browse의 독립 state `/tmp/campfire-godot-account-docs/browse.json`으로 공식 문서를 열어 아래 API를 다시 확인하고 network/UI 구현 담당자에게 전달했다. 기존 browse daemon이나 사용자 Godot 프로세스를 재시작하지 않았다.

- [HTTPRequest.request](https://docs.godotengine.org/en/stable/classes/class_httprequest.html#class-httprequest-method-request): 네 번째 인자는 `request_data:String`이다. `JSON.stringify({"credential": credential})`를 body로, `Content-Type: application/json`을 header로, `HTTPClient.METHOD_POST`를 method로 전달한다. 반환 `OK`는 HTTP 응답 성공이 아니라 요청 생성 성공이며 tree 밖은 `ERR_UNCONFIGURED`, 진행 중 재호출은 `ERR_BUSY`다. `request_completed`의 transport result와 HTTP 201을 각각 검증한다.
- [LineEdit](https://docs.godotengine.org/en/stable/classes/class_lineedit.html): `secret:bool=true`는 모든 문자를 `secret_character`로 표시한다. `clear()->void`는 text를 지우며 `text_changed(new_text:String)`는 text가 바뀔 때 발생한다. 마스킹은 저장·로그 방지 기능이 아니므로 credential은 로그인 호출 직후 필드에서 지우고 별도 진단에 넣지 않는다.
- [OptionButton](https://docs.godotengine.org/en/stable/classes/class_optionbutton.html): `add_item(label:String,id:int=-1)`로 항목을 등록하고 `item_selected(index:int)`에서 사용자 선택을 처리한다. `select(idx:int)`는 programmatic 선택이다. 사용자 선택 signal을 가정하지 말고 CLI/테스트에서는 모드 적용 handler를 명시적으로 호출한다.
- [WebSocketPeer](https://docs.godotengine.org/en/stable/classes/class_websocketpeer.html): `poll()`로 연결·수신·송신 상태를 진행시키며 ready state에 맞춰 packet을 읽는다. JSON 전송은 `send_text(String)->Error`를 사용한다. 계정 token도 기존 JSON 인증 메시지에만 넣고 URL에는 넣지 않는다.
- 로컬 `/opt/homebrew/bin/godot --help`에서 `--resolution WxH`, `--position X,Y`, `--single-window`, `--path`, `--script`, `--audio-driver Dummy`를 확인했다. 두 실제 native 클라이언트는 별도 프로세스로 실행한다. `--headless`는 dummy renderer라서 native 화면 증거로 세지 않는다.

JSON 정밀도, `Input.parse_input_event`, renderer PNG 캡처의 기존 공식 근거는 위 기록을 따른다. 새 구현은 기존 안전 정수 검사와 실제 엔진 parse/통합 검사를 유지한다. Inventory 공용 `players`가 정수라는 계약과 HTTP account 응답에 `guest_id`가 없다는 계약은 Godot API가 아니라 현재 `account_server.rs` 및 `docs/protocol.md`에서 확인했다.
