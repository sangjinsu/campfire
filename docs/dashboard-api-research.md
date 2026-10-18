# Dashboard 공식 API 조사 기록

확인일: 2026-10-18. 승인 계획 구현 전 공식 문서와 해당 버전 source를 조사한 결과를 보존한다. 이 문서는 실제 런타임 검증 결과가 아니다.

| 구성 | 확인 사실 | 공식 근거 |
| --- | --- | --- |
| sysinfo 0.39.6 | MSRV 1.95이므로 repo Rust 1.96과 호환. `default-features = false`, `system` feature 사용 | [crate 문서](https://docs.rs/sysinfo/0.39.6/sysinfo/) |
| PID/process | `get_current_pid() -> Result<Pid, &'static str>`; 하나의 System을 유지; `process(pid)`는 Option | [get_current_pid](https://docs.rs/sysinfo/0.39.6/sysinfo/fn.get_current_pid.html), [System](https://docs.rs/sysinfo/0.39.6/sysinfo/struct.System.html) |
| refresh 범위 | `refresh_processes_specifics(ProcessesToUpdate::Some(&[pid]), true, ProcessRefreshKind::nothing().with_cpu().with_memory().without_tasks())`; nothing도 tasks 기본 true이므로 without_tasks 명시 | [ProcessRefreshKind](https://docs.rs/sysinfo/0.39.6/sysinfo/struct.ProcessRefreshKind.html), [ProcessesToUpdate](https://docs.rs/sysinfo/0.39.6/sysinfo/enum.ProcessesToUpdate.html) |
| CPU/RSS | 첫 CPU는 준비 표본. 실제 경과 간격을 상수 이상 보장. CPU 100%=한 logical core, multicore는 초과 가능. memory는 RSS bytes. process 없음/refresh 실패는 unavailable | [Process](https://docs.rs/sysinfo/0.39.6/sysinfo/struct.Process.html), [MINIMUM_CPU_UPDATE_INTERVAL](https://docs.rs/sysinfo/0.39.6/sysinfo/constant.MINIMUM_CPU_UPDATE_INTERVAL.html) |
| CPU 간격 | 조사한 Linux/macOS 구현의 최소 간격은 200ms. 숫자 하드코딩 대신 상수 사용 | [sysinfo source](https://docs.rs/sysinfo/0.39.6/src/sysinfo/lib.rs.html) |
| Tokio 1.53.3 | interval 첫 tick 즉시, 기본 Burst이므로 Skip 명시. Skip 후 tick 간격은 period보다 짧아질 수 있으므로 CPU 실제 경과 별도 확인 | [interval](https://docs.rs/tokio/1.53.3/tokio/time/fn.interval.html), [MissedTickBehavior](https://docs.rs/tokio/1.53.3/tokio/time/enum.MissedTickBehavior.html) |
| watch | borrow guard 즉시 clone/release. send는 receiver 없음 시 실패하지만 send_replace는 마지막 값을 저장 | [watch Sender](https://docs.rs/tokio/1.53.3/tokio/sync/watch/struct.Sender.html) |
| sampler 수명 | 시작된 spawn_blocking은 abort로 중단되지 않고 runtime 종료 대기에 영향을 줄 수 있으므로 전용 std thread 선택 | [spawn_blocking](https://docs.rs/tokio/1.53.3/tokio/task/fn.spawn_blocking.html), [JoinHandle](https://doc.rust-lang.org/1.96.0/std/thread/struct.JoinHandle.html) |
| Axum 0.8.9 | 기존 Router<()>와 dashboard router 합성; GET에 대한 HEAD도 인증 경계에 포함 | [Router](https://docs.rs/axum/0.8.9/axum/struct.Router.html), [get](https://docs.rs/axum/0.8.9/axum/routing/method_routing/fn.get.html) |
| secret 비교 | sha2 0.10.9의 고정 길이 digest와 subtle 2.6.1의 ConstantTimeEq 사용 | [sha2](https://docs.rs/sha2/0.10.9/sha2/), [ConstantTimeEq](https://docs.rs/subtle/2.6.1/subtle/trait.ConstantTimeEq.html) |
| static assets | include_str!로 HTML/CSS/JS를 binary에 포함 | [include_str](https://doc.rust-lang.org/1.96.0/std/macro.include_str.html) |
| browser | AbortController만으로 이미 완료된 응답의 반영을 막을 수 없으므로 generation guard 병행; visibility/pagehide에서 timer·요청 정리 | [AbortController](https://developer.mozilla.org/en-US/docs/Web/API/AbortController), [Page Visibility](https://developer.mozilla.org/en-US/docs/Web/API/Page_Visibility_API), [pagehide](https://developer.mozilla.org/en-US/docs/Web/API/Window/pagehide_event) |
| CSP | 같은 origin의 정적 script/style 사용. inline을 쓰려면 hash 또는 매응답 nonce가 필요하므로 세 자산으로 분리 | [CSP](https://developer.mozilla.org/en-US/docs/Web/HTTP/Guides/CSP) |

소스 확인 사실: `Campfire::metrics()`는 atomic/semaphore 값만 읽고 manager mutex를 잡지 않는다. 기존 metrics는 누적 counter와 lifetime max이며 game/room별 지표가 아니다. 기존 `/health`는 health 응답만 제공한다. WS room.list와 create/admit은 manager lock을 잡은 상태로 actor를 기다릴 수 있다. room actor만 game state를 소유하며 publish가 snapshot sequence를 증가시킨다. room.members에는 예약 seat도 포함된다. 기존 `session.closed`의 room 삭제는 instance 확인이 없어 이번 구현에서 보강해야 한다. `Config`/`Options` public struct 필드 추가는 literal 사용자의 source compatibility에 영향을 준다.

이 조사로 새로운 HTTP 오류율, p95, CPU quota, 실제 외부 TLS 상태를 측정했다고 해석하지 않는다.
