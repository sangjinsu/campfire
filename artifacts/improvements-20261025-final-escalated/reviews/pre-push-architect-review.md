# 최종 독립 Architect 검토 — Pre-push 승인

검토일: 2026-10-25. 현재 판정은 **코드·로컬 검증 Pre-push APPROVED / Delivery는 실제 GitHub CI 대기**다. 알려진 package smoke 실패와 문서 고지는 해결됐고 추가 blocking finding은 없다. 이 승인은 정상 push를 진행하기 위한 독립 기술 검토이며, 실제 CI 또는 배포 완료 판정은 아니다.

## 대상과 방법

- 기준 `main`: `d04f6f19c8228a72e71613657b97d9b8dcefee3c`.
- 최초 검토 HEAD: `647aaf1994d58906d70436f90de80827f45d44b5`, `codex/project-improvements`. 후속 검토는 그 HEAD 이후 package script/test 및 아래 문서의 현재 작업 트리를 대상으로 했다.
- 승인된 `docs/superpowers/specs/2026-10-25-runtime-hardening-design.md`, `docs/superpowers/plans/2026-10-25-project-improvements.md`와 47개 변경 파일의 범위, production 핵심 경로, 테스트/runner/CI와 문서를 대조했다.
- Task 1, Task 2·3, Task 4·5와 Task 6·7의 독립 리뷰 및 두 수정 재리뷰를 읽고 통합 경계를 별도로 검토했다.
- 제품 source/docs 수정, Cargo/build, 브라우저/서버 실행, 기존 PID·포트·원본 target 조작은 하지 않았다. Git은 읽기 전용 diff만 사용했다. 이 파일만 상태 보고 용도로 작성한다. 후속 검토에서 Python unit 17개를 독립 재실행했고, archive/lockfile/source 및 새 `/tmp` binary를 읽어 SHA를 직접 대조했다.

## 현재 소스 판정

현재 검토 범위에서 보안·동시성·종료·하위호환·UI·QA 격리의 추가 blocking finding은 없다.

- session State는 lifecycle gate 복제/검증과 map mutation에 사용하고 actor 왕복 전에 해제한다. identity → session → State 순서, Weak identity registry, max_sessions operation permit, caller 취소 후 owned reconciliation을 확인했다. 만료/rotation/Leave/late admission은 generation 및 정확한 room instance로 한정된다. factory는 State 밖에서 실행되며 invisible slot이 수용량에 포함된다.
- account Rotation commit과 token map swap 사이에는 await가 없고 취소한 prepare의 generation을 뒤로 돌리지 않는다. cleanup은 busy lifecycle gate를 기다리지 않는다. 삭제된 session의 진행 중 작업도 operation permit을 유지한다.
- List는 watch summary를 읽고 actor에 요청하지 않는다. public/game/visible/cursor 필터와 유효 항목 limit+1 처리가 유지된다.
- control/invalid token bucket은 연결마다 분리되며 reader에서 response replay보다 먼저 소비한다. malformed/binary/application oversize의 유한 오류·종료, 최초 인증 실패 즉시 종료, 기존 input budget 및 Config/CLI literal 구조 보존을 확인했다.
- actor는 Stop을 큐와 독립적으로 받으며 queued control snapshot을 data보다 먼저 처리한다. idle wake control의 별도 cycle은 freed slot 재충전으로 generation fence가 data 뒤로 넘어가지 않게 한다. data와 due tick은 cycle별 유한량이며 yield한다.
- shutdown은 초기 State 획득부터 하나의 deadline을 사용한다. 남은 room supervisor, connection permit, owned operation permit 수를 보고한다. 실행 중 동기 callback을 강제로 중단한다는 주장은 없다.
- recent latency는 5종별 512개/60초 상한, lock 밖 정렬, nearest-rank와 empty=null을 유지한다. command replay는 processed/failed/duration 표본과 분리된다. dashboard는 명시적 projection으로 개인정보·payload를 추가하지 않는다.
- chart range/실제 표본 선택/null·0/gap/boot reset/error delta 경로와 pointer의 SVG 역변환을 대조했다. 기존 F2의 넓은 SVG 좌표 오류는 source와 desktop/mobile fixture에 반영되었다.
- QA runner는 새 owned root의 BROWSE_STATE_FILE/CHROMIUM_PROFILE을 사용하고 같은 env로 stop한다. stop 실패 후 다른 owned process 정리를 계속하며 profile을 보존하고 실패를 기록한다. 사용자 process/profile fallback은 없다.
- CI는 준비 완료 후 세 live suite가 앞 suite 실패와 무관하게 실행되는 조건을 갖고 실패를 success로 바꾸지 않는다. 설치·action/tool pin의 설정과 실제 GitHub runner 성공은 구분해야 한다.

## 읽어서 확인한 실행 증거

`/tmp/campfire-improvements-20261025/root-final-commands.json`과 대응 원본 로그에서 fmt, clippy, workspace all-targets tests, no-default check, server/examples build, 별도 ignored 실행의 exit 0을 확인했다. workspace는 141 passed + 4 ignored, 별도 ignored 실행은 4 passed로 합계 145개다. ignored 이름은 reconnect grace, slow-reader isolation/p95, 8/16명 각 60초 soak다. 이는 실제 Godot client 검증과 별도 증거다.

`root-node-task5-fix1.log`는 23 UI 시나리오, package 수정 전 `root-python-all.log`는 16개 테스트를 기록한다. 수정 후 `artifacts/improvements-20261025-final-escalated/root-verification-logs/root-python-final.log`와 evidence JSON은 17개/exit 0이다. 본 리뷰에서도 `python3 -B -m unittest discover -s tools/tests -p 'test_*.py'`를 독립 재실행하여 **17개 통과**를 확인했다(`/tmp/campfire-improvements-20261025/architect-python-final.log`). Python 로그의 의도한 stop timeout/nonzero fixture는 failure summary를 검사하는 회귀 테스트다. Cargo/Node 검증은 직접 재실행하지 않고 root 원본 로그를 읽었다.

`root-source-freeze-verification.json`은 검토 HEAD와 worktree의 freeze 84개 source 일치, aggregate `684240daa66fd580d6569a6261ba9839ec577b806d2d3e822d464eea38e5c0dc`, mismatch 0을 기록한다. 새 서버 binary SHA는 `68f9f31b3187f5023bee93115caa287450603f8489b65d5511536671adc0cfb2`, account binary SHA는 `e32527b99e63a470d7d5aa8dde4306435f8e7908941b44522dd37fb2ef481722`다. 원본 target 보존 값과 PID health는 root가 소유 도구로 검증한 증거이며 본 리뷰가 기존 process를 직접 접근한 것은 아니다.

## 후속 package·문서 검토와 해결 상태

1. **Package blocker ADDRESSED:** `artifacts/improvements-20261025-package-fixed3/summary.json`과 9개 명령 로그에서 모두 exit 0을 확인했다. runtime은 정상 `cargo package`의 verify build를 수행했다. arena/server는 앞선 archive 추출 경로의 local patch와 `--exclude-lockfile --no-verify`로 archive를 만들고, 별도 checkout/archive consumer의 실행(`consumer_ok games=2 shutdown_completed=true`) 및 server archive의 `cargo check --locked --offline`을 수행했다. 후속 compile 성공과 archive 생성만 성공한 단계는 구분되어 있다.
2. runtime/arena/server 세 archive SHA, archive 안의 MIT LICENSE 전체 바이트, crate별 lockfile 포함 상태를 직접 재계산했다. 모두 summary와 일치한다. runtime만 lockfile을 포함하고 arena/server는 제외한다. 보존된 checkout/archive/server consumer lockfile 세 개의 SHA도 전부 일치한다. 원본 저장소 Cargo.lock의 현재 SHA는 전후 증거와 같은 `9eac7d6c46e40c63abea7881d151ecebe0e975cbe3c977d7e46efb9c7fd61fe0`다. 미게시 내부 dependency를 로컬로 대체한 소비 검증이라는 제한이 summary와 `docs/reuse.md`에 명시되어 있으므로 이번 범위에 수용한다. 이 결과는 lockfile을 포함한 배포 archive의 재현 설치나 `cargo install --locked` 검증을 뜻하지 않는다.
3. **실환경 증거 확인:** `artifacts/improvements-20261025-final-escalated/summary.json`의 success=true, browser 15항목, ui_probe 20개 전부 성공, dashboard OFF/ON × 실제 Godot 8/16명 네 load 성공, account 성공과 admin privacy 응답 193회 검사를 확인했다. desktop 648px/mobile 324px SVG의 별도 diagnostics JSON에서 range 61/62/63개, focus, 첫 점·중간 점 pointer, counter reset, null/zero가 모두 성공이다. account 원본 summary의 native Alice 45/Bob 41개와 transport 성공도 확인했다. 통제된 chart/history fixture와 실제 부하 결과는 별도로 판단한다.
4. live source aggregate 전후는 `3edf2f7c6a857e274f52c989ce6af70a804dad30d813a7bee35dfb72bad1b017`로 같고 server/account/Godot/browse binary 전후 SHA도 같다. owned process/browser stop/state cleanup은 true, cleanup_errors=[], administrator secret scan 174개 파일이다. **runner manifest 실제 개수는 72개**이며 이전 중간 보고의 70개를 정정한다. 본 리뷰가 72개 전부 현재 파일과 직접 비교해 mismatch 0을 확인했다. 별도 root freeze 84개 중 현재 차이는 `tools/package_smoke.py`와 `tools/tests/test_package_smoke.py` 두 개뿐이다. 새 `/tmp` 서버·계정 binary SHA도 live 성공 당시와 같다. 따라서 package 도구 변경 때문에 Rust·웹·Godot 통합 suite를 반복할 근거는 없다.
5. **문서 고지 ADDRESSED:** `docs/protocol.md:61`에 List의 마지막 PublishedRoom·비원자 snapshot 설명이 추가됐다. `docs/reuse.md`에는 MetricsSnapshot 필드 추가에 따른 exhaustive struct literal 변경 필요성과 package lockfile 제외·소비 한계가 명시됐다. `docs/project-improvements-2026-10-25.md`는 Rust 145/Node 23/Python 17, package fixed3, live 72와 root 84의 차이, 이전 실패 보존, CI 미실행을 정확히 구분한다. `docs/verification-results.md`는 과거 검증과 신규 검증 기록을 구분해 연결한다.

## Pre-push 승인 범위와 Delivery 잔여 조건

**Spec compliance: APPROVED. Code quality: APPROVED. Pre-push local verification: APPROVED.** 현재 작업 트리의 재검토 범위에 새 Critical/Important finding은 없다. package 방식은 실제 publish를 하지 않는 이번 목표와 고지된 제한 안에서 수용한다. 별도 staging 재패키징·manifest provenance 변환을 추가할 필요는 없다.

승인 당시 package source SHA-256:

- `tools/package_smoke.py`: `dda14fc8b1b97a88ddd1f7fbd143b105950e881f9bfba451bed4a22da25d3dfd`
- `tools/tests/test_package_smoke.py`: `1c989bdb47fd97b3170dba29a6bdb31eb193f4c6f6b875556713b0f8a2eaf5e8`

후속 package 변경 commit은 `8ce48d654c930f8aaad73b9e7cfdb4d3488f0a82`다. `artifacts/improvements-20261025-final-escalated/root-verification-logs/post-package-source-verification.json`은 해당 commit의 84개 source blob과 작업 트리 일치, 기존 freeze 대비 package 도구 두 파일만 변경, live 72개 불변 및 새 서버·계정·원본 target binary의 보존 값을 기록한다. 이는 본 리뷰의 직접 SHA 대조와 일치한다.

승인 후 정상 push로 실제 quick CI를 실행하고, 실행 commit SHA·run URL·결과를 기록해야 한다. scheduled/manual workflow는 실제 실행하지 않았다면 계속 ‘설정됨’으로 표기한다. 실제 CI 결과 확인 전에는 delivery 전체 완료로 판정하지 않는다. runtime/browser/Godot source가 변경되면 새 freeze 및 영향 있는 통합 검증이 필요하며, 문서의 승인/CI 상태 갱신만으로 제품 테스트를 반복할 필요는 없다.

## 범위 제한

이 검토와 loopback 테스트는 실제 crates.io publish/공개 registry 소비, WAN·TLS 배포, 10배 규모 용량, 장기 운영을 검증하지 않는다. bounded shutdown은 scheduler가 진행하는 동안의 API 대기 한도이며 동기 callback의 강제 종료 보장이 아니다. Pre-push 승인은 실제 GitHub CI·배포 완료를 의미하지 않는다.
