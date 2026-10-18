# Campfire 조회 전용 대시보드 설계

상태: 2026-10-18 사용자 승인 계획. 기준 checkout: `f5b83a3`, 구현 branch: `codex/server-dashboard`.

## 1. 범위와 구성

외부의 단일 Campfire 프로세스를 HTTPS로 조회하는 관리자 웹 대시보드다. 전체 지표, game별 룸 목록과 정원·연결 인원·재접속 예약 인원·최근 갱신, 최근 15분 전역 이력, 프로세스 CPU/RSS를 제공한다. DB, 별도 Node 서비스, SSO, 룸 종료·kick, 사용자별 ID 목록, game payload 조회는 범위 밖이다.

`campfire-runtime`은 관측 DTO와 읽기 API를 제공하고, `campfire-server::dashboard`는 인증·수집·HTTP·정적 화면을 소유한다. HTML/CSS/JS 세 파일을 `include_str!`로 binary에 포함한다. 기존 `Config`, `cli::Options`의 public 필드와 `Campfire::router()` 형식은 보존한다. 기본 서버와 `examples/account_server.rs` 모두 기존 router에 dashboard router를 merge하고 serve 전체 기간 handle을 보유한다. 별도 프로세스로 실행하면 각자 자기 프로세스만 관측한다.

```rust
Campfire::observer(&self) -> RuntimeObserver
RuntimeObserver::snapshot(&self) -> Option<RuntimeObservation>
DashboardOptions::from_env() -> Result<Option<DashboardOptions>, DashboardError>
Dashboard::start(observer: RuntimeObserver, options: DashboardOptions)
    -> Result<Dashboard, DashboardError>
Dashboard::router(&self) -> axum::Router<()>
Dashboard::shutdown(&mut self) // async, idempotent
```

`RuntimeObserver`는 `Weak<Inner>`를 보유한다. runtime 소멸 시 `None`, 종료 시작 후 `stopping = true`를 반환한다. 공개 DTO는 Serialize 가능한 primitive 값만 포함한다.

| DTO | 필드 |
| --- | --- |
| `RuntimeObservation` | `metrics: MetricsSnapshot`, `stopping: bool`, `uptime_ms: u64`, `game_ids: Vec<String>`, `rooms: Vec<RoomObservation>` |
| `RoomObservation` | `game_id`, `room_id`, `room_instance_id`: String; `public`: bool; `capacity`, `connected_members`, `reserved_members`: usize; `server_tick`, `snapshot_seq`, `summary_age_ms`: u64 |

## 2. 관측·수명·보관 계약

manager가 actor를 await하는 기존 경로와 분리된 짧은 `RwLock` observation directory를 둔다. 등록 game ID와 instance까지 포함한 room key별 watch receiver만 보관한다. 잠금 순서는 manager/modules → observation directory이며, directory guard 안에서는 await·game callback·manager 재진입을 금지한다. reader는 receiver 목록을 복사하고 lock을 해제한 뒤 `watch`의 마지막 값을 즉시 clone한다.

actor가 생성·멤버 변화·약 1초 heartbeat에서 `send_replace`로 summary를 발행한다. 관측은 `Game::snapshot`, `private_snapshot`, `publish()`를 호출하거나 sequence를 증가시키지 않는다. 연결과 예약은 actor가 실제 보유한 seat 상태이며 합계는 capacity 이하이다. 닫힌 sender는 조회에서 제외한다. observation directory와 기존 `state.rooms` 모두 제거 대상 instance가 일치해야 한다. `active_rooms`는 actor 수명당 정확히 한 번 감소한다. 기존 종료 supervisor의 game/room-only 삭제는 이 계약에 맞게 수정한다.

collector는 1초 간격 `MissedTickBehavior::Skip`으로 immutable cache를 교체한다. HTTP는 cache와 monotonic clock만 읽는다. 원본 atomic 지표와 room 요약은 하나의 원자적 트랜잭션이 아니므로 전이 중 일시적으로 개수가 다를 수 있다.

OS sampler는 전용 `std::thread` 최대 하나다. `System` 인스턴스를 유지하고 현재 PID의 CPU·memory만 refresh한다. `without_tasks()`로 task 수집을 끈다. 실제 CPU refresh 간격은 `MINIMUM_CPU_UPDATE_INTERVAL` 이상이어야 한다. 첫 CPU 표본은 null, 이후 0은 정상 idle일 수 있다. CPU는 논리 코어 1개 = 100%이며 100% 초과를 허용한다. RSS 단위는 bytes다. 실패·정체 시 null과 unavailable/stale 상태를 제공하고 룸 collector는 계속 동작한다.

종료 시 취소 신호로 신규 수집을 막는다. async shutdown은 `is_finished()` 확인 후 join하며 총 대기 상한은 2초다. 미완료 OS thread는 detach하고 교체 thread를 만들지 않는다. `Drop`은 취소만 수행하고 기다리지 않는다. 정상 serve 종료와 오류 경로 모두 dashboard/runtime 정리를 호출한다. 이미 실행 중인 OS 호출의 강제 취소를 보장하지 않는다.

이력은 monotonic 시각 기준 최근 15분과 최대 900개를 모두 적용한다. 전역 지표·프로세스 표본만 저장하고 방별 이력은 저장하지 않는다. 누락 구간을 0이나 복제값으로 채우지 않는다. 재시작은 새 boot ID와 빈 이력으로 시작한다. 누적 counter는 실제 경과시간으로 rate를 구하고, lifetime max는 실행 이후 최댓값으로 표시한다. 전체 룸의 tick 합계를 서버 Hz로 표시하지 않는다. 미측정 HTTP 오류율·p95·CPU quota를 만들어 표시하지 않는다.

## 3. HTTP·보안·화면 계약

| Endpoint | 계약 |
| --- | --- |
| `GET /admin/api/status` | 현재 전역 지표, 등록 game별 요약, 프로세스 측정과 freshness |
| `GET /admin/api/rooms` | game 필터, instance 포함 keyset cursor, 기본·최대 100개 페이지 |
| `GET /admin/api/history` | boot ID와 `after_seq`로 증분 조회, 최대 900개 전역 표본 |

공통 envelope는 `{schema_version: 1, boot_id, state, sample_seq, collected_at, cache_age_ms, data}`다. `state`는 `warming_up|ready|stale|stopped`, `collected_at`은 Unix ms 또는 null, `cache_age_ms`는 ms 또는 null이다. freshness 판단에는 monotonic clock을 사용한다. 모든 age는 ms, 메모리는 bytes, CPU는 percent이다.

확정된 data 계약:

- status: `{uptime_ms, stopping, metrics, rates, rooms, games, process}`. `metrics`는 기존 MetricsSnapshot allowlist다. `rates`는 `ticks_per_second`, `inputs_per_second`, `snapshots_per_second`, `admission_rejections_per_second`, `shared_queue_overloads_per_second`이며 첫 표본과 해당 counter 감소 시 null이다. `rooms`는 `{total, public, private, connected_members, reserved_members}`, `games`는 각 `{game_id, ...roomTotals}`다. `process`는 `{pid, state, cpu_percent, rss_bytes, collected_at, age_ms}`이며 process state는 `warming_up|ready|stale|unavailable`, 측정값과 시각/age는 null 가능하다.
- rooms query: `game_id`, `visibility=all|public|private`, `q`, `limit=100`, `cursor=opaque`. data는 `{rooms: [RoomObservation + state], next_cursor, total_filtered}`다. cursor는 필터와 정렬 key에 결합하며 유효하지 않은 값은 400이다. 모든 페이지는 현재 cache를 읽으므로 룸이 페이지 사이에 변할 수 있다.
- history query: `after_seq`, `boot_id`, `limit=900`. data는 `{samples, reset, oldest_seq}`다. 각 sample은 `{sample_seq, collected_at, interval_ms, metrics, rates, rooms, process}`이며 rooms는 전역 합계다. 최초 조회·boot 불일치·유실된 이력 cursor는 `reset: true`와 현재 보관된 실제 표본을 반환한다. oldest_seq는 null 가능하다.
- 오류는 공통 envelope에 `error: {code, message}`를 추가하고 `data: null`을 반환한다. 현재 status의 process와 room age는 HTTP 응답 시 재계산한다. history 안의 process 상태·age는 각 표본이 수집될 당시의 값을 보존한다. 과거 표본을 현재 시각으로 다시 stale 처리하지 않는다.

정적 자산 경로는 `assets/index.html`, `assets/app.js`, `assets/style.css`로 고정한다.

`CAMPFIRE_ADMIN_TOKEN` 미설정/빈 문자열은 기능 OFF로 모든 admin 경로가 404다. nonempty 값은 32–256바이트 ASCII graphic 문자이며 공백·제어 문자·잘못된 길이는 startup error다. 자동 trim, CLI secret, secret Debug/Serialize를 금지한다. token digest는 검증된 constant-time 비교를 사용한다.

활성화된 `/admin`과 같은 origin의 JS/CSS는 token 입력 shell만 공개한다. 데이터 API에는 하나의 정확한 Bearer header만 허용하고 query·cookie·game token은 인증으로 인정하지 않는다. 누락·오류·중복 인증 header는 401이며 secret을 echo/log하지 않는다. HEAD는 GET과 같은 인증, 변경 method는 405다. 모든 admin 응답은 no-store이며 CSP self-only, frame 차단, no-referrer, nosniff를 적용한다. CDN과 CORS 허용을 추가하지 않는다. API DTO에는 settings·credentials·claims·player ID·public/private game payload를 넣지 않는다.

token은 browser 메모리에만 보관한다. 입력 후 입력창을 비우고 URL/storage/cookie/log에 저장하지 않는다. 5초 자동 갱신과 수동 갱신을 제공하고 요청을 중첩하지 않는다. OFF는 향후 polling을 멈춘다. hidden/pagehide는 timer와 진행 요청을 취소한다. logout·401·이전 request generation의 늦은 응답은 화면을 다시 채우지 못한다.

actor/resource/cache stale는 서버에서 3초 기준으로 평가한다. 브라우저는 마지막 성공 이후 10초와 paused 상태를 별도로 표시하여 5초 polling 사이마다 stale가 깜박이지 않게 한다. 빈 game/룸 없음, 초기 수집, 실패, stale를 구분한다.

HTTPS는 reverse proxy에서 종단한다. backend 직접 노출 제한과 Authorization 로그 배제 예시를 문서화한다. 이번 구현 완료는 실제 외부 TLS 배포 검증을 의미하지 않는다.

## 4. 수락 기준과 근거

- 가상 clock: 1초 Skip, 900개/15분 동시 제한, gap, boot reset, freshness 경계.
- runtime: 연결/예약/재접속/만료/종료/panic, instance 재사용과 지연 종료, active_rooms 정확 감소, observer가 runtime을 붙잡지 않음.
- 격리: manager await 지연 중 관측 지속, 관측으로 game callback 또는 tick/snapshot sequence 추가 변화 없음.
- 인증·비밀: OFF 404, invalid env startup 실패, Bearer/HEAD/method 검증, sensitive sentinel이 JSON/HTML/log에 없음.
- 자원 수집: 첫 CPU null, 실패/정체에도 룸 cache 갱신, worker 한 개 제한, 2초 shutdown/Drop 비차단.
- 화면: 필터·pagination·증분 history, hidden/OFF/logout/401과 늦은 응답, 초기·paused·stale 상태.
- 기존 workspace 테스트/lint와 arena 없는 host 구성이 통과한다. 실제 8명·16명 접속/이동/재접속 중 dashboard polling을 검증하고 관측 전후 tick 지연·CPU·RSS를 기록한다.
- 구현 완료 전 독립 Architect 검증을 받는다. 배포·동기화·목표 부하의 증거를 서로 구분한다.

소스 확인: `crates/campfire-runtime/src/session.rs`의 manager await·room 생성/종료, `room.rs`의 actor 독점 상태·publish sequence, `metrics.rs`의 atomic 누적/최댓값, `campfire-server/src/main.rs`의 router composition. 공식 API 근거는 [dashboard-api-research.md](../../dashboard-api-research.md)에 보존한다.
