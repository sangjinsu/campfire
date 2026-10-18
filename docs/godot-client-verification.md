# Godot 클라이언트 검증 기록

검증 대상은 Godot 4.7.2 프로젝트 `examples/godot`이며 기존 Rust arena를 사용한다. Rust 서버의 이전 검증 결과는 [서버 검증 기록](verification-results.md)에 별도로 유지한다. GUI와 8명/16명 headless 클라이언트는 `scripts/campfire_client.gd`를 공유한다.

## Scene smoke 검증

```sh
godot --headless --path examples/godot --script tests/ui_smoke.gd
```

UI 구현 전에는 `UI_SMOKE_FAIL: main scene exists`로 종료 코드 1을 확인했다. main scene, UI 상태 전환, authoritative snapshot 표시와 입력 처리를 구현한 뒤 같은 명령에서 `UI_SMOKE_PASS`, 종료 코드 0을 확인했다. 실제 scene을 인스턴스화하고 게스트/방 버튼 활성 상태, 룸 instance metadata, 참가자·점수·ack 표시, +y 좌표 변환, 실제 `Input.parse_input_event` W 키, 텍스트 focus와 application focus 상실 시 입력 정지, 900px 레이아웃 경계를 검사한다. 소스 문자열 비교로 동작을 추정하지 않는다.

초기 sandbox 실행에는 macOS 시스템 CA 및 사용자 Godot 설정 경로 접근 오류가 별도로 출력됐다. 실제 그래픽 검증은 승인된 프로젝트 실행으로 수행하며 엔진 parse/runtime 오류와 환경 경고를 구분한다.

## 실제 그래픽 캡처

1280×800 로비 이미지는 `artifacts/godot/ui-lobby.png`에 있다. Godot Compatibility renderer가 `OpenGL API 4.1 Metal - Compatibility - Apple M2`로 실행된 것을 확인했다. 캡처는 `RenderingServer.frame_post_draw` 후 프로젝트 viewport에서 가져왔고 `UI_CAPTURE ... (OK)` 및 1.5초 자동 종료 코드 0을 확인했다. 이미지에서 한글 font, 코트 경계, 연결 전 버튼 상태와 좌측 스크롤 구성을 직접 확인했다.

900×600에서는 전체 canvas를 축소하는 대신 레이아웃을 다시 배치하도록 설정했다. 초기 canvas 축소 상태에서 `small windows reflow without shrinking Korean text` 회귀 assertion이 실패하는 것을 확인하고, content scaling을 disabled로 바꾼 뒤 `UI_SMOKE_PASS`를 확인했다. 최종 `artifacts/godot/ui-minimum.png`는 실제 900×600 창의 렌더링으로 한글과 버튼이 기본 font 크기를 유지한다.

예약 만료 후 인증 성공(can_resume=false)과 logout 뒤 이전 코트·방 식별자·점수가 남는 경계를 UI smoke에 추가했다. 수정 전 4개 assertion 실패(종료 1), 수정 후 UI_SMOKE_PASS(종료 0)를 확인했다. 최종 승인된 실행에서 `tests/client_unit.gd`의 `PASS: CampfireClient unit invariants`와 scene smoke를 모두 확인했으며 엔진 runtime/parse 오류가 없었다.

전체 multiplayer와 실제 버튼·키 입력 통합 결과는 [Godot 전체 검증 기록](godot-verification.md)에 있다. 로비 이미지 자체는 실서버 multiplayer를 증명하지 않는다.

## Architect 지적에 대한 UI 회귀

인증 응답에는 `can_resume=true`였으나 실제 resume 처리 전에 예약이 끝나는 경계를 별도로 추가했다. Adapter가 빈 방/lobby 상태에서 `RESUME_EXPIRED`, `ROOM_NOT_FOUND`, `STALE_INSTANCE`를 알리면 이전 코트, 점수와 방 식별자를 비운다. 일반 snapshot이 leave/logout 진행 상태를 해제하던 경계도 검사했다. 수정 전 해당 상태·잠금·안내 위치 assertion 15개가 실패했고, 수정 후 `UI_SMOKE_PASS`를 확인했다.

진행 중 작업은 방 입장/복구 full sync 신호, 퇴장/종료 또는 그 작업에 대응하는 오류에서 해제한다. 일반 snapshot, 다른 목록 응답, `game.input` 오류로 leave/logout 버튼 잠금을 풀지 않는다. 오류 signal에서 adapter의 `last_error_command`를 읽어 현재 진행 작업과 대응하는지 확인한다. 대응 leave/logout 실패 뒤 재시도가 가능하고, 무관한 입력 오류나 목록 응답에는 잠금을 유지하는 추가 회귀도 통과했다.

안내 문구를 좌측 스크롤 영역에서 헤더 바로 아래로 옮겼다. 실제 `artifacts/godot/ui-minimum.png`를 다시 캡처해 900×600에서 안내, 한글 크기, 코트 및 하단 통계가 겹치지 않는 것을 확인했다. Scene smoke는 안내가 상단에 유지되는지, 창 너비 안에서 줄바꿈하는지, 코트 높이 260px 이상을 유지하는지 검사한다.

최종 targeted 명령 `godot --headless --path examples/godot --script tests/ui_smoke.gd`와 `tests/client_unit.gd`에서 각각 `UI_SMOKE_PASS`, `PASS: CampfireClient unit invariants`, 종료 코드 0을 확인했다. 최종 multiplayer와 실서버 UI 결과 및 Architect 판정은 별도 전체 검증 보고서에 따른다.
