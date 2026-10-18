# Campfire Dashboard Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 단일 Campfire 서버의 조회 전용 관리자 대시보드를 기존 binary에 제공한다.

**Architecture:** runtime의 별도 관측 directory/watch를 1초 collector가 읽고 HTTP는 immutable cache만 읽는다. server 모듈이 env Bearer 인증·정적 자산·900개 전역 이력을 제공하고 CPU/RSS는 전용 thread 하나가 수집한다.

**Tech Stack:** Rust 1.96, Axum 0.8.9, Tokio 1.53.3, sysinfo 0.39.6, sha2 0.10.9, subtle 2.6.1, HTML/CSS/JavaScript.

**Spec:** [승인된 설계](../specs/2026-10-18-dashboard-design.md). 사용자 IMPLEMENT 승인에 따라 문서 저장 후 재승인 없이 실행한다.

## Global Constraints

- 기존 Config/Options public 필드와 runtime router 형식 보존; 조회 전용·game/player payload 비노출.
- 1초 Skip 수집, 최근 15분 및 최대 900개, UI 5초, server stale 3초, browser last-success 10초.
- process sampler 전용 std thread 최대 하나; shutdown 대기 최대 2초, 미완료 detach, 재생성 금지, Drop 취소만.
- admin key env-only 32–256 ASCII graphic bytes, 미설정/empty OFF, browser memory-only, HTTPS reverse proxy.
- 기본 서버/account_server 모두 통합; 실제 외부 TLS 배포는 검증 범위 밖.

## Review Focus

- 이전 instance의 늦은 종료가 새 room을 삭제하지 않고 active_rooms는 actor마다 한 번만 감소한다(Task 1).
- 기존 manager await 지연 및 OS 호출 정체가 HTTP/room collector를 멈추지 않는다(Task 1–2).
- HEAD/중복 Authorization/오류 응답과 sentinel 데이터가 인증·privacy 경계를 우회하지 않는다(Task 2).
- boot 변경·유실된 history·페이지 도중 룸 종료가 잘못된 누적이나 무한 재조회로 이어지지 않는다(Task 2–3).
- hidden/logout/401 후 완료된 이전 fetch가 화면에 민감 데이터를 되살리지 않는다(Task 3).

## Task 1: runtime 관측 API와 actor 수명

**Files:** Create `crates/campfire-runtime/src/observation.rs`; Modify `src/lib.rs`, `src/session.rs`, `src/room.rs` in that crate; Test runtime unit tests/`tests/invariants.rs`.

**Interfaces:** `Campfire::observer() -> RuntimeObserver`; `snapshot() -> Option<RuntimeObservation>`; exact DTO fields는 spec 1절.

- [x] weak observer, 멤버 lifecycle, instance 지연 종료, manager 지연, 관측 callback/sequence 불변의 실패 테스트를 작성한다.
- [x] `cargo test -p campfire-runtime`으로 새 테스트의 실패 원인을 확인한다.
- [x] 별도 directory와 actor `send_replace` 요약을 구현하고 기존 room 삭제도 instance 일치 검사로 보강한다.
- [x] `cargo test -p campfire-runtime`으로 기존/새 테스트 통과를 확인한다.

## Task 2: 인증·cache·이력·process·HTTP

**Files:** Create `crates/campfire-server/src/dashboard/` module; Modify server `Cargo.toml`, `src/lib.rs`; Test `crates/campfire-server/tests/dashboard.rs` 및 module unit tests.

**Interfaces:** spec의 `DashboardOptions::from_env`, `Dashboard::start/router/shutdown`; `/admin/api/status`, `/rooms`, `/history`. backend 소유자가 frontend와 exact wire DTO/cursor/error contract를 공유한다.

- [x] env/인증/method/privacy, 900개·15분·boot·gap, pagination, sampler 지연·종료 테스트를 먼저 작성한다.
- [x] `cargo test -p campfire-server dashboard`로 실패 원인을 확인한다.
- [x] typed allowlist JSON, fixed freshness, immutable cache, single thread sampler, key digest constant-time 인증과 보안 header를 구현한다.
- [x] `cargo test -p campfire-server dashboard`로 통과를 확인한다. 첫 CPU null, 실제 idle 0, RSS bytes와 모든 경계값을 확인한다.

## Task 3: 정적 화면과 두 composition root

**Files:** Create dashboard HTML/CSS/JS assets; Modify `crates/campfire-server/src/main.rs`, `examples/account_server.rs`, README/운영 문서. 파일 세부 ownership은 coordinator가 배정한다.

**Interfaces:** Task 2의 router와 HTTP DTO. frontend는 서버 cache를 5초마다 조회하고 token을 메모리에만 보관한다.

- [x] boot/history reset, 필터·keyset pagination, hidden/OFF/logout/401·late response의 검증 절차를 먼저 마련한다.
- [x] 전체 지표·process/game 요약·룸·15분 이력과 초기/빈/실패/paused/stale 표시를 구현한다.
- [x] 두 composition root에서 router merge와 serve 전체 handle 수명, 정상/오류 shutdown을 연결한다.
- [x] same-origin CSP와 외부 자산 없음, token storage 없음, 갱신 중복 없음, 늦은 응답 무시를 browser에서 검증한다.
- [x] HTTPS reverse proxy/backend 노출 제한/로그 secret 배제와 실제 배포 검증의 한계를 문서화한다.

## Task 4: 통합 검증과 독립 검토

**Files:** 기존 integration/load 검증 entrypoint와 필요한 dashboard 전용 integration test; 변경 목록과 결과는 coordinator가 관리한다.

- [x] `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`가 통과하는지 확인한다.
- [x] `cargo check -p campfire-server --no-default-features` 및 기존 custom-host/account 예제 호환성을 확인한다.
- [x] 실제 8명/16명 이동·재접속 동안 dashboard polling을 수행하고 동기화/입장/부하 측정을 구분하여 기록한다.
- [x] 코드·로그·응답의 privacy sentinel 검사와 `git diff --check`를 수행한다.
- [x] 독립 Architect에게 최종 diff·테스트 증거를 검토받고 발견 사항을 수정/재검증한다.
- [x] coordinator가 승인된 Git 작업을 수행하고 commit/push 결과와 외부 TLS 미검증 범위를 보고한다.
