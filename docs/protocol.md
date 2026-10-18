# Campfire protocol v1

전송은 UTF-8 JSON WebSocket이다. TLS 외부 노출에서는 HTTPS/WSS를 사용한다. client command에는 `v:1`, `type`, 연결 안에서 고유한 `request_id`(비어 있지 않은 최대 128 byte 문자열)를 넣는다. 미지원 버전, 알 수 없는 type/필드, 누락 필드와 잘못된 타입은 오류다. 입력은 부호 없는 64-bit 증가 정수이며 JavaScript client는 정밀도 손실에 주의한다.

`POST /v1/games/{game_id}/guests`는 body 없이 호출한다. 성공 HTTP status는 201이며 응답은 `guest_id`, `token`, `expires_at`(Unix milliseconds), `game_id`다. 미등록 게임은 404, 발급 rate 초과는 429, 서버 수용량 거부는 503이다. 토큰은 한 game_id 전용이며 서버에는 SHA-256 hash만 보관한다. URL/query/access log에 토큰을 넣지 않는다. `GET /health`는 게임/토큰 상태를 공개하지 않는다. 이 두 경로와 `GET /v1/games/{game_id}/ws` 외의 HTTP API는 없다.

WS 연결 뒤 기본 5초 안에 다음 메시지를 먼저 보낸다.

```json
{"v":1,"type":"session.authenticate","request_id":"auth-1","token":"<guest token>"}
```

`session.authenticated`는 `game_id`, `guest_id`, `expires_at`, `room_id`, `room_instance_id`, `can_resume`, `reservation_expires_at`를 반환한다. 룸이 없으면 식별자와 예약 시각은 null이다. 재인증은 구 연결을 무효화하고 기존 활성 자리를 예약으로 바꾼다. 이미 예약된 자리의 기한은 연장하지 않는다. 만료 시각 표시는 runtime 생성 시 고정한 monotonic/Unix 기준점으로 변환하므로 재인증마다 같은 예약의 표시 시각이 바뀌지 않는다. 조작 재개에는 `resume`이 필요하다. generation은 서버 내부 권한이며 client가 보내는 generation을 신뢰하지 않는다.

| Client type | 필수 추가 필드 | 직접 성공 응답 |
| --- | --- | --- |
| `session.authenticate` | `token` | `session.authenticated` |
| `session.logout` | 없음 | `session.logged_out`, 토큰/자리 폐기 후 연결 종료 |
| `room.create` | `settings` object | 생성자 자동 입장 후 `room.entered` full sync |
| `room.list` | `cursor`(첫 요청 null), `limit`(1~100) | `room.listed`: `rooms`, `next_cursor` |
| `room.join` | `room_id`, `room_instance_id` | `room.entered` full sync |
| `room.leave` | `room_id`, `room_instance_id` | `room.left`, 즉시 자리 해제; 재전송 성공 |
| `resume` | `room_id`, `room_instance_id` | `room.resumed` full sync |
| `game.input` | `room_id`, `room_instance_id`, `input_seq`, `payload` | `game.input.accepted`: `input_seq` |

모든 직접 응답과 오류는 `request_id`를 돌려준다. 선택적 `game_id`를 command에 넣으면 인증 경로의 게임과 같아야 한다. 실제 room lookup과 resume은 token/connection/room 게임을 다시 검증한다. 목록은 같은 게임의 공개 룸만 포함하며 항목은 `game_id`, `room_id`, `room_instance_id`, `members`, `capacity`다. 예약 자리도 `members`와 정원에 포함한다. cursor는 서버가 발급한 값을 그대로 사용한다.

한 게스트는 예약을 포함해 한 룸만 가질 수 있다. 현재 룸이 있는데 다시 create/join하면 새 membership을 만들지 않는다. 정원 확인과 임시 입장/commit/rollback은 룸 task가 소유한다. commit 전 실패/취소는 자리와 임시 룸을 제거하고, commit 후 응답 실패는 단절 예약으로 복구한다.

연결별 bounded 응답 cache에 같은 `request_id`/같은 본문이 있으면 저장된 응답을 재전송한다. 같은 ID에 다른 본문은 `REQUEST_ID_CONFLICT`다. cache에서 제거되거나 새 연결인 경우 무한한 exactly-once 보장은 없으며 membership/instance/seq 규칙이 중복 변경을 막는다.

```json
{"v":1,"type":"room.create","request_id":"create-1","settings":{"capacity":16,"public":true}}
{"v":1,"type":"room.list","request_id":"list-1","cursor":null,"limit":20}
{"v":1,"type":"room.join","request_id":"join-1","room_id":"<room>","room_instance_id":"<instance>"}
{"v":1,"type":"game.input","request_id":"input-1","room_id":"<room>","room_instance_id":"<instance>","input_seq":1,"payload":{"move_x":1,"move_y":0}}
```

`game.input.accepted`는 수신 수락이다. 실제 처리 완료는 snapshot의 `last_processed_input_seq`로만 확인한다. runtime은 플레이어별 tick 안에서 최신 유효 방향 하나를 선택하고 더 오래된 입력을 폐기한다. stale/중복/현재 generation의 마지막 수신 번호에서 과도한 도약은 거부한다. 적용 직전 generation을 다시 검증하고 연결 교체 시 구 입력과 arena 보관 방향을 폐기한다. 새 generation의 seq 기준은 마지막 실제 처리 ack로 재설정한다. 클라이언트는 이전 미처리 입력을 자동 재생하지 않는다.

정기 `snapshot`에는 request_id가 없다. `room.entered`/`room.resumed`는 같은 snapshot 필드를 포함한 full sync다.

```json
{"v":1,"type":"snapshot","game_id":"arena","room_id":"<room>","room_instance_id":"<instance>","server_tick":120,"snapshot_seq":60,"last_processed_input_seq":19,"state":{"players":[{"id":"<guest>","x":2.5,"y":1.0,"connected":true}],"shared_score":0,"goal":{"x":5.0,"y":0.0,"radius":0.5,"active":true}}}
```

ack는 수신자 자신의 값이다. server_tick/snapshot_seq는 재접속으로 초기화하지 않는다. snapshot에는 game_id도 포함된다. 단일 writer는 초기 full sync를 reliable queue에서 먼저 보낸 뒤 최신 snapshot slot을 읽는다. client는 다른 instance와 이미 적용한 snapshot_seq 이하를 버린다. 재접속 full sync는 현재 상태를 새 기준으로 적용한다.

단절 시 기본 30초 예약하며 arena는 즉시 이동을 멈춘다. 유예/토큰 TTL/로그아웃 중 먼저 발생한 조건이 자리를 해제한다. 인증된 새 연결에서 다음 요청을 보내 같은 플레이어와 instance를 복구한다.

```json
{"v":1,"type":"resume","request_id":"resume-1","room_id":"<room>","room_instance_id":"<instance>"}
```

명시적 leave 뒤에는 일반 join이 가능하고 이전 예약 resume은 거부한다. 마지막 활성/예약 자리가 사라지면 룸이 종료한다. 정상 종료와 unwind panic은 directory/session 참조를 정리하고 가능한 연결에 `room.closed`(`room_id`, `room_instance_id`, `reason`)를 보낸다. restart 뒤 세션/룸은 복원하지 않는다.

오류는 다음 형식이다. 알 수 없는/복구 불가능한 JSON에서는 request_id가 null일 수 있다.

```json
{"v":1,"type":"error","request_id":"join-1","code":"ROOM_FULL","message":"Room is full","retryable":false}
```

| 오류 | 의미 |
| --- | --- |
| `UNAUTHENTICATED` | 미인증, 만료/폐기 token, 구 연결 |
| `GAME_MISMATCH` | token/경로/대상 game 경계 불일치 |
| `ROOM_NOT_FOUND` | 대상 룸 없음 |
| `ROOM_FULL` | 활성+예약 자리 정원 초과 |
| `RESUME_EXPIRED` | 유효 예약 없음/만료 |
| `STALE_INSTANCE` | 과거 room epoch |
| `STALE_INPUT` | 중복/역순 입력 |
| `RATE_LIMITED` | 개인 또는 발급 빈도 제한 |
| `SERVER_BUSY` | 공유 queue/서버 수용량 포화, retryable true |
| `INVALID_MESSAGE`, `UNSUPPORTED_VERSION` | envelope/schema 오류 |
| `INVALID_INPUT`, `INVALID_SETTINGS` | 게임 입력/설정 거부 |
| `REQUEST_ID_CONFLICT` | 같은 요청 ID의 다른 본문 |

추가 오류에는 `GAME_NOT_FOUND`(미등록 게임), `ALREADY_IN_ROOM`(기존 멤버십), `FRAME_TOO_LARGE`(개인 JSON 한도), `CANCELLED`(인증/입장 중 연결 취소), `GAME_FAILED`(factory panic)가 있다. 오류 code의 실제 상세 분류는 runtime tests와 함께 유지한다. 공유 queue 포화는 개인 위반에 포함하지 않는다. JSON application 한도 초과는 처리하지 않고 위반으로 센다. transport는 fragmentation 우회를 제한하는 별도 안전 상한(설정의 4배)을 두며 이를 넘는 frame/message는 즉시 종료될 수 있다. 개인 frame/빈도 위반이 지속되거나 reliable queue가 포화되면 그 연결을 종료하고 예약 절차를 적용한다. writer는 동일 send future를 기한 사이에도 유지하며, 설정한 횟수까지 완료되지 않으면 연결을 종료한다. reliable full sync를 첫 timeout 때 버리고 후속 snapshot으로 진행하지 않는다. snapshot slot은 이전 상태를 대체할 수 있으므로 중간 snapshot 모두 수신할 것을 요구하지 않는다. arena에는 별도 필수 reliable 게임 이벤트가 없으며 최신 full sync로 복구한다.
