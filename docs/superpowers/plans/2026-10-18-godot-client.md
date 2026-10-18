# Godot Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 기존 Campfire arena의 한국어 Godot 플레이 클라이언트와 재현 가능한 실제 Godot 검증을 제공한다.

**Architecture:** 하나의 protocol adapter Node가 게스트 HTTP, WebSocket, 방 식별자와 서버 snapshot을 소유한다. Control scene은 조작과 표시를 맡고 arena Control은 서버 좌표를 그린다. GUI와 headless 통합 클라이언트는 같은 adapter를 쓴다.

**Tech Stack:** Godot 4.7.2 GDScript, Rust Campfire server, Python integration coordinator.

**Spec:** `docs/superpowers/specs/2026-10-18-godot-client-design.md`

## Global Constraints
- Rust 게임 규칙, Cargo 의존성과 wire protocol은 바꾸지 않는다.
- Bearer token은 메모리에 보관하고 로그/UI에 노출하지 않는다.
- 기본 viewport 1280×800, 최소 900×600, 한글 SystemFont fallback을 사용한다.
- 위치와 점수의 권한은 서버에 있으며 위쪽은 월드 +y다.
- GUI와 실제 headless Godot 클라이언트는 같은 CampfireClient adapter를 쓴다.
- 구현 전 baseline 커밋과 공식 API를 확인하며 coordinator가 커밋을 담당한다.

## Review Focus
- URL/game 입력란에 WASD를 입력해도 참가자가 움직이지 않아야 한다.
- Focus 상실, 퇴장과 단절은 방향을 지우고 재접속 때 재생하지 않아야 한다.
- 방 GUID/instance와 16명 목록은 최소 viewport에서도 읽을 수 있어야 한다.
- 예약 만료와 만실 뒤에는 사용 가능한 재시도 버튼을 유지해야 한다.
- 캐시를 커밋하지 않아도 clean import 후 F5/F6와 CLI가 실행돼야 한다.

### Task 1: Adapter와 클라이언트 동작 (network 담당)
**Files:** `examples/godot/scripts/campfire_client.gd`, `examples/godot/tests/client_unit.gd`.
**Interfaces:** Protocol v1을 소비하고 spec의 CampfireClient Node 계약을 제공한다.
- [x] Adapter 구현 전 `client_unit.gd`의 protocol/state 단위 테스트를 실행해 종료 코드 1과 `FAIL: resume full sync must be accepted`를 확인했다. 구현 후 같은 테스트에서 GREEN을 확인했다. 이는 실제 네트워크 전송 검증이 아닌 protocol/state 단위 RED→GREEN이며, 실제 Godot bot 최초 실행과 구분한다.
- [x] HTTP/WS 인증, 방 명령, full sync/최신 snapshot 필터와 20Hz 방향 전송을 구현했다.
- [x] 단위 테스트와 clean headless import를 실행하고 커밋 담당에게 source checkpoint를 요청했다. 최신 targeted 실행의 adapter unit은 `PASS: CampfireClient unit invariants`, scene smoke는 `UI_SMOKE_PASS`로 종료 코드 0을 확인했다.

### Task 2: 플레이 코트와 실제 scene 테스트 (UI 담당)
**Files:** project.godot, scenes/main.tscn, scripts/main.gd, scripts/arena_view.gd, tests/ui_smoke.gd.
**Interfaces:** Task 1 adapter와 protocol snapshot envelope를 사용한다. 제공 인터페이스: main.client and unique controls GuestConnect, Reconnect, ForceDisconnect, CreateRoom, RefreshRooms, NextRooms, RoomList, JoinRoom, LeaveRoom, ServerURL, GameID, Capacity, ArenaView, Status, RoomIdentity, Participants, Stats.
- [x] main scene 부재를 검사하는 SceneTree assertion을 추가하고 실행한다: `godot --headless --path examples/godot --script tests/ui_smoke.gd`; 예상: main scene missing으로 실패.
- [x] 실제 main scene, 작업대 레이아웃, 상태별 버튼, 방 조작과 서버 snapshot arena를 구현한다.
- [x] Snapshot/목표, 실제 key event, 텍스트 focus, focus 상실, 최소 레이아웃과 만실/만료 메시지를 검사하고 같은 명령에서 UI_SMOKE_PASS를 확인한다.
- [x] frame_post_draw 후 1280×800 및 900×600 실제 로비/코트를 캡처하고 root와 clipping을 확인한다.
- [x] 커밋 담당에게 source checkpoint를 요청한다.

### Task 3: 실제 Godot multiplayer 근거 (QA 담당)
**Files:** integration/bot SceneTree scripts, Python coordinator and official API notes.
**Interfaces:** 동일한 Task 1 adapter로 별도 Godot 프로세스를 Rust 서버에 연결한다.
- [x] 실제 클라이언트 생성/목록/입장/이동/재접속 assertion과 게임 격리·예약 만료·로그아웃의 오류·거부 상황 테스트 및 발견 경계의 회귀 검증을 추가했다. 최초 실제 8명 bot 검증은 adapter v1 구현 뒤 실행해 기능 assertion이 통과했으며, baseline adapter에서 실패를 관찰한 것으로 기록하지 않는다. 초기 실행의 source hash 변경 및 뒤이은 fixture/로그아웃 실패 원본은 보존했다.
- [ ] 중간 checkpoint 뒤 고정된 최종 소스로 실제 8명/16명, 처리 입력 ack, 다른 참가자, 공동 점수, 재접속의 동일 플레이어/instance와 정지 입력을 root가 독립 검증한다. 이전 실행의 개별 assertion 통과를 최종 전체 실행 통과로 간주하지 않는다.
- [x] 지금까지 실행한 명령·시간·개별 결과를 기록하고 실패 원본을 보존했다. 최종 독립 실행 결과는 실행이 끝난 뒤 별도로 추가한다.

### Task 4: 사용자 문서와 최종 검증 (UI + coordinator)
**Files:** examples/godot/README.md, root README Godot section, docs/godot-client-verification.md.
**Interfaces:** Task 2/3에서 검증한 명령과 결과를 사용한다.
- [x] 서버 시작, 편집기/F5/F6, 두 번째 창, 조작, 재접속/만료와 실제 Godot 검증을 문서화한다.
- [ ] Clean import, 단위/scene smoke/실제 통합 검증을 실행하고 coordinator가 Rust 회귀 검증을 수행한다.
- [ ] Fresh Architect가 최종 소스와 근거를 검토하고 blocking finding을 해결한 뒤 최종 커밋을 요청한다.
