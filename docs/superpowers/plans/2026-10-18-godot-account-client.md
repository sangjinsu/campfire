# Godot Account Client Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Godot에서 계정 로그인, 수집 명령, 개인 상태를 제공하고 실제 두 native 클라이언트와 기존 Arena 회귀를 검증한 뒤 커밋·푸시한다.

**Architecture:** 기존 CampfireClient transport와 UI 경계를 유지한다. 명시적인 game profile validator로 Arena와 Inventory schema를 선택하며, 미확인 단발 명령 한 개를 membership에 바인딩한다. Rust runtime 동작은 변경하지 않는다. 개발용 account_server에는 독립 QA 실행을 위한 optional `CAMPFIRE_DEMO_BIND`만 추가하고 기본 `127.0.0.1:3001`과 게임 계약을 유지한다.

**Tech Stack:** Godot 4.7.2 / GDScript, HTTPRequest, WebSocketPeer, 기존 Rust/Tokio/Axum 개발 예제, Python 검증 launcher.

**Spec:** `docs/superpowers/specs/2026-10-18-godot-account-client.md`

## Global Constraints

- credential/token은 메모리만 사용하며 로그·CLI 인자·아티팩트에 기록하지 않는다.
- default profile은 `arena`, Inventory UI는 `inventory`; game ID와 profile은 별개다.
- sequence는 `2^53-1` 이하, 미확인 명령은 한 개, 재시도는 같은 seq·payload다.
- 새로운 Rust runtime 동작이나 외부 IdP는 도입하지 않는다.
- 다른 worker 변경을 되돌리지 않는다. 각 소유 범위를 지키고 coordinator만 최종 통합·Git 전달한다.

## Review Focus

- 잘못된 계정 교환 또는 늦게 도착한 HTTP callback이 이전 identity의 private 표시를 복원하지 않는가: Task 1.
- terminal 실패와 실행 전 거부를 구별해 command seq를 잘못 소비하지 않는가: Task 1.
- snapshot ack만 받은 뒤 결과를 임의 성공 처리하지 않는가: Task 1.
- profile·주소·game 변경과 logout 직후 이전 inventory가 화면에 남지 않는가: Task 2.
- 실제 두 native 창 증거 없이 headless 통과를 GUI 성공으로 보고하지 않는가: Task 3.

## 소유권

| 담당 | 파일 |
|---|---|
| network | `examples/godot/scripts/campfire_client.gd`, client 단위/transport 검사 |
| UI | `examples/godot/scripts/main.gd`, UI 전용 검사 |
| design | 이 spec/plan 및 `docs/godot-api-research.md` 추가 단락 |
| QA/coordinator | 계정 native 통합 검사·launcher·증거, README/reuse 사용법, 최종 검증·Git |

### Task 1: 계정 인증과 명령 복구 계약

**Files:** Modify `examples/godot/scripts/campfire_client.gd`; test `examples/godot/tests/client_unit.gd`, `client_unit_transport.gd`, 계정 전용 client test.

**Interfaces:** spec의 `set_game_profile`, `connect_account`, `send_game_command`, `retry_pending_command`, pending 조회 API와 public properties/signals를 생산한다. 기존 guest/room API를 소비·보존한다.

- [x] account HTTP schema/WS alias 일치, inventory schema, private 생략·identity 변경 폐기 테스트를 먼저 추가하고 실패를 확인한다.
- [x] 공식 HTTPRequest JSON POST와 bounded schema 검증, profile 선택을 구현한다. stale callback epoch guard를 유지한다.
- [x] 명령 terminal 성공·실패, 실행 전 거부, 같은 payload retry, timeout/resume/fullsync dedup, ack-only/eviction 테스트를 추가하고 실패를 확인한다.
- [x] 한 개 pending ledger와 membership binding을 구현한다. snapshot 전체 검증 후 원자적으로 상태를 반영한다.
- [x] `/opt/homebrew/bin/godot --headless --path examples/godot --script res://tests/client_unit.gd`와 추가 client test를 실행해 exit 0과 PASS를 확인한다. 변경 파일과 검사 결과를 coordinator에게 전달한다.

### Task 2: 모드와 계정·개인 상태 UI

**Files:** Modify `examples/godot/scripts/main.gd`; test UI 전용 단위/smoke 검사.

**Interfaces:** Task 1의 정확한 API·signals·properties를 소비한다. Guest Arena/Account Inventory 모드, masked credential, 수집·재확인 controls, 공용/private/result 표시를 생산한다.

- [x] 기존 UI 구조 위에 모드 전환·로그인 입력 삭제·Inventory 이동 차단·단절/로그아웃 private 표시 삭제 검사부터 추가한다.
- [x] `OptionButton.item_selected(index)`와 `LineEdit.secret=true`를 사용한다. programmatic mode 선택에서는 선택 처리 경로도 명시적으로 호출한다.
- [x] Inventory의 `players:uint`와 `total_collected:uint`, `private_state.inventory`, 자신의 `command_results`를 서로 분리해 표시한다. 미확인 명령이 있으면 새 collect를 막고 재시도 가능 여부를 반영한다.
- [x] 계정 오류 code와 처리 불명 상태를 표시한다. 기존 Guest Arena의 이름·버튼·자동 실행 경로와 한글 테마를 유지한다.
- [x] UI 검사 및 기존 `ui_smoke.gd`를 실행해 exit 0을 확인하고 native 통합용 node 경로와 조작 계약을 QA에 전달한다.

### Task 3: 실제 두 창과 기존 Arena 검증

**Files:** 계정 native 통합 검사/launcher, `artifacts/` 새 실행 폴더, `README.md`, `docs/reuse.md`, 이번 검증 보고서.

**Interfaces:** production `main.gd`의 실제 UI 조작과 `CampfireClient`의 관측 상태를 소비한다. machine-readable 결과, 두 renderer PNG, 로그, source provenance를 생산한다.

- [x] 두 독립 native Godot 프로세스와 별도 account_server를 띄운다. credential은 환경 변수로 주입하고 출력하지 않는다. 기존 사용자 프로세스는 건드리지 않는다.
- [x] Alice/Bob 로그인·동일 room·각 collect·public 일치·private 및 결과 분리를 UI 경로로 확인한다. invalid credential/disabled auth와 rotation·disconnect/resume·중복 없는 retry를 검증한다.
- [x] 실제 렌더러의 `RenderingServer.frame_post_draw` 후 양쪽 창 PNG를 저장하고 직접 시각 확인한다. GUI 이벤트 사용 여부와 관측 결과를 보고서에 구분한다.
- [x] `python3 -u tools/validate_godot.py --godot /opt/homebrew/bin/godot --counts 8 16 --duration 20 --output artifacts/<이번-실행>/legacy`를 실행한다. 기존/신규 Godot parse/runtime 오류와 credential 로그 누출도 확인한다.
- [x] source hash·실행 명령·exit code·엔진 버전·제약을 기록하고 README/reuse에 계정 모드 실행법을 갱신한다.

### Task 4: 독립 검토와 Git 전달

**Files:** 전체 diff 및 새 검증 보고서; coordinator 소유.

**Interfaces:** Tasks 1–3의 소스·검사·두 GUI 증거를 독립 Architect가 읽고 승인 또는 구체적 수정을 요청한다.

- [ ] `git diff --check`와 변경 범위에 맞는 최종 검사를 실행하고 source 동결 여부를 확인한다.
- [ ] 독립 Architect에게 인증 binding, private 폐기, 명령 복구 및 실제 두 창 증거를 검토받는다. 지적 수정 시 해당 검사만 다시 실행한다.
- [ ] 승인 뒤 변경을 커밋하고 `git push origin main`으로 normal push한다. `git ls-remote origin refs/heads/main`과 로컬 HEAD 일치를 확인한다.
- [ ] 검증한 동작, 원본 증거 경로, commit SHA, 외부 IdP 미연동 한계를 한국어로 보고한다.
