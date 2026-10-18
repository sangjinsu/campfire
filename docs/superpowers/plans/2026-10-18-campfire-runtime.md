# Campfire Runtime Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 사양에 따른 Rust 공통 runtime, 정적 arena 모듈, 실행 서버 및 headless 검증을 구현한다.

**Architecture:** 세션 manager가 게스트 명령을 직렬화하고 각 room task가 멤버십과 게임 상태를 독점한다. Transport는 bounded reliable queue와 최신 snapshot 한 슬롯을 사용하며 정적 GameModule은 네트워크 타입 없이 동기 전이만 수행한다. 사용자가 승인한 병렬 실행에 따라 아래 세 소유권 단위가 동시 작업하고 root가 통합 검증한다.

**Tech Stack:** Rust, Tokio, Axum WebSocket, Serde JSON, OS cryptographic randomness, SHA-256.

**Spec:** `docs/specification.md` (제공된 원문 그대로 보존).

## Global Constraints

- 단일 프로세스, 단일 노드; 신뢰하는 정적 Rust 모듈만 실행한다.
- 기본 정원 16명, 30Hz tick, 15Hz full snapshot, 게스트 TTL 24시간, 재접속 유예 30초, 인증 기한 5초.
- 16KiB JSON frame, 게임 입력 60/s burst 30, room input queue 1,024, reliable queue 64, 최신 snapshot 1 슬롯.
- 외부 HTTP는 `GET /health`, `POST /v1/games/{game_id}/guests`, `GET /v1/games/{game_id}/ws`만 제공한다.
- game_id/token/connection/room 경계, room_instance_id epoch, generation 재검증, 처리된 입력만 ack, full sync 먼저 송신.
- delta synchronization, 분산 directory, Redis/DB, MMO/FPS transport, dashboard, OOM/process-abort 격리는 범위 밖이다.
- 코드 주석은 영어, 사용자 문서는 한국어. 네트워크 API는 공식 문서 검증 뒤 사용한다. commit/push/deploy는 하지 않는다.

## Review Focus

- 같은 게스트의 동시 인증/입장 및 지연 disconnect가 새 membership을 훼손하지 않는지 Task 1/3에서 확인한다.
- 생성/입장 commit 전 취소와 commit 후 송신 실패의 rollback/예약 전환을 Task 1/3에서 확인한다.
- 미처리 구 generation 입력과 잘못된 payload가 ack 또는 게임 상태를 바꾸지 않는지 Task 1/2/3에서 확인한다.
- 느린 수신자와 공유 queue 포화가 정상 플레이어 tick/위반 카운터에 전파되지 않는지 Task 1/3에서 확인한다.
- 만료, panic, 상한 거부 뒤 자리와 counter/rate key가 회수되는지 Task 1/3에서 확인한다.

---

### Task 1: 공통 runtime (runtime worker 소유)

**Files:** `Cargo.toml`, `Cargo.lock`, `crates/campfire-runtime/**`.

**Interfaces:** `Campfire::new(Config)`, `register_game(game_id, impl GameModule)`, `router() -> axum::Router`, `shutdown().await`, `metrics()`; `GameModule::create(&Value,&GameContext)->Result<Box<dyn Game>,GameError>`; `Game`의 player_joined/player_left/player_connection_changed/validate_input/apply_input/tick/snapshot.

- [x] 공식 Tokio/Axum/getrandom API와 cancellation/drop 동작을 확인한다.
- [x] protocol parsing, config bounds, manual-clock expiry, generation/seq/cache, capacity/cancellation/overload/panic 테스트를 먼저 작성하고 실패를 확인한다.
- [x] 인메모리 hash token, issuer/global bounded rate keys, capped connection/session/room accounting, 직렬 session manager를 구현한다.
- [x] `(game_id, room_id)` directory, provisional admission commit/rollback, actor lifecycle와 panic supervision을 구현한다.
- [x] bounded input/reliable queues, 최신 snapshot slot, 단일 writer, input validation/rate/abuse 분리와 tick delay metrics를 구현한다.
- [x] 최신 유효 입력만 tick에 적용하고 generation/epoch 검사와 처리 완료 ack/full sync 순서를 보장한다.
- [x] `cargo test -p campfire-runtime` 통과 및 Task 3와 통합 검증을 확인한다.

### Task 2: Arena, 실행 서버, 문서 (arena/bin/docs worker 소유)

**Files:** `crates/campfire-arena/**`, `crates/campfire-server/Cargo.toml`, `crates/campfire-server/src/**`, `crates/campfire-server/examples/**`, `README.md`, `docs/specification.md`, `docs/protocol.md`, `docs/configuration.md`, 본 계획, `.superpowers/sdd/campfire-runtime/progress.md`.

**Interfaces:** runtime의 위 계약을 소비하고 `ArenaModule`을 제공한다. server crate는 테스트가 `Config`, `Campfire` 및 `ArenaModule`을 조합해 in-process server를 실행할 수 있게 재노출한다. `--game ID`와 `arena` Cargo feature로 정적 등록 게임을 선택한다.

- [x] 원문을 `docs/specification.md`로 복사하고 소유권/공개 계약을 합의한다.
- [x] arena의 fixed-dt 속도/대각선 정규화/경계/공동 목표 중복 획득/단절 정지/불신 payload/settings 거부 테스트를 먼저 작성하고 실패를 확인한다.
- [x] `ArenaModule: GameModule`과 결정적인 arena 상태 전이를 구현한다.
- [x] CLI/env parsing의 상한/잘못된 값 테스트를 먼저 작성하고 실패를 확인한다.
- [x] `campfire-server` 실행, 유효 설정 검사, 선택한 모듈 등록, graceful shutdown과 redacted tracing을 구현한다.
- [x] 실제 HTTP/WS headless 예제를 제공하고 실행한다. README/protocol/configuration 문서의 예제를 바이너리 `--help`와 대조한다.
- [x] `cargo test -p campfire-arena -p campfire-server`, `cargo build --workspace`, feature 제외 빌드를 통과한다.

### Task 3: Headless 통합 및 성능 증거 (research/tests worker 소유)

**Files:** `crates/campfire-server/tests/**`, `docs/api-research.md`, `docs/verification*.md`.

**Interfaces:** Task 1의 in-process router/clock/metrics와 Task 2의 `ArenaModule`을 소비한다. 테스트 client는 wire protocol을 직접 사용한다.

- [x] 외부 API 공식 출처와 실제 signature/feature를 검증하고 두 implementer에게 전달한다.
- [x] 실제 WS 8/16명 기본 흐름, 마지막 자리 경합, 게임 경계, 동시 인증, resume/expiry, 오래된 generation/instance/seq, ack/full sync 순서의 failing tests를 작성한다.
- [x] admission cancellation, slow-reader, abuse, shared queue saturation, session/connection/room/rate-key cap 및 panic cleanup 검증을 작성한다.
- [x] 동일 초기 상태와 입력의 manual-clock replay 결정성을 확인한다.
- [x] 8명과 16명 각 최소 60초 시나리오를 실행하고 snapshot 간격/input 처리/tick 지연/queue 깊이를 기록한다.
- [x] 느린 client 1명과 정상 15명의 snapshot p95를 기본 실행과 비교해 2배 이내인지 확인하고 필요하면 수정 후 재실행한다.
- [x] 실행 명령, source build id, 설정, 환경, 실제 결과와 미검증 한계를 문서에 남긴다.

### Final Integration Gate (root)

- [x] 사양 coverage, 본 계획 및 progress ledger를 검토한다.
- [x] root가 formatting, all-target/all-feature clippy(`-D warnings`), 빠른 workspace 테스트 44개 및 빌드/feature 선택을 독립 실행한다.
- [x] root가 최종 동결 소스로 실제 headless 예제를 새로 독립 실행한다.
- [x] 새 Architect가 소유권/경계/취소/검증 증거를 검토하고 지적을 수정한다.
- [x] 문서에서 완료 여부는 실행된 증거에 한해 보고하고 CCU/인터넷/운영 검증 한계를 명시한다.

최종 Architect: APPROVED, blocking finding 없음. 근거: `docs/architect-review.md`.
