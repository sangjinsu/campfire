# Campfire 프로젝트 개선: 구현과 검증 인계

기록일: 2026-10-25. 대상 branch는 `codex/project-improvements`, 기준 commit은 `d04f6f19c8228a72e71613657b97d9b8dcefee3c`, 최초 로컬 Rust·실환경 검증 대상 commit은 `647aaf1994d58906d70436f90de80827f45d44b5`다. [승인 설계](superpowers/specs/2026-10-25-runtime-hardening-design.md)와 [실행 계획](superpowers/plans/2026-10-25-project-improvements.md)에 대한 증거를 아래에 연결한다. **로컬 구현·통합·패키지 검증이 완료됐다. GitHub runtime validation은 성공했고, 첫 quick CI 실패를 테스트 수정으로 해결한 뒤 새 quick CI도 성공했다.** 두 성공 run의 commit은 다르며 아래에 각각 기록한다.

증거의 commit은 단계별로 다르다. 최초 소스 동결과 Mac 실환경 검증은 `647aaf1`, 패키지 수정은 `8ce48d654c930f8aaad73b9e7cfdb4d3488f0a82`, 문서 기록 및 첫 GitHub 두 run은 `45e87bf09e363aa147f63d3f8cd27aae6d9f24d8`에 해당한다. 첫 CI 뒤의 테스트 전용 수정은 `84af2abec020a722ac660fac479717dca17ff51f`다. 각 수치와 바이너리 해시는 해당 단계에만 적용한다.

## 변경된 동작

| 영역 | 확인할 동작 |
| --- | --- |
| Session과 room | 전역 `State` mutex를 잡은 채 room actor 응답을 기다리지 않는다. Session별 lifecycle gate와 identity gate가 순서를 보존하며, 취소 뒤 늦은 actor 결과도 좌석·generation·room instance 기준으로 조정한다. `room.list`는 마지막 발행 요약을 읽는다. 목록 전체가 한 시점의 원자적 snapshot이라는 보장은 없다. |
| 연결 제어 | 연결별 control 예산 기본값은 초당 10/burst 20, invalid 예산은 초당 2/burst 4다. 기존 input 예산과 분리하고 cache replay도 control 예산에 넣는다. |
| 종료와 지표 | Stop은 일반 control queue와 분리되고, 유한 control batch 뒤 data와 due tick에 기회를 준다. `shutdown_with_timeout`은 남은 room/connection/operation을 보고하며 기존 `shutdown()`은 5초 제한 wrapper다. 최근 tick delay/duration, input latency/duration, command duration은 각 60초·최대 512개 표본으로 p95/p99를 제공한다. 명령 처리/실패/replay 수는 구분한다. |
| Dashboard | 실제 이력에서 1/5/15분 표시 범위를 고르고 pointer·키보드로 같은 표본을 읽는다. `null`과 측정값 0, 빈 시간 구간, 누적 오류의 증가분과 counter reset을 구분한다. |
| 검증·배포 준비 | QA runner는 별도 target/binary 경로, 전용 browser state/profile을 받는다. MIT LICENSE, 세 crate의 Rust MSRV 1.96 및 패키지 메타데이터가 추가됐다. 빠른 CI와 수동·주간 실환경 workflow가 설정됐다. |

동시성·취소·Stop·지표·UI·QA 격리의 source 검토 근거는 [독립 Architect 감사](../.superpowers/sdd/2026-10-25-project-improvements/final-architect-audit.md)다. 수정 범위는 독립 검토에서 승인됐으며 CI의 실제 통과 여부는 run별로 아래에 기록한다.

## 로컬 소스와 자동 검증

검증은 macOS/Darwin arm64, Rust 1.96.1, 분리된 `CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target`에서 수행됐다. [명령별 종료 코드](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-final-commands.json)에는 아래 Cargo 7단계가 모두 exit 0으로 기록돼 있다. 재실행 시 예제와 기본 서버 바이너리를 **둘 다** 빌드해야 실제 QA가 변경된 서버를 사용한다.

```sh
export CARGO_TARGET_DIR=/tmp/campfire-improvements-20261025-target
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo check -p campfire-server --no-default-features
cargo build -p campfire-server
cargo build -p campfire-server --no-default-features --example account_server
cargo test -p campfire-server --test headless --all-features --locked --offline -- --ignored --nocapture --test-threads=1
node --test tools/tests/*.mjs
python3 -m unittest discover -s tools/tests -p 'test_*.py'
```

Cargo 일반 실행은 **141 passed, 4 ignored**였고, 별도 ignored 실행에서 grace 만료, slow reader, 8명·16명 각각 60초 soak **4 passed**였다. 따라서 실행된 Rust 테스트는 총 145개다. [일반 테스트 로그](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-final-tests.log), [ignored 로그](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-final-ignored.log), [8명](../artifacts/improvements-20261025-final-escalated/root-regression-artifacts/soak-8.json)·[16명](../artifacts/improvements-20261025-final-escalated/root-regression-artifacts/soak-16.json)·[slow reader](../artifacts/improvements-20261025-final-escalated/root-regression-artifacts/slow-reader.json) 측정 원본을 보존했다. 별도 [Node 로그](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-node-task5-fix1.log)는 dashboard 23개 시나리오를 기록한다. 패키지 도구 수정 전 [Python 로그](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-python-all.log)는 unittest 16개, 수정 후 독립 [최종 Python 로그](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-python-final.log)와 [해시·종료 코드](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-python-final-evidence.json)는 **17개 통과, exit 0**을 기록한다.

[소스 동결 확인](../artifacts/improvements-20261025-final-escalated/root-verification-logs/root-source-freeze-verification.json)은 검증 당시 위 commit과 작업 트리의 84개 대상 파일이 일치하고 불일치가 0개임을 기록한다. 그 범위의 aggregate SHA-256은 `684240daa66fd580d6569a6261ba9839ec577b806d2d3e822d464eea38e5c0dc`다. 이후 패키지 단계에서는 `tools/package_smoke.py`와 `tools/tests/test_package_smoke.py` 두 파일, 첫 CI 뒤에는 `crates/campfire-server/tests/headless.rs` 한 파일만 추가로 바뀌었다. 제품 Rust, manifest/lock, UI, Godot, QA runner와 workflow 입력은 변하지 않았다는 [후속 소스 대조](../artifacts/improvements-20261025-ci-rate-fix/verification.json)를 보존했다. 최초 Mac 실환경 실행의 새 서버 SHA-256은 `68f9f31b3187f5023bee93115caa287450603f8489b65d5511536671adc0cfb2`, 계정 예제는 `e32527b99e63a470d7d5aa8dde4306435f8e7908941b44522dd37fb2ef481722`다. 기존 checkout의 `target/debug/campfire-server` SHA-256은 `b1338640176776557099605aeaf7046fbd25979b9fea6ffc20551f6459ed34f7`로 별도다.

## 실제 loopback 통합 실행

다음 명령으로 새 target의 서버와 Godot, 별도 browser/profile을 사용했다. 실행 결과 원본은 [통합 summary](../artifacts/improvements-20261025-final-escalated/summary.json), 개별 설명은 [QA 보고](../.superpowers/sdd/2026-10-25-project-improvements/task-8-live-report.md)에 있다.

```sh
python3 tools/validate_dashboard.py --suite all \
  --target-dir /tmp/campfire-improvements-20261025-target \
  --browse /Users/sangjinsu/.claude/skills/gstack/browse/dist/browse \
  --godot /opt/homebrew/bin/godot \
  --duration 20 \
  --output artifacts/improvements-20261025-final-escalated
```

`summary.json`의 `success`는 true다. 실제 Godot client의 dashboard 비활성/활성 × 8/16명 네 조합은 모두 약 20초 동안 성공했고, 활성 시 HTTP polling은 각각 203회와 210회로 오류 0회였다. Browser 항목 15개와 UI probe 판정 20개가 통과했다. 여기에는 실제 104개 room의 100+4 paging, private/filter/search, desktop/mobile 배치와 API 개인정보 제외가 포함된다. 1/5/15분 범위, 키보드·pointer, null/0, 이력 gap, 오류 counter reset, 지연 응답·logout은 **통제된 browser fixture**에서 확인한 동작이며 부하 측정으로 읽지 않는다. [Browser 점검](../artifacts/improvements-20261025-final-escalated/browser-checks.json)과 [UI probe](../artifacts/improvements-20261025-final-escalated/ui-probe/summary.json)가 세부 근거다.

계정 transport와 실제 native Godot 장면도 통과했다. Alice 45개, Bob 41개 점검과 관리자 HTTP 응답 193개에 대한 private 값 부재 검사는 [계정 회귀 summary](../artifacts/improvements-20261025-final-escalated/account-regression/summary.json) 및 [Alice](../artifacts/improvements-20261025-final-escalated/account-regression/alice.json)·[Bob](../artifacts/improvements-20261025-final-escalated/account-regression/bob.json)에 있다. 이는 Godot Control signal 기반 실행으로, 물리적 OS 클릭을 뜻하지 않는다.

QA runner가 소유한 프로세스 8개는 모두 종료됐고 browser stop과 전용 profile 정리는 성공했다. 생성물 174개 파일의 관리자 비밀값 검사도 통과했다. 기존 서버 PID 69169, manager PID 68561, browser PID 69199 및 원본 port 3000은 재시작·정리 대상에 넣지 않았다. Runner의 **72개** 파일 선택 manifest는 실행 전후 aggregate `3edf2f7c6a857e274f52c989ce6af70a804dad30d813a7bee35dfb72bad1b017`로 같았다. 이 값은 위의 **84개 동결 manifest와 범위·계산법이 다르므로 서로 비교하지 않는다.** 서버와 계정 바이너리 SHA도 실행 전후 동일했다.

첫 기본 sandbox 실행은 loopback bind 전에 `Operation not permitted`로 끝났다. [실패 summary](../artifacts/improvements-20261025-final/summary.json)를 보존하고 권한 있는 독립 실행의 성공 결과만 제품 검증으로 계산했다. 이 실패를 제품 결함이나 통과로 세지 않는다.

## 패키지 검증과 CI 상태

메타데이터와 실제 MIT LICENSE 파일, 세 crate의 MSRV 1.96 선언, 내부 dependency version 0.1.0은 로컬에서 확인됐다. 첫 [package smoke summary](../artifacts/improvements-20261025-package/summary.json)는 `campfire-arena` archive 생성 중 미게시 `campfire-runtime`을 offline crates.io index에서 해석하려다 exit 101로 실패했다. 수정 후 첫 [재실행 summary](../artifacts/improvements-20261025-package-fixed/summary.json)도 같은 단계에서 실패했다. 이 실패를 성공 기록으로 덮어쓰지 않았다.

[최종 package smoke summary](../artifacts/improvements-20261025-package-fixed3/summary.json)는 **status: passed**다. runtime은 실제 `cargo package` 검증을 통과했고, arena/server는 archive 생성 후 추출본을 컴파일했다. 별도 checkout consumer와 archive consumer는 실행까지 통과했으며 server archive는 `cargo check --locked`를 통과했다. 세 package의 정규화 manifest에서 MIT, MSRV 1.96, 설명·repository·내부 버전을 검사했다. 이 smoke는 미게시 내부 crate에 임시 `[patch.crates-io]` source override를 사용하고 arena/server archive 생성 시 `--exclude-lockfile`을 적용한다. 앞선 [fixed2 통과 기록](../artifacts/improvements-20261025-package-fixed2/summary.json)과 최초 실패 기록도 보존한다.

| Archive | SHA-256 | 포함 파일 | `Cargo.lock` 포함 |
| --- | --- | ---: | --- |
| `campfire-runtime` | `67b549eb7ec6593fa479f4ca1872b73e233bc2bcfd44fe0f90b2b75cb1409636` | 20 | 예 |
| `campfire-arena` | `2b502137f528720236e2b1a428028bee846ed4f39406c73464f020abc532d4f6` | 5 | 아니요 |
| `campfire-server` | `885eee3a63763295b7b62747af732a8902916b79b15393f411d48a7d1054c4bd` | 20 | 아니요 |

후속 컴파일의 [checkout](../artifacts/improvements-20261025-package-fixed3/lockfiles/checkout.Cargo.lock), [archive](../artifacts/improvements-20261025-package-fixed3/lockfiles/archive.Cargo.lock), [server archive](../artifacts/improvements-20261025-package-fixed3/lockfiles/server-archive.Cargo.lock) 생성 lockfile과 각 SHA-256은 summary에 보존했다. 저장소 원본 `Cargo.lock` SHA-256은 전후 `9eac7d6c46e40c63abea7881d151ecebe0e975cbe3c977d7e46efb9c7fd61fe0`로 같다. arena/server archive에서 lockfile을 뺀 이유는 미게시 내부 crate 버전을 offline crates.io index에서 찾을 수 없기 때문이다. 따라서 이 통과는 **로컬 package 내용과 임시 source override를 사용한 소비·컴파일 검증**이다. lockfile이 없는 배포 archive의 의존성 재현성, `cargo install --locked`, crates.io 게시 및 공개 registry 다운로드는 검증하지 않았다. `--no-verify`로 archive를 만든 단계와 추출본을 후속 컴파일한 단계는 구분한다.

[빠른 CI](../.github/workflows/ci.yml)는 push/PR에서 fmt, clippy, Rust/Node/Python 테스트, no-default check, package smoke를 실행하도록 설정됐다. [Runtime validation](../.github/workflows/runtime-validation.yml)은 수동·주간 실행과 Godot/browser/account artifact 보존을 설정했다. 수동 runtime의 실제 성공과 첫 quick CI 실패는 다음 절에 따로 기록한다. 주간 예약 실행은 아직 성공 기록이 없으므로 ‘설정됨’으로만 표기한다.

## GitHub Actions 실제 실행과 후속 테스트 수정

첫 두 run의 `headSha`는 `45e87bf09e363aa147f63d3f8cd27aae6d9f24d8`이고 runner는 Ubuntu 24.04다. 원본 GitHub metadata·로그·artifact ZIP, API digest와 대조한 SHA-256 및 안전 추출본은 [증거 manifest](../artifacts/improvements-20261025-github/evidence-manifest.json)와 [독립 QA 보고](../.superpowers/sdd/2026-10-25-project-improvements/task-8-github-report.md)에 있다. 수정 후 quick run의 `headSha`는 `84af2abec020a722ac660fac479717dca17ff51f`다.

| Workflow | Run | 실제 결론 |
| --- | --- | --- |
| Runtime validation | [40158222582](https://github.com/sangjinsu/campfire/actions/runs/40158222582) | **success**. 준비·Godot·account·dashboard·artifact 업로드 단계 성공. |
| Push quick CI | [40158205928](https://github.com/sangjinsu/campfire/actions/runs/40158205928) | **failure**. fmt·clippy 성공, Rust tests 한 건 실패. 이후 no-default, Node, Python, package 단계는 skipped. |
| 수정 후 push quick CI | [40159325035](https://github.com/sangjinsu/campfire/actions/runs/40159325035) | **success**. Rust fmt·clippy·tests, no-default, Node·Python, package 및 독립 소비, artifact 업로드 단계 모두 성공. |

Runtime run은 Rust/Cargo 1.96.0, Node 24.22.1, Bun 1.4.3, Godot 4.7.2와 고정 gstack commit `e66c43c57209a17e2d9a79847a2e985c27ca80bd`를 사용했다. 별도 Godot 8명·16명은 각각 공개 상태 비교 19회로 성공했다. 계정 transport와 native Alice 45개·Bob 41개 점검, browser 15개·UI probe 20개, dashboard OFF/ON × 8/16명 네 부하 조합이 성공했다. 활성 dashboard polling은 8명 216회·16명 225회, 오류 0회였고 관리자 응답 237개의 private 값 비노출을 확인했다. 생성 프로세스·browser profile 정리, 소스·바이너리 전후 해시 및 비밀값 검사도 성공했다. 근거는 [Godot](../artifacts/improvements-20261025-github/runtime/extracted/godot/summary.json), [계정](../artifacts/improvements-20261025-github/runtime/extracted/accounts/summary.json), [Dashboard](../artifacts/improvements-20261025-github/runtime/extracted/dashboard/summary.json) 원본이다. Linux 실행 바이너리 SHA-256은 Mac 바이너리와 별도로 [QA 보고](../.superpowers/sdd/2026-10-25-project-improvements/task-8-github-report.md)에 있다. Mac의 203/210회 polling·193개 privacy 응답과 Linux의 216/225회·237개를 합산하지 않는다.

첫 quick CI의 [Rust 원본 로그](../artifacts/improvements-20261025-github/quick/extracted/rust-tests.log)는 `issuer_and_global_guest_rate_limits_recover`의 `headless.rs:1161`에서 기대 `429` 대신 실제 HTTP `201`을 기록한다. 이 run의 headless 결과는 17 passed, 1 failed, 4 ignored였으며 뒤 단계는 실행되지 않았다. 테스트는 guest bucket 10/s에서 매번 새 HTTP client로 보낸 세 요청이 100ms 이내라고 가정했다. 100ms마다 토큰 하나가 재충전되는 계약과 어긋난다. CI 로그에는 요청별 경과시간이 없어 CI에서 정확히 어느 구간이 100ms를 넘었는지는 단정할 수 없다. [수정 보고](../artifacts/improvements-20261025-ci-rate-fix/ci-rate-test-fix-report.md)는 제어된 실제 120ms 지연(측정 126.6ms)으로 같은 201/429 실패를 재현하고, Tokio paused clock과 blocking guard·공유 client·응답 body 소비를 사용해 virtual 0ms/99ms/100ms 경계를 검증했다. 이는 테스트의 시계 전제를 고친 것이며 제품 rate 설정을 바꾸지 않았다.

수정 commit `84af2abec020a722ac660fac479717dca17ff51f`는 `crates/campfire-server/tests/headless.rs`의 테스트와 helper 한 파일에 한정됐다. 로컬 [focused 로그](../artifacts/improvements-20261025-ci-rate-fix/campfire-rate-green.log)는 1개, [headless 로그](../artifacts/improvements-20261025-ci-rate-fix/campfire-rate-headless.log)는 18개 통과와 기존 ignored 4개 유지, [workspace 로그](../artifacts/improvements-20261025-ci-rate-fix/root-ci-fix-workspace-tests.log)는 독립 재실행 **141 passed/4 ignored**를 기록한다. fmt·clippy 결과 및 변경 파일 해시는 [수정 보고](../artifacts/improvements-20261025-ci-rate-fix/ci-rate-test-fix-report.md)와 [검증 JSON](../artifacts/improvements-20261025-ci-rate-fix/verification.json)에 있다. 수정 후 quick run의 [metadata](../artifacts/improvements-20261025-github/quick-recovery/run-metadata.json)는 `completed/success`와 모든 gate의 `success`를, [artifact API 기록](../artifacts/improvements-20261025-github/quick-recovery/artifacts-api.json)은 commit·artifact digest를 보존한다. 첫 실패 run의 결론은 그대로 failure다.

성공한 Linux runtime run은 테스트 수정 전 commit `45e87bf`에서 실행됐다. [후속 소스 동등성 확인](../artifacts/improvements-20261025-ci-rate-fix/source-equivalence-84af2ab.json)은 현재 `84af2ab`과 비교해 변경 경로가 headless 통합 테스트 한 개이며, live runner manifest 72개 중 달라진 것도 그 파일 하나임을 기록한다. 제품 Rust, manifest/lock, web asset, Godot, QA runner 및 workflow는 같으므로 기존 Linux runtime 실행을 변경되지 않은 제품 입력에 대한 근거로 사용할 수 있다는 독립 검토 판정이 있다. **Runtime 성공과 새 quick 성공을 동일 commit의 한 번 실행으로 표시하지 않는다.**

후속 빌드에서 별도 target의 서버 바이너리 SHA-256은 `780bafe9477df946ab9328ac1f6be24511554c7840f930ca6a29553ddafd0319`로, 이전 Mac live 실행의 `68f9f31b...`과 다르다. [빌드 확인](../artifacts/improvements-20261025-ci-rate-fix/final-build-verification.json)은 재링크 이후 값을 기록하지만 바이트 차이의 원인은 확정하지 않았다. 이전 Mac live suite가 이 새 바이너리를 실행했다고 주장하지 않는다. 제품 소스가 같은 Linux runtime CI 성공과 최초 Mac suite는 각 run의 source·binary 해시에 묶어 해석한다. 원본 checkout의 `target/debug/campfire-server` SHA는 변하지 않았다.

## 적용 범위

이 결과는 해당 Mac의 loopback HTTP, 해당 Godot/browser와 약 20초 live client 실행 및 별도 60초 Rust soak의 회귀 근거다. WAN, HTTPS/TLS 배포, 대규모·장기 운영 성능은 측정하지 않았다. `shutdown_with_timeout`은 scheduler가 진행할 때 호출자의 대기와 미완료 수를 보고하며, 동기 `GameModule` callback을 강제로 중단하지 않는다. 공개 registry에서 내부 crate가 보이는지와 다른 머신의 설치 성공도 아직 확인하지 않았다.
