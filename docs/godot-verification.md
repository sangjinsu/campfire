# Godot 클라이언트 검증 기록

환경: macOS arm64, Godot `4.7.2.stable.official.ed1daf0bf`, Rust 1.96.1, 실제 TCP loopback. GUI와 bot 모두 `examples/godot/scripts/campfire_client.gd`를 사용한다. Python은 실제 서버와 Godot 프로세스를 시작하고 결과를 검사하는 표준 라이브러리 launcher이며 WebSocket client를 대체하지 않는다.

## 재현 명령과 판정 기준

```sh
python3 tools/validate_godot.py --godot /opt/homebrew/bin/godot --counts 8 16 --duration 20
```

launcher는 서버를 빌드한 뒤 임시 포트의 `/health`가 실제 HTTP 200 및 `{"status":"ok"}`인지 확인한다. Godot 프로세스를 각각 8개/16개 띄워 HTTP guest 발급 → WebSocket 인증 → 공개 room 생성 → 실제 room.list 검색 → 같은 room instance 입장을 실행한다. 전원이 준비됐다는 파일 barrier 뒤 20초 동안 각 bot이 방향을 보내고 실제 서버 snapshot을 받는다. 종료 및 예외 시 모든 자식 프로세스를 정리한다.

각 client는 처리 ack 진행, 서버 tick/snapshot seq 단조 증가, 자기 위치 변화와 전체 인원 도달을 검증한다. public state 비교는 **같은 snapshot_seq**의 전체 state를 client 전원에서 정확히 비교한다. 수신자마다 다른 `last_processed_input_seq`는 public state hash에서 제외한다. 이 공통 state에서도 모든 참가자의 원격 위치 변화를 확인한다. 서로 다른 tick에서 읽은 최종 위치를 단순 동등 비교하지 않는다. 관측 state는 최대 120개, 원격 단절 위치는 최대 64개로 제한한다.

마지막 client는 측정 중 연결을 강제로 끊고 최소 700ms 뒤 같은 token으로 재인증/자동 resume한다. guest/room/instance 동일성, 기존 처리 ack·tick·seq 미후퇴를 검증한다. 나머지 모든 client가 해당 참가자의 `connected:false` → `true`를 관측해야 하며 예약 중 위치가 정지했는지도 검사한다. 입력 수신 수락 응답만으로 이동 완료를 판정하지 않는다.

통합 UI는 실제 scene과 native Button focus/`ui_accept` 이벤트를 사용해 연결·생성·목록·선택·입장·퇴장·단절·재접속을 수행한다. 실제 `InputEventKey D`를 주입하고 처리 ack 및 서버 위치 이동을 확인한다. renderer의 `frame_post_draw` 뒤 viewport PNG를 저장한다. 별도 `arena-b` adapter로 목록 격리와 원래 게임 room 입장 거부를 확인한다. 만료 후 이전 화면 정리와 로그아웃 token/화면 정리도 회귀 범위다.

검증용 서버 설정은 `--guest-burst 100 --guest-rate-per-second 100 --reconnect-grace-ms 2000`이다. 기본 발급 burst 10 때문에 16개 프로세스 시작이 차단되지 않게 하고 기본 30초 예약 만료를 2초로 줄인다. 사용자 서버 기본 설정을 바꾸지 않는다. 실제 기본 30초 경계는 기존 Rust [검증 보고서](verification-results.md)의 독립 근거로 유지한다.

`summary.json`에는 Godot/Rust 버전, Git commit/status, 서버 실행 명령, production/test 전체 source SHA-256, 실행 binary SHA-256, 정확한 invocation을 기록한다. 실행 중 source가 변하거나 bot 보고서 누락·engine parse/runtime 오류·token 출력이 발견되면 전체 성공을 거부한다. 모든 결과는 해당 loopback 환경의 검증이며 public deployment나 인터넷 지연, 대규모 CCU 성능을 의미하지 않는다.

## 수행 기록

- [초기 8명 smoke](../artifacts/godot-smoke/summary.json): 실제 Godot 8명 15.64초의 기능 assertion과 14개 공통 public state 비교는 통과했다. 동시에 network adapter 수정이 있어 source hash guard가 전체 실행을 실패로 표시했다.
- [첫 8/16 실행](../artifacts/godot-validation/summary.json): 8명 20.61초·19개 공통 state, 16명 20.66초·20개 공통 state의 이동/처리 ack/원격 정지/동일 guest resume assertion은 통과했다. UI fixture가 이전 목록 row를 새 목록 응답으로 너무 일찍 읽어 전체 실행은 실패했다. target room row가 나타날 때까지 기다리도록 fixture를 수정했다.
- [수정 후 UI 검증](../artifacts/godot-ui-debug/integration.json): native UI 및 실제 렌더러, 예약 만료, 다른 게임 격리 등 20개 검증이 통과했다. [실제 서버 화면](../artifacts/godot-ui-debug/ui-live.png)은 두 참가자와 서버 처리 ack를 담는다.
- [로그아웃 회귀 발견](../artifacts/godot-ui-final/integration.json): 예약 만료 후 stale 화면 정리까지 22개 검증은 통과했으나 실제 서버 로그아웃에서 WebSocket close와 마지막 응답이 같은 poll에 도착해 token 정리가 완료되지 않는 경계를 드러냈다. 실패 원본을 유지한다. 공식 엔진 소스에서 CLOSED 전환 시 수신 큐가 지워지는 계약을 확인했다. 사용자 로그아웃 의도 이후 연결이 닫히면 로컬 자격을 폐기하도록 adapter를 수정했고 일반 단절은 자격을 유지한다. [수정 후 native UI 24개 검증](../artifacts/godot-ui-logout-fixed/integration.json)이 통과했다. 이 확인은 로컬 자격 폐기이며 마지막 서버 응답 수신을 주장하지 않는다. 서버의 실제 token 폐기는 Rust `token_expiry_and_logout` 통합 테스트의 별도 근거다. 최종 소스 동결 후 독립 실행 결과를 아래에 기록한다.

UI 단위 회귀 RED/GREEN 및 크기별 실제 렌더러 검증은 [UI 보조 검증 기록](godot-client-verification.md)에 있다. 기존 Rust 8/16명 60초 및 느린 수신자 측정 원본은 변경하지 않았다.

## 최종 검증

최종 adapter v1.3/UI 동결 후 `python3 tools/validate_godot.py --ui-only --output artifacts/godot-ui-final-v13`가 종료 코드 0으로 통과했다. [28개 native UI 검증](../artifacts/godot-ui-final-v13/integration.json)은 4.657초에 완료됐으며 source 변경 guard와 runtime/parse/token 출력 검사도 통과했다. 여기에는 실제 leave/logout 요청의 응답 적용 전 버튼 잠금, cross-game join 오류 뒤 정상 guest 자격 보존과 후속 목록 성공이 포함된다. [실제 최종 UI 화면](../artifacts/godot-ui-final-v13/ui-live.png)은 두 참가자와 서버 입력 처리 ack를 표시한다.

이 실행의 전체 source SHA-256은 `b48d63f111d9e499f80bc53f1040b0e4c84d3fa14891eac6edbb63c85c96edf9`, 서버 binary SHA-256은 `a8cbcc9964b1be2b009a42edfe77267a3161480b2ae6bde0f948ba8223359081`이다. [summary](../artifacts/godot-ui-final-v13/summary.json)에 정확한 명령과 모든 버전/설정을 보존한다.

Root는 source checkpoint `00ea582b65df73e4c2d8502095f1934c12ebba28`에서 다음 전체 명령을 독립 실행했다. [최종 summary](../artifacts/godot-independent/summary.json)는 종료 코드 0 및 `success:true`이며 앞의 동일 source/binary hash를 기록한다.

```sh
python3 tools/validate_godot.py --godot /opt/homebrew/bin/godot --counts 8 16 --duration 20 --output artifacts/godot-independent
```

| 실제 Godot 프로세스 | 측정 시간 | 전원 공통 state 비교 | client별 최종 처리 ack 범위 | snapshot 간격 p95 최대 |
|---|---:|---:|---:|---:|
| [8명](../artifacts/godot-independent/clients-8/result.json) | 20.620860초 | 20개 | 386–424 | 69ms |
| [16명](../artifacts/godot-independent/clients-16/result.json) | 20.637036초 | 19개 | 379–430 | 73ms |

두 경우 모두 정확히 같은 snapshot_seq에서 전원의 public state·server_tick이 같았고 모든 플레이어의 위치 변화와 공동 점수 0→1을 관측했다. 마지막 client는 같은 guest/room/instance로 resume했으며 나머지 전원이 원격 단절·예약 중 정지·복귀를 확인했다. p95는 각 client의 전체 측정 간격을 집계한 최대값이며 의도한 700ms 단절의 긴 간격도 원본 max 지표에 보존한다.

[최종 native UI](../artifacts/godot-independent/integration.json)는 macOS 실제 renderer에서 28개 검증을 4.611초에 통과했고 예상하지 않은 오류는 0개였다. 예상된 `GAME_MISMATCH` 뒤 자격을 유지하고 후속 목록 조회도 성공했다. [최종 live screenshot](../artifacts/godot-independent/ui-live.png), client별 JSON/log와 실패 원본을 함께 보존한다. [최종 독립 Architect 검토](godot-architect-review.md)는 **APPROVED**이며 미해결 blocking finding이 없다. 검토자는 최종 source/binary hash와 원본 결과를 대조하고 adapter unit·scene smoke를 직접 재실행했다. Root는 실행 뒤 해당 테스트 서버와 Godot bot 프로세스가 남지 않았음도 확인했다.

### Root 독립 정적·회귀 확인

최종 source checkpoint `00ea582` 직전 동결한 같은 소스에서 root는 [Godot clean import](../artifacts/godot/independent/import.log), [client unit](../artifacts/godot/independent/client-unit.log)의 `PASS: CampfireClient unit invariants`, [scene smoke](../artifacts/godot/independent/ui-smoke.log)의 `UI_SMOKE_PASS`를 독립 실행해 모두 종료 코드 0을 확인했다. 900×600 및 native live UI PNG도 직접 열어 검사했다.

Rust 소스는 이번 Godot 추가에서 변경하지 않았다. root가 새로 실행한 `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`가 모두 종료 코드 0으로 통과했다. 빠른 테스트 44개가 통과했고 기존 장시간 테스트 4개는 ignored였다. 기존 실제 8/16명 60초 및 느린 reader 측정은 Rust baseline의 원본 근거로 유지하며 이번에 다시 실행했다고 표시하지 않는다.
