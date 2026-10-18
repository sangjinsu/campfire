# Campfire runtime 확장 명세

기준: `b94c427`, protocol v1, Rust 1.96 / edition 2024. 대상은 기존 계정 인증 교환, 단발 명령, 플레이어 전용 상태다. 기존 guest 발급과 Godot 이동 입력 계약을 유지한다.

## 1. 경계와 보장

- 호스트 애플리케이션이 외부 credential을 검증한다. Campfire는 검증된 identity를 game-scoped opaque session으로 교환한다. 특정 IdP SDK, 사용자 DB, 결제, 영구 저장은 포함하지 않는다.
- room actor만 게임 상태를 변경한다. `game.input`은 기존 tick별 latest-value lane을 유지한다. `game.command`는 독립 bounded FIFO에서 actor가 순차 실행한다.
- 명령 중복 방지는 살아 있는 동일 room membership에 한정한다. 재접속·동일계정 token rotation 동안 유지하지만 leave, grace/session 만료, room 종료, 서버 재시작 이후에는 유지하지 않는다. durable exactly-once를 보장하지 않는다.
- `Game::snapshot()`은 공용 데이터만 반환하는 신뢰된 호스트 코드다. runtime은 별도의 수신자별 `private_snapshot()` 결과를 해당 수신자에게만 전송한다. 게임이 공용 snapshot에 비밀을 넣으면 runtime이 이를 판별할 수 없다.
- 단일 프로세스/in-memory 구성이다. 외부 검증 I/O는 global state mutex 밖에서 수행하지만 기존 방 관리의 전역 mutex 구조를 분산 시스템으로 재설계하지 않는다.

## 2. 호스트 계정 API

새 `auth.rs` 공개 타입과 `lib.rs` re-export:

```rust
pub struct AccountIdentity {
    pub issuer: String,
    pub subject: String,
    pub claims: serde_json::Value,
}
pub enum PlayerIdentity {
    Guest,
    Account(AccountIdentity),
}
pub struct AccountGrant {
    pub identity: AccountIdentity,
    pub valid_for: std::time::Duration,
}
pub enum AuthError {
    InvalidCredential,
    Unavailable,
}
pub trait AccountAuthenticator: Send + Sync + 'static {
    fn authenticate<'a>(
        &'a self,
        game_id: &'a str,
        credential: &'a str,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<AccountGrant, AuthError>> + Send + 'a>>;
}
```

`AccountIdentity`와 `PlayerIdentity`는 `auth.rs`에 정의하고 `lib.rs`에서 re-export한다. 두 타입은 Clone + Debug를 지원하되 `AccountIdentity`의 custom Debug는 issuer/subject/claims를 모두 `[redacted]`로 가린다. credential은 저장하거나 로그에 남기지 않는다. `claims`는 검증기만 제공하고 클라이언트의 임의 identity/claims 입력은 거부한다.

```rust
pub fn Campfire::with_authenticator(
    config: Config,
    authenticator: std::sync::Arc<dyn AccountAuthenticator>,
) -> Result<Campfire, ConfigError>;
```

기존 `Campfire::new(config)`는 guest-only로 계속 작동한다. 기본 server CLI에 실계정 verifier를 임의 설치하지 않는다. 호스트 통합 예제와 fake verifier 기반 테스트를 제공한다.

### HTTP 계약

`POST /v1/games/{game_id}/sessions`

```json
{"credential":"host-issued-bearer-or-opaque-token"}
```

성공 `201`:

```json
{"player_id":"a_...","token":"opaque-runtime-token","expires_at":1790000000000,"game_id":"arena"}
```

`expires_at`은 기존과 같은 Unix milliseconds다. 반환 token으로 기존 WebSocket `session.authenticate`를 호출한다. guest 발급 응답은 그대로 유지한다. `session.authenticated`는 호환용 `guest_id`를 유지하고 `player_id`를 추가한다. `guest_id`와 `player_id` 모두 runtime seat identifier이며 외부 subject가 아니다. issuer/subject/claims/credential을 다른 플레이어나 공용 snapshot에 자동 추가하지 않는다.

| 조건 | HTTP / error code |
|---|---|
| 등록 안 된 game | 404 / `GAME_NOT_FOUND` |
| verifier 미설정 | 501 / `ACCOUNT_AUTH_DISABLED` |
| JSON/schema 오류 또는 빈 credential | 400 / `INVALID_MESSAGE` |
| body 크기 초과 | 413 / `FRAME_TOO_LARGE` |
| Content-Type 누락 또는 미지원 | 415 / `INVALID_MESSAGE` |
| credential 거부 | 401 / `UNAUTHENTICATED` |
| verifier timeout | 504 / `AUTH_TIMEOUT` |
| verifier 장애 또는 잘못된 grant | 503 / `AUTH_UNAVAILABLE` |
| issuance rate 제한 | 429 / `RATE_LIMITED` |
| 동시성·session·shutdown 제한 | 503 / `SERVER_BUSY` |

JSON 문법·schema 오류는 Axum 기본 status와 무관하게 400으로 정규화하고 payload 413 및 Content-Type 415는 보존한다. 외부 verifier의 내부 오류 문자열은 HTTP 응답에 노출하지 않는다. 실패/취소/timeout은 session을 만들거나 기존 token을 폐기하지 않는다.

### 자원·수명 제한

Config 공개 필드는 추가하지 않는다. 기존 `Config { ... }` literal의 source compatibility를 유지한다.

| 제한 | 기존 설정 / 기본값 |
|---|---|
| account 요청 body 및 credential 최대 bytes | `max_frame_bytes` / 16 KiB |
| claims JSON 직렬화 최대 bytes | `max_frame_bytes` / 16 KiB |
| issuer / subject 길이 | 각각 1..=128 / 1..=256 bytes, 공백만 있는 값 거부 |
| verifier 최대 실행 시간 | `auth_timeout` / 5 s |
| verifier 동시 요청 수 | `max_unauthenticated_connections` / 256, HTTP verifier와 미인증 WS가 동일 semaphore 공유 |
| 발급 rate | 기존 guest per-source/global bucket 공유: 2/s burst 10, 100/s burst 200 |
| account session 최대 lifetime | `guest_ttl` / 86,400 s |
| active session 수 | 기존 `max_sessions` / 10,000, guest/account 합산 |

`valid_for`는 인증 요청 시작 시각 기준 남은 유효기간이며, 검증기는 그 약정에 맞춰 반환한다. 0보다 크고 Config가 허용하는 finite duration 범위여야 한다. verifier를 시작한 시각부터 `min(valid_for, guest_ttl)`로 deadline을 계산하여 검증 지연으로 권한 수명을 늘리지 않는다. verifier가 반환한 시점과 state commit 시점 모두 deadline·shutdown을 재검사한다. timeout, cancellation, panic/실패 경로에서 semaphore permit을 반환한다. 호스트 verifier가 실제 credential 만료·권한을 검사하는 책임을 가진다.

### 동일 계정 token rotation

identity key는 `(game_id, issuer, subject)`다. 살아 있는 동일 key의 session은 한 개만 유지한다. issuance는 state mutex 안에서 직렬화하며 동시 login의 최종 성공 token만 유효하다.

1. verifier 성공과 grant 검증이 끝난 뒤 기존 유효 session을 찾는다.
2. 새 token과 hash를 생성한다. 기존 player identifier, room reference, room의 command ledger를 유지한다.
3. prepare 단계의 room `Control::Replace`는 generation을 증가시키고 old output을 닫아 이전 연결과 queued work를 차단한다. `Rotation`의 공유 mutex가 `RotationDecision::Pending { valid_until }`, `Committed`, `Aborted` 중 하나를 관리한다. candidate grace는 `PendingRotation`에 따로 보관하고 기존 grace/claims/token deadline에 prospective 값을 적용하지 않는다. `valid_until`은 기존 seat의 grace와 token lifetime 상한이다. 모든 await 동안 기존 token map entry를 유지한다.
4. room의 기존 reservation은 연장하지 않는다. commit 후 deadline은 `min(existing_grace_deadline, new_token_deadline)`이다. 기존 예약이 없으면 `min(now + reconnect_grace, new_token_deadline)`를 사용한다. 새 credential은 token 수명을 갱신하지만 재접속 grace를 갱신하지 않는다.
5. 마지막 await 이후 shutdown과 새 grant deadline을 다시 검사한다. 공유 decision mutex 안에서 기존 seat의 `valid_until`과 새 deadline이 아직 유효할 때만 Committed로 전환한다. 이 결정과 old token hash 제거·새 hash 삽입 사이에는 await가 없다. actor는 expire/tick/control/data 처리 전에 `settle_rotations`로 committed deadline/claims와 candidate grace를 적용한다. expire와 commit이 겹치면 같은 decision을 기준으로 새 deadline 적용 또는 abort를 결정하므로 기존 TTL 만료만으로 성공한 rotation의 seat/ledger를 제거하지 않는다. 이후 old token 인증/old socket 명령을 거부한다.
6. prepare와 최종 commit 사이 취소/timeout/오류는 issuer의 `RotationGuard`가 abort하고 새 claims/token/deadline/candidate grace를 적용하지 않는다. actor에서 pending member가 사라져 `PendingRotation`이 Drop되는 경우도 abort하여 뒤늦은 session commit을 막는다. 기존 output은 이미 닫혔을 수 있으므로 old token으로 재인증할 수 있지만 grace/session 만료를 넘겨 seat를 복구하지 않는다. 이미 만료된 기존 session을 cleanup하는 await 이후에도 shutdown과 새 grant deadline을 다시 검사한다.
7. 새 WebSocket에서 authenticate 후 resume하면 기존 player/room/command ledger와 갱신 identity를 이용한다. 만료된 membership은 되살리지 않는다.

새 grant의 identity claims는 room에 전달한다. generation fence와 deadline/identity 교체는 actor에서 일관되게 적용하고, 더 짧은 grant로 이미 만료된 seat는 즉시 제거한다. 기존 token 만료/로그아웃 후 새 로그인은 새 session이며 이전 membership 복구를 보장하지 않는다. bounded session 수에서 선형 검색을 사용할 수 있다. 10배 규모 확장 시 account index를 추가하는 것은 후속 최적화다.

## 3. Game 확장 hooks

기존 Game 구현에 새 필수 메서드를 추가하지 않는다.

```rust
fn player_joined_with_identity(&mut self, player_id: &str, identity: &PlayerIdentity) {
    self.player_joined(player_id);
}
fn player_identity_changed(&mut self, _player_id: &str, _identity: &PlayerIdentity) {}
fn validate_command(&self, _player_id: &str, _payload: &Value) -> Result<(), GameError> {
    Err(GameError::new("COMMAND_UNSUPPORTED", "game commands are not supported"))
}
fn apply_command(&mut self, _player_id: &str, _payload: &Value) -> Result<Value, GameError> {
    Err(GameError::new("COMMAND_UNSUPPORTED", "game commands are not supported"))
}
fn private_snapshot(&self, _player_id: &str) -> Option<Value> { None }
```

초기 admission은 `player_joined_with_identity`를 한 번 호출한다. 계정 rotation 시 기존 seat에는 `player_identity_changed`를 호출하며 재입장 callback을 중복 호출하지 않는다. Guest identity는 `Guest`다. Game이 계정에 따른 인벤토리/권한을 구성하려면 join hook에서 값을 저장하고 identity change에서 갱신한다.

모든 hook은 신뢰된 synchronous code이며 blocking I/O를 하지 않는다. `apply_command`의 Err는 게임이 mutation을 남기지 않는 계약이다. runtime은 임의의 게임 mutation을 rollback할 수 없다. panic은 기존 room supervisor의 종료/격리 정책을 따른다.

## 4. 단발 명령 wire 계약

요청:

```json
{"v":1,"type":"game.command","request_id":"use-1","game_id":"arena","room_id":"r_...","room_instance_id":"i_...","command_seq":1,"payload":{"action":"use_item","slot":2}}
```

`game_id`의 optional/일치 검사와 request_id 1..=128 bytes 규칙은 기존과 같다. room_id, room_instance_id, command_seq, payload는 필수다. 미등록 필드를 거부한다.

terminal 성공:

```json
{"v":1,"type":"game.command.result","request_id":"use-1","game_id":"arena","room_id":"r_...","room_instance_id":"i_...","command_seq":1,"ok":true,"result":{"used":true}}
```

terminal 게임 오류:

```json
{"v":1,"type":"game.command.result","request_id":"use-1","game_id":"arena","room_id":"r_...","room_instance_id":"i_...","command_seq":1,"ok":false,"error":{"code":"ITEM_MISSING","message":"item does not exist","retryable":false}}
```

접수/대기 ACK는 없다. 결과는 reliable lane으로 요청자에게만 전송한다. 성공 결과와 게임 오류 모두 terminal이며 sequence를 소비하고 ledger에 기록한다. `validate_command` 오류는 apply를 호출하지 않는다. `apply_command` 오류도 기록해 중복 실행을 방지한다.

### FIFO, 순서, 재시도

- room에 input과 독립적인 bounded `mpsc` command queue를 둔다. capacity는 `room_input_capacity` / 기본 1024를 재사용한다. command 실행은 room actor의 receive branch에서 즉시 이루어진다. tick별 입력 대체와 달리 아이템 사용 같은 단발 행위를 덮어쓰지 않기 위해서다. tick/input과의 전역 순서는 actor 처리 순서이며 네트워크 전송 시간순을 보장하지 않는다.
- reader의 기존 60/s, burst 30 bucket을 input과 command가 공유한다. request mailbox 32, reliable queue 64, frame 한도는 유지한다.
- membership별 `command_seq`는 1부터 시작해 terminal 실행마다 정확히 1씩 증가한다. 0은 `STALE_COMMAND`, expected보다 큰 값은 `COMMAND_SEQUENCE_GAP`으로 거부한다. 이 비소비 오류는 기존 `error` envelope를 쓴다.
- room/player/generation/active-output/token-deadline 검사를 actor에서 replay와 mutation 직전에 수행한다. state lock을 잡은 채 command 결과를 기다리지 않는다.
- queue full은 `SERVER_BUSY`, authorization/room 오류는 기존 error code를 반환한다. 이 경로는 sequence를 소비하지 않으므로 같은 sequence로 재시도한다.
- terminal ledger는 member별 FIFO `request_cache_capacity` / 기본 128개다. fingerprint는 payload의 canonical JSON이며 request_id를 제외한다. clone에 `sort_all_objects()`를 적용해 중첩 object key를 재귀 정렬하므로 호스트가 `serde_json/preserve_order`를 켜도 key 순서만 다른 payload는 동일하다. 배열 순서는 유지하여 서로 다른 명령으로 구분한다. 동일 seq + 동일 payload는 retained 결과를 현재 request_id로 반환한다. 동일 seq + 다른 payload는 `COMMAND_SEQUENCE_CONFLICT`다.
- high-water 이하이지만 ledger에서 퇴출된 seq는 `STALE_COMMAND`다. 결과가 없어도 다시 실행하지 않는다. high-water는 ledger eviction으로 감소하지 않는다.
- `game.command`도 일반 request cache의 동일 request_id/다른 body 충돌 검사(`REQUEST_ID_CONFLICT`)를 유지한다. 동일 body cache hit라도 저장된 응답을 바로 반환하지 않고 actor의 membership/generation/deadline 및 sequence ledger를 다시 검사한다. 일반 cache가 퇴출되어도 command_seq ledger가 중복 실행을 막는다. 새 request_id로 재시도할 수 있으며 결과의 request_id를 현재 요청 값으로 바꾼다. 일시적 SERVER_BUSY 결과를 cache hit로 확정하지 않는다.
- actor가 generation 교체 전에 처리한 명령은 이미 terminal이다. 교체 후에 처리되는 이전 generation 명령은 비소비 거부한다. 큐에 들어갔다는 사실만으로 실행을 보장하지 않는다. 응답을 받지 못한 클라이언트는 resume 후 동일 seq/payload를 재시도한다.
- terminal 기록은 결과 전송보다 먼저 저장한다. reliable output 과부하/연결 종료로 결과 전달이 실패해도 seat가 유효한 동안 재생할 수 있다.
- 재접속은 pending command queue를 신규 generation 소유로 바꾸지 않는다. 고세대 명령을 거부한 뒤 새 세대의 재시도를 정상 처리한다. explicit leave 후 새 membership은 sequence 1로 시작한다. room instance가 달라지면 이전 instance 명령은 `STALE_INSTANCE`로 거부한다.

`apply_command` 성공 값은 trusted game output이다. result ledger의 항목 수를 제한하지만 임의 Game payload 메모리까지 강제 격리하지 않는다. 게임 개발자는 작고 bounded한 결과를 반환해야 한다.

## 5. 수신자별 상태와 resume

`room.entered`, `room.resumed`, `snapshot`에는 기존 `state` 구조를 유지하고 다음 필드를 추가한다.

```json
{"last_processed_command_seq":3,"private_state":{"inventory":["key"]}}
```

`private_state`는 `Game::private_snapshot(player_id)`가 Some일 때만 포함한다. None이면 필드를 생략한다. 공용 state와 병합하지 않아 필드 충돌·실수에 의한 public 승격을 막는다. 모든 full/initial/periodic/resume 경로에서 동일 projection을 사용한다. 공용 snapshot은 publish당 한 번 계산해도 되지만 private snapshot은 수신자별로 계산하고 별도 Value를 구성한다.

`room.entered`와 `room.resumed` full sync에는 해당 membership의 retained terminal 결과만 ordered `command_results` 배열로 제공한다. 최초 입장의 배열은 비어 있다. periodic snapshot은 배열 없이 last_processed_command_seq만 제공한다. 각 항목은 `game.command.result`와 같은 내용이며 원래 request_id를 유지한다. 클라이언트는 request_id가 아닌 `(room_instance_id, command_seq)`로 처리한 결과를 중복 제거한다. 배열은 최대 request_cache_capacity개이고 전체 무제한 이력이 아니다. `last_processed_command_seq`는 terminal 게임 오류도 포함한다.

새 연결/방 전환 전에 기존 latest snapshot을 비우고 기존 reliable full sync 우선 순서를 유지한다. old generation output에 새로운 private state/result를 전송하지 않는다. 동일 연결에서 일반 request cache가 과거 full sync를 재생하는 경우도 recipient는 동일 인증 session으로 제한되며, command cache는 이 경로를 사용하지 않는다.

## 6. 검증 게이트

1. 기존 workspace test 및 Godot guest/movement/resume 호환 테스트를 모두 유지한다.
2. account: valid/invalid/disabled/timeout/invalid grant, body/claims bounds, rate/concurrency exhaustion과 permit 반환, 다른 game token 거부, issuer collision, 동일 identity rotation, old token/socket 거부, room/player 유지, TTL 단축과 grace 비연장, concurrent rotation, 취소 시 candidate grace 미적용, commit 직후 기존 TTL 만료에도 seat/ledger 보존, actor Drop 후 늦은 commit 거부, logout/expiry cleanup.
3. commands: 여러 명령 FIFO 적용, latest input coalescing 동시 유지, validation/apply error terminal, 동일 seq 재시도 및 payload conflict, ledger eviction 비재실행, queue-full 비소비, generation 교체 전후 경합, reply loss 후 resume, TTL/leave/room instance fence, malformed envelope, 기본 및 `serde_json/preserve_order` 환경의 중첩 object key 정렬·배열 순서 구별.
4. privacy: A/B 서로 다른 secrets 및 결과를 주고 create/join full sync, periodic snapshot, resume, token rotation, 다른 room 전환에서 peer 데이터가 없음을 확인한다. Guest와 기존 Game의 None 기본값도 확인한다.
5. 시간 경합은 Tokio paused clock과 명시적 channel barrier로 재현한다. arbitrary sleep으로 timeout/rotation correctness를 주장하지 않는다.
6. 최종 `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace` 통과 후 Architect review를 받는다. commit/push 증거는 테스트와 별도로 보고한다.
