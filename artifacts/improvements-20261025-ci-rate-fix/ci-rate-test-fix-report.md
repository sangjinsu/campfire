# CI rate-limit 통합 테스트 진단 및 수정

기준 커밋: `45e87bf09e363aa147f63d3f8cd27aae6d9f24d8`. 수정 범위는 `crates/campfire-server/tests/headless.rs`의 `guest_request` helper와 `issuer_and_global_guest_rate_limits_recover` 한 테스트다. 제품 소스, 설정 기본값, dependency, 다른 테스트는 변경하지 않았다.

## 실패 증거와 원인

- 원본 GitHub quick run `40158205928`은 `issuer_and_global_guest_rate_limits_recover`에서 실제 HTTP `201`, 기대 `429`로 실패했다. 원본 증거: `artifacts/improvements-20261025-github/quick/failed-steps.log` 344–359행. headless 결과는 17 passed / 1 failed / 4 ignored였다.
- `crates/campfire-runtime/src/rate.rs:15`는 `tokio::time::Instant` 경과초 × rate로 token을 보충한다. `session.rs:679` 이하의 실제 guest issuer가 이 시계를 읽고 source/global bucket을 차례로 소비한다. rate 10/s일 때 100ms가 지나면 한 token이 복구되는 것이 정상이다.
- 기존 테스트는 burst=2, rate=10/s에서 HTTP 요청 세 번이 100ms 안에 처리된다고 전제했다. `guest_request`는 요청마다 새 reqwest Client와 연결을 만들었으며, 경과시간을 통제하거나 측정하지 않았다.
- 원본 CI에는 요청별 elapsed가 없어 CI에서 어느 작업이 100ms 이상 지연됐는지는 확정할 수 없다. Client 생성/연결/OS 스케줄링의 개별 기여도도 측정되지 않았다. 다만 잘못된 시간 전제와 그에 따른 동일 실패는 아래 로컬 실험으로 직접 확인했다.

## RED 재현

원본 테스트에서 두 번째 성공 이후 세 번째 요청 전에 다음 제어 지연만 추가했다. rate, burst, assertion은 바꾸지 않았다.

```rust
let wall_started = Instant::now();
let clock_started = tokio::time::Instant::now();
tokio::task::spawn_blocking(|| std::thread::sleep(Duration::from_millis(120)))
    .await
    .unwrap();
let limited = guest_request(&issuer).await;
```

실행 명령:

```sh
CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target CARGO_BUILD_JOBS=4 cargo test -p campfire-server --test headless issuer_and_global_guest_rate_limits_recover --locked --offline -- --exact --nocapture
```

결과(`/tmp/campfire-rate-red.log`, exit 101):

```text
controlled delay: wall=126.557792ms, rate clock=126.557792ms, third status=201 Created
assertion `left == right` failed
  left: 201
 right: 429
test result: FAILED. 0 passed; 1 failed
```

최초 sandbox 내 시도는 localhost bind에서 EPERM으로 멈췄다. 이는 제품 실패와 분리했고 승인된 localhost 테스트 권한으로 재실행한 위 결과가 RED 근거다.

## 수정 계약

- `#[tokio::test(start_paused = true)]`로 rate clock을 제어한다. HTTP 서버/router와 실제 TCP 요청은 유지한다.
- HTTP I/O 대기 중 Tokio 자동 시간 진행을 막기 위해 `spawn_blocking` 안에서 std mpsc receiver를 기다리는 guard를 둔다. 공식 설치 소스 Tokio 1.53.3 `src/time/clock.rs:120–159`의 Auto-advance/Preventing auto-advance rustdoc 및 `src/runtime/blocking/schedule.rs:25`의 inhibit 호출을 확인했다. Cargo.lock의 실제 버전도 1.53.3이다.
- sender는 테스트 stack의 소유자 하나뿐이다. assertion panic/early return에서도 sender Drop이 recv 대기를 해제한다. 정상 경로는 sender를 명시적으로 drop하고 blocking task join을 기다린다.
- 공유 reqwest client를 사용하고 응답 JSON body를 소비해 연결 재사용이 가능하게 한다. 이 개선만으로 시간 정확성을 보장한다고 가정하지 않는다.
- source/global 각각 기존 burst=2, rate=10/s를 유지한다. 두 요청은 정확히 201, 이후 정확히 429와 RATE_LIMITED 및 해당 scope를 확인한다.
- 제어된 wall 120ms 지연 fixture를 유지하면서 virtual clock이 0ms임을 확인한다. 이어 virtual 99ms에서는 429, 100ms에서는 201 한 번, 같은 시각의 다음 요청은 다시 429인지 검증한다. 시간 단위, 경계, burst 차감, source/global 중 하나의 limiter 누락을 잡는다.
- 기존 회복 확인용 wall sleep 120ms를 제거했다. 남은 wall 지연은 flaky 조건을 의도적으로 재현하는 fixture이며 대기 후 무조건 성공시키는 retry나 threshold 완화가 아니다.

## 새 로컬 검증 결과

모든 Cargo 실행은 지정된 임시 target과 build jobs 4를 사용했다. 실행 중인 제품 서버/브라우저 및 원본 repo target/debug는 수정하지 않았다.

| 검증 | 결과 | 증거 |
|---|---|---|
| focused regression | 1 passed, 0 failed, exit 0 | `/tmp/campfire-rate-green.log` |
| headless 전체 | 18 passed, 0 failed, 기존 4 ignored, 2.45s, exit 0 | `/tmp/campfire-rate-headless.log` |
| cargo fmt --all -- --check | exit 0 | `/tmp/campfire-rate-fmt.log` |
| cargo clippy --workspace --all-targets --locked --offline -- -D warnings | exit 0 | `/tmp/campfire-rate-clippy.log` |
| git diff --check | exit 0 | 명령 결과 |

기존 ignored 4개는 reconnect_grace_expires, slow_reader_isolated_and_p95, soak_16_clients_60_seconds, soak_8_clients_60_seconds이며 이 작업에서 상태를 바꾸지 않았다. 전체 workspace 테스트, Architect 승인, 커밋/push 및 새 GitHub quick 재실행은 상위 에이전트가 이어서 검증한다. 로컬 GREEN은 원본 GitHub 실패를 지우거나 원본 run을 성공으로 재분류하지 않는다.
