# Task 1 독립 리뷰

검토일: 2026-10-25. 검토 대상은 `task-1-review.diff`, `task-1-brief.md`, `task-1-report.md` 및 runtime hardening spec의 Task 1 계약이다. `session.rs`, `room.rs`, `observation.rs`의 production 흐름과 관련 회귀 테스트를 대조했다.

- **Spec compliance: APPROVED**
- **Quality verdict: APPROVED**
- **Blocking findings: 없음.** 검토 범위에서 재현 조건이 구체적인 미해결 계약 위반을 찾지 못했다.

## 승인 근거

- `session.rs:707-737`: State 아래 lifecycle Arc를 복제한 뒤 lock을 해제하고 gate를 획득하며, State 재획득 후 Arc 동일성과 shutdown을 재검증한다. gate 대기 자체도 shutdown으로 해제된다. rotation은 새 Session에 동일 lifecycle Arc를 이전한다(:589-600).
- `session.rs:329-437`: account 발급은 기존 issuance permit 안에서 identity Weak registry를 prune/조회한다. identity → session → State 순서이며 identity/session/operation/issuance guard 모두 owned rotation task가 보유한다. coordinator 수는 `max_sessions` semaphore(:101)로 제한된다.
- `session.rs:760-783`, `:973-1000`, `:1585-1605`: gate와 operation permit을 확보한 뒤 owned task를 만들며, 큐 공간을 기다리는 중 caller가 종료되면 enqueue를 취소한다. enqueue 후에는 oneshot 결과를 회수한다. 성공한 Enter의 caller가 사라져도 manager membership을 반영한 뒤 동일 generation Disconnect로 reservation을 남긴다.
- `session.rs:793-872`, `:894-942`, `:1437-1468`: actor 대기 전 State를 해제한다. 결과 반영 시 generation/deadline/동일 room instance를 확인하며, 소유자를 잃은 admission은 해당 generation의 Leave로 보상한다. Disconnect는 actor가 반환한 실제 reservation deadline을 사용해 늦은 ack로 grace를 늘리지 않는다.
- `session.rs:462-620`: account rotate는 actor Replace 후 State에서 재검증하고, prepare 취소 시 기존 token과 actor reservation을 보존한다. `Rotation::commit()`과 token-map 교체 사이에는 await가 없다. 취소한 prepare가 generation을 전진시켜도 manager generation을 되돌리지 않아 후속 auth가 더 큰 값을 사용한다.
- `session.rs:1119-1258`: factory 전에 invisible slot/active_rooms를 예약하여 pending 생성도 max_rooms에 포함한다. factory callback은 State 밖에서 실행하며, 실패는 정확한 RoomRef만 회수한다. actor admission 실패는 Stop/closure를 거쳐 slot을 제거한다. actor에 이미 들어갔지만 manager commit이 실패한 경우의 generation Leave는 `room.rs:280-282`의 empty-room 종료와 supervisor 정리로 이어진다.
- `session.rs:1629-1688`: cleanup은 busy lifecycle을 기다리지 않고 건너뛰며, State mutation을 마친 뒤 generation-specific Leave를 별도 owned task로 보낸다. 삭제된 session의 미완료 작업도 operation permit을 계속 보유한다.
- `session.rs:1077-1117`, `observation.rs:118-147`: List는 public/visible/game/cursor handle을 복사한 뒤 State를 풀고 watch cache를 읽는다. closed/instance 불일치 summary를 건너뛰면서 limit+1 유효 항목을 확보한다. actor 왕복은 없다.
- `session.rs:1470-1507`: 늦은 이전 instance의 종료는 새 instance map/session을 지우지 않는다. factory 실패와 actor supervisor는 각 생성 경로에서 active_rooms를 회수한다.

## 검증 근거

root가 독립 실행한 `/tmp/campfire-improvements-20261025/root-runtime-task1.log`의 최종 결과를 직접 확인했다: **60 unit + 2 integration passed**, failed/ignored 0. 보고서에 기록된 clippy all-targets 통과는 담당자 실행 근거이며 이 리뷰에서 다시 실행하지 않았다.

테스트 소스에서 다음 수용 조건을 확인했다: slow Replace 중 다른 session issue/auth/List 진행, cache-only List와 closed/private/invisible pagination, enqueue 전 취소, 실제 actor Enter commit 뒤 caller 취소와 정확한 connected/reserved 좌석 수, 늦은 Disconnect deadline, 취소된 짧은 rotation의 기존 reservation 보존, cleanup 중 다른 만료 session 진행, 삭제된 session의 operation permit 유지, factory의 State unlock/pending capacity, 이전 generation Leave fence.

## 범위와 한계

Task 3의 별도 Stop 신호·공정성·bounded shutdown·recent metrics는 후속 작업이며 현재 shutdown의 무제한 대기를 Task 1 반려 사유로 삼지 않았다. 동기 game callback을 강제 중단하거나 실제 부하 성능을 보장하는 검증은 아니다. Cargo/build/browser 실행, 소스 수정, 기존 서비스 PID 조작은 하지 않았다.
