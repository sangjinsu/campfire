# 실제 Godot 두 창의 동일 방 확인 — 2026-10-18

기존 첫 번째 Godot 창 A와 서버를 유지하면서 두 번째 실제 그래픽 창 B를 같은 방에 연결했다. **두 참가자의 연결과 양방향 이동 반영을 확인했으며 두 창을 연결된 상태로 남겼다.** 최종 독립 Architect 판정은 **APPROVED**다.

## 실제 연결과 소스 범위

기준 commit은 `65df9b8`이다. Production main scene, UI·adapter·서버와 기존 테스트 도구는 변경하지 않았다. 이번에 추가한 파일은 실제 production scene을 여는 [보조 테스트 script](../examples/godot/tests/live_second_client.gd)와 UID, 이번 원본 증거 및 이 문서다. Helper의 SHA-256과 최종 식별자는 [이번 summary](../artifacts/godot-two-windows-20261018-01/summary.json)에 기록했다.

| 역할 | 실제 프로세스 | 입력·연결 방식 |
|---|---:|---|
| 기존 서버 | 63797 | `127.0.0.1:3000`, arena, 정원 16 |
| 기존 창 A | 65028 | 기존 native GUI 유지, CUA로 실제 OS W키 입력 |
| 두 번째 창 B | 25278 | 별도 native GUI, production `main.tscn`과 `CampfireClient` 사용 |

B는 기존 `GuestConnect` → 실제 `RoomList`의 정확한 room/instance 선택 → `JoinRoom` scene handler로 입장했다. Headless peer나 가짜 방 상태를 표시한 화면이 아니다. B의 실제 renderer는 `OpenGL API 4.1 Metal / Compatibility / Apple M2`이며 창 크기는 900×600이다.

[최초 입장 보고서](../artifacts/godot-two-windows-20261018-01/joined.json)는 **21:03:27 KST**에 `ready:true`, 오류 빈 배열, 실제 참가자 2명 모두 `connected:true`를 기록했다. 다음 식별자가 최종 snapshot에서도 유지된다.

```text
room_id: r_6b8c7889e92f54359a0408fca89339238b361a7d1a76541e999e4a3030ae8c95
room_instance_id: i_26906e272a6a5344b2710d58ce0bcf464a7a15b29673262b67a7db76275f7dca
```

A guest는 `g_445…`, B guest는 `g_0f2…`이며 전체 guest ID와 공개 상태는 JSON 원문에 보존한다. Token은 화면·명령 파일·보고서에 저장하지 않는다.

## 이동과 상대 창의 관측

| 동작 | 실제 입력 방식 | 서버 좌표와 상대 창 확인 |
|---|---|---|
| A → B | A 코트에 CUA OS W키 800ms | A `(3.3333333,0)` → `(3.3333333,3.99999996)`, B가 같은 guest/room/instance의 원격 이동 수신 |
| B → A | B helper가 production adapter에 오른쪽 방향 약 800ms 전달 | B `(0,0)` → `(4.33333329,0)`, A native GUI에서 원격 점 이동 관측 |

A 이동은 [A 화면 전](../artifacts/godot-two-windows-20261018-01/a-before-a-move.png)·[후](../artifacts/godot-two-windows-20261018-01/a-after-a-move.png) 및 [B의 원격 관측 JSON](../artifacts/godot-two-windows-20261018-01/a-to-b-observation.json)에서 확인한다. B가 받은 snapshot sequence는 27105 → 27540이며 A의 x는 유지되고 y가 증가했다.

B 이동에서는 production main의 키보드 polling을 잠깐 멈추고 기존 adapter의 방향 입력을 사용했다. 이는 B의 OS 키 입력 검증을 뜻하지 않는다. 이동 종료 후 방향을 0으로 보내고 main processing을 복구했다. [이동 원문](../artifacts/godot-two-windows-20261018-01/b-right-move-01-movement.json)은 처리 ack 1232 → 1251, 위치 증가 및 `main_process_restored:true`를 기록한다. [B 이동 후 A 실제 화면](../artifacts/godot-two-windows-20261018-01/a-after-b-move.png)에서 A는 `(3.33,4)`에 유지되고 B 원격 점은 `(4.33,0)` 근처에 표시된다.

A 이미지는 native CUA screenshot 원본이고, B 이미지는 `RenderingServer.frame_post_draw` 뒤 실제 프로젝트 viewport에서 저장했다. [GUI 행동 기록](../artifacts/godot-two-windows-20261018-01/a-gui-observation.md)에 입력과 관측 순서를 보존했다.

## 초기 연결 시도와 교정

macOS CUA가 같은 bundle ID의 첫 번째 Godot 프로세스만 선택하는 제약 때문에 두 번째 창을 조작하는 테스트 helper를 추가했다. 기존 A와 서버를 보호하면서 처음 추가한 B 창과 입장 실패 helper만 정확한 PID·명령을 확인해 교체했다.

초기에는 이미지에서 전사한 instance의 `73262b67` 부분을 `732626b7`로 잘못 전달하여 정확한 instance 검색 gate가 timeout을 보고했다. [초기 config](../artifacts/godot-two-windows-20261018-01/b-config-attempt1.json), [초기 timeout](../artifacts/godot-two-windows-20261018-01/b-errors-attempt1.json), 진단 중 실제 공개 목록과 실패 화면을 보존했다. [서버 room.list 원문](../artifacts/godot-two-windows-20261018-01/b-room-list.json)에서 같은 room_id의 instance를 그대로 가져와 config를 교정했다. Gate를 완화하거나 제품 코드를 바꾸지 않았다. 초기 timeout은 전사 오류이며 최종 연결의 `errors:[]`와 구분한다.

## 연결 유지와 증거 동결

**21:05:37 KST**에 `handoff` 명령을 실행해 방향 0, main processing 복구, 마지막 PNG/JSON 저장을 완료했다. 이후 helper의 command polling·telemetry 파일 쓰기를 멈추고 production scene·client·native 창은 계속 실행했다. [최종 handoff](../artifacts/godot-two-windows-20261018-01/handoff.json)는 `frozen:true`, `main_process_enabled:true`, 오류 빈 배열과 두 참가자의 `connected:true`를 기록한다. 최종 B 처리 ack는 2247, server tick은 57228, snapshot sequence는 28614다.

[마지막 B 실제 화면](../artifacts/godot-two-windows-20261018-01/b-handoff-01.png)을 보존했고 [파일 안정성 확인](../artifacts/godot-two-windows-20261018-01/b-handoff-stability.json)은 동결 뒤 live/handoff/log hash가 유지됨을 확인한다. [21:06:12 KST 프로세스·health 확인](../artifacts/godot-two-windows-20261018-01/b-runtime-preserved.json)에서 서버와 A/B PID가 모두 유지됐고 `/health`는 HTTP 200 및 `{"status":"ok"}`를 반환했다.

새 helper는 실제 Godot parse check와 UID 생성을 위한 headless import 1회가 통과했다. 전체 8/16명 suite를 이번 두 창 확인에서 반복하지 않았다. Root와 Architect는 양방향 원본 PNG/JSON, 동일 방 식별자, production diff 없음, handoff 및 세 프로세스 유지를 독립 대조해 승인했다.
