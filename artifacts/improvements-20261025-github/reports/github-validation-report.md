# Task 8 GitHub Actions 실제 실행 기록

두 실행 모두 GitHub가 보고한 `headSha`는 `45e87bf09e363aa147f63d3f8cd27aae6d9f24d8`이다. 실행 환경은 workflow의 `ubuntu-24.04`이다. 원본 run metadata, 단계 로그, 게시 artifact ZIP과 안전 추출본은 `artifacts/improvements-20261025-github/`에 보존했다. ZIP의 SHA-256은 GitHub artifact API digest와 일치했다. 추출 전에 절대 경로·상위 경로 이동·symlink·중복 이름·암호화와 확장 크기를 검사했다. `evidence-manifest.json`에 모든 추출 파일의 SHA-256과 크기를 기록했다.

## Runtime validation: 성공

수동 실행 [40158222582](https://github.com/sangjinsu/campfire/actions/runs/40158222582)의 `live-validation` job과 준비·Godot·account·dashboard·항상 업로드 단계가 모두 `success`로 완료됐다. GitHub 게시 artifact `runtime-validation-40158222582`의 ZIP SHA-256은 `5cf0db0a9a335ea24cf9ab9c9231133fc293743494667383f41ec801d453b11a`이고, 안전 추출 파일은 249개다. 근거: `runtime/run-metadata.json`, `runtime/workflow.log`, `runtime/artifacts-api.json`, `runtime/artifact.zip` 및 `runtime/extracted/`.

게시된 `manifest.txt`는 Rust/Cargo 1.96.0, Node 24.22.1, Python 3.12.3, Bun 1.4.3, Godot `4.7.2.stable.official.ed1daf0bf`, Playwright 1.62.1 및 gstack commit `e66c43c57209a17e2d9a79847a2e985c27ca80bd`를 기록한다. Linux 검증 서버 SHA-256은 `9ba3d1da8b945332789cf4c70f124c993f0e968b0c4c727acabd6e8a98a248e1`, 계정 예제는 `6c6267383622e28cb60b3de66a18dc27c750920b326574e2ac19ebaa38337f20`, Godot는 `8d106cbe6144c2dc7e881d61d2429c1a8a76e6b22ef48bd5e48dcf934953f71e`, browse는 `15eb65008d0c0ea00c436bf023a70b136ea176a48ef39fd8406cda264bc050be`다. 이 값은 macOS 로컬 바이너리 해시와 별도로 기록했다.

- `godot/summary.json`: 실제 Godot 8명·16명 시나리오 모두 성공, 각각 약 20.74초·20.79초 측정 및 공통 상태 비교 19회. UI integration도 성공.
- `accounts/summary.json`: 실제 guest/account transport와 서로 다른 native Godot 장면 Alice 45개·Bob 41개 점검 성공. owned process cleanup, source·binary 전후 해시 일치, 13개 파일의 자격 정보 검사 통과.
- `dashboard/summary.json`: 실제 gstack browser 15개 점검과 UI probe 20개 통과. Godot 부하 8/16명 × dashboard 비활성/활성 네 조합 성공. dashboard 활성 시 HTTP polling은 8명 216회, 16명 225회이며 오류 0회. account dashboard의 응답 237개에서 player ID/private 값 비노출을 확인했다. owned process 및 브라우저 stop/profile cleanup 모두 성공, 오류 0개, source·binary 전후 해시 일치, 173개 생성 파일의 관리자 비밀값 검사 통과.

브라우저 점검 중 지연 응답, visibility, 오류 응답, null/0, 차트 범위와 focus는 통제 fixture를 이용한다. 실제 Godot 부하·HTTP polling 값과 구분한다. 실행은 CI의 loopback/Linux 환경을 검증하며 외부망·TLS·다른 규모의 용량을 보증하지 않는다.

## Push CI: 실패

push 실행 [40158205928](https://github.com/sangjinsu/campfire/actions/runs/40158205928)의 `fast` job은 `Rust formatting`과 `Rust lint`까지 성공했으나 `Rust tests` 단계가 exit 101로 실패했다. 원문은 `crates/campfire-server/tests/headless.rs:1161:5`의 `issuer_and_global_guest_rate_limits_recover`에서 세 번째 issuer guest 요청의 상태가 기대 `429` 대신 실제 `201`이었다. 해당 headless integration test 파일은 17 pass·1 fail·4 ignored였다. 그 뒤 `Server without default features`, Node UI, Python runner/package tests, package/consumer 단계는 **skipped**이므로 이 run에서 통과했다고 주장할 수 없다. 실패에도 `Upload fast-check evidence`는 성공했다. 근거: `quick/run-metadata.json`, `quick/failed-steps.log`, `quick/extracted/rust-tests.log`.

게시 artifact `fast-checks-40158205928`의 ZIP SHA-256은 `88045896e493a6d47a81871c08bfaec04c7f963cc0731cfac59195bb0b891d77`이고 안전 추출 파일은 6개다. Rust 테스트 실패는 GitHub CI의 실제 실패이며, 로컬 통과 결과나 별도 runtime workflow 성공으로 대체하지 않는다. `guest_burst=2`, `guest_rate_per_second=10`의 테스트가 호출마다 새 HTTP client를 만드는 점을 보면 요청 사이 토큰 재충전 가능성이 있다. 이는 코드 읽기에 기반한 추론이며 원인은 별도 조사·수정·새 run으로 확인해야 한다.

원본 로그·metadata·추출본 등 241개 텍스트 파일의 token 형태 검사에서 탐지 0건이었다. 각 runner summary의 자격 정보 검사 결과는 그 실행이 생성한 파일 범위에 한정된다. 원본 ZIP, 원본 GitHub 로그, 실패 run metadata를 그대로 보존했으며 이 기록에서 rerun, workflow dispatch, cancel, 코드 수정은 하지 않았다.

## Quick CI 복구 실행: 성공

후속 test/helper 수정 커밋 `84af2abec020a722ac660fac479717dca17ff51f`의 push 실행 [40159325035](https://github.com/sangjinsu/campfire/actions/runs/40159325035)은 `fast` job과 fmt, clippy, Rust tests, no-default server, Node UI, Python runner/package tests, package/consumer, artifact upload의 모든 단계를 `success`로 완료했다. 원본 실패 run 40158205928의 `failure`와 건너뛴 단계 기록은 그대로 유지한다. 새 실행의 GitHub `headSha`는 정확히 `84af2abec020a722ac660fac479717dca17ff51f`이며 이전 Runtime validation의 head SHA `45e87bf09e363aa147f63d3f8cd27aae6d9f24d8`과 구분된다. 제품·UI·Godot 검증 소스가 바뀌지 않은 범위에 대한 기존 Runtime validation 재사용은 별도 Architect 승인에 따른다. 후속 커밋의 테스트 바이너리 SHA를 이전 빌드와 비교하여 제품 동일성을 주장하지 않는다.

새 `quick-recovery/extracted/rust-tests.log`에는 총 141 pass, 0 fail, 4 ignored가 기록되고, 이전 실패였던 `issuer_and_global_guest_rate_limits_recover`도 `ok`다. `node-tests.log`의 dashboard 시나리오 23건과 `python-tests.log`의 unittest 17건이 통과했다. `package-smoke/summary.json`은 runtime package 검증, checkout consumer, 추출 archive consumer, 추출 server archive check를 모두 `passed`로 기록한다. 세 crate archive의 SHA-256·파일 목록, 정규화 manifest의 Rust MSRV 1.96/MIT LICENSE, 세 consumer lockfile 및 원본 `Cargo.lock` 불변 증거가 함께 업로드됐다. arena/server archive에 `Cargo.lock`이 포함되지 않아 패키지의 재현 가능한 resolution 및 `cargo install --locked`는 검증하지 못했다. 공개 crates.io publish/download도 시도하지 않았다.

새 artifact `fast-checks-40159325035`의 ZIP SHA-256 `a8e11b3ab8694ffb6561ea8a121d9268638631fb3b16dec520ab1fb9ccfeaadd`는 GitHub API digest와 일치했다. 29개 member를 경로 이동·symlink·중복·암호화·확장 크기 검사 후 `quick-recovery/extracted/`에 추출했다. 원본 `quick-recovery/workflow.log`, `run-metadata.json`, `artifact.zip`, `artifacts-api.json`을 보존했고, 이 경로의 텍스트 26개 파일에서 token 형태 탐지 0건이었다. `evidence-manifest.json`에 새 실행 항목을 추가하면서 이전 두 실행 항목의 상태와 파일 해시는 바꾸지 않았다.
