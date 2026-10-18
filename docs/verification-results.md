# Campfire 검증 결과

검증 환경: macOS/Darwin arm64, Rust 1.96.1, TCP loopback, 실제 monotonic clock. 소스 식별은 unborn Git 저장소의 HEAD 대신 Cargo.toml/Cargo.lock 및 crates의 Rust/manifest 파일을 정렬해 path+NUL+content SHA256으로 계산한다. 단위 테스트의 manual clock과 실제 TCP 시간 측정은 구분한다.

## 빠른 검증

root 독립 최종 pipeline에서 빠른 테스트 **44개**, formatting, workspace all-target/all-feature clippy `-D warnings`가 통과했다. headless 18개는 2.44초에 통과했고 arena 포함/제외 빌드도 통과했다. 실제 TCP headless는 18개이며, 임의 frame/입력 검증, 정원 경합, game_id 경계, 재접속 generation, admission 취소와 자원 상한·회수는 [검증 추적표](verification-coverage.md)에 연결했다.

- 실제 TCP: `cargo test -p campfire-server --test headless --offline -- --nocapture`
- 정적 확인: `cargo clippy -p campfire-server --test headless --offline -- -D warnings`
- 최초 sandbox EPERM은 localhost bind 권한 문제였으며 승인된 재실행으로 검증했다.
- 실행한 서버 바이너리의 `GET /health`는 정확히 `{"status":"ok"}`를 반환했고 같은 바이너리의 headless walkthrough도 통과했다.

반복 인증 표시 만료 시각의 +1ms drift를 stress로 재현하고 per-runtime Instant/Unix origin으로 수정했다. 수정 후 같은 source `99645f3b66237b825e6d1472f4c35bda20d816c3fcdd2659658aa380fa355c6d`에서 20/20 통과했다. [수정 전 요약](../artifacts/repeated-auth-before-fix.json), [대표 실패](../artifacts/repeated-auth-run-01.log), [수정 후 20회 요약](../artifacts/repeated-auth-stress.json)을 보존한다. 구현 중 이전 4초 응답 timeout은 최종 20회에서는 재현되지 않았다. 이 timeout의 원인을 표시 drift와 같다고 단정하지 않는다.

## 실제 장기 검증

root가 소스를 동결한 뒤 다음 명령을 단독 실행했다. 느린 reader fixture 수정 후 해당 테스트와 빠른 전체 검증을 별도로 다시 실행했다.

```sh
cargo test -p campfire-server --test headless --all-features --locked --offline -- --ignored --nocapture --test-threads=1
```

실행 로그: [long-run.log](../artifacts/long-run.log). 최초 전체 ignored 실행은 **3 통과 / 1 실패**였다. 실제 31초 grace 및 8명·16명 soak는 통과했고, 느린 reader fixture가 준비 중 정상 reader도 읽기를 멈춰 정상 연결을 종료시키는 문제가 드러났다. 서버 코드는 그대로 유지하고 입장 즉시 reader task를 시작해 모든 16명이 full room snapshot을 확인한 뒤 측정을 시작하도록 fixture를 수정했다. 256KiB state·100ms write deadline·8초 측정·p95 2배 합격 기준은 그대로다. baseline 종료 후 runtime을 명시적으로 shutdown해 두 실행의 부하가 겹치지 않게 했다.

| 실행 | 실제 시간 | 정상 client snapshot 간격 p95 최대 | 처리 입력 | 룸 snapshot 발행 | 입력 queue 깊이 최대 |
|---|---:|---:|---:|---:|---:|
| 8명 soak | 60.00435초 | 68.80833ms | 4,800 | 900 | 7 / 설정 1,024 |
| 16명 soak | 60.00999초 | 71.00604ms | 9,600 | 901 | 16 / 설정 1,024 |

[8명 원본 측정](../artifacts/soak-8.json), [16명 원본 측정](../artifacts/soak-16.json)은 빌드·설정·환경·플레이어별 입력/snapshot/간격 및 tick 지연·queue 지표를 담고 있다. 두 실행의 소스 식별자는 `99645f3b66237b825e6d1472f4c35bda20d816c3fcdd2659658aa380fa355c6d`이며 원본 값을 변경하지 않았다.

fixture 수정 후 느린 reader targeted 실행은 통과했다. 정상 16명 baseline에서 서버 종료 수 0, 읽기를 중단한 1명+정상 15명 실행에서 **write deadline 종료 수 정확히 1, 정상 연결 15개 유지**를 metrics로 확인했다. root 독립 최종 실행의 정상 p95는 baseline **87.799959ms** / 중단 시 **78.5505ms**로 **0.894653배**였다. p95 집계는 각 client 간격 p95의 95백분위수이며 15~16개 표본에서는 최대값이다. 원본 [slow-reader.json](../artifacts/slow-reader.json)에 양쪽 개별 측정을 보존한다. 최종 targeted 실행은 16.68초에 통과했고 전체 소스 식별자는 `3eb02c2f975952d962ab184e5bfb08d1ccf0ae7f1ec6b2c85a9f286fc3e63e0d`다. 측정 중 소스 변경은 없었다.

[production 소스 연속성](../artifacts/production-source-continuity.json)은 fixture 수정 전후 서버 production manifest/lock/src SHA256이 `08466360cfcdbb1771e48fd9e4c738717ad9333dbfb86b63b756d4f93344fa95`로 같고 `soak()` 함수 원문도 같음을 확인한다. 따라서 수정 전 통과한 8명/16명/31초 측정은 같은 production 동작의 유효한 근거로 유지한다. test 파일 변경 때문에 느린 reader artifact의 전체 소스 hash는 별도이며 두 hash를 같다고 표시하지 않는다.

## 해석 범위

이동 속도의 메시지 빈도 불변성은 deterministic 단위 검증으로 확인한다. Arena는 여러 입력을 받아도 tick 전 이동하지 않고 고정 dt 한 번만 계산하며, runtime은 tick 안의 최신 유효 입력 하나만 적용한다. 대각선 정규화도 같은 고정 tick에서 검증한다. 실제 TCP에서 동시에 시작했다고 가정한 거리 비교는 tick 정렬 오차를 포함하므로 이 증거와 구분한다.

느린 reader 테스트는 256KiB snapshot을 사용하는 신뢰 가능한 test module로 OS 소켓 buffer를 실제로 채운다. 모든 reader는 setup부터 계속 읽으며 측정 시작 뒤 한 reader만 멈춘다. 서버의 write deadline/reliable queue 종료 지표가 증가해야 성공이며 테스트가 먼저 소켓을 drop해 성공으로 판정하지 않는다. rate key 상한 테스트의 서로 다른 발급 주체는 trusted host ConnectInfo adapter로 모델링한다. 사용자 forwarded header는 신뢰하지 않는다.

이 결과는 해당 컴퓨터·loopback 설정의 회귀 기준이다. 실제 인터넷 지연, public deployment, 대규모 CCU 및 장시간 운영 품질을 검증한 결과가 아니다.
