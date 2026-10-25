# Campfire 런타임·운영 개선 설계

작성일: 2026-10-25. 상태: 사용자 구현 승인 범위를 현재 소스와 대조한 실행 설계. 이 문서는 구현·성능 검증 결과를 의미하지 않는다.

## 목표와 범위

느린 room 하나가 다른 session의 인증·목록·정리를 막지 않게 한다. control 및 malformed WebSocket 요청의 비용을 제한하고, actor 종료와 운영 지표의 의미를 명확히 한다. 검증 도구를 실행 중인 서비스와 격리하며, CI·배포 패키지 준비·dashboard 표본 탐색을 개선한다.

DB, 분산 room, 중앙 지표 수집, 실제 crates.io publish, 기존 운영 프로세스 재시작은 범위 밖이다. 현재 단일 프로세스·메모리 저장 모델과 protocol v1을 유지한다.

## 확인된 출발점

- `session.rs`의 `State` mutex 아래에서 authenticate, disconnect, Create/Join/Resume/Leave/Logout, account rotation, cleanup이 actor 채널을 기다린다. Input/Execute는 이미 unlock 후 응답을 기다린다.
- `room.rs`의 `RoomRef`는 game/id/instance를 식별하며 generation은 연결 소유권을 구분한다. account `Rotation`은 actor 준비와 token-map 변경 사이의 동기 commit/abort 결정이다. 이 보호 장치를 삭제하지 않는다.
- Enter는 actor에서 member를 만들고 reliable full sync를 보내는 지점이 commit이다. 이후 연결 전송 실패는 이미 확보한 좌석을 reservation으로 보존한다.
- `PublishedRoom`과 `ObservationDirectory`가 이미 actor와 분리된 최신 요약을 제공한다. public room 목록은 별도의 actor List 왕복 없이 만들 수 있다.
- reader의 token bucket은 Input/Execute에만 적용된다. malformed JSON에는 violation 누적이 없다.
- actor select는 tick을 최우선으로 선택한다. Stop은 일반 control 큐에 들어가며 `shutdown()`은 actor와 transport의 완료를 제한 없이 기다린다.
- dashboard 이력은 15분/900표본이며 기존 `max_*`는 프로세스 생애 누적 최대값이다. dashboard model은 지표를 명시적으로 projection한다.
- 검증 Python은 저장소 `target/debug`와 개인 gstack 설치 경로를 가정한다. browse state만 별도이며 Chromium profile을 분리하지 않는다.

## 전역 제약

- 새 Cargo 실행은 `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target`를 사용한다.
- 작업 branch는 `codex/project-improvements`이며 main을 직접 수정하지 않는다. 최종 commit/push는 root 담당자가 승인 범위 안에서 수행한다.
- 기존 server PID 69169, manager PID 68561, browser PID 69199를 종료·재시작·재사용하지 않는다. 환경 전체나 credentials를 출력하지 않는다.
- 기존 `Config` 및 server `cli::Options`의 public struct literal을 깨지 않는다. 새 선택 옵션은 별도 타입/constructor로 제공한다.
- Rust MSRV는 workspace의 `1.96`, edition은 `2024`를 모든 crate에서 상속한다. 라이선스는 기존 MIT 선언을 실제 LICENSE로 뒷받침한다.
- 새 framework/DB/metric backend는 도입하지 않는다. 기존 Tokio, std, SVG, Python stdlib, Node test runner를 사용한다.

## 1. session 동시성과 취소 안전성

### 잠금과 소유권

전역 State는 lookup, 용량 예약, map 변경, generation 검증에만 사용한다. 이 mutex를 보유한 상태에서는 actor send/oneshot/다른 async lock을 기다리거나 GameModule callback을 실행하지 않는다.

Session에 안정적인 `Arc<tokio::sync::Mutex<()>>` lifecycle gate를 둔다. token 회전 시 동일 player의 gate를 새 token entry로 이동한다. gate 획득 절차는 다음과 같다.

1. State 아래에서 token entry의 gate를 clone한다.
2. State를 풀고 gate를 획득한다.
3. State를 다시 짧게 잡아 token 존재, gate 동일성(`Arc::ptr_eq`), generation, deadline, shutdown, room instance를 재검증한다.
4. actor handle과 요청 데이터를 복사하고 State를 푼다.
5. actor 처리 후 gate를 유지한 채 State에서 같은 조건을 재검증하고 map을 갱신한다.

동일 계정의 새 HTTP 발급은 `(game_id, issuer, subject)`별 gate로 직렬화한다. identity gate → session gate → State 순서만 허용한다. identity gate registry는 Weak 참조를 정리하며 발급의 기존 admission semaphore 범위 안에서만 확장한다. 전역 account gate 하나는 다른 계정까지 막으므로 사용하지 않는다.

### 요청 취소와 늦은 결과

단순히 `drop(state)`를 추가하거나 actor 대기에 timeout을 두는 것으로는 충분하지 않다. Enter 전송 후 HTTP/WS future가 취소되어도 actor가 좌석을 확보할 수 있다.

작은 내부 owned lifecycle operation을 사용한다. gate를 획득하고 검증한 뒤 actor 왕복과 map commit/보상을 소유하는 task로 옮기고 caller에는 oneshot 결과를 준다. **session별 활성 operation은 최대 1개**이며 대기자는 새 actor task를 생성하지 않는다. 생성 직후와 actor enqueue 전에 reply receiver/output 생존 여부를 확인한다. enqueue 이후에는 caller 취소만으로 operation을 abort하지 않는다. 정상 actor 응답 또는 actor closure까지 회수하여 아래 표의 조정을 마친다. 일반 lifecycle에 새로운 임의 timeout을 넣지 않는다.

이는 caller가 취소돼도 작업을 안전하게 정리하는 방식이며 무한 callback을 종료하는 장치가 아니다. coordinator는 별도 max_sessions 크기의 operation semaphore permit을 task 전체 수명 동안 보유한다. 삭제된 session의 미완료 작업도 permit을 계속 점유하며, permit이 없으면 새 lifecycle 작업은 SERVER_BUSY로 거부한다. session map에서 제거되었다고 작업 상한을 다시 쓸 수 있게 해서는 안 된다. coordinator와 보유 메모리는 이 operation 상한 및 room/admission 상한으로 제한하고 shutdown 시 caller에게 종료 상태를 반환한다. 장기간 응답 없는 actor의 operation은 해당 session에만 영향을 준다. operation task tracking은 종료 보고와 자원 accounting에 사용한다.

| 전이 | 선형화/commit 지점 | 실패·취소·늦은 결과 |
|---|---|---|
| Create | State에서 invisible room slot 예약, actor Enter 성공 후 같은 instance를 visible로 만들고 session 연결 | factory는 State 밖에서 실행. create/admission 실패는 해당 instance만 Stop·회수. max_rooms는 준비 중 slot도 포함. 취소 후 Enter가 이미 commit되었으면 session에 결과 반영 후 Disconnect로 reservation 보존 |
| Join/Resume | actor Enter commit 후, 같은 gate/generation/reference 확인하여 session 연결 | actor가 성공했으면 caller 취소와 무관하게 membership을 반영한다. output 종료는 Disconnect로 예약. session 자체가 만료/삭제되어 더는 소유자가 없으면 해당 generation에 한정한 Leave로 보상 |
| Authenticate | gate 아래 generation 증가 및 이전 output 폐쇄, actor Replace, 새 output/예약의 State 반영 | 실패 때 오래된 generation을 재사용하지 않는다. 취소된 새 output은 연결하지 않으며 actor Replace 결과 reservation을 남긴다 |
| Disconnect | generation 일치 확인 후 output 제거, actor 예약 결과 반영 | 과거 generation의 늦은 disconnect는 새 output/예약에 영향 없음. grace는 actor 기준으로 한 번만 시작하며 반복 호출로 늘리지 않음 |
| Leave/Logout | gate 아래 해당 room의 Leave ack 후 session membership 제거 또는 token 삭제 | caller 취소 후에도 완료. Leave는 generation을 반드시 지정. 늦은 결과는 바뀐 instance/session을 건드리지 않음 |
| Account rotation | 기존 Rotation prepare 후, State 안에서 재검증하고 `Rotation::commit()`과 token-map swap을 await 없이 실행 | 이전 token map은 commit 전까지 유지. caller 취소/기한 만료/shutdown 시 Rotation abort. 이미 actor generation이 전진했다면 session generation도 정합하게 유지하고 다음 인증이 더 큰 generation을 발급. commit 뒤 응답 손실은 새 로그인으로 회복 |
| Cleanup | 만료 대상 snapshot 후 각 gate를 `try_lock_owned`하여 재검증하고 제거 | busy gate는 이번 cleanup에서 건너뛴다. gate를 기다리며 전체 cleanup을 막지 않는다. 이미 제거한 session의 actor cleanup은 generation과 instance에 한정하며 독립적으로 진행 |
| Room closed | 정확한 RoomRef에 해당하는 directory/session만 제거 | 재사용된 room id의 새 instance는 보존. room/operation accounting은 정확히 한 번 감소 |

Room factory의 panic 처리는 현재 catch_unwind 계약을 유지한다. 동기 factory 자체의 실행 시간 상한을 새로 보장하지 않는다.

### 목록

State에서 현재 game의 visible/public handle 목록과 page 경계를 복사하고 즉시 unlock한다. `PublishedRoom` watch의 짧은 borrow에서 connected + reserved를 읽는다. closed publisher/다른 instance는 제외한다. 정렬된 handle을 순회해 유효한 요약을 limit+1개까지 모아 page/cursor를 결정하고, closed 항목을 건너뛰어도 살아 있는 후속 room이 누락되지 않게 한다. 정렬, cursor, limit 1..100, private room 비노출을 유지한다. member 수는 마지막 발행 요약이며 원자적인 전체 room snapshot이 아님을 protocol 문서에 표시한다. actor 왕복은 0회다.

## 2. transport 제어 비용

별도 public `ConnectionLimits` 타입을 `connection_limits.rs`에 둔다. 필드는 `control_rate_per_second: u32`, `control_burst: u32`, `invalid_rate_per_second: u32`, `invalid_burst: u32`이며 기본값은 각각 10, 20, 2, 4다. 0과 overflow 위험 값은 ConfigError로 거부한다.

`RuntimeOptions { connection_limits: ConnectionLimits }`, `Campfire::new_with_options(config, options)`, `Campfire::with_authenticator_and_options(config, authenticator, options)`를 추가한다. 기존 두 constructor는 기본 options를 사용한다. 기존 Config literal을 변경하지 않는다. server는 별도 환경 변수 `CAMPFIRE_CONTROL_RATE_PER_SECOND`, `CAMPFIRE_CONTROL_BURST`, `CAMPFIRE_INVALID_RATE_PER_SECOND`, `CAMPFIRE_INVALID_BURST`를 읽는 helper로 확장하고 기존 CLI Options 필드는 유지한다.

- Input/Execute는 기존 input bucket 계약을 유지한다.
- Authenticate/List/Create/Join/Leave/Resume/Logout은 connection-local control bucket을 사용한다. cache hit/replay도 reader에서 먼저 소비한다.
- malformed envelope, oversized/binary 등 유효하지 않은 application frame은 invalid bucket을 소비하고 `abusive_frames` 및 기존 abuse violation에 포함한다. 최초 인증 오류는 현재 즉시 종료 규칙을 유지한다.
- 반복 invalid frame은 bucket 여유가 있어도 violation limit에서 연결을 닫는다. rate 초과 error를 무한히 생성하지 않는다.
- 기존 frame/message 크기·semaphore·bounded request queue를 유지한다. 새 options가 없으면 기존 정상 Godot 흐름이 통과해야 한다.

## 3. actor 공정성과 bounded shutdown

RoomHandle에 큐와 별개인 watch Stop 신호를 추가한다. stop 요청은 대기 없이 `send_replace(true)`로 전달한다. actor는 loop 진입·control batch 사이·callback 이후 Stop을 확인한다. legacy Control::Stop은 내부 테스트 이행 동안 호환 가능하나 runtime 종료는 별도 신호를 사용한다.

각 cycle에서 이미 대기 중인 control의 길이를 snapshot하고 최대 control channel capacity(현재 64)만 처리한다. 그 snapshot에 포함된 Replace/Leave를 data 처리보다 먼저 수행하여 기존 generation barrier를 보존한다. 계속 들어오는 control은 다음 cycle로 넘긴다. 이어서 준비된 data 1개, due tick 최대 1개를 처리하고 cooperative yield한다. 빈 cycle은 Stop 우선 select로 잠든다. MissedTickBehavior::Skip과 고정 dt를 유지한다. 단순히 `biased`를 지우는 변경은 queued old-generation command 회귀를 일으키므로 금지한다.

`Campfire::shutdown_with_timeout(timeout: Duration) -> ShutdownReport`를 추가한다. `ShutdownReport`는 `completed: bool`, `remaining_rooms: u64`, `remaining_connections: u64`, `remaining_operations: u64`를 제공한다. 기존 `shutdown().await -> ()`는 기본 5초를 사용하고 미완료를 로그로 남긴다.

단 하나의 전체 deadline 안에서 shutdown 표시, 모든 output close, 모든 room Stop, supervisor/transport/operation 종료를 기다린다. room별 timeout을 누적하지 않는다. deadline 도달 시 report를 반환하고 pending task를 완료로 꾸미지 않는다. 가능한 async task 취소는 cooperative이며 실행 중인 동기 Game callback이나 spawn_blocking 작업을 강제로 중단하지 못한다. current-thread runtime이 동기 callback으로 막히면 timer도 실행되지 않으므로 wall-clock hard bound를 약속하지 않는다. 정상적으로 scheduler가 진행되는 runtime에서 API wait를 제한한다.

## 4. 운영 지표

기존 max 값은 생애 최대값으로 유지한다. 각 latency 종류(tick delay, tick duration, input latency, input duration, command duration)에 대해 최대 512개, 최대 60초의 최근 표본을 보존한다. 짧은 std Mutex의 ring에 기록하고 snapshot은 복사 후 lock 밖에서 정렬한다. p95/p99는 nearest rank이며 표본이 없으면 null이다. 최근 표본 개수와 window_ms=60000을 함께 노출하여 적은 표본의 한계를 드러낸다.

단발 command의 `commands_processed`, `commands_failed`, `command_replays`, `max_command_duration_micros`를 추가한다. processed는 validate/apply를 실제로 한 새 contiguous command, failed는 그 결과의 실패, replays는 actor result cache hit다. generation/sequence 오류는 실행으로 세지 않는다. duration은 새 command의 validate+apply 비용만 측정하고 중복 replay를 혼합하지 않는다.

public `MetricsSnapshot`에 `recent: RecentMetricsSnapshot`을 추가하고 dashboard model은 새 구조를 명시적으로 projection한다. credential, identity, payload는 표본에 포함하지 않는다. metric snapshot은 actor나 session gate를 기다리지 않는다. 공개 snapshot 필드 추가는 exhaustive struct literal 사용자에게 소스 변경이 필요할 수 있으므로 release note에 명시한다.

## 5. dashboard 탐색

기존 900표본/15분 저장 상한을 유지한다. 1분·5분·15분 버튼은 표시 범위만 변경한다. end는 마지막 유효 표본 timestamp이며 새 데이터를 발명하거나 없는 기간을 0으로 채우지 않는다. 기본 선택은 15분이다.

SVG hover는 가장 가까운 실제 표본의 시각·값·단위를 공통 readout에 표시한다. 차트는 keyboard focus 가능하고 Left/Right/Home/End로 표본을 읽는다. null은 '측정값 없음', 실제 0은 0이며 2.5초 초과 gap은 연결하지 않는다. 선택 sample_seq가 갱신/보존 범위 밖으로 나가면 가장 가까운 표본으로 이동하고 logout/boot 변경 시 초기화한다.

오류 요약은 선택 범위의 가장 이른 표본과 최신 표본의 누적 카운터 차이로 계산한다. 대상은 admission_rejections, abusive_frames, abusive_inputs, shared_queue_overloads, reliable_queue_disconnects, write_deadline_disconnects, room_panics 및 commands_failed다. boot 변경/카운터 감소/두 표본 미만은 증가분 '—'로 표시하고 관찰 기간을 함께 쓴다. 합계는 이벤트 수이며 중복 카운트될 수 있으므로 고유 사고 수로 표현하지 않는다.

## 6. 재현·CI·패키지

검증 runner는 `--target-dir`(기본 CARGO_TARGET_DIR 또는 repo target), `--browse`(기본 BROWSE_BIN 또는 PATH), `--godot`(기본 GODOT 또는 PATH)를 받는다. 세 Python runner와 중첩 호출·binary hashing이 같은 resolved binary를 사용한다. 개인 절대 경로 fallback은 제거한다. browse suite만 browse 설치를 요구한다.

각 실행마다 새 temp root 아래 BROWSE_STATE_FILE과 CHROMIUM_PROFILE을 모두 지정한다. 상속된 사용자 profile을 덮어쓰며 종료·삭제는 그 실행 소유 root와 PID만 대상으로 한다. 실제 browser 전에 subprocess mock으로 두 runner의 state/profile이 다르고 모든 호출/stop에 동일 env가 쓰이는지 확인한다.

빠른 CI는 PR/push에서 fmt, clippy -D warnings, Rust workspace tests/all-targets, server no-default-features check, Node UI tests, Python runner unit tests를 실행한다. scheduled/manual job은 명시적으로 준비한 Rust/Godot/Node/gstack 버전과 Linux Chromium 의존성으로 8·16 clients, account, 실제 browser suite를 실행한다. 소스·binary·tool hash, 버전, 원본 summary/log 및 실패 screenshot을 업로드한다. 미설치/실행 불가는 skip-success로 처리하지 않는다. action/tool ref는 실행 시 공식 upstream의 존재를 검증해 고정한다.

MIT LICENSE, workspace repository=https://github.com/sangjinsu/campfire, crate별 description, rust-version 상속, 내부 의존성 `path + version = "0.1.0"`를 추가한다. cargo package의 결과와 실제 registry publish 준비를 구분한다. runtime은 package 검증을 수행한다. arena/server는 미게시 내부 crate 때문에 registry resolution이 실패할 수 있으므로 로컬 registry/source replacement를 사용한 offline 검증 또는 정확한 제한 보고를 남긴다. `--no-verify`만 통과한 것을 컴파일 검증이라고 하지 않는다. 별도 temp consumer는 workspace 외부에서 runtime+arena를 사용하고 자체 GameModule, router, observer, shutdown API를 컴파일·실행한다. 패키지 tarball 소비 검증과 checkout path 소비 검증을 구분한다.

## 수용 기준과 한계

느린 actor A를 test barrier로 멈췄을 때 B의 issue/auth/List/command가 진행한다. 같은 session의 재인증/rotation/cleanup/취소 경쟁에서 좌석·token·generation이 일치한다. actor late Enter는 소유자를 잃지 않고 늦은 Leave는 새 generation을 지우지 않는다. control flood·invalid flood가 유한 오류/연결 종료로 끝난다. 지속 tick/control 부하에서도 data 및 Stop이 진행한다. bounded shutdown 결과는 실제 남은 작업을 보고한다.

최종 검증에는 기존 Rust account/rotation/privacy 회귀, 새 manual-clock 동시성 테스트, Node/Python 테스트, 새 binary의 실제 Godot 8·16 load, account 회귀, desktop/mobile browser가 모두 포함된다. source와 binary hash가 검증 전후 동일해야 한다. loopback 결과를 WAN/10배 규모의 용량 보장으로 표현하지 않는다.

## 기술 근거

부모 조사에서 확인한 공식 API 계약을 따른다: [Tokio Mutex](https://docs.rs/tokio/1.53.3/tokio/sync/struct.Mutex.html), [mpsc Sender](https://docs.rs/tokio/1.53.3/tokio/sync/mpsc/struct.Sender.html), [spawn_blocking](https://docs.rs/tokio/1.53.3/tokio/task/fn.spawn_blocking.html), [Axum WebSocketUpgrade](https://docs.rs/axum/0.8.9/axum/extract/ws/struct.WebSocketUpgrade.html). FIFO mutex와 timeout은 application transaction의 rollback을 제공하지 않는다. 구현 담당자는 새 API를 쓰기 전에 실제 lockfile 버전의 공식 문서를 다시 확인한다.
