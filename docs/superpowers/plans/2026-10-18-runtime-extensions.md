# Campfire Runtime Extensions Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** 기존 계정으로 room session을 발급하고, 재접속에도 중복 실행하지 않는 단발 명령과 수신자별 비공개 상태를 지원한다.

**Architecture:** async host verifier가 game-scoped account identity를 검증하고 현재 session actor 경계 안에서 token을 교체한다. room actor의 command FIFO와 per-member ledger가 단발 작업을 처리하며 snapshot projection이 수신자별 private state를 분리한다. 기존 guest/input protocol은 유지한다.

**Tech Stack:** Rust 1.96 / edition 2024, Tokio 1, Axum 0.8, serde_json, 기존 workspace 의존성.

**Spec:** `docs/superpowers/specs/2026-10-18-runtime-extensions.md`

## Global Constraints

- protocol v1과 기존 guest/Godot 이동·재접속 계약을 유지한다.
- Config 공개 필드는 추가하지 않는다. 기존 `Config { ... }` literal의 source compatibility를 유지한다.
- Config의 기존 limits를 명세 표대로 재사용하며 외부 IdP SDK, 영구 저장, UI는 추가하지 않는다.
- identity key는 `(game_id, issuer, subject)`다. credential과 claims를 공용 상태/로그에 자동 노출하지 않는다.
- command_seq는 membership별 1부터 연속 증가한다. terminal 게임 오류도 소비하며 퇴출된 결과의 명령은 재실행하지 않는다.
- 단일 프로세스/in-memory 보장만 제공한다. durable exactly-once를 주장하지 않는다.
- production 핵심 통합은 한 worker가 소유한다. 다른 worker의 변경을 되돌리지 않는다.

## Review Focus

- 검증기가 지연되는 동안 shutdown/TTL이 바뀌면 grant가 성공해도 session을 만들지 않아야 한다. Task 1의 delayed grant 테스트가 고정한다.
- 같은 subject라도 issuer/game이 다르면 별도 session이며 다른 사용자의 seat를 교체하지 않아야 한다. Task 1의 namespace collision 테스트가 고정한다.
- 결과 출력이 실패한 직후 token rotation이 일어나도 결과를 재실행하지 않아야 한다. Task 3의 lost-reply/resume 테스트가 고정한다.
- 제한된 ledger의 가장 오래된 결과를 잃어도 높은 sequence만 기억해 stale 명령을 실행하지 않아야 한다. Task 3의 eviction 테스트가 고정한다.
- 과거 방의 latest snapshot이나 peer의 private state가 full sync/재접속에 섞이지 않아야 한다. Task 2/4의 모든 projection 경로 테스트가 고정한다.

---

## 파일 소유와 병렬 경계

| 담당 | 소유 파일 | 책임 |
|---|---|---|
| runtime 통합 worker | `crates/campfire-runtime/src/auth.rs` (신규), `game.rs`, `config.rs`, `lib.rs`, `protocol.rs`, `session.rs`, `room.rs`, `transport.rs`, 필요 시 `metrics.rs`, `output.rs` | 모든 production contract와 내부 unit tests; 공유 경계 충돌 방지 |
| extension_integration | `crates/campfire-server/tests/extensions.rs`, `crates/campfire-server/examples/account_server.rs` (신규) | 공개 API와 HTTP/WebSocket black-box 검증 및 injected verifier 예제; production 파일 편집 금지 |
| 문서 writer | `README.md`, `docs/protocol.md`, `docs/configuration.md`, `docs/reuse.md` | 사용자 사용법과 실제 wire/API contract 동기화 |
| 설계 담당 | 본 plan/spec | source 검토와 명세 동기화 |
| Architect reviewer | read-only | fresh diff와 검증 증거 확인 |

생산 구현 순서는 Task 2(private state)와 Task 3(commands)의 기본 경로를 먼저 완성하고 Task 1(auth/rotation)을 연결하는 것이다. Task 번호는 기능별 참조 번호다. 모두 같은 worker가 소유하며 auth 연동 경합 테스트는 Task 1 완료 뒤 실행한다. 외부 테스트 담당은 이 문서의 public signatures가 동결되면 fake Game/verifier와 harness를 작성할 수 있다. docs/example은 통합 worker가 API를 공개한 뒤 검증한다. 코드가 커져도 무관한 대규모 파일 이동은 하지 않는다. `codex/runtime-extensions`에서 검증·commit한 후 main을 fast-forward하고 `origin main`에 normal push한다. Git 작업은 coordinator가 지정한 담당만 수행하고 force push하지 않는다. 실행 증거는 [공개 검증 보고서](../../runtime-extensions-2026-10-18.md)와 링크한 원본 artifact에 기록한다.

Stage 1 command/private core는 Architect 승인과 runtime 22 / headless 18 pass를 거쳐 `93d51a7`로 commit했다. 이후 account·rotation 경합 수정과 canonical fingerprint 변경을 포함한 runtime은 기본/`serde_json/preserve_order` 각각 39 pass를 확인했다. 두 rotation 경합은 실제 actor·paused-clock에서 RED→GREEN으로 재현했고 actor Drop 후 늦은 commit 거부도 검증했다. 최종 root 검사는 workspace 79 / no-default 61 / preserve-order 39 pass, ignored 장기 4 pass, 실제 Godot 8명·16명 및 native UI 28 checks pass다. 최종 Architect는 Spec·Quality·Security 모두 APPROVED, Critical/Important 0으로 판정했다. [원본과 출처](../../runtime-extensions-2026-10-18.md)를 참조한다. 아래 구현·최종 검증 항목은 실제 근거로 완료 처리하고 Git 전달은 실제 push 이후 기록한다. 초기 RED 실행은 최종 GREEN 결과에서 소급 추정하지 않는다.

### Task 1: 계정 인증과 세션 rotation

**Files:**
- Create: `crates/campfire-runtime/src/auth.rs`
- Modify: `crates/campfire-runtime/src/lib.rs`, `game.rs`, `session.rs`, `room.rs`, `transport.rs`
- Test: 위 모듈의 내부 unit tests, `crates/campfire-server/tests/extensions.rs`

**Interfaces:**
- Consumes: 기존 Config, Session, `RoomRef`, `Control::Replace`, `Admission`.
- Produces: 명세의 `AccountIdentity`, `PlayerIdentity`, `AccountGrant`, `AuthError`, `AccountAuthenticator`, `Campfire::with_authenticator(config, Arc<dyn AccountAuthenticator>)`.
- Produces: `Game::player_joined_with_identity(&mut self, &str, &PlayerIdentity)`와 `player_identity_changed(&mut self, &str, &PlayerIdentity)` default hooks.
- Internal: Session/Admission에 `PlayerIdentity`를 보관·전달한다. `Control::Replace`는 generation/output을 fence하고 candidate grace를 별도 PendingRotation에 보관한다. `RotationDecision` 공유 mutex의 Pending/Committed/Aborted 결정과 actor settle이 deadline/identity 적용을 조정한다. PendingRotation의 Drop은 pending 결정을 abort한다. 두 identity 타입은 `auth.rs`에 두고 custom Debug에서 전체 account identity를 가린다. 계정 route는 `/v1/games/{game_id}/sessions`.

- [x] **Step 1: account 실패 및 기존 호환 테스트 작성.** `account_valid_grant_issues_game_scoped_session`, `account_disabled_rejects_without_changing_guests`, `account_rejects_invalid_and_oversized_grants`를 작성한다. valid_for=0, issuer empty/129 bytes, subject empty/257 bytes, claims 16 KiB 초과는 session을 만들지 않는다. guest-only TestGame 구현은 수정 없이 컴파일되어야 한다.
- 계획상 초기 RED 단계:  **Step 2: 실패 확인.** `cargo test -p campfire-runtime account_`를 실행해 신규 API 부재 또는 동작 assertions 실패를 확인한다. 초기 명령의 별도 원본 로그는 보존되지 않았다. Worker는 account API의 expected compile failure와 command/private의 14개 기존 pass·2개 expected failure를 관측 보고했다. 최종 구현·회귀 검증은 별도 원본 근거로 확인했다.
- [x] **Step 3: auth 공개 타입, 생성자, bounded route를 구현.** 객체 안전한 boxed Future trait를 사용하고 추가 SDK 의존성을 도입하지 않는다. HTTP schema는 credential만 허용하며 16 KiB body 한도를 적용한다. JSON 문법/schema는 400으로 정규화하고 body 413, 누락/미지원 Content-Type 415는 보존한다. state lock 밖에서 5초 timeout, 미인증 WS와 공유하는 256-permit semaphore, shared issuance rate를 적용한다.
- [x] **Step 4: rotation·clock 경합 테스트 작성.** `account_rotation_preserves_player_room_and_fences_old_token`, `account_rotation_does_not_extend_grace`, `account_namespace_uses_game_issuer_subject`, `account_delayed_grant_rechecks_shutdown_and_expiry`, `account_timeout_releases_capacity`에 exact assertions를 둔다. claims 갱신 hook의 호출·값과 token deadline 단축에 의한 room expiry를 확인한다. 취소 시 prospective grace 미적용, commit 후 기존 TTL 경과 시 seat/ledger 보존, PendingRotation Drop 후 늦은 commit 거부를 실제 actor 테스트로 검증한다. concurrency는 channel barrier, TTL은 paused clock을 쓴다.
- [x] **Step 5: 동일 identity의 atomic token rotation을 구현.** 새 random token 생성 실패는 기존 session을 유지한다. generation 증가와 actor Replace가 old output 및 queued work를 fence한다. 모든 await 동안 old token map entry를 보존한다. 마지막 await 뒤 shutdown/new deadline을 재검사하고 공유 decision mutex에서 old-seat valid_until까지 확인해 commit한 뒤 await 없이 token map을 교체한다. actor는 expire/tick/control/data 전에 settle하며 commit/expiry 경쟁을 같은 decision으로 해결한다. commit 전 취소 및 PendingRotation Drop은 abort하고 old token/claims/deadline과 기존 grace를 유지한다. 이미 닫힌 연결은 old token으로 재인증할 수 있다. 만료된 session cleanup await 이후에도 발급 전 최종 검사를 반복한다. membership 보존과 `min(existing_grace,new_deadline)`도 확인한다. 새 인증은 기존 WS 경로를 재사용한다.
- [x] **Step 6: 해당 테스트와 기존 session 회귀를 검증.** `cargo test -p campfire-runtime` 결과가 모두 pass여야 한다. 별도 테스트 worker는 public API 사용법의 signature 차이를 즉시 통합 worker에게 전달한다.
- [x] **Step 7: coordinator checkpoint.** diff가 명세와 일치하는지 확인하고 통합 전 부분 commit은 coordinator의 결정에 따른다. 외부 push는 최종 Task 5에서 수행한다.

### Task 2: 수신자별 private state projection

**Files:**
- Modify: `crates/campfire-runtime/src/game.rs`, `room.rs`
- Test: `room.rs` 내부 tests, `crates/campfire-server/tests/extensions.rs`

**Interfaces:**
- Consumes: 기존 `Room::sync`/`publish`/`Output`; identity hook 구현 전에도 독립 실행 가능.
- Produces: `Game::private_snapshot(&self, player_id: &str) -> Option<Value>` (default None), 모든 full/periodic 메시지의 optional `private_state`.

- [x] **Step 1: 비공개 projection 실패 테스트 작성.** `private_state_is_recipient_scoped_for_initial_periodic_and_resume`은 A의 secret=A, B의 secret=B를 사용한다. create/join/periodic/resume 메시지에서 자기 secret만 포함되고 public state는 동일한지 확인한다. `legacy_game_omits_private_state`는 필드 자체가 없는지 검사한다.
- 계획상 초기 RED 단계:  **Step 2: 실패 확인.** `cargo test -p campfire-runtime private_state` 실행 결과가 새 assertion에서 실패해야 한다. 초기 명령의 별도 원본 로그는 보존되지 않았다. Worker는 account API의 expected compile failure와 command/private의 14개 기존 pass·2개 expected failure를 관측 보고했다. 최종 구현·회귀 검증은 별도 원본 근거로 확인했다.
- [x] **Step 3: default hook과 공통 projection 구현.** 공용 state와 별도로 private_state를 넣고 publish loop는 `(player_id, member)`를 순회한다. None은 필드 생략이다. full sync 직전 latest clear와 reliable 우선 순서를 유지한다.
- [x] **Step 4: token rotation과 room switch 누출 테스트 추가 및 통과 확인.** `private_state_does_not_leak_across_rotation_or_room_switch`에서 old output에 새 private state가 가지 않는지 확인한다. `cargo test -p campfire-runtime`을 실행한다.
- [x] **Step 5: coordinator checkpoint.** snapshot 메시지의 기존 state 및 sequence 필드가 유지되는지 diff를 확인한다.

### Task 3: FIFO 명령, terminal 결과, 재접속 ledger

**Files:**
- Modify: `crates/campfire-runtime/src/game.rs`, `protocol.rs`, `room.rs`, `session.rs`, `transport.rs`
- Test: 위 모듈 내부 tests, `crates/campfire-server/tests/extensions.rs`

**Interfaces:**
- Consumes: 기존 generation/token-deadline guards, Task 1의 rotation, Task 2의 recipient sync projection.
- Produces: `validate_command(&self, &str, &Value) -> Result<(), GameError>`, `apply_command(&mut self, &str, &Value) -> Result<Value, GameError>` defaults와 `game.command`/`game.command.result` wire contract.
- Internal: room당 별도 command sender/receiver, member당 high-water 및 bounded `(command_seq, payload_fingerprint, terminal_response)` FIFO ledger. RoomHandle에 sender를 추가한다. command 결과는 oneshot으로 session dispatcher에 반환하고 transport가 요청자 reliable lane에 전송한다.
- Produces: 모든 sync의 `last_processed_command_seq`, entered/resumed full sync의 own retained `command_results` array; periodic은 배열 제외.

- [x] **Step 1: protocol/default hook 실패 테스트 작성.** 누락/음수 command_seq, 누락 room_instance_id, unknown field는 parse 오류다. 기존 Game은 compile되고 command 호출 시 terminal `COMMAND_UNSUPPORTED`를 반환해야 한다.
- [x] **Step 2: command wire 및 trait defaults 구현.** command_seq 1부터 연속 증가, frame/rate limits는 기존 설정을 재사용한다. 명세의 error code와 result 구조를 그대로 쓴다. fingerprint는 clone.sort_all_objects() 후 hash하여 preserve_order에서도 중첩 key 순서는 무시하고 배열 순서는 구별한다.
- [x] **Step 3: actor FIFO·terminal 오류 테스트 작성.** `commands_execute_fifo_while_inputs_coalesce`, `command_validation_and_apply_errors_are_terminal`, `command_queue_full_does_not_consume_sequence`를 작성한다. 세 command의 payload가 모두 순서대로 한 번씩 적용되고 두 input은 마지막 값만 tick에서 적용되는지 검사한다.
- [x] **Step 4: 실패 확인 후 actor 실행/ledger 구현.** `cargo test -p campfire-runtime command_`로 실패를 확인한다. command branch는 generation/output/deadline 검사→sequence/replay 판단→validate→apply→terminal ledger 기록→oneshot 응답 순서다. input lane을 변경하지 않고 command queue capacity는 `room_input_capacity`로 잡는다.
- [x] **Step 5: replay·eviction·generation 경합 테스트 작성.** `command_duplicate_replays_with_new_request_id`, `command_payload_conflict_is_rejected`, `command_evicted_result_never_reexecutes`, `command_lost_reply_resumes_without_reexecution`, `command_old_generation_cannot_mutate_after_rotation`을 구현한다. ledger capacity=2로 seq1/2/3 이후 seq1이 `STALE_COMMAND`인지, game apply count가 3 그대로인지 확인한다. replace 전 실행과 replace 후 거부를 각각 barrier로 확인한다.
- [x] **Step 6: transport 일반 cache 우회 및 resume projection 구현.** 일반 request_id 충돌 검사는 유지하고 동일 body cache hit의 응답 단축만 우회한다. cache hit도 actor의 membership/generation/deadline 및 ledger를 재검증한다. 일시적 SERVER_BUSY가 재시도를 막지 않아야 한다. request_id만 바꾼 동일 seq 재시도를 room으로 전달한다. full sync 배열은 own ledger만 포함하고 result를 저장한 뒤 output 실패가 발생해도 ledger를 잃지 않는다.
- [x] **Step 7: sequence/room 경계 회귀 실행.** expected보다 높은 seq는 `COMMAND_SEQUENCE_GAP`, seq0은 `STALE_COMMAND`, wrong instance는 `STALE_INSTANCE`이고 비소비인지 확인한다. leave 후 새 membership seq1, TTL 뒤 old generation 거부를 검증한다. `cargo test -p campfire-runtime` 전체가 pass여야 한다.
- [x] **Step 8: coordinator checkpoint.** 요청자 외 output에 result가 들어가는 경로가 없는지 read-only diff review를 수행한다.

### Task 4: 호스트 예제와 end-to-end 검증

**Files:**
- Create: `crates/campfire-server/tests/extensions.rs`, `crates/campfire-server/examples/account_server.rs`
- Modify: `README.md`, `docs/protocol.md`, `docs/configuration.md`, `docs/reuse.md`

**Interfaces:**
- Consumes: Task 1~3의 공개 API와 wire contract.
- Produces: fake verifier와 secret/counter Game을 사용한 실제 HTTP/WebSocket tests; 호스트가 verifier를 주입하는 컴파일 가능한 예제.

- [x] **Step 1: HTTP/WebSocket 테스트 fixture 구성.** loopback listener에 runtime router를 띄우고 두 계정/두 게임을 등록한다. extensions 테스트는 arena feature가 없는 `--no-default-features`에서도 실행한다. credential 값은 테스트 전용이며 실서비스 비밀을 읽지 않는다.
- [x] **Step 2: 전체 흐름 assertions 작성.** account exchange→WS authenticate→create/join→command duplicate→disconnect→token rotation→resume에서 player_id 유지, terminal count=1, own result만 수신, own private state만 수신하는지 확인한다. 다른 game의 token/room admission을 거부한다.
- [x] **Step 3: guest compatibility 회귀 실행.** 기존 `crates/campfire-server/tests/headless.rs`와 기존 guest protocol이 수정 없이 pass인지 확인한다. Godot client는 새 optional fields를 무시하며 state 구조가 그대로인지 reader source 또는 해당 smoke 테스트로 확인한다.
- [x] **Step 4: 계정 예제 작성과 문서 동기화.** 외부 인증 서버를 대체하는 test/demo verifier라는 점을 명확히 표시하고 production용 hardcoded token을 권장하지 않는다. 모든 limits, command retry/eviction, account namespace, private data 책임, in-memory 한계를 문서에 옮긴다.
- [x] **Step 5: 해당 target 검증.** `cargo test -p campfire-server --no-default-features --test extensions --offline`, `cargo test -p campfire-server --test headless`, `cargo check -p campfire-server --no-default-features --example account_server --offline`가 pass여야 한다.

### Task 5: 전체 검증, Architect review, commit/push

**Files:** 전체 변경 diff (coordinator 소유).

**Interfaces:**
- Consumes: 모든 구현/테스트/문서 결과.
- Produces: fresh validation evidence, Architect 승인, 사용자 요청한 main commit 및 origin push 결과.

- [x] **Step 1: 최신 working tree와 diff를 검토.** 타 worker의 변경을 보존하고 spec에 없는 scope 증가 및 비밀이 포함되지 않았는지 확인한다.
- [x] **Step 2: 전체 검사 실행.** `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`를 실행해 exit code 0과 실패 0을 확인한다. 기존 ignored tests는 성공으로 세지 않는다.
- [x] **Step 3: Architect에게 fresh diff와 명령 결과로 검증 요청.** rotation atomicity, replay boundaries, private projection, backward compatibility를 독립 확인받는다. 발견 사항은 수정 후 해당 regression과 최종 검사를 재실행한다.
- [x] **Step 4: 실제 반영 상태에 맞게 문서 정리.** plan/spec/API 문서가 최종 구현과 일치하고 검증하지 않은 production/IdP/Godot 성능을 완료로 쓰지 않는지 확인한다.
- [ ] **Step 5: coordinator가 commit 및 push.** 파일을 검토 후 stage하고 feature branch에서 `git commit -m "feat: add account sessions commands and private state"`한 뒤 검증된 branch를 main에 fast-forward하고 `git push origin main`을 실행한다. push가 실패하면 원인을 해결하거나 정확한 차단 상태를 보고하며 성공으로 주장하지 않는다.
- [ ] **Step 6: 사용자에게 증거 전달.** 지원 API, 세 기능의 검증 결과, commit SHA 및 push 상태, 단일 노드/in-memory 한계를 간결히 보고한다.

## 자체 검토 결과

명세 각 절은 Task 1(계정/API/rotation), Task 2(private projection), Task 3(commands/ledger), Task 4(호스트 활용/black-box), Task 5(완료 증거)에 매핑한다. public type 및 hook 이름은 명세와 동일하다. Review Focus의 다섯 경계는 해당 Task의 명시적 test 이름으로 고정했다. 이 계획은 사용자 요청에 따라 구현·검증·commit/push까지 진행하는 실행 기준이며 추가 승인 대기를 요구하지 않는다.
