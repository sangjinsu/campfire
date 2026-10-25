# Task 2·3 독립 Spec / Quality 리뷰

검토 기준: `task-2-3-review.diff`(Task 1 커밋 `6f70d2b` 이후 증분), 각 task brief/report, `docs/superpowers/specs/2026-10-25-runtime-hardening-design.md`, 관련 현재 소스와 root 검증 로그. 리뷰 과정에서 제품 소스 수정, Cargo 실행, browser 실행, 프로세스·환경 조작은 하지 않았다.

## Task 2

- **Spec compliance: APPROVED**
- **Quality: APPROVED**

### Critical

없음.

### Important

없음. 이번 범위에서 재현 조건과 코드 경로로 뒷받침되는 차단 결함을 발견하지 않았다.

### 확인 근거

- `crates/campfire-runtime/src/connection_limits.rs:5`의 public 옵션과 기본값 10/20/2/4, 양수 및 유한 상한 검증이 brief와 일치한다. 기존 `Config`/server `Options` 필드를 늘리지 않고 기존 constructor가 기본 options에 위임한다.
- `crates/campfire-runtime/src/transport.rs:243`에서 연결별 input/control/invalid bucket을 각각 생성한다. `transport.rs:325`의 Input/Execute 분류 및 reader의 예산 소비는 `transport.rs:379`의 response cache 조회보다 앞서므로 cached control replay가 예산을 우회하지 않는다. 공유 request queue 포화는 개인 위반으로 집계하지 않는다.
- `transport.rs:457`의 invalid 처리에서 binary·parse/schema·application 크기 초과가 공통 위반 횟수와 invalid bucket을 소비한다. 최초 인증 오류는 즉시 종료하고, 이후에도 예산 소진 또는 `max_abuse_violations`에서 종료하므로 오류 응답 생성이 무한히 이어지지 않는다. 기존 frame ceiling, connection/unauthenticated permit, input 예산 및 오류 형태를 유지한다.
- `crates/campfire-server/src/connection_limits.rs:4`의 helper는 지정된 네 환경 변수만 읽으며 잘못된 값은 오류로 반환한다. 기본 서버와 account example 모두 helper 결과를 새 constructor에 전달한다. 설정·protocol 문서의 기본값과 재시도 설명도 구현과 일치한다.
- paused-clock refill/모든 옵션 필드의 0·상한 초과, 별도 연결 예산, control flood, malformed/binary 종료, cache replay, 정상 Create/List/Leave/Resume의 테스트가 있다. 104-room pacing은 이전 Task 4 승인 범위에서 유지된다.

### Misc / 비차단 관찰

추가 수정 요청 없음. 실제 Godot 8·16 부하와 최종 browser 검증은 root의 별도 최종 게이트이며 이 소스 리뷰의 승인만으로 그 결과를 주장하지 않는다.

## Task 3

- **Spec compliance: APPROVED**
- **Quality: APPROVED**

### Critical

없음.

### Important

없음. shutdown, actor 순서, 관측·카운터 수명과 Task 1 lifecycle 보장 간 차단 회귀를 발견하지 않았다.

### 확인 근거

- `crates/campfire-runtime/src/room.rs:255`에서 Stop의 현재 값과 sender 종료를 먼저 확인한다. 최대 64개의 queued control snapshot을 data보다 먼저 처리하고 control 사이와 data/tick callback 이후에도 Stop을 확인한다. 이어서 준비된 data 1개와 due tick 최대 1개를 처리한다. Input/Command 교대, Skip, 고정 dt와 cooperative yield를 유지한다.
- `room.rs:263`의 idle-select pending control은 별도 control cycle로 종료하고 다음 cycle의 queued snapshot을 먼저 처리한다. 따라서 pending 1개가 queue slot을 비운 뒤 producer가 채우는 경우에도 queued tail generation fence를 data 뒤로 보내지 않는다. `room.rs:288`의 observation publication과 empty 처리는 yield 전에 실행된다. exact pending1+queued64 concurrent interleaving의 실행 검증은 주장하지 않으며, full queued64(List 63개 + tail Replace)의 결정론적 회귀와 구분한다.
- `crates/campfire-runtime/src/session.rs:227`은 하나의 deadline으로 초기 State 획득부터 종료 대기까지 제한한다. output close와 모든 room의 별도 watch Stop을 actor await 없이 전달하고 State를 놓은 후 기다린다. room은 supervisor의 `closed` 정리가 끝날 때 active count를 반환하며, connection permit은 reader/writer 및 disconnect 종료까지, owned operation permit은 Task 1 coordinator 수명 전체에 유지된다. `session.rs:263`의 report는 이 잔여 수량을 읽고 deadline 만료를 완료로 바꾸지 않는다.
- `session.rs:214`의 기존 `shutdown() -> ()`는 5초 wrapper이며 미완료 수량만 기록한다. 동기 callback의 강제 중단이나 scheduler가 멈춘 경우의 wall-clock 보장을 문서·보고서에서 주장하지 않는다. factory 도중 Stop, 늦은 admission, shutdown 후 session 소멸은 기존 instance/generation/commit 검증과 별도 Stop으로 처리된다.
- `crates/campfire-runtime/src/metrics.rs:47`은 latency 5종 각각 512개·60초 ring과 기존 lifetime max를 유지한다. `metrics.rs:60`의 snapshot은 bounded copy/expiry 이후 lock 밖에서 정렬하며 nearest-rank p95/p99와 empty=null, 실제 0=0을 제공한다.
- `room.rs:679` 이후 command cache hit는 `command_replays`만 증가한다. `room.rs:708`의 새 contiguous command validate/apply만 duration과 processed/failed에 포함된다. generation/sequence 거부는 실행 표본에 포함되지 않는다.
- `crates/campfire-runtime/src/observation.rs:183`은 Weak runtime, cached watch summary와 metrics만 읽으며 session gate·actor 요청·게임 callback을 호출하지 않는다. recent expiry는 metric 저장소 내부 정리이며 game/session state를 변경하지 않는다. 새 표본과 로그에는 credential·identity·payload가 추가되지 않는다.

### Misc / 비차단 관찰

추가 수정 요청 없음. bounded shutdown의 범위는 scheduler가 진행되는 동안의 API wait이며 callback 강제 종료가 아니다. 공개 MetricsSnapshot 확장은 별도 release-note 통합 범위다.

## 읽은 검증 근거와 범위

- `/tmp/campfire-improvements-20261025/root-final-tests.log`: 완료된 workspace all-targets 결과 총 **141 passed, 0 failed, 4 ignored** 확인. 여기에는 runtime 71 unit, connection-limit integration 8, invariants 2가 포함된다.
- `root-final-clippy.log`, `root-final-no-default.log`, `root-final-build.log`, `root-final-build-account.log`: 각각 성공 종료의 `Finished` 기록 확인.
- owner 보고서의 RED→GREEN, Stop full queue/초기 true, blocked callback 종료 보고, observer publication 회귀, full control fence, recent expiry/percentile 및 command counter 테스트와 실제 테스트 소스를 대조했다.
- 읽은 시점 `root-final-ignored.log`는 실시간 soak 실행 중이었다. 이 리뷰는 ignored 4개 전체 통과, 새 binary의 Godot/browser 통과, WAN 또는 확장 성능 보장을 완료 사실로 포함하지 않는다.

**최종 결론: Task 2와 Task 3 모두 Spec compliance / Quality APPROVED. 현재 제품 소스 수정이 필요한 finding 없음.**
