# Task 4·5 수정 1차 scoped 재검토

검토일: 2026-10-25. 대상은 `task-4-5-fix1-review.diff`, 각 작업보고서 Fix round 1, 이전 리뷰 F1/F2다. 기존 finding 해결 여부와 수정으로 생긴 regression만 확인했으며 package/CI 및 다른 runtime 변경은 범위에서 제외했다.

## Task 4 / F1

- **Finding 상태: ADDRESSED**
- **Spec compliance: APPROVED**
- **Quality verdict: APPROVED**

`tools/validate_dashboard.py:180-208`에서 browser stop 실패를 기록한 후 모든 소유 프로세스·handle 정리를 계속한다. stop 실패 시 `browser_started`를 유지하고 `closed`를 설정하지 않으므로 명시적 `close()` 재시도가 가능하다. `cleanup()`(:210-221)은 종료 미확인 상태에서 profile 삭제를 거부한다. main의 finally(:534-554)는 browser stop 확인/cleanup 결과/오류 목록을 summary에 기록하며 미확인을 실패 exit code로 처리한다.

`tools/tests/test_validation_tools.py:104-170`은 timeout 이후 다른 소유 자원 정리, profile 보존, 성공 재시도 후 owned root 삭제, timeout/nonzero 각각의 실패 summary를 검증한다. 새 상태·profile 격리 및 동일 env 사용은 유지된다. 이전 F1 재현 경로의 조기 latch와 summary 중단은 해소되었으며, 이 수정 범위에서 새 blocking regression은 찾지 못했다.

## Task 5 / F2

- **Finding 상태: ADDRESSED**
- **Spec compliance: APPROVED**
- **Quality verdict: APPROVED**

`crates/campfire-server/src/dashboard/assets/app.js:620-624`가 `DOMPoint(clientX, clientY).matrixTransform(getScreenCTM().inverse())`로 실제 SVG 사용자 좌표를 얻는다. desktop의 좌우 여백과 mobile의 축소가 기존 전체 폭 비례식과 달리 반영된다. null screen transform 및 비정상 x 처리도 포함한다.

`tools/tests/dashboard_ui_control.mjs:354-372`는 620px desktop SVG의 첫 점·중간 점과 390px mobile의 첫 점을 keyboard readout과 대조한다. 실제 browser fixture(`tools/dashboard_browser_control.js:15-69`)도 path의 실제 첫 점·중간 점을 화면 좌표로 옮겨 pointer를 전달하고 표본 841/871 및 keyboard 값 일치를 검사한다. 실제 1/5/15분 범위의 61/62/63개 표본, counter reset, CPU null/0 fixture와 판정식이 일치한다. `validate_dashboard.py`는 desktop·mobile 둘 다 호출하고 별도 결과를 남긴다. 이전 맨 왼쪽 clamp만 검사하던 허점이 해소되었으며 수정 범위의 새 blocking regression은 찾지 못했다.

## 독립 실행 근거와 한계

- `python3 -B -m unittest discover -s tools/tests -p test_validation_tools.py`: **9 tests passed**, exit 0. 테스트 중 의도한 nonzero/timeout summary는 `success=false`를 기록했다. 담당자 전체 12개 결과와 달리 이번 재실행은 package 테스트를 제외한 scoped 9개다.
- `node --test tools/tests/dashboard_ui_control.mjs`: **23 scenarios passed**, exit 0. 이번 실행 Node는 v26.12.0이다.
- browser/Cargo/build 실행, 소스 수정, 기존 사용자 프로세스 조작은 하지 않았다. 실제 desktop/mobile geometry 및 browser 실행 성공은 root의 통합 QA에서 별도로 확인해야 한다. 이 승인은 코드·fixture·mock 검증 범위의 승인이다.

두 Task의 이전 blocking finding은 모두 해결되었고 남은 scoped 수정 요청은 없다.
