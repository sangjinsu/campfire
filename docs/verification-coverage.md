# 검증 추적표

실제 TCP WebSocket 검증: `crates/campfire-server/tests/headless.rs`. 빠른 18개는 2026-10-18 승인된 localhost 실행에서 통과했다. 장기 기준의 실행 수치와 fixture 수정 이력은 [결과 보고서](verification-results.md)에 기록한다. `artifacts/fast-headless.json`은 첫 환경 실패와 재실행 결과, 명령 및 환경을 기록한다.

| 사양 기준 | 독립 검증 | 실행 근거/범위 |
|---|---|---|
| 8/16명 각각 실제 60초, 입력/snapshot/tick/queue | ignored `soak_8_clients_60_seconds`, `soak_16_clients_60_seconds` | 각 60.00435/60.00999초 통과; artifacts/soak-{8,16}.json |
| 15명+두 마지막 자리 경합, 16명만 확정 | `last_seat_race_is_atomic` | 실제 TCP 통과 |
| guest/ws/list/join/resume 게임 경계 | `game_isolation_and_stale_instance`, `schema_and_explicit_game_mismatch_are_rejected` | 실제 TCP 통과; 다른 게임 토큰 WS 인증, explicit game_id mismatch, 목록 격리와 정원 불변 |
| 첫 메시지/인증 기한 | `authentication_deadline_and_invalid_first_message` | 실제 TCP, 설정 100ms 통과 |
| 토큰 TTL/로그아웃 폐기 | `token_expiry_and_logout` | 실제 TCP, 설정 250ms 통과; 활성 소켓 종료 및 같은 토큰 재인증 거절 |
| 요청 replay/다른 본문, leave idempotency | `replay_and_idempotent_leave` | 실제 TCP 통과 |
| full sync 선행, 수신자별 ack, seq/payload | `Client::enter` 첫 frame assertion, `input_validation_and_recipient_ack` | 실제 TCP 통과; 중복/역순/과도한도약, 위치/점수 위조 거부 |
| replacement는 resume 전 조작 불가, old close 무효 | `replacement_requires_resume_and_preserves_membership` | 실제 TCP 통과; 처리 ack 유지 후 다음 입력 진행 |
| 동일 guest 동시 인증+서로 다른 룸 입장 | `concurrent_authentication_and_cross_room_admission_has_one_membership` | 실제 TCP 통과; 성공 입장1개, 두 룸 멤버합=소유자2+guest1, 새 인증/resume 뒤 지연 old socket 종료에도 ack 진행 |
| 재인증이 예약 기한 연장하지 않음 | `repeated_authentication_does_not_extend_reservation` | 실제 TCP, 설정 500ms 통과; 두 인증의 만료 시각 동일 |
| 실제 30초 grace 전/후 | ignored `reconnect_grace_expires` | 실제 31초 대기 전/후 통과 |
| admission commit 전 cancel / 후 응답실패 | runtime room `cancellation_before_admission_has_no_membership_or_game_side_effect`, `response_failure_after_commit_preserves_resumable_reservation` | runtime 소유 manual-clock 단위 테스트; root 최종 workspace 빠른검증44개 통과에 포함 |
| old generation 미처리 input 폐기와 ack 보존 | runtime room `replacement_discards_unprocessed_old_generation_and_reuses_processed_sequence` | runtime 소유 manual-clock 단위 테스트; 최종 workspace 결과로 확인 |
| 고정 tick/최신 입력, 대각선, 권한 | arena 단위 7개 + runtime `newest_valid_input_is_applied_once_per_tick_and_invalid_does_not_advance_ack` | Arena 단위 통과 보고; TCP valid/invalid 입력 통과. 허용 전송 빈도 간 실제 네트워크 이동거리 비교는 단위 결정성 증거와 구분 |
| 느린 reader의 실제 backpressure/개별 종료/정상 p95<=2x | ignored `slow_reader_isolated_and_p95` | 최초 fixture setup 읽기중단 결함으로 실패 후 수정. 같은256KiB/100ms에서 targeted통과: baseline16유지/종료0, slow15유지/write종료1; root최종독립p95비0.894653(87.799959→78.5505ms), targeted16.68초통과 |
| frame/rate abuser 정상 연결 분리 | `abusive_connection_is_isolated` | 실제 TCP 통과; 반복 20KiB 및 빈도 위반, 정상 플레이어 ack 지속 |
| 공유 queue 포화 SERVER_BUSY, 개인 위반 불변 | `shared_queue_saturation_is_not_abuse` | 실제 TCP 통과; queue=1/16명×20 burst/동시 barrier; busy 관측, abuse counter 불변 |
| 세션 상한/만료/회수 | `caps_and_cleanup` | 실제 TCP 통과; max_sessions=2, 만료 후0 및 새 발급 |
| 연결/미인증 각각 상한/permit 회수 | `connection_and_unauthenticated_caps_refund_permits` | 실제 TCP 통과; authenticated1+unauth1, 초과거부, drop 후0 |
| 룸 상한 및 실패/단절 생성 회수 | `room_cap_refunds_rejected_and_disconnected_creation` | 실제 TCP 통과; invalidsettings/factory거부, cap거부, leave 및 단절유예 후환급. 단절이 commit 전/후 어느 순간인지는 이 wire test에서 단정하지 않음 |
| 주체별/서버전체 발급 rate | `issuer_and_global_guest_rate_limits_recover` | 실제 TCP 통과; Server::exact로 test helper burst override 미사용; 각 burst2 초과429 및 refill |
| rate key 상한/TTL 회수 | `rate_key_cap_and_ttl_reclaim_trusted_source_entries` | 실제 TCP+trusted host ConnectInfo adapter. 서로 다른 issuer는 test adapter가 주입하며 사용자 forwarded header를 신뢰하지 않음 |
| 룸 unwind panic 정리/종료 알림/resume거부 | `panic_supervision_cleans_directory` | 실제 TCP 통과; 의도적 trusted game panic, room.closed, 빈 목록, room_panics=1 |
| 마지막 예약 만료 및 결정성 | runtime `same_manual_clock_trace_replays_tick_ack_expiry_and_state_deterministically`, `admission_full_sync_is_reliable_before_latest_snapshot_and_expiry_releases_seat` + arena replay | runtime paused 단위 테스트가 동일 tick/ack/만료/공개 state를 비교 |
| 세션/rate key/룸 예약 manual-clock 회수 | runtime `manual_clock_reclaims_session_rate_keys_room_and_reservation_before_reissue` | max_sessions=1/max_rate_keys=1, t+1s 활성 token과룸해제, t+2s key회수 후 새source발급 |
| 생성 취소시 temporary room rollback 및 용량반환 | runtime `cancelled_create_rolls_back_temporary_room_and_refunds_room_capacity` | 게임 factory의 test hook로 commit 전 output close, directory0/새생성가능 확인 |
| reliable full sync 첫 write timeout | runtime `reliable_send_survives_first_deadline_without_dropping_full_sync` | paused fake sink에서 첫 timeout에도 동일 메시지 send future 유지 검증 |
| 포화된 룸 control queue에서 shutdown | runtime `shutdown_delivers_stop_even_when_control_queue_is_full` | control queue가 찬 경로에서도 stop 전달과 supervisor정리 검증 |
| 예약 표시 시각 안정성 | runtime `reservation_expiry_timestamp_is_stable_across_manual_clock_reauthentication` + TCP 반복20회 | 수정 전 +1ms drift재현 후 fixed runtime Instant/Unix origin으로수정; 수정후동일소스20/20 통과 |

실제 TCP test를 sandbox 기본 권한으로 처음 실행하면 `bind(127.0.0.1:0)`가 EPERM으로 실패했다. 동일 명령을 승인된 escalation으로 재실행해 18개 통과를 확인했다. 이 환경 실패는 서버 기능 실패와 구분한다. 로그의 intentional trusted game unwind는 panic 격리 테스트가 유도한 사건이다.

`cargo clippy -p campfire-server --test headless --offline -- -D warnings` 통과. 모든 테스트는 포화/종료 등 관측을 assertion으로 요구하며 미관측을 성공으로 바꾸지 않는다. 실인터넷/배포/장시간/대규모 CCU 품질은 이 loopback 검증의 범위 밖이다.

최종 root 독립 fast 실행: 18 passed / 0 failed / 4 ignored, 2.44초; 전체빠른검증44개통과. 구현이 동시에 수정되던 중 이전 실행에서 반복인증 테스트 응답 timeout 1회가 있었으며 단독 재실행(0.65초), 최종 전체 실행 모두 통과했다. assertion 또는 timeout 완화는 하지 않았다.

반복 인증 stress 근거: `artifacts/repeated-auth-before-fix.json` 및 대표 실패 `repeated-auth-run-01.log`는 +1ms 표시 drift를 보존한다. 수정 후 `artifacts/repeated-auth-stress.json`은 같은 source SHA256 `99645f3b66237b825e6d1472f4c35bda20d816c3fcdd2659658aa380fa355c6d`로 20/20 통과했다. 이전 4초 timeout은 이 20회에서 재현되지 않았으며 그 원인을 표시 시각 drift와 동일하다고 단정하지 않는다.

최종 root 독립 검증: formatting, workspace all-target/all-feature clippy -Dwarnings, 빠른44개, 수정된느린reader16.68초 모두통과. 최종전체소스SHA256 `3eb02c2f975952d962ab184e5bfb08d1ccf0ae7f1ec6b2c85a9f286fc3e63e0d`. 기존8/16원본hash와서버productionhash연속성은 결과보고서에분리기록한다.
