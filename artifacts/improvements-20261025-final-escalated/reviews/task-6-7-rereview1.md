# Task 6·7 수정 1차 재검토

범위: `task-6-7-review.md`의 Task 7 F1, Task 6 F2, 비차단 LICENSE 검사만 재확인. 수정된 `tools/package_smoke.py`, 해당 단위 테스트, `.github/workflows/runtime-validation.yml`, Task 6·7 수정 보고와 정적 검사 로그를 읽었다. Cargo, GitHub runner, 설치, 서버·브라우저 실행은 하지 않았다.

| 기존 항목 | 판정 | 근거 |
|---|---|---|
| F1: 첫 실환경 suite 실패 시 뒤 suite skip | **ADDRESSED (정적)** | `.github/workflows/runtime-validation.yml:78-96,104-105,113-114,122-124`에서 준비 마지막 step `validation_ready`에 id를 주고, 세 suite 모두 `!cancelled() && steps.validation_ready.conclusion == 'success'`를 명시한다. 앞 suite 실패 후에도 해당 조건을 평가해 나머지를 실행하며, 준비 실패/취소 시에는 실행하지 않는다. `continue-on-error`가 없으므로 suite 실패는 job 실패로 남고 artifact 업로드는 `always()`다. GitHub runner 실제 해석·실행은 아직 필요하다. |
| F2: 기본 package smoke가 저장소 `target` 사용 | **ADDRESSED** | `tools/package_smoke.py:82-88,201-223`이 `--target-dir` > 비어 있지 않은 `CARGO_TARGET_DIR` > 실행별 임시 `target` 순으로 선택하고 `target_dir` 및 `target_source`를 summary에 남긴다. 무지정 문서 명령은 이제 임시 target을 쓰며, 명시적 CI target은 유지된다. `tools/tests/test_package_smoke.py:114-133`의 우선순위 및 모의 main 테스트가 이를 확인한다. |
| 비차단: LICENSE 첫 줄만 확인 | **ADDRESSED** | `tools/package_smoke.py:27,149-153`이 루트 LICENSE의 고정 SHA-256을 확인한 뒤 archive LICENSE 전체 바이트를 비교한다. `tools/tests/test_package_smoke.py:99-112`가 첫 줄만 남긴 파일을 거부한다. 현재 루트·세 crate LICENSE 네 파일의 SHA-256은 모두 `51089a80b7758f0afa852cae8dee25073a8eb5e42fc8a185ba8c91cb84874b9f`다. |

이번 수정 범위에서 새로 확인된 breakage는 없다. 직접 재실행한 `python3 -m unittest tools.tests.test_package_smoke`는 7개 통과했다. 담당자 로그의 전체 Python 16개 통과와 workflow 구조 검사는 각각 `/tmp/campfire-improvements-20261025/task6-fixround1-all-python.log`, `/tmp/campfire-improvements-20261025/task7-fixround1-workflow.log`에 있다. 이 정적·단위 검증은 최종 세 archive/checkout consumer 성공이나 GitHub quick/수동/예약 CI 성공을 증명하지 않는다. 두 실행 결과는 통합 단계에서 계속 확인해야 한다.
