# Architect 검토

상태: **APPROVED** — 사양의 1차 구현 범위에 대해 소스 검토, 결정적 단위 검증과 실제 WebSocket 검증 근거를 확인했다. 미해결 blocking finding은 없다.

첫 장시간 실행의 느린 수신자 테스트는 모든 입장이 끝날 때까지 정상 client도 읽지 않는 fixture 때문에 `Close(None)`으로 실패했다. 입장 직후 reader를 시작하고 16명 상태 확인 barrier 뒤 대상 1명만 읽기를 정지하도록 수정했다. 같은 256KiB 상태·100ms 쓰기 기한을 유지한 독립 재실행에서 통과했다. 실패 원본 `artifacts/long-run.log`를 보존하며, 첫 장시간 묶음 전체가 통과했다고 주장하지 않는다.

## 범위와 근거

`docs/specification.md`의 단일 프로세스·신뢰된 정적 Rust 모듈·8/16명 검증 범위를 기준으로 runtime, arena, 서버 CLI, headless 테스트 소스를 직접 확인했다. `docs/api-research.md`의 실제 선택 버전 확인 기록도 대조했다. public deployment나 분산 운영에 대한 승인은 포함하지 않는다.

독립 실행: `cargo test --workspace --lib --offline` — arena 7개, runtime 14개, CLI 3개, 총 24개 통과. 이는 장시간 통합 테스트 실행을 대체하지 않는다.

## 발견 사항과 수정 확인

- **수정 확인 — reliable 송신 취소:** 기존 writer가 `SinkExt::send` timeout 뒤 해당 메시지를 버리고 다음 메시지를 진행했다. `poll_ready` 단계에서 대기 중이면 full sync가 누락될 수 있었다. 현재 `transport::write_message`는 동일 send future를 pin하여 반복 deadline 동안 유지하고, 최종 timeout에는 연결을 종료한다. snapshot 순서 기록도 전송 성공 뒤 변경한다. `reliable_send_survives_first_deadline_without_dropping_full_sync`가 최초 deadline 후 full sync 1회 전달을 검증한다.
- **수정 확인 — 종료 제어 전달:** `shutdown`이 `try_send(Stop)` 결과를 무시하던 경로를 보강했다. 현재 Stop 전달과 room receiver 종료를 기다리고, 감독 정리 및 연결 permit 회수를 확인한다. 기존 전역 session 직렬화에서는 control queue 포화 재현이 제한적이지만 종료 신호를 버리지 않는 계약이 더 명확하다.
- **확인 — 메모리 상한:** 입력·control·연결 request·reliable queue가 유한하며 snapshot은 watch 최신 1개 슬롯이다. transport write buffer도 16MiB로 제한한다. 신뢰된 게임 모듈이 생성하는 임의 크기 공개 상태의 비용은 모듈 작성자의 책임이며 process OOM 격리는 보장하지 않는다.
- **수정 확인 — 예약 시각의 안정성:** wall clock 현재 시각과 반올림된 잔여 시간을 매번 합산하던 응답 변환을 시작 시점의 monotonic/Unix 기준값으로 고정했다. 동일 예약의 만료 시각을 반복 인증으로 변경하지 않는 manual clock 테스트가 있다.

## 불변 조건 검토

| 영역 | 소스 및 테스트 판단 |
|---|---|
| 게임 경계 | token·connection·command·room game 검사, `(game_id, room_id)` 조회, instance 검사가 있다. |
| 단일 멤버십 | session state mutex를 유지한 상태로 admission/replace/leave 확인까지 기다린다. 게스트 간에도 직렬화하므로 단순하지만 장래 확장 병목이 될 수 있다. |
| admission | commit 전 닫힌 output/reply는 멤버십 변경 없이 거절한다. commit 후 reliable 실패는 자리를 예약으로 전환한다. 임시 생성은 공개 목록에서 숨기고 실패 시 actor 종료를 확인한다. |
| 재접속과 입력 | 교체 시 pending과 수신 sequencer를 실제 processed 번호로 되돌린다. 입력 수신 및 실제 적용 직전 generation을 검사한다. 예약 기한은 반복 인증으로 연장하지 않는다. |
| 상태 순서 | full sync는 reliable queue, 후속 상태는 최신 슬롯이다. 단일 writer가 reliable을 우선하고 instance/sequence를 검사한다. |
| 과부하 | 공유 input queue 포화는 retryable SERVER_BUSY이며 개인 위반을 올리지 않는다. 개인 크기·빈도 위반은 해당 Output만 닫는다. |
| 생명주기 | room JoinHandle 결과를 감독하여 정상 종료와 unwind panic 모두 directory/session 참조를 정리하고 room.closed를 보낸다. |
| arena | 입력 자체로 이동하지 않고 fixed tick으로 이동한다. 대각선 정규화·경계·단절 정지·목표 중복 획득 방지·클라이언트 위치/점수 거부가 있다. |

## 최종 실행 근거

| 검증 | 확인한 결과 |
|---|---|
| 최종 빠른 검증 | 전체 44개 통과, headless 18개 포함. fmt 및 workspace/all-targets/all-features clippy `-D warnings` 통과. `artifacts/fast-headless.json`의 `root_independent_final_pipeline` 참조. |
| 8명 실제 TCP | 60.004초, 처리 입력 4,800개, 공유 queue 최대 깊이 7. `artifacts/soak-8.json`. |
| 16명 실제 TCP | 60.010초, 처리 입력 9,600개, 공유 queue 최대 깊이 16. `artifacts/soak-16.json`. |
| 실제 유예 경계 | `reconnect_grace_expires` 통과. `artifacts/long-run.log`에서 개별 결과 확인. |
| 느린 수신자 | baseline 연결 16개·종료 0개, slow 연결 15개·write deadline 종료 1개. 정상 snapshot p95 87.800ms → 78.551ms, 비율 **0.894653 ≤ 2**. `artifacts/slow-reader.json`. |
| manual clock | 세션/rate key 상한·만료·room 회수·재발급, 임시 생성 취소 회수, full control queue 종료, 동일 trace 재생, 예약 timestamp 안정성 통과. |
| 같은 guest 경합 | `concurrent_authentication_and_cross_room_admission_has_one_membership`이 두 인증·서로 다른 룸 입장 후 한 멤버십 및 구 연결 종료 무효를 검증한다. 최종 headless 통과 목록 확인. |
| 실행 예제 | 실제 binary `/health` 응답과 headless 인증→생성→입력→재접속→퇴장→로그아웃 walkthrough 통과 기록 확인. |

8/16명 실행의 테스트 포함 source SHA-256은 `99645f3b66237b825e6d1472f4c35bda20d816c3fcdd2659658aa380fa355c6d`, fixture 수정 후 최종 source는 `3eb02c2f975952d962ab184e5bfb08d1ccf0ae7f1ec6b2c85a9f286fc3e63e0d`다. 서버 구현 및 Cargo 설정/lock의 별도 해시 `08466360cfcdbb1771e48fd9e4c738717ad9333dbfb86b63b756d4f93344fa95`는 전후 동일함을 Architect가 직접 다시 계산했다. `soak()` 내용도 유지되어 기존 장시간 결과와 새 fixture 결과를 동일 서버 구현의 증거로 인정한다. 상세 범위는 `artifacts/production-source-continuity.json`에 있다.

이동 빈도 독립성은 arena의 다중 입력 후 tick 전 이동 없음·fixed dt·대각선 정규화 테스트와 runtime의 tick당 최신 유효 입력 1회 적용 테스트를 조합한 결정적 근거다. 별도 실제 TCP 1Hz 대 60Hz 이동 거리 비교를 실행한 것으로 해석하지 않는다.
