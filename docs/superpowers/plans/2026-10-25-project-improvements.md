# Campfire Project Improvements Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** room 간 간섭과 제어 요청의 비용을 줄이고, 종료·관측·검증·재사용 계약을 현재 단일 프로세스 구조에서 강화한다.

**Architecture:** 전역 State에는 짧은 map 작업만 남기고 session별 lifecycle 소유권으로 actor 왕복을 직렬화한다. 기존 room instance/generation/Rotation을 보존하며 별도 Stop, bounded recent metrics, 독립 QA 환경을 추가한다. protocol v1과 기존 Config/CLI literal은 유지한다.

**Tech Stack:** Rust 1.96 / edition 2024, Tokio, Axum, vanilla JS/SVG, Node test runner, Python stdlib, Godot, gstack browse.

**Spec:** `docs/superpowers/specs/2026-10-25-runtime-hardening-design.md`

## Global Constraints

- 새 Cargo 실행은 `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target`를 사용한다.
- 작업 branch는 `codex/project-improvements`이며 main을 직접 수정하지 않는다. 최종 commit/push는 root 담당자가 승인 범위 안에서 수행한다.
- 기존 server PID 69169, manager PID 68561, browser PID 69199를 종료·재시작·재사용하지 않는다. 환경 전체나 credentials를 출력하지 않는다.
- 기존 `Config` 및 server `cli::Options`의 public struct literal을 깨지 않는다. 새 선택 옵션은 별도 타입/constructor로 제공한다.
- Rust MSRV는 workspace의 `1.96`, edition은 `2024`를 모든 crate에서 상속한다. 라이선스는 기존 MIT 선언을 실제 LICENSE로 뒷받침한다.
- 새 framework/DB/metric backend는 도입하지 않는다. 기존 Tokio, std, SVG, Python stdlib, Node test runner를 사용한다.
- 실제 registry publish와 운영 프로세스 교체는 하지 않는다. 구현 승인은 이미 받았으므로 별도 계획 승인 대기 없이 집행한다.

## Review Focus

- Enter enqueue 직후 caller 취소: actor가 성공하면 session의 좌석 또는 reservation이 복구되고 orphan 좌석이 없어야 한다(Task 1).
- 같은 계정의 두 rotation과 cleanup/room closure: token-map swap과 Rotation decision이 어긋나지 않으며 약한 gate registry가 계속 커지지 않아야 한다(Task 1).
- control queue가 계속 차는 동안 과거 generation의 Execute와 Stop 도착: 이전 command가 교체 이후 실행되지 않고 data/Stop도 진행해야 한다(Task 3).
- 104-room fixture와 정상 resume/cache replay: control budget의 보안 의도를 약화하지 않고 rate-aware fixture로 통과해야 한다(Task 2, 4).
- boot 교체·null·실제 0·표본 gap·counter 감소: UI가 잘못된 오류 증가분이나 이어진 차트를 만들지 않아야 한다(Task 5).

## 소유권과 순서

| 담당 | 소유 파일 | 단계 |
|---|---|---|
| Runtime worker | `crates/campfire-runtime/src/{session,room,metrics,observation,lib}.rs`, 필요 시 `lifecycle.rs`, runtime inline tests, `crates/campfire-server/src/main.rs`의 constructor/shutdown 통합 | Task 1 → Task 3. Task 2 공개 options 통합도 이 담당자가 수행 |
| Transport worker | `crates/campfire-runtime/src/{transport,connection_limits}.rs`, 새 transport tests; server 별도 limits helper 파일 | Task 1이 interface를 준비한 뒤 Task 2 |
| QA/tools worker | `tools/validate_*.py`, `tools/tests/test_validation_tools.py`, 필요 시 `tools/validation_paths.py`, 검증 문서 | Task 4; runtime worker와 파일 충돌 없이 가능 |
| UI worker | `crates/campfire-server/src/dashboard/{model.rs,tests.rs,assets/*}`, `tools/tests/dashboard_ui_control.mjs`, 필요 시 새 chart test | Task 3 지표 contract 이후 Task 5 |
| Packaging/CI worker | Cargo manifests, LICENSE, README/reuse docs, `.github/workflows/*`, package smoke 도구 | Task 6 → Task 7; tools 담당 종료 후 도구 파일 조정 |
| 독립 QA/Architect | 읽기 검토, 새 output 아래 검증 artifact, 최종 review 문서 | Task 8 |

동시에 같은 파일을 수정하지 않는다. 모든 worker는 다른 작업자의 변경을 되돌리지 않는다. 소유권 변경은 root가 명시적으로 전달한다. 각 task 완료 시 변경/검증/제약을 root에 보고하고 commit은 root가 묶어서 수행한다.

### Task 1: 전역 잠금 분리와 lifecycle 소유권

**Files:** Modify runtime `session.rs`, `room.rs`, `observation.rs`; Create 필요 시 `lifecycle.rs`; Test 동일 파일 inline modules.

**Interfaces:**
- `Session`에 `lifecycle: Arc<tokio::sync::Mutex<()>>` 추가. token rotation 후에도 Arc를 보존한다.
- `ObservationDirectory::room_summary(&self, reference: &RoomRef) -> Option<RoomObservation>` 추가. 짧은 watch borrow만 사용하고 closed/다른 instance는 None.
- 기존 `Inner::{authenticate,disconnect,command}`, issue/account entrypoint의 public 동작은 유지한다. owned task에 필요한 receiver만 `self: &Arc<Self>`로 바꿀 수 있다.
- 별도 `RuntimeOptions`/ConnectionLimits constructor 통합은 Task 2의 명시적 타입을 기다린다. 이 작업에서 Config public 필드는 늘리지 않는다.

- [ ] 느린 actor를 channel barrier로 멈추는 test fixture를 작성한다. `hardening_slow_room_does_not_block_other_session`은 A의 Enter/Replace 대기 중 B의 issue/auth/List가 완료됨을 검증한다. `hardening_list_uses_cached_summary`는 control receiver를 처리하지 않아도 public visible 목록과 cursor가 반환됨을 검증한다.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo test -p campfire-runtime hardening_ -- --nocapture`를 실행해 새 회귀가 현재 구현에서 실패/timeout하는지 확인한다. test timeout은 test 자체만 종료하며 실제 서비스를 사용하지 않는다.
- [ ] stable gate clone → unlock → gate 획득 → State 재검증 helper를 구현한다. lock 순서를 identity → session → State로 고정한다. 목록은 캐시로 변경한다.
- [ ] `hardening_cancelled_enter_reconciles_seat`, `hardening_late_leave_cannot_remove_replacement`, `hardening_create_cancel_releases_slot`, `hardening_parallel_rotation_and_expiry`, `hardening_identity_gate_registry_is_bounded`, `hardening_removed_sessions_do_not_release_operation_capacity_early` 테스트를 추가한다. 각 테스트는 State의 membership/generation, 실제 actor member 수, active_rooms와 max_rooms reservation 수를 함께 확인한다.
- [ ] session별 하나의 owned operation이 actor send/ack와 commit/보상을 소유하게 한다. caller 취소는 enqueue 전에는 취소, enqueue 후에는 안전한 결과 회수로 처리한다. 새 coordinator는 gate 획득 후에만 만들며 별도 max_sessions 크기의 operation semaphore permit을 전체 task 수명 동안 보유한다. session이 삭제되어도 permit은 완료 전 반환하지 않고 가용 permit이 없으면 SERVER_BUSY로 거부한다. account identity registry는 Weak 항목을 prune하고 발급 semaphore가 잡힌 작업만 진입시킨다.
- [ ] Create factory는 State 밖으로 옮기되 pending slot을 max_rooms에 포함한다. invisible entry와 accounting의 생성/rollback을 동일 instance로 확인한다. cleanup은 busy session gate를 건너뛰고 나머지 session을 계속 처리한다. 삭제 후 actor 정리는 generation=None이 아닌 구체적 generation으로 보낸다.
- [ ] `Rotation::commit()`과 map swap 사이에 await가 없는지 검토한다. 취소된 rotation prepare가 generation을 전진시킨 경우 다음 auth가 이를 넘어서는지 테스트한다. 기존 `queued_old_generation_commands_are_fenced_by_control_barrier` 및 account rotation 테스트를 유지한다.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo test -p campfire-runtime`를 실행한다. State guard를 잡은 상태의 `.await`와 user Game callback이 없음을 source review로 확인하고 변경 파일/실행 test 결과를 root에 보고한다.

### Task 2: control·invalid frame 예산과 호환 options

**Files:** Create runtime `connection_limits.rs`; Modify `transport.rs`; runtime 담당이 `lib.rs`, `session.rs`, server `main.rs`에 통합. Create server `connection_limits.rs`와 export에 필요한 `lib.rs` 변경은 server 소유권을 root와 조정한다. Update `docs/configuration.md`, `docs/protocol.md`.

**Interfaces:**
- `ConnectionLimits { control_rate_per_second: u32, control_burst: u32, invalid_rate_per_second: u32, invalid_burst: u32 }`, Default=10/20/2/4, `validate(&self) -> Result<(), ConfigError>`.
- `RuntimeOptions { connection_limits: ConnectionLimits }`는 Default를 제공한다.
- `Campfire::new_with_options(config: Config, options: RuntimeOptions) -> Result<Self, ConfigError>`.
- `Campfire::with_authenticator_and_options(config: Config, authenticator: Arc<dyn AccountAuthenticator>, options: RuntimeOptions) -> Result<Self, ConfigError>`.
- server helper `connection_limits_from_env(get: impl Fn(&str) -> Option<String>) -> Result<ConnectionLimits, String>`는 spec의 네 환경 변수만 읽는다. 기존 Options 필드를 늘리지 않는다.

- [ ] `hardening_control_flood_is_limited`, `hardening_malformed_frames_close_within_violation_limit`, `hardening_input_and_control_budgets_are_independent`, `hardening_replay_consumes_control_budget`를 추가한다. paused clock에서 burst 초과, refill, 0/overflow 옵션 거부, connection별 독립성을 확인한다.
- [ ] 새 test filter로 실패를 확인한 뒤 reader에 control/invalid bucket을 추가한다. invalid application frame은 parse 실패도 violation을 증가시킨다. 최초 인증 오류 즉시 종료와 기존 input bucket을 유지한다.
- [ ] options constructor와 env helper를 통합하고 기존 Config/Options literal compile test를 유지한다. protocol error shape/request_id/redaction을 바꾸지 않는다.
- [ ] 정상 Authenticate → Create/Join → Leave/Resume 흐름과 cache replay를 테스트한다. 104-room fixture의 요청 시각·burst를 확인하여 필요하면 fixture의 control 요청을 pace한다. 기본 limits를 fixture 편의를 위해 무제한으로 높이지 않는다.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo test --workspace transport`와 runtime 전체 test를 실행하고 실제 적용되는 defaults/env precedence를 문서화한다.

### Task 3: Stop 우선 처리, 공정성, 종료 보고와 최근 지표

**Files:** Runtime worker만 `room.rs`, `session.rs`, `metrics.rs`, `observation.rs`, `lib.rs`, server `main.rs` 통합을 수정한다.

**Interfaces:**
- `Campfire::shutdown_with_timeout(&self, timeout: Duration) -> ShutdownReport`.
- `ShutdownReport { completed: bool, remaining_rooms: u64, remaining_connections: u64, remaining_operations: u64 }` public export.
- 기존 `shutdown(&self)`는 5초 wrapper, 기존 return type `()` 유지.
- `LatencySnapshot { sample_count: u64, p95_micros: Option<u64>, p99_micros: Option<u64> }`.
- `RecentMetricsSnapshot { window_ms: u64, tick_delay: LatencySnapshot, tick_duration: LatencySnapshot, input_latency: LatencySnapshot, input_duration: LatencySnapshot, command_duration: LatencySnapshot }`.
- `MetricsSnapshot.recent: RecentMetricsSnapshot`, 추가 u64 필드 `commands_processed`, `commands_failed`, `command_replays`, `max_command_duration_micros`.

- [ ] `hardening_stop_bypasses_full_control_queue`, `hardening_saturated_tick_and_controls_allow_data`, `hardening_shutdown_reports_pending_without_claiming_completion`을 작성하고 실패를 확인한다. barrier로 지연된 callback 테스트는 별도 worker thread/release 신호를 사용해 test runner 자체를 영구 정지시키지 않는다.
- [ ] 독립 watch Stop을 RoomHandle/Channels에 연결한다. 이미 queued된 control batch 최대 64개 → 준비된 data 1개 → due tick 1개의 cycle을 구현하고 Stop을 batch 사이에 확인한다. loop 끝에 yield한다. 기존 generation barrier test를 반드시 실행한다.
- [ ] 모든 room에 먼저 stop 신호를 보내고 하나의 전체 deadline으로 supervisor/transport/owned operations를 기다린다. deadline 도달 시 정확한 report를 반환한다. synchronous callback에 대한 abort 보장을 문서/로그에 넣지 않는다.
- [ ] metrics 테스트 `recent_window_expires_and_caps_512`, `recent_nearest_rank_percentiles_include_zero`, `command_replay_does_not_increment_processed`, `failed_command_is_counted_once`를 작성한다. now/clock을 주입해 sleep 없이 60초 expiry, 512개 capacity, p95/p99, empty null을 확인한다.
- [ ] bounded ring 및 explicit recent snapshot을 구현한다. sort는 metric lock 밖에서 한다. 기존 lifetime max와 입력 sequence 의미를 유지한다. command duration은 새 contiguous command의 validate/apply만 포함한다.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo test -p campfire-runtime`를 실행한다. observer는 session gate/actor에 접근하지 않고 개인정보를 추가하지 않는지 확인한다. 지표 이름/타입을 UI worker에 전달한다.

### Task 4: QA browser·binary 격리와 portable probe

**Files:** Modify `tools/validate_dashboard.py`, `tools/validate_godot.py`, `tools/validate_godot_accounts.py`; Create `tools/validation_paths.py`, `tools/tests/test_validation_tools.py`; Update `docs/verification-coverage.md`.

**Interfaces:**
- 세 runner 공통 `--target-dir`, `--godot`; dashboard는 `--browse`. target 기본은 CARGO_TARGET_DIR 또는 repo target, 실행 파일 기본은 해당 env 또는 PATH.
- `resolve_target_dir(explicit: str | None) -> pathlib.Path`, `resolve_executable(explicit: str | None, env_name: str, executable: str) -> pathlib.Path`를 공통 모듈에서 제공한다. env 내용 전체는 로그하지 않는다.
- runner마다 unique `BROWSE_STATE_FILE`과 `CHROMIUM_PROFILE`을 같은 owned temp root에 만든다. nested account/guest runner도 resolved binary 경로를 받는다.

- [ ] Python subprocess mock test를 작성한다. 두 Runner의 profile/state가 다름, inherited CHROMIUM_PROFILE을 덮어씀, 모든 browse/stop 호출 env가 일관됨, target-dir에 따른 actual binary와 hash source가 일치함을 검증한다.
- [ ] `python3 -m unittest discover -s tools/tests -p 'test_*.py'`로 실패를 확인한다.
- [ ] hardcoded homebrew/개인 gstack/target-debug 경로를 제거한다. missing tool은 해당 suite만 명확히 실패하고 실행하지 않은 검증을 success로 기록하지 않는다. browse stop/cleanup은 owned state/profile만 사용한다.
- [ ] fixture control 요청을 필요 시 rate-aware로 만든다. 운영 프로세스를 쓰는 fallback을 금지하고 source/binary/tool hash 및 versions 기록을 보존한다. 계정 token/관리자 key는 기존 scan으로 유출 여부를 확인한다.
- [ ] Python tests와 `python3 tools/validate_dashboard.py --help`를 실행한다. browser 실검증 전 독립 QA가 state/profile 분리와 owned process cleanup 소스를 확인한다.

### Task 5: 표본 범위·접근성·오류 증가분 dashboard

**Files:** Modify dashboard `assets/{app.js,index.html,style.css}`, `model.rs`, `tests.rs`; Test `tools/tests/dashboard_ui_control.mjs` 및 필요 시 새 `dashboard_charts.mjs`.

**Interfaces:** Task 3의 `RecentMetricsSnapshot`을 서버 model에서 explicit projection한다. 기존 API 필드는 유지한다. chart range는 60000/300000/900000ms, 기본 900000ms. history 보존은 900표본/900000ms.

- [ ] Node fixture로 `range_filter_keeps_real_samples`, `keyboard_and_hover_read_same_sample`, `counter_reset_has_no_negative_delta`, `null_and_zero_remain_distinct`, `boot_change_clears_selection`을 작성한다. 실제 DOM/control regression harness를 재사용하고 새 runtime 구조를 fixture에 추가한다.
- [ ] `node --test tools/tests/*.mjs`로 실패를 확인한다.
- [ ] 범위 버튼과 공통 readout을 추가하고 Left/Right/Home/End, pointer closest sample을 구현한다. 시간 gap 2500ms 초과는 잇지 않는다. 선택 sample_seq의 eviction, logout, boot reset을 처리한다.
- [ ] 선택 범위 누적 counter delta와 실제 관찰 시간을 표시한다. 감소/두 표본 미만은 '—', null은 '측정값 없음', 0은 0이다. `commands_failed`를 포함하고 이벤트 합계를 고유 사고 수라고 표기하지 않는다.
- [ ] 새 recent p95/p99/표본 수와 command count를 런타임 표에 추가한다. 기존 max 라벨은 생애 최대값임을 표시한다. model whitelist/privacy tests를 갱신한다.
- [ ] Node tests와 `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo test -p campfire-server dashboard`를 실행한다. 실제 browser에서 desktop/mobile, keyboard focus, hover, range, 기존 polling 취소/logout 회귀는 Task 8에서 확인한다.

### Task 6: 패키지 메타데이터와 독립 소비 검증

**Files:** Modify root/세 crate `Cargo.toml`, README, `docs/reuse.md`; Create `LICENSE`, `tools/package_smoke.py` 및 test fixture가 필요하면 `tools/tests/test_package_smoke.py`.

**Interfaces:** 모든 crate `rust-version.workspace=true`, workspace repository `https://github.com/sangjinsu/campfire`, 정확한 crate description, 내부 path dependency version `0.1.0`. package smoke는 temp workspace만 쓰고 publish는 실행하지 않는다.

- [ ] 현재 `cargo metadata --no-deps`와 `cargo package -p campfire-runtime --list`에서 license/repository/description/MSRV/포함 파일의 결함을 기록한다. 불필요한 snapshot test는 만들지 않는다.
- [ ] MIT LICENSE와 metadata를 추가한다. package include/exclude는 `.git`, 검증 secret, 거대한 artifacts가 package에 들어가지 않는 실제 list를 기준으로 결정한다.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo package -p campfire-runtime --allow-dirty`로 runtime package build 검증을 실행한다. sandbox/network 실패를 source 실패와 구분한다.
- [ ] arena/server의 미게시 내부 dependency를 실제 registry 접근 없이 제공하는 local source replacement 또는 local registry를 package smoke에 구성한다. tarball을 풀어 정상화된 manifest를 사용하고 검증에 쓴 replacement를 보고한다. package archive 생성만 `--no-verify`로 했다면 후속 컴파일 성공 여부를 따로 표시한다.
- [ ] 별도 temp consumer에서 runtime+arena 및 자체 GameModule을 사용하여 register/router/observer/새 options/시간 제한 shutdown을 compile/run한다. checkout path consumer와 tarball consumer의 증거를 별도로 남긴다.
- [ ] 결과에 archive sha256, file list, MSRV check, 성공한 consumer 유형, 미검증 registry 부분을 기록하고 root에 보고한다.

### Task 7: 빠른 CI와 예약·수동 실환경 검증

**Files:** Create `.github/workflows/ci.yml`, `.github/workflows/runtime-validation.yml`; Update `docs/verification-coverage.md`, README. 필요 시 Task 4 담당과 합의해 tool 설치 script를 별도 추가한다.

**Interfaces:** Task 4의 path CLI, Task 6 package smoke. artifact에 source hash, binary hash, tool versions/hash, suite summaries, logs/screenshots를 포함한다. 운영 secret을 요구하지 않고 test용 credentials만 실행 중 생성한다.

- [ ] 공식 upstream에서 Rust/Godot/gstack 설치 방식과 action ref를 확인한다. 지원 버전과 pin ref를 workflow에 명시한다. 새 checkout에서 개인 경로가 없어도 동작해야 한다.
- [ ] PR/push workflow에 fmt check, clippy workspace all-targets -D warnings, workspace all-targets tests, server no-default-features check, Node/Python tests를 넣는다. package smoke는 가용 offline dependencies와 함께 실행한다.
- [ ] schedule/workflow_dispatch job에 tool 설치, 새 target build, 8·16 Godot 및 account/browser suite를 넣는다. 각 suite는 별도 artifact dir를 사용하고 실패 artifact를 always 업로드한다.
- [ ] workflow syntax 및 실제 명령을 로컬에서 확인한다. PR workflow가 credential 없이 작동하고 scheduled dependency download 실패가 성공으로 취급되지 않는지 확인한다.
- [ ] root가 push 후 CI run을 확인한다. 아직 실행되지 않은 scheduled job은 '설정됨'으로만 보고하며 실제 통과로 주장하지 않는다. 수동 실행이 허용된 경우 workflow_dispatch 결과까지 추적한다.

### Task 8: 최종 통합·독립 Architect 검증·인계

**Files:** Update `docs/verification-results.md`와 새 검증 기록 문서; artifacts는 새 디렉터리 아래 생성. source mutation이 끝난 뒤 증거를 수집한다.

- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo fmt --all -- --check`.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo clippy --workspace --all-targets -- -D warnings`.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo test --workspace --all-targets` 및 `cargo check -p campfire-server --no-default-features`를 같은 target env로 실행한다.
- [ ] `node --test tools/tests/*.mjs` 및 `python3 -m unittest discover -s tools/tests -p 'test_*.py'`.
- [ ] `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target cargo build --workspace --examples` 후 resolved browse/Godot executable을 확인한다. 현재 운영 binary를 사용하지 않는다.
- [ ] `python3 tools/validate_dashboard.py --suite all --target-dir /tmp/campfire-improvements-20261025-target --browse "$BROWSE_BIN" --godot "$GODOT" --duration 20 --output artifacts/improvements-20261025-final`을 실행한다. 변수는 해당 tool 경로만 설정하고 환경 전체를 출력하지 않는다. 이미 존재한 output은 새 suffix로 변경한다. 8·16 load, account, browser 개별 summary가 모두 success여야 한다.
- [ ] 실제 browser에서 1/5/15분 범위, hover/keyboard readout, null/0, 오류 delta, 104-room paging/privacy, mobile overflow, logout 및 polling 회귀 증거를 확인한다. 제어 fixture와 실제 load 데이터의 증거를 구분한다.
- [ ] source/binary hashes가 실행 전후 동일하고 owned process cleanup 및 secret scan이 통과하는지 확인한다. 기존 PID에 대한 종료/재사용이 없음을 확인한다.
- [ ] 독립 Architect는 State-held await/콜백, lifecycle cancellation, generation/instance/rotation, stop fairness, bounded task/metric memory, Config literal 호환, package 검증 범위를 재검토한다. 검증 실패는 수정 후 관련 test만 재실행하고 통합 변경이 있으면 최종 suite를 갱신한다.
- [ ] root가 승인된 commit/push를 수행하고 Git 상태·remote commit·CI 결과를 확인한다. 최종 보고는 구현/로컬 검증/실제 Godot-browser/패키지/CI를 구분하고 미완료 항목을 명시한다.

## 실행 메모

큰 재작성 대신 먼저 Task 1의 gate/취소 회귀를 통과시키고 그 뒤 transport와 종료/지표를 붙인다. normal lifecycle actor wait에는 자동 retry나 timeout을 억지로 추가하지 않는다. 이 계획의 bounded shutdown은 scheduler가 작동하는 범위의 대기 제한이다. sync game callback의 강제 종료와 WAN 성능은 검증 대상이 아니다.
