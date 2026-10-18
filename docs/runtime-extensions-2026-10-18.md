# 계정·단발 명령·전용 상태 검증 — 2026-10-18

이번 변경의 계정 인증 연결, token rotation, FIFO 단발 명령, 수신자 전용 상태를 Rust 단위·실제 HTTP/WebSocket 테스트로 검증했다. 기존 Godot 게스트 클라이언트도 실제 8명·16명과 native UI에서 회귀 검증했다. 아래 실행 검사는 모두 통과했고 최종 Architect의 Spec·Quality·Security 판정은 **APPROVED**다. Critical/Important finding은 0개다. 검증된 branch의 main fast-forward 및 normal push도 성공했으며 아래 Git 전달 기록과 원격 이력으로 확인할 수 있다.

## 대상과 증거

- 명령·전용 상태 checkpoint: `93d51a7af37cf8e43660df98204f9df02f5f8989`.
- 계정·rotation 코어 checkpoint: `63df83dc4218d5558731688e3bb6174b9a538a2a`.
- 8명·16명 계정 테스트만 추가한 checkpoint: `8b93dfab18ac6e0364a8ad0a9176f43850e95890`. 이후 production·test·tool 소스는 변경하지 않았다.
- [기계 판독 결과](../artifacts/runtime-extensions-20261018-01/results.json), [Godot 전체 summary](../artifacts/runtime-extensions-20261018-01/godot/summary.json), [최종 소스 hash](../artifacts/runtime-extensions-20261018-01/source-before.txt).

최종 fast 검사 전후 source hash는 `17115071638ecc64a45aed22f50594c70a076a5af85b2a37902a6a37a0fb4f5c`로 같고, Godot launcher의 실행 전후 guard도 같은 값을 확인했다. macOS arm64, Rust `1.96.1`, Godot `4.7.2.stable.official.ed1daf0bf`, TCP loopback 환경이다. Godot 실행 시작은 **2026-10-18 21:49:55 KST**다.

## Rust 기능 및 회귀 검사

최신 동결 파일을 대상으로 coordinator가 다음 명령을 직렬 실행해 모두 exit 0을 확인했다. 표의 구성별 개수는 같은 테스트의 반복 실행을 포함하므로 합산해 고유 테스트 수로 표현하지 않는다.

| 명령 | 결과 | 원본 |
|---|---|---|
| `cargo fmt --all --check` | 통과 | [fmt.log](../artifacts/runtime-extensions-20261018-01/fmt.log) |
| `cargo clippy --workspace --all-targets --all-features --offline -- -D warnings` | 통과 | [clippy.log](../artifacts/runtime-extensions-20261018-01/clippy.log) |
| `cargo test --workspace --offline` | 79 passed, 0 failed, 4 ignored | [workspace.log](../artifacts/runtime-extensions-20261018-01/workspace.log) |
| `cargo test --workspace --no-default-features --offline` | 61 passed, 0 failed | [no-default.log](../artifacts/runtime-extensions-20261018-01/no-default.log) |
| `cargo test -p campfire-runtime --features serde_json/preserve_order --offline` | 39 passed, 0 failed | [preserve-order.log](../artifacts/runtime-extensions-20261018-01/preserve-order.log) |

기본 구성 79개는 arena 7, runtime 37, invariant 2, server 3, extension 12, headless 18개다. `--no-default-features` 구성에서는 campfire-server의 arena 기능에 의존하는 headless 18개는 제외되지만, workspace member campfire-arena의 7개는 여전히 실행되어 총 61개다. 이전 77/59개 결과는 8명·16명 계정 테스트 두 개를 추가하기 전 실행이며 [before-scale/](../artifacts/runtime-extensions-20261018-01/before-scale/)에 보존했다.

새 기능의 실제 HTTP/WebSocket [extension 테스트 12개](../crates/campfire-server/tests/extensions.rs)는 다음 경계를 확인한다.

- verifier 비활성·실패·만료·timeout, 클라이언트가 identity를 지정하는 요청 거부, 검증 대기 중 health/게스트 발급 진행, permit 회수.
- `(game_id, issuer, subject)` 계정 범위, 교차 게임 token 거부, 동시 로그인에서 마지막 token만 유효, rotation 뒤 동일 player·room 보존과 이전 socket 폐기.
- 단발 명령의 FIFO·정확한 한 번 적용, 성공/실패 terminal 결과, 중복 replay, payload 충돌, sequence gap, cache eviction, request cache와 stale instance 경계.
- 초기·주기·resume 상태와 명령 결과에서 수신자 자신의 secret만 수신. 8명 및 16명이 실제로 같은 방에 들어가 각각 명령 3개를 보내고 전원이 동일한 공용 상태 24/48개 및 자신의 ack=3을 확인했다. 처음/마지막 플레이어 두 명은 재인증·resume 뒤 동일 player, 공용 상태, 개인 상태 및 결과 ledger 3개를 확인하고 중복 재시도가 재실행되지 않음을 검증했다.

실제 actor·paused clock 단위 검사는 rotation 준비 취소 시 기존 identity/deadline 보존, commit 전 만료·shutdown 경계, actor Drop 후 늦은 commit 거부, 이전 token TTL 뒤 승인된 rotation의 ledger 보존, old generation fencing도 확인한다. 이 테스트는 외부 IdP를 사용하지 않는다.

## 실제 TCP 장기 검사

별도 QA가 다음 명령을 한 번 실행했다. **4 passed / 0 failed, 168.67초, exit 0**이며 기본 fast 검사에서 ignored된 네 개를 여기서 실제 실행했다.

```sh
cargo test -p campfire-server --test headless --offline -- --ignored --test-threads=1 --nocapture
```

[원본 ignored.log](../artifacts/runtime-extensions-20261018-01/ignored.log)에 기본 30초 grace 만료, 느린 reader 격리, 16명/8명 각각 60초 결과가 있다.

| 시나리오 | 실제 시간 | client별 snapshot 간격 p95 최대 | 원본 |
|---|---:|---:|---|
| 16명 soak | 60.007605초 | 68.411125ms | [soak-16.json](../artifacts/runtime-extensions-20261018-01/soak-16.json) |
| 8명 soak | 60.003924초 | 69.124542ms | [soak-8.json](../artifacts/runtime-extensions-20261018-01/soak-8.json) |

[느린 reader 원본](../artifacts/runtime-extensions-20261018-01/slow-reader.json)은 정상 client 16명 baseline 뒤 느린 연결 하나를 격리하고 정상 15명에서 실패 0개를 확인했다. p95 비율은 `0.482266`으로 기준 `<=2`를 통과했고 write deadline disconnect 1회였다. 이는 같은 fixture의 통과 기준이며 성능 향상 수치로 일반화하지 않는다.

장기 검사는 core commit `63df83` 시점에 시작했고, 실행 중 다른 worker가 별도 target에서 extension 테스트 두 개를 추가했다. production core와 이 장기 테스트 소스는 바뀌지 않았다. 장기 JSON의 source hash는 측정 당시 읽은 파일 집합의 값으로 slow-reader는 `9a6003ba…`, soak은 `e2a2df97…`이며 최종 동결 파일 집합 hash와 구분한다. 테스트가 legacy 경로에 출력한 세 JSON은 이번 폴더로 원본 복사한 뒤 기존 `artifacts/{slow-reader,soak-16,soak-8}.json`을 HEAD bytes로 복원했다. 기존 증거는 변경하지 않았다.

## 실제 Godot 게스트 회귀

이 검증은 새 계정 UI나 Godot에서의 단발 명령 기능 구현을 뜻하지 않는다. 기존 production `CampfireClient`와 UI를 변경하지 않고 새 runtime의 게스트 호환성을 검증했다.

```sh
python3 -u tools/validate_godot.py --godot /opt/homebrew/bin/godot --counts 8 16 --duration 20 --output artifacts/runtime-extensions-20261018-01/godot
```

launcher가 `cargo build -p campfire-server --locked --offline`을 직렬 수행하고 별도 `127.0.0.1:57540` 서버를 health 응답으로 확인했다. launcher가 서버 시작 전에 읽은 binary SHA-256은 `c1a4e44279278503cda868d7e9e0e12912482742628e06e8a2cb61b45030ea7b`다. 검증 뒤 디스크 binary도 같은 값이었고 추가 Cargo 명령을 실행하지 않았다. 실행 프로세스 메모리 hash를 측정한 것은 아니다.

| 시나리오 | 실제 측정 | 전원 동일 snapshot 공용 상태 | 최종 입력 처리 ack | snapshot 간격 p95 최대 |
|---|---:|---:|---:|---:|
| [Godot 8개](../artifacts/runtime-extensions-20261018-01/godot/clients-8/result.json) | 20.627837초 | 19개 일치 | 384–410 | 73ms |
| [Godot 16개](../artifacts/runtime-extensions-20261018-01/godot/clients-16/result.json) | 20.635983초 | 19개 일치 | 375–435 | 72ms |
| [macOS native UI](../artifacts/runtime-extensions-20261018-01/godot/integration.json) | 4.995초 | 28개 검증 통과 | 예상하지 않은 오류 0개 | — |

실제 Godot 프로세스가 guest 발급·인증·방 생성/목록/입장, 자신의 위치와 ack 진행, 원격 peer 이동, 한 peer 단절 중 위치 정지와 동일 guest/room/instance 복귀를 확인했다. Native UI는 실제 scene 버튼과 Godot input event로 퇴장/선택/입장/이동, 요청 중 버튼 잠금, 예약 만료 후 화면 폐기, cross-game 거부 뒤 자격 보존, logout 화면 초기화를 검증했다. 검증용 guest burst/rate는 100, grace는 2초로 명시적으로 설정했고 제품 기본값을 바꾸지 않았다.

[실제 renderer 캡처](../artifacts/runtime-extensions-20261018-01/godot/ui-live.png)를 직접 열어 한글·참가자 2/16·코트·입력 처리 ack 18·tick 42·snapshot 21을 확인했다. Launcher의 전체 runtime/parse 오류 및 credential 로그 검사도 통과했다.

## 호스트 연동 예제의 검증 범위

통합 담당 worker의 functions 실행 관측 보고는 `account_server`의 health HTTP 200, 계정 교환 HTTP 201, 서로 다른 opaque player ID, graceful 종료 0을 확인했다. 별도 원본 파일은 전달되지 않아 독립 raw-log 근거로 세지 않는다. 이 예제의 환경 변수 credential verifier는 개발용이며 외부 IdP 실연동 검증은 수행하지 않았다.

문서 담당 worker는 README/reuse의 `HostAuthenticator` Rust block을 `/private/tmp/campfire-docs-check`의 library로 추출하고 `CARGO_TARGET_DIR=/private/tmp/campfire-docs-target cargo check --offline`을 실행해 exit 0·경고 0을 관측했다. 별도 artifact log가 없는 worker 관측 보고다. 공개 snippet은 이후 코어의 private transaction 변경으로 바뀌지 않았다. 실제 인증 서비스·DB·운영 verifier는 호스트의 책임이다.

## 보존 및 최종 검토 상태

이번 실행은 기존 플레이 서버나 창을 stop/kill/restart하는 명령을 사용하지 않았다. 완료 뒤 원래 서버 PID `63797`과 Godot PID `65028` 및 3000 포트 health HTTP 200을 확인했다. 이전 두 번째 Godot PID `25278`은 완료 시점 조회에서 보이지 않았고 coordinator도 원래 GUI `65028`만 관측했다. 종료 원인은 추정하지 않았으며 두 창이 계속 살아 있다고 기록하지 않는다.

최종 Architect는 원본 fast/장기/Godot 결과, 디스크 binary hash, source provenance와 문서의 no-default 설명 정정을 직접 검토해 **APPROVED**했다. Spec·Quality·Security 모두 승인됐고 미해결 Critical/Important finding은 0개다. 최종 문서·증거 커밋 `784e9ba9a2c92d71aae1e53914d02a8937d9f6b0`을 만든 뒤 원격 main이 기존 `b94c427` 그대로임을 fetch로 확인했다. 검증 branch와 완전히 같은 tree로 main을 fast-forward하고 `git push -u origin main`을 성공시켰다. 이어 `git ls-remote origin refs/heads/main`의 SHA가 로컬 HEAD·origin/main과 모두 `784e9ba`로 일치하고 upstream `origin/main`, working tree clean임을 확인했다. 이 문단은 그 실제 push 이후 기록하는 문서 상태 정합화이며 최신 문서 커밋은 [원격 main 이력](https://github.com/sangjinsu/campfire/commits/main/)에 표시된다. Godot summary의 `8b93dfa`는 실제 검증 시점의 소스 커밋이며 이후 문서·증거 커밋과 달라도 검증한 source bytes는 같다. 최종 Git 반영 결과는 이 보고서의 전달 기록과 저장소 원격 이력에서 확인한다.
