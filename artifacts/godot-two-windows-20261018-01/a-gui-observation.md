# 두 실제 Godot 창 확인: A 창의 네이티브 입력·시각 관측

기존 A Godot 창과 localhost:3000 서버를 유지했다. A는 `g_445…` 게스트로 이미 방에 입장해 있었고, CUA에서 실제 native 창을 선택해 관측·조작했다. macOS의 동일 bundle ID를 쓰는 별도 프로세스는 CUA가 첫 번째 프로세스만 선택했으므로 B는 별도의 실제 그래픽 main scene을 실행하는 보조 runner를 사용했다. Headless 창이나 adapter만 띄운 화면으로 대체하지 않았다.

## 행동과 원본 이미지

1. `a-before-a-move.png`: A와 B가 같은 방에 연결된 뒤 A 창에서 참가자 2/16을 확인했다. A 불빛 링은 대략 (3.33,0), B 청록 점은 (0,0)이었다. 전체 room/instance와 A guest 식별자가 함께 보인다.
2. C가 B의 이동 전 snapshot/화면을 저장한 뒤 A 코트를 CUA로 클릭하고 `pressKey("w", {durationMs:800})`를 한 번 실행했다. `a-after-a-move.png`에서 A 링은 동일 x의 위쪽(y 약4)으로 이동했고 B 점은 원점에 유지됐다. B가 받은 실제 server snapshot의 정확한 전후 좌표와 같은 room/instance 확인은 `a-to-b-observation.json` 및 `b-a-os-before-01.json`, `b-a-os-after-01.json`에 있다.
3. A의 추가 입력을 중단한 상태에서 C가 B의 production adapter에 오른쪽 방향을 약800ms 전달했다. 이 B 입력은 OS 키 입력이 아닌 보조 runner의 방향 입력이다. 실제 그래픽 B main scene과 동일 연결의 처리 ack·서버 좌표는 `b-right-move-01-movement.json`에 남았다.
4. `a-after-b-move.png`: A 창에서 B 청록 점이 원점에서 목표 근처 오른쪽(x 약4.33,y0)으로 이동한 것을 직접 확인했다. A 링은 (3.33,4)에 유지됐고 참가자 2/16, 실시간 연결 상태와 동일한 방 식별자가 보였다. 이미지의 A 하단 표시 값은 입력 처리 확인 32797, server tick 55812, snapshot 27906이다.

A 캡처는 CUA의 native `getScreenshot()` 원본을 그대로 PNG 파일에 저장했다. B 캡처는 보조 runner가 실제 OpenGL 프로젝트 viewport에서 얻은 원본이다. B는 별도 실제 Godot 창이며 GUI source와 protocol adapter는 production 파일을 그대로 사용했다. B의 일시적인 자동 입력과 캡처를 위한 helper 사용 범위를 사용자에게 구분해 설명해야 한다.

## 현재 상태와 한계

두 실제 창과 기존 서버는 종료하지 않았고, 마지막 관측에서 두 참가자가 연결된 상태였다. A GUI 조작 담당자는 B 창 선택을 위한 추가 CUA 탐색이나 세 번째 창 실행을 하지 않았다. A 방 생성 자체는 이번 행동 이전에 이미 수행된 상태였고, 이번 확인에서는 그 방에 B가 실제 목록의 room/instance를 사용해 입장했다.

초기 B 입장 시도에서는 이미지에서 instance의 `73262b67` 일부를 `732626b7`로 잘못 전사해 목록 검색 timeout이 발생했다. 서버 목록 원문과 대조해 교정했고 초기 실패 파일도 남겼다. 최종 방 식별자는 수기 전사 문자열보다 `b-room-list.json` 및 최종 snapshot JSON 원문을 기준으로 삼는다. 이 전사 실수는 클라이언트나 서버 오류로 분류하지 않는다.
