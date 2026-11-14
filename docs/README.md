# Campfire 문서 색인

## 현재 동작의 기준

코드와 함께 갱신하는 문서다. 서로 다르면 이 문서들이 우선한다.

- [JSON 프로토콜](protocol.md) — HTTP·WebSocket 계약과 오류 code
- [실행 설정](configuration.md) — CLI·환경 변수·기본값, 신뢰 proxy, heartbeat
- [관리자 대시보드](dashboard.md) — 키 설정, API, HTTPS reverse proxy
- [재사용 가이드](reuse.md) — Rust API와 Axum 합성
- [Godot 예제](../examples/godot/README.md)
- [검증 추적표](verification-coverage.md) — 요구별 테스트와 CI 범위

## 설계 기록

작성 시점의 판단을 보존한다. 이후 변경은 반영하지 않을 수 있다.

- [초기 설계 사양](specification.md)
- [Architect 검토](architect-review.md) · [Godot Architect 검토](godot-architect-review.md)
- 사전 조사: [API](api-research.md) · [Godot API](godot-api-research.md) · [대시보드 API](dashboard-api-research.md)
- [설계 spec과 실행 계획](superpowers/)

## 날짜별 작업·검증 기록

해당 날짜와 commit에 대한 증거다. 수치는 그 실행에만 적용한다.

- 2026-10-25 [runtime 강화·CI·패키지](project-improvements-2026-10-25.md)
- 2026-10-23 [대시보드 UI](dashboard-ui-2026-10-23.md)
- 2026-10-18 [runtime 확장](runtime-extensions-2026-10-18.md) · [Godot 계정 UI](godot-accounts-2026-10-18.md) · [Godot 자동 검증](godot-autotest-2026-10-18.md) · [Godot 두 창](godot-two-windows-2026-10-18.md) · [대시보드 검증](dashboard-verification-2026-10-18.md)
- 날짜 없는 초기 기록: [서버 검증](verification-results.md) · [Godot 검증](godot-verification.md) · [Godot 클라이언트 검증](godot-client-verification.md)
