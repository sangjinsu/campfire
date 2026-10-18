# Godot 계정·단발 명령·개인 상태 클라이언트

## 목적과 범위

기존 Guest Arena 플레이를 유지하면서 Godot 샘플에서 Account Inventory에 로그인하고, 방을 공유하며, 단발 수집 명령과 자신의 private inventory를 확인한다. 기존 Rust runtime과 개발용 `account_server`의 프로토콜을 사용한다. 개발용 서버에는 독립 QA 포트를 위한 optional `CAMPFIRE_DEMO_BIND`만 추가하며 기본 `127.0.0.1:3001`은 유지한다. 외부 IdP, 영구 계정 저장소, runtime 프로토콜 변경은 이 작업의 범위에 없다.

기준 checkout은 `a5396690da80db4fbfc129e36d70c25d17b79a0c`, 설치 엔진은 Godot `4.7.2.stable.official.ed1daf0bf`다. 실제 소스와 이번 실행 증거를 기준으로 완료를 판단한다.

## 사용자 흐름

- 모드 선택은 `Guest Arena` / `Account Inventory`다. 기본 서버와 game ID는 각각 `http://127.0.0.1:3000` / `arena`, `http://127.0.0.1:3001` / `inventory`다.
- profile은 상태 schema와 입력 UI 선택이며 game ID namespace와 별개다. 다른 등록 game ID도 해당 schema를 제공하면 사용 가능하다. 주소/game/profile 변경은 disconnected 상태에서 수행하고 이전 자격·방·개인 표시를 폐기한다.
- Arena의 게스트 접속, 방 생성·목록·입장·퇴장, 이동, 재접속 흐름은 유지한다. Inventory는 마스킹 credential 입력과 계정 로그인, 같은 방 조작을 제공한다. Inventory에서 이동 input은 보내지 않는다.
- credential은 로그인 호출 직후 입력란에서 지운다. 파일·CLI 인자·로그·진단·스크린샷에 저장하지 않는다. runtime token은 client Node 메모리에만 보관한다.
- Inventory 본문은 공용 상태, 내 private inventory, 최근 내 명령 결과를 분리해 표시한다. 수집 대상은 `wood`, `stone`, `key`다. 처리 미확인 명령은 재확인 버튼으로 같은 요청 내용을 재전송한다.
- 같은 계정 로그인으로 token이 회전되면 동일 player와 살아 있는 room membership을 resume한다. 다른 계정은 이전 계정의 개인 상태·명령 결과를 즉시 숨기고 새 identity로 시작한다. 잘못된 credential과 계정 기능 미설정은 각 서버 오류 code를 표시한다.

## 네트워크 계약

`POST /v1/games/{game_id}/sessions`, `Content-Type: application/json`, body `{"credential":"..."}`. 성공 HTTP 201은 `{player_id, token, expires_at, game_id}`이며 `guest_id` 필드가 없다. 게스트 endpoint의 기존 응답은 유지한다. WS `session.authenticated`의 `player_id`와 호환 alias `guest_id`는 발급받은 player ID와 같아야 한다. HTTP 발급과 WS 인증 모두 game ID 및 정수·식별자 schema를 검증한다. credential을 URL에 넣지 않고 redirect는 허용하지 않는다.

Inventory 예제의 공용 `state`는 `{total_collected:uint, players:uint}`다. `players`는 배열이 아니다. `private_state`는 `{inventory:Array[String]}`이며 각 계정의 최초 item은 `<subject>-starter-item`이다. private 필드가 생략된 새 snapshot은 이전 private 내용을 유지하지 않고 비운다. 공개 상태에 private를 병합하지 않는다.

명령은 `game.command`이며 `{room_id,room_instance_id,command_seq,payload:{action:"collect",item:"wood"|"stone"|"key"}}`를 보낸다. terminal 결과는 `game.command.result`이고 성공은 `ok:true,result:{inventory_count:uint}`, 게임 거부는 `ok:false,error:{code,message,retryable}`다. 성공과 게임 오류 모두 sequence를 소비한다. `SERVER_BUSY` 등 실행 전 거부는 소비하지 않는다.

## 고정 client/UI 인터페이스

기존 `connect_guest`, room·logout·reconnect API와 signals는 유지한다.

| 추가 API | 계약 |
|---|---|
| `set_game_profile(profile:String)->bool` | `arena` 또는 `inventory`, disconnected에서 선택. 변경 시 이전 바인딩 상태 폐기 |
| `connect_account(url:String,game:String,credential:String)->bool` | HTTP 교환 후 WS 인증. 진행중 요청·연결과 겹치지 않음 |
| `send_game_command(payload:Dictionary)->bool` | 활성 membership, 미확인 명령 없음일 때 새 seq 전송 |
| `retry_pending_command()->bool` | 같은 membership의 보관 payload·seq로 재전송 |
| `has_pending_game_command()->bool` | 아직 terminal 결과를 확정하지 못한 명령 유무 |
| `can_retry_pending_command()->bool` | 현재 연결·membership에서 보관 명령을 재전송할 수 있는지 |
| `command_result_received(result:Dictionary)` | 검증·중복 제거한 자신의 terminal 결과 |

추가 public properties: `player_id:String`, `game_profile:String="arena"`, `private_state:Dictionary={}`, `command_results:Array=[]`, `last_processed_command_seq:int=0`, `pending_command_seq:int=0`. `guest_id`는 기존 소비자용 player alias다. `last_error_command`는 HTTP account 오류에 대한 별도 문맥과 `game.command` 문맥을 제공하고 UI는 해당 작업 잠금만 해제한다.

## 명령과 개인 상태의 수명

1. 한 membership에서 미확인 명령은 한 개만 허용한다. `command_seq`는 input과 독립적으로 1부터 연속 증가하며 정밀도 안전 상한 `2^53-1`을 넘지 않는다.
2. 전송 때 payload를 deep-copy하고 origin/game/profile/player/room instance에 묶는다. timeout이나 일반 단절은 이 명령을 폐기하지 않는다. 재전송은 같은 seq·payload이며 request ID는 새로 발급할 수 있다.
3. full sync의 `command_results`는 현재 membership 범위와 각 schema를 확인한 뒤 `(room_instance_id,command_seq)`로 중복 제거한다. 정기 snapshot의 ack만으로 성공 결과를 생성하거나 pending을 종료하지 않는다.
4. resume에서 ack는 올라갔지만 보관 결과가 이미 퇴출됐으면 `COMMAND_RESULT_UNAVAILABLE`로 표시하고 새 명령을 막는다. 사용자는 방 퇴장·새 membership으로 복구할 수 있다. 과거 결과를 재실행하거나 성공으로 추정하지 않는다.
5. 단절 시 개인 표시를 비우고 조작을 막는다. 동일 계정·membership resume을 위한 token, 명령 복구 metadata는 유지한다. origin/game/profile/player 변경, logout, leave, 만료, room closure, 새 membership은 개인 상태·결과·pending을 모두 비운다.
6. stale HTTP callback·이전 socket·다른 game/room/instance의 응답은 상태를 바꾸지 못한다. 잘못된 전체 sync를 부분 적용하지 않는다. 같은 정기 snapshot sequence와 ack 회귀도 거부한다.

## 검증과 완료 기준

- Godot 단위 검사는 HTTP/schema/alias 오류, 잘못된 inventory state, private 생략 초기화, 안전 정수, 외부 room/instance 응답, duplicate result, terminal 게임 오류, timeout·retry·resume, ledger eviction과 identity 전환을 검증한다.
- 실제 `account_server`와 서로 다른 두 native Godot 프로세스가 각각 Alice/Bob으로 로그인한다. UI control 경로로 같은 방 입장, 양쪽 collect, 동일 public count, 다른 private inventory, 자신의 결과만 표시됨을 확인한다.
- 한 쪽 재로그인/token rotation 및 단절·resume 뒤 동일 player·room·inventory를 확인하고 미확인 명령의 중복 적용이 없음을 검증한다. invalid credential, verifier disabled, logout/profile 변경의 표시 초기화도 확인한다.
- 각 native 창은 실제 renderer 프레임 뒤 PNG를 남기고 사람이 읽을 수 있는 화면·JSON 결과·로그를 함께 보존한다. 실제 GUI 두 프로세스 증거와 headless 검사를 구분한다.
- 기존 `tools/validate_godot.py --counts 8 16 --duration 20` 및 Godot legacy unit/UI 검사를 통과한다. source hash/commit, 엔진 버전, 명령·exit code를 기록한다. 기존 사용자의 서버·창을 종료하지 않는다.
- 독립 Architect 검토 후 커밋·normal push하고 원격 main SHA 일치를 확인한다. 단위 통과를 실제 두 창 플레이 또는 push 증거로 대신하지 않는다.
