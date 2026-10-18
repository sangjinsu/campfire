# Godot 자동 테스트 — 2026-10-18

2026-10-18 **20:34:30 KST**에 사용자의 자동 테스트 요청으로 실행을 시작했다. 이번 새 실행에서 실제 Godot 8명·16명, native UI 28개 검증 및 Rust/Godot 회귀 확인이 통과했다. 기존 플레이 서버와 Godot 창은 유지했다. Production·test·tool 소스는 수정하지 않았다.

## 대상과 명령

검증 commit은 `874c068a0412409107ad9616788737e85c4ed0bf`다. Godot `4.7.2.stable.official.ed1daf0bf`, Rust `1.96.1`, macOS arm64 loopback 환경에서 같은 `CampfireClient` adapter를 사용하는 별도 Godot 프로세스를 실행했다.

```sh
python3 tools/validate_godot.py --godot /opt/homebrew/bin/godot --counts 8 16 --duration 20 --output artifacts/godot-autotest-20261018-01
```

[이번 전체 실행 summary](../artifacts/godot-autotest-20261018-01/summary.json)는 종료 코드 0 및 `success:true`다. Source SHA-256은 `b48d63f111d9e499f80bc53f1040b0e4c84d3fa14891eac6edbb63c85c96edf9`, launcher가 서버 시작 전에 기록한 binary SHA-256은 `a8cbcc9964b1be2b009a42edfe77267a3161480b2ae6bde0f948ba8223359081`이며 이전 승인된 검증 소스와 같다. 실행 도중 source가 변하지 않았다는 guard도 통과했다. Binary hash는 디스크 파일의 측정값이며 실행 중 프로세스의 메모리 hash를 직접 측정한 값은 아니다.

멀티플레이/UI 검증은 기존 3000 포트와 분리한 임시 서버 `127.0.0.1:51456`을 사용했다. 검증용 게스트 발급 burst/rate는 100, 재접속 예약은 2초다. 사용자용 기본 설정을 변경하지 않았다.

## 실제 Godot 실행 결과

| 검증 | 결과 | 실제 측정 | 전원 공통 public state | 최종 처리 ack 범위 | client별 snapshot 간격 p95 최대 |
|---|---|---:|---:|---:|---:|
| [Godot 8개](../artifacts/godot-autotest-20261018-01/clients-8/result.json) | 통과 | 20.886777초 | 19개 | 382–413 | 80ms |
| [Godot 16개](../artifacts/godot-autotest-20261018-01/clients-16/result.json) | 통과 | 20.654712초 | 19개 | 377–445 | 81ms |
| [실제 native UI](../artifacts/godot-autotest-20261018-01/integration.json) | 28개 통과 | 4.620초 | macOS 실제 renderer | 예상하지 않은 오류 0개 | — |

각 bot은 실제 guest 발급·인증·생성·목록·입장 후 서버가 계산한 위치와 처리 ack 진행을 확인했다. 정확히 같은 snapshot_seq에서 전원의 public state와 server_tick을 비교했고 모든 플레이어의 원격 위치 변화를 확인했다. 마지막 bot을 끊은 뒤 같은 guest/room/instance로 resume했으며 나머지 전원이 예약 중 위치 정지와 복귀를 관측했다.

Native UI는 실제 scene의 버튼과 Godot에 주입한 키 이벤트로 생성·목록·선택·입장·퇴장·이동·재접속·로그아웃을 검사했다. 요청 처리 중 버튼 잠금, 예약 만료 뒤 화면 초기화, 다른 game의 방 숨김/입장 거부와 거부 뒤 정상 자격 유지·목록 재조회도 통과했다. 로그아웃 검증은 로컬 자격/화면 폐기를 확인하며 마지막 서버 응답 수신을 뜻하지 않는다.

[실제 서버 UI 캡처](../artifacts/godot-autotest-20261018-01/ui-live.png)를 직접 열어 두 참가자, 한글, 코트와 서버 처리 ack 표시를 확인했다. 이 이미지는 headless DummyRenderer 결과가 아니다. 전체 launcher의 runtime/parse 오류와 token 출력 검사도 통과했다.

## 별도 Rust/Godot 회귀 확인

동시에 수행한 [회귀 summary](../artifacts/godot-autotest-checks-20261018-01/summary.json)는 **20:35:52 KST** 기준 `success:true`이며 다음 6개 명령이 모두 종료 코드 0이다. 정확한 redirection·절대 log 경로와 각 결과는 [commands.json](../artifacts/godot-autotest-checks-20261018-01/commands.json)에 보존한다.

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace` — 44개 통과, 실패 0개, 장시간 검증 4개 ignored.
- Godot `tests/client_unit.gd` — adapter 단위 검증 통과.
- Godot `tests/ui_smoke.gd` — 실제 scene smoke 검증 통과.
- Godot `tests/client_unit_transport.gd` — 실제 서버의 logout/close 및 일반 단절 자격 보존 검증 통과. 이 검증은 유지 중인 `http://127.0.0.1:3000`에 별도 guest로 연결했다.

최초 Godot 3개 호출에서는 상대 `--log-file` 경로가 프로젝트 경로를 기준으로 해석되어 log 파일 열기 오류가 출력됐다. 해당 호출도 테스트 assertion은 통과했지만 이 실행을 오류 없는 최종 근거로 삼지 않았다. `*-attempt1-relative-logpath.log`로 원본을 보존하고, 절대 log 경로로 Godot 3개를 다시 실행해 최종 로그 오류 0개를 확인했다. 이는 호출 경로 수정이며 제품 소스를 수정한 회귀 대응이 아니다.

## 병렬 Cargo 산출물 식별 경계

최종 검토 중 병렬 Cargo 검사 뒤 디스크의 `target/debug/campfire-server` hash가 전체 실행 summary의 `a8cb…`와 다른 `5bd418812f1510bbb411a316ff6e6d2ad0194558cbc0e657ac81cdc89fc90112`임을 발견했다. 다른 Cargo 작업이 끝난 뒤 **20:36:52 KST**에 다음 normal build를 정확히 한 번 직렬 실행했다.

```sh
cargo build -p campfire-server --locked --offline
```

[전후 hash 기록](../artifacts/godot-autotest-20261018-01/normal-build-recheck.json)과 [빌드 원본 로그](../artifacts/godot-autotest-20261018-01/normal-build-recheck.log)에 종료 코드 0, source 변경 없음, 빌드 후 hash가 `a8cbcc9964b1be2b009a42edfe77267a3161480b2ae6bde0f948ba8223359081`로 전체 실행 기록과 정확히 일치함을 보존했다. 병렬 검사 후의 디스크 파일과 전체 실행이 기록한 산출물을 구분하며, source 변경이나 제품 수정으로 해석하지 않는다. 이 확인을 위해 테스트를 반복하거나 서버를 재시작하지 않았다.

## 플레이 세션 보존과 범위

실행 전후 기존 서버 PID `63797`과 Godot PID `65028`이 유지됨을 확인했다. 기존 `127.0.0.1:3000/health`는 `{"status":"ok"}`를 반환했고 이번 임시 서버 51456 포트는 종료 뒤 연결이 거절됐다. 기존 플레이 프로젝트를 중단하거나 다시 실행하지 않았다.

이번 검증은 같은 기기의 실제 프로세스·TCP loopback·약 20초 시나리오다. 기존 60초 soak/느린 reader 및 기본 30초 예약 경계 테스트를 이번에 다시 실행한 것으로 기록하지 않는다. 원본 서버 검증과 앞선 구현 승인은 각각 기존 보고서에 유지한다.

## 최종 독립 검토

Root가 이번 원본 summary·소스 무변경·실행 결과와 직렬 normal build의 binary 일치를 독립 대조했다. 최종 Architect 판정은 **APPROVED**이며 미해결 blocking finding이 없다. Root는 기존 3000 포트 health 정상, 플레이 PID 두 개 유지, Godot MCP debug 오류 0도 확인했다. 이번 변경은 새 자동 검증의 보고서와 원본 증거만 기록한다.
