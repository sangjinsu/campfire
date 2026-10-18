# Godot 클라이언트 독립 Architect 검토

최종 판정: **APPROVED**. 승인된 Godot 플레이 클라이언트 범위에서 미해결 blocking finding이 없다. 아래 발견의 수정, 독립 단위/scene 회귀, 소스가 고정된 8/16명 및 native UI 전체 실행 근거를 확인했다.

## 검토 범위

`CampfireClient`의 HTTP/WS 생명주기, 자격증명 origin/game 결합, full sync와 순서/ack 검증, UI 상태/입력 정지, 실제 Godot 프로세스 검증 도구와 기록을 승인된 설계에 대조했다. 검토자는 production source와 test를 수정하지 않았다. Rust baseline은 `5ffbbb1`이며 검토 시 `crates/`, `Cargo.toml`, `Cargo.lock`의 baseline diff는 없었다.

## 발견 및 수정 확인

1. **해결 확인:** 로그아웃 응답과 WebSocket close가 같은 poll에 도착하면 엔진이 수신 큐를 비워 `session.logged_out`를 놓친다. `artifacts/godot-final/integration.json`은 실제 실패를 보존한다. adapter는 전송한 logout 의도를 기록하고 연결 종료/timeout 시 로컬 자격과 방 정보를 폐기한다. 응답 수신과 로컬 폐기를 구분하는 주석·README·검증 문구를 확인했다. `artifacts/godot-ui-final-v13/integration.json`의 native logout 및 화면 정리 assertion이 통과한다.
2. **해결 확인:** 인증에서는 resume 가능하지만 요청 처리 전에 예약이 만료되는 경로에서 이전 코트/점수/식별자가 남던 문제는 `main.gd`의 `_on_error`가 lobby 및 빈 room 상태를 확인해 `_on_left`를 호출하도록 수정됐다. `RESUME_EXPIRED`, `ROOM_NOT_FOUND`, `STALE_INSTANCE` 3개 경로 회귀를 확인했다.
3. **해결 확인:** 일반 snapshot이 leave/logout pending action을 비우던 처리를 제거하고 room full sync, 대응 command 오류 및 종료에서만 잠금을 해제한다. 무관한 input 오류/list 응답은 잠금을 유지한다. 오류 emit 직전 설정되는 `last_error_command`를 통한 대응 관계와 단위/scene 회귀를 확인했다.

상태 안내는 scroll 밖으로 이동하여 최소 viewport에서도 보인다. 명령의 `GAME_MISMATCH`는 정상 guest 자격을 유지하고 인증의 game mismatch만 자격을 폐기하는 경계도 단위 및 실제 native UI 검증으로 확인했다.

## 확인한 구조와 근거의 한계

- HTTP timeout/body limit/redirect 금지와 callback epoch, WebSocket buffer/packet/pending 상한, 프레임별 bounded drain 및 입력 전송 상한이 명시되어 있다. token은 adapter 메모리에 있으며 pending metadata와 UI에 담지 않는다.
- 방 식별자는 game/room/instance를 함께 검증한다. 일반 snapshot은 증가하는 sequence와 후퇴하지 않는 tick/processed ack만 적용하며 resume는 full sync의 processed ack + 1에서 입력을 다시 시작한다. `game.input.accepted`는 처리 완료 ack로 사용하지 않는다.
- 실제 main scene은 서버 snapshot으로 위치를 표시하고, text focus와 application focus 상실 시 방향을 0으로 보낸다. native renderer의 실제 서버 화면을 직접 열어 한글, 참가자, 코트와 처리 ack 표시를 확인했다.
- Python 도구는 별도 Godot 프로세스를 각각 실행하며 모든 프로세스가 GUI와 동일한 adapter를 사용한다. 동일 snapshot sequence의 전체 public state와 server tick을 비교하고, 각 참가자의 위치 변화, 처리 ack 진행, 원격 단절 중 정지 및 복귀를 검증한다. client별 ack를 public state 비교에서 제외하는 것은 프로토콜에 부합한다.
- 앞선 8/16명 실행에는 각각 19개 공통 public state와 실제 공동 점수 0→1이 기록되어 있으나, 해당 전체 실행은 UI logout 실패로 성공하지 않았다. 이를 최종 통과로 간주하지 않는다.
- 검증 환경은 loopback, 20초 이동, 발급 burst/rate 100, 예약 2초다. 인터넷 지연/배포/대규모 CCU 또는 기본 30초 예약 경계의 실측을 주장하지 않는다.

## 최종 승인 근거

고정된 production/test source hash와 서버 binary hash를 포함한 8/16명+native UI 전체 실행 성공, fresh import/adapter unit/scene smoke, 위 발견의 회귀 및 Rust baseline 무변경을 확인했다. 실패 원본과 최종 성공 실행은 별도 경로로 유지한다.

중간 source commit `00ea582`에서 Architect가 직접 `client_unit.gd`와 `ui_smoke.gd`를 macOS 실행 환경으로 다시 실행했다. 각각 `PASS: CampfireClient unit invariants`, `UI_SMOKE_PASS`와 종료 코드 0을 확인했고 engine 오류는 없었다. 직접 계산한 source hash는 `b48d63f111d9e499f80bc53f1040b0e4c84d3fa14891eac6edbb63c85c96edf9`이며 v1.3 native UI 보고서와 일치한다. `git diff --exit-code 5ffbbb1 -- crates Cargo.toml Cargo.lock`도 변경 없이 통과했다.

Root가 실제 macOS 환경에서 수행한 [최종 전체 실행](../artifacts/godot-independent/summary.json)의 `success:true`를 Architect가 원본 JSON, 개별 client 보고서, 실제 screenshot 및 로그와 독립 대조했다. source hash를 다시 계산하고 실행 binary hash `a8cbcc9964b1be2b009a42edfe77267a3161480b2ae6bde0f948ba8223359081`도 실제 파일과 비교했다. 검증 checkpoint는 `00ea582b65df73e4c2d8502095f1934c12ebba28`이다.

| 검증 | 최종 근거 |
| --- | --- |
| 실제 Godot 8개 | 20.620860초, 공통 public state 20개, 최소 처리 ack 386, 전원 이동·정원·순서 assertion 통과 |
| 실제 Godot 16개 | 20.637036초, 공통 public state 19개, 최소 처리 ack 379, 전원 이동·정원·순서 assertion 통과 |
| 재접속 | 두 정원 모두 동일 guest/room/instance resume, 나머지 전원의 단절/복귀 관측 및 예약 중 위치 정지 |
| 공동 점수 | 두 정원 모두 공통 state에서 0→1 확인 |
| Native UI | macOS renderer, 28개 assertion, 4.611초, 실패 및 예상외 오류 0 |
| 실행 로그 | 서버/클라이언트/UI 26개 로그의 runtime/parse 오류 및 token 출력 패턴 재검사 통과 |
| 그래픽 | 최종 실서버 PNG와 900×600 PNG 직접 확인, 한글·코트·상태 안내 표시 |
| Import와 서버 회귀 | clean import 로그 확인; Rust source/Cargo baseline 무변경. Root의 fmt/clippy 및 빠른 테스트 44개 결과는 별도 검증 문서에 기록 |

Native UI assertion은 Godot에 주입한 실제 `InputEventKey`/`ui_accept`가 scene의 입력 경로와 서버 이동에 연결되는 것을 검증한다. 물리 키보드를 사람이 조작한 플레이 세션을 주장하지 않는다. 8/16개 프로세스는 같은 기기의 loopback 테스트이며 장시간 soak 또는 공용 인터넷 서비스 부하 검증으로 확대 해석하지 않는다.
