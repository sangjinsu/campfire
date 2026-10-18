# 대시보드 검증 — 2026-10-18

실제 브라우저 104룸, OFF/ON 각각 8·16개 Godot client, 계정 두 native 창의 86개 검사가 통과했습니다. 독립 Architect가 구현·최종 diff·검증 증거·운영 문서를 검토하고 **최종 APPROVED, 차단 사항 0건**으로 승인했습니다. 승인 범위에는 metadata 설명 1문장 정정과 그 provenance도 포함합니다. 외부 HTTPS 배포·WAN 성능·장기간 운영·대규모 CCU는 이 검증과 승인에 포함되지 않습니다. 원래 사용자가 실행한 서버와 Godot 창은 종료하거나 재시작하지 않았습니다.

## 실행 출처와 불변 조건

기준 branch는 `codex/server-dashboard`이며 실제 QA HEAD는 runtime checkpoint `c93885a`입니다. dashboard/server/UI 변경은 dirty working tree에서 검사하므로 commit ID만으로 소스를 재현했다고 주장하지 않습니다. 각 `summary.json`의 `git_status_before`, 전체 `source_manifest`, source SHA-256과 binary SHA-256 before/after가 실제 출처입니다. QA runner는 Cargo build를 수행하지 않습니다. coordinator가 Rust 검사와 두 binary 빌드를 순서대로 완료한 후 lease를 넘겼으며 측정과 Cargo 작업을 겹치지 않았습니다.

| 실행 binary | SHA-256 |
|---|---|
| `target/debug/campfire-server` | `c38b242934bd09b9a330bbce383481bfaf6618728b9baa4934bfc593e78c8249` |
| `target/debug/examples/account_server` | `7917758129c63cb6f007626cc9a29836af2da8769afc974fce93c0c371cb5b8b` |

최종 통합 실행은 다음 명령입니다.

```sh
python3 tools/validate_dashboard.py --suite all --output artifacts/dashboard-final-20261018-2328
```

[최종 summary](../artifacts/dashboard-final-20261018-2328/summary.json)는 모든 suite PASS입니다. source SHA-256 before/after는 `2a41c6939e9cc5f7026ef16f527936c28dc4211b3d9948e6f3aefb715ed1b55d`로 같고 위 두 binary 및 Godot hash도 그대로였습니다. 이 최종 manifest는 `tools/validate_godot.py` 자체를 명시적으로 포함합니다. 첫 browser 실행에는 이 기존 runner가 manifest에서 빠져 있어 보강한 뒤 전체를 같은 source로 다시 검증했습니다. 첫 artifact는 지우거나 덮어쓰지 않았습니다.

최종 browser는 [14개 그룹](../artifacts/dashboard-final-20261018-2328/browser-checks.json)을 통과했습니다. [desktop viewport 1440×1100](../artifacts/dashboard-final-20261018-2328/dashboard-desktop-viewport.png), [mobile viewport 390×844](../artifacts/dashboard-final-20261018-2328/dashboard-mobile-viewport.png), [원본 전체 화면](../artifacts/dashboard-final-20261018-2328/dashboard-mobile.png)을 함께 보존했습니다. mobile viewport 상단은 당시 scroll 위치 때문에 logo 일부가 잘렸고 full-page 원본에는 모두 있습니다. [geometry](../artifacts/dashboard-final-20261018-2328/browser-mobile-geometry.json)는 body/root client/scroll 폭 모두 390px, table client 352px/scroll 780px/overflow-x auto이므로 가로 넘침이 표 안에 제한됨을 보여줍니다. [CPU 0 제어 화면](../artifacts/dashboard-final-20261018-2328/dashboard-cpu-zero-controlled.png)은 synthetic zero가 `0`, unknown null이 `—`로 구분되는 actual browser 검사의 일부입니다.

실제 관리자 key 값은 artifact와 전용 gstack audit/log **155개 파일**에 없었습니다. [runtime log scan](../artifacts/dashboard-final-20261018-2328/runtime-log-scan.json)은 Godot/server 원본 log 63개에서 예상하지 않은 runtime/parse/error가 없음을 확인합니다. browser의 의도한 wrong-key 401은 별도 기록입니다. runner와 계정 하위 runner의 owned process 정리가 모두 완료됐습니다. 추가 계측 뒤 [읽기 전용 process 확인](../artifacts/dashboard-load-info-20261018-2331/process-preservation-check.json)에서도 원래 PID 63797/65028이 살아 있고 QA fixture Godot 잔여 process가 없는 것을 확인했습니다.

source manifest는 Cargo manifest/lock·Rust 소스·dashboard HTML/CSS/JS·dashboard driver/control tests·Godot project/scripts/scenes/UID를 포함합니다. docs와 artifact 자체는 제외합니다. 실행 중 이 소스나 binary가 바뀌면 성공하지 않습니다. 모든 runner는 새 출력 디렉터리를 만들고 자신이 만든 `Popen` 객체만 정리합니다. 최종 측정 종료 뒤의 metadata 설명 1문장 정정은 아래 provenance에 별도로 기록합니다.

## 실제 브라우저와 HTTP

첫 실행:

```sh
python3 tools/validate_dashboard.py --suite browser --output artifacts/dashboard-browser-20261018-2318
```

[첫 summary](../artifacts/dashboard-browser-20261018-2318/summary.json)는 PASS이며 source digest before/after가 `3540d72332c65f9d01f63f0962594ef4bc5f90f390b4beff3a333f7fa60747c5`로 같았습니다. 관리자 키가 artifact와 전용 gstack audit/log 22개 파일에 없는 것을 검사했고, owned process 정리도 완료했습니다.

실제 Godot production adapter / WebSocketPeer로 **104개 룸(2 game, public/private 각 52개)**을 생성·유지했습니다. 첫 페이지 100행, 다음 4행, 이전 페이지 복귀, game/private 필터, 실제 room ID 검색과 metadata drawer를 확인했습니다. [실제 room fixture](../artifacts/dashboard-browser-20261018-2318/real-room-fixture.json)와 [검사 범위](../artifacts/dashboard-browser-20261018-2318/browser-checks.json)에 상세가 있습니다.

| 원본 화면 | 범위 |
|---|---|
| [로그인](../artifacts/dashboard-browser-20261018-2318/login-desktop.png) | 실제 `/admin` shell |
| [빈 runtime](../artifacts/dashboard-browser-20261018-2318/empty-desktop.png) | 실제 HTTP의 0룸과 첫 CPU null |
| [desktop 전체](../artifacts/dashboard-browser-20261018-2318/dashboard-desktop.png) | 100행 첫 페이지 |
| [다음 페이지](../artifacts/dashboard-browser-20261018-2318/dashboard-page-two.png) | 남은 4개 실룸 |
| [private 상세](../artifacts/dashboard-browser-20261018-2318/dashboard-private-drawer.png) | arena-b의 비공개 metadata |
| [390px 전체](../artifacts/dashboard-browser-20261018-2318/dashboard-mobile.png) | 좁은 화면과 table 가로 스크롤 |
| [warming-up 제어 fixture](../artifacts/dashboard-browser-20261018-2318/dashboard-warming-controlled.png) | 실제 browser에서 null-data 상태를 제어한 화면 |

실제 HTTP로 잘못된/없는 Bearer의 401, HEAD 인증, 변경 method의 405, same-binary 자산과 no-store/CSP/nosniff를 확인했습니다. 실제 브라우저의 잘못된 키 로그인도 401로 거부됐습니다. [status](../artifacts/dashboard-browser-20261018-2318/http-status.json)·[rooms](../artifacts/dashboard-browser-20261018-2318/http-rooms.json)·[history](../artifacts/dashboard-browser-20261018-2318/http-history.json)는 실제 서버 응답입니다. [console](../artifacts/dashboard-browser-20261018-2318/browser-console.txt)의 오류는 의도한 잘못된 로그인 요청의 401 한 건입니다. [로그아웃 상태](../artifacts/dashboard-browser-20261018-2318/browser-cleared-state.txt)는 두 storage가 비고 cookie/입력값/room 행이 지워졌음을 보여줍니다.

레이스 검증은 gstack browser의 JavaScript에서 fetch 완료 시점·visibility event를 제어했습니다. auto OFF는 진행 응답을 허용하고 이후 5초 polling을 막는지, hidden은 abort하는지, visible은 갱신하는지, logout 후 늦은 실제 응답은 데이터를 되살리지 못하는지 확인했습니다. 401와 warming-up은 명시적인 synthetic response입니다. 이 검사는 물리 OS 클릭·실제 OS background 전환을 주장하지 않습니다. 900개/15분 이력 경계는 아래 Rust 가상 clock 근거를 사용하며 브라우저에서 15분을 실시간 기다리지 않았습니다.

로그인 키는 runner 메모리에 생성하고 서버 process env에만 공급합니다. browser 입력은 gstack 문서화된 `eval <file>`에 private `/tmp` 디렉터리의 0600 FIFO를 전달합니다. 평문 key 파일·CLI 인자·URL·JSON을 만들지 않고 메모리 JS를 pipe로 공급한 뒤 FIFO를 즉시 제거합니다. gstack audit에는 FIFO 경로만 남습니다. 실제 key 값으로 모든 artifact와 전용 daemon 파일을 scan합니다.

## Rust 회귀와 privacy 경계

coordinator의 fresh 검사는 workspace `--all-targets` **105 passed / 4 기존 장시간 ignored**, workspace `--no-default-features` **87 passed**, strict all-targets/all-features Clippy 및 fmt 통과입니다. no-default의 실제 명령은 `cargo test --workspace --no-default-features --locked --offline`이며 standalone arena 테스트도 포함합니다. [workspace](../artifacts/dashboard-rust-20261018-2326/workspace.log), [no-default](../artifacts/dashboard-rust-20261018-2326/no-default.log), [Clippy](../artifacts/dashboard-rust-20261018-2326/clippy.log), [계정 예제 빌드](../artifacts/dashboard-rust-20261018-2326/build-account.log), [서버 빌드](../artifacts/dashboard-rust-20261018-2326/build-server.log)의 원본 로그를 보존했습니다. UI의 제어 테스트 **9개**도 마지막 측정 이후 다시 통과했고 [원본 stdout](../artifacts/dashboard-rust-20261018-2326/node-ui-control.log)을 저장했습니다. 이 Node 검사는 작은 DOM fixture이며 실제 브라우저/레이아웃 검사와 구분합니다. 독립 Architect 최종 승인은 완료됐으며 실제 commit/main push 결과는 마지막 Git 전달 기록에 보존합니다.

backend 가상 clock 검사는 count/15분 동시 이력 제한, skip/gap, boot reset과 cursor 유실, rate counter 감소, 현재 cache/actor/resource age와 과거 process age 보존을 검사합니다. OS 검사에는 첫 CPU null·idle 0·100% 초과, 첫 publish 전 종료·지연, 기존 publish 이후 stale, collector 독립 진행, 단일 worker와 shutdown/Drop 경계가 포함됩니다. [backend tests](../crates/campfire-server/src/dashboard/tests.rs)와 [실제 HTTP tests](../crates/campfire-server/tests/dashboard.rs)를 참고하세요.

runtime의 실제 계정/room 관측 회귀는 credential·settings·claims·command/input payload·public/private state·command result sentinel을 심고 observer DTO에 없음을 검사합니다. API는 별도의 typed allowlist projection을 사용합니다. browser의 실제 API JSON에서도 identity·credential·settings·private state·command/payload 필드가 없는 것을 확인했습니다. 이 두 근거를 연결하며, browser fixture 자체가 sentinel payload를 가진 custom game을 실행했다고 주장하지 않습니다.

## 실제 8명·16명과 대시보드 관측

```sh
python3 tools/validate_dashboard.py --suite load --output artifacts/dashboard-load-20261018-2322
```

OFF8 → OFF16 → ON8 → ON16을 순서대로 실행합니다. 기존 `validate_godot.validate_soak`의 동일 snapshot sequence에서의 전원 공용 상태 비교·입력 ack·서버 이동·원격 정지·같은 guest/room/instance resume 검사를 그대로 사용합니다. 테스트용 `dashboard_bot.gd`는 기존 bot을 상속하고 단절을 1.4초 유지하여 1초 collector가 실제 reserved seat를 관측할 수 있게 합니다. OFF/ON 양쪽에 같은 fixture와 2초 grace를 적용합니다. 측정 구간은 각각 실제 20초이며 동기화 성공과 관측 overhead를 구분합니다.

ON은 실제 status cache를 약 10Hz로 추가 조회합니다. 정상 UI의 5초 polling보다 강한 QA용 부하입니다. 연결·예약·예약 해제 후 전원 복구와 poll latency를 raw JSON으로 남깁니다. OS `ps` CPU는 플랫폼별 평균이며, macOS에서는 최근 최대 1분의 decaying average이므로 dashboard의 sysinfo interval CPU와 같은 정의로 비교하지 않습니다. RSS는 `ps` KiB를 bytes로 변환하며 OFF/ON에 같은 방식으로 수집합니다. 단일 host·단회 순차 비교이므로 통계적 성능 보장이나 외부 네트워크 성능으로 확대 해석하지 않습니다.

최종 통합 실행의 부하 결과:

| 인원 | dashboard | client별 snapshot 간격 p95의 최댓값 | 동일 sequence 공용 상태 비교 | admin poll |
|---:|---|---:|---:|---|
| 8 | OFF | 69ms | 18개 | 없음 |
| 16 | OFF | 69ms | 18개 | 없음 |
| 8 | ON | 69ms | 18개 | 202회, 오류 0 |
| 16 | ON | 70ms | 18개 | 209회, 오류 0 |

ON8/16 모두 connected 8/16, reserved 1, 그 뒤 예약 0과 전원 연결 복구를 실제 cache에서 관측했습니다. poll latency 중앙값은 약 0.94/1.04ms, 관측한 process RSS 최댓값은 15,826,944/17,858,560 bytes입니다. 원본은 [ON8](../artifacts/dashboard-final-20261018-2328/load-enabled-8/dashboard-polls.json), [ON16](../artifacts/dashboard-final-20261018-2328/load-enabled-16/dashboard-polls.json)에 있습니다. 이 실행은 상속 환경 `RUST_LOG=warn`이어서 OFF의 tick-delay 최대값은 **미수집(null)**입니다. snapshot p95를 tick-delay로 대신 해석하지 않습니다.

OFF/ON 양쪽의 tick 지연은 소스·binary를 바꾸지 않은 추가 실행에서 직접 수집했습니다.

```sh
RUST_LOG=info python3 tools/validate_dashboard.py --suite load --output artifacts/dashboard-load-info-20261018-2331
```

[추가 summary](../artifacts/dashboard-load-info-20261018-2331/summary.json)도 PASS이며 source before/after `2a41c693…`, binary hash가 최종 all 실행과 같습니다. 각 서버의 기본 10초 INFO metrics log 세 표본(시작 직후 포함)에서 실제 lifetime maximum을 읽었습니다. tick 지연 열은 percentile이 아닙니다. [수치·환경·추출 범위 JSON](../artifacts/dashboard-load-info-20261018-2331/performance-comparison.json)에 원본 경로와 보조 지표의 정의를 보존했습니다.

| 인원 | dashboard | snapshot 간격 p95 최대 | INFO log tick 지연 최대 | `ps` CPU 중앙값 | `ps` RSS 최대 |
|---:|---|---:|---:|---:|---:|
| 8 | OFF | 70ms | 39.050ms | 4.80% | 14,188,544 bytes |
| 16 | OFF | 70ms | 28.700ms | 12.15% | 16,711,680 bytes |
| 8 | ON | 70ms | 21.730ms | 6.00% | 15,728,640 bytes |
| 16 | ON | 70ms | 81.225ms | 12.50% | 18,677,760 bytes |

ON16에서 단회 tick 지연 최대가 더 크게 관측됐습니다. ON8에서는 더 낮았으며 네 조건의 client snapshot p95는 모두 70ms였습니다. 짧은 순차 비교에서 수집한 실제 차이를 그대로 제시하며 동일 성능이나 원인별 overhead를 보장하지 않습니다. ON8/16 admin poll은 202/209회, 오류 0, 최대 연결·예약·전원 복구가 모두 관측됐고 동일 sequence의 공용 상태 비교는 각 조건 18개였습니다. [추가 log scan](../artifacts/dashboard-load-info-20261018-2331/runtime-log-scan.json)은 예상하지 않은 runtime/parse/error 0건입니다.

### 측정 후 metadata 설명 정정

측정 종료 후 driver의 `process-os-samples.json` 설명에서 `ps` CPU를 lifetime average라고 잘못 쓴 **한 문장만** 정정했습니다. 수집/계산/실행 코드는 바꾸지 않았고 원본 summary·raw 수치·잘못된 원문 metadata를 그대로 남겼습니다. macOS 정의는 [로컬 공식 man ps 발췌](../artifacts/dashboard-load-info-20261018-2331/macos-ps-man-excerpt.txt)로 확인했습니다.

- [실행 당시 driver 원본](../artifacts/dashboard-load-info-20261018-2331/source-before-metadata-fix.py)의 SHA-256 `72c9bf87c68182787cb8d42cff12525ead689d0cfff4bcef09b2bcf3b5a03d76`은 실행 manifest와 일치합니다.
- [1문장 unified patch](../artifacts/dashboard-load-info-20261018-2331/metadata-description.patch), [기존/최종 manifest·SHA errata](../artifacts/dashboard-load-info-20261018-2331/metadata-errata.json)를 저장했습니다.
- 최종 driver SHA-256은 `e008929d1d6698fc14d2d309f7f8c16aca333e44526d7be88bec39247652cea7`, 최종 source aggregate는 `f73cbfbbe5ffa0aeb232570c1170ba90e4d673dc37bfb843390d0c1ca0ff1bd7`입니다. 실행 manifest 65개 중 다른 항목은 이 driver 하나뿐입니다. Rust/UI 자산·binary는 같습니다.
- `python3 -m py_compile tools/validate_dashboard.py`는 정정 후 통과했습니다. 동작이 같은 metadata 문구 정정 때문에 전체 부하 검사를 반복하지 않았으며 독립 Architect가 이 차이와 보존한 provenance를 확인하고 최종 승인했습니다.

## 계정·private state의 실제 native 회귀

계정 suite는 기존 `validate_godot_accounts.py`의 transport 전체와 두 독립 native production scene을 재사용하면서 기본 서버와 account server에 관리자 관측을 활성화합니다. 실제 개인 inventory와 command 결과의 수신자 분리·동일 command 재시도·응답 유실·자연 timeout/resume·token rotation 등의 범위는 기존 runner와 동일합니다. Control signal과 생성한 입력 event로 조작하며 물리 OS 입력을 주장하지 않습니다. native PNG는 실제 Godot renderer의 결과입니다.

[계정 summary](../artifacts/dashboard-final-20261018-2328/account-regression/summary.json)는 Alice 45개 + Bob 41개, 총 **86개 검사 PASS**입니다. 서로 다른 native PID 43388/43389가 같은 room/instance의 public state `{players:2,total_collected:4}`를 보았고 개인 inventory 항목 수 4/2, 자신의 command 결과 수 3/1을 확인했습니다. [Alice PNG](../artifacts/dashboard-final-20261018-2328/account-regression/alice-shared.png)와 [Bob PNG](../artifacts/dashboard-final-20261018-2328/account-regression/bob-shared.png)는 실제 renderer 캡처입니다.

병행 admin HTTP response **195개**에서 실제 native player ID·`alice-starter-item`/`bob-starter-item` 및 private/command/claims/credential 필드가 없음을 검사했습니다. account native 서버에서 connected 2와 reserved 1을 관측했고 poll 오류는 0입니다. 이는 private game payload가 실제 존재하는 실행에서 API에 포함되지 않음을 확인하는 근거입니다. [native 기간 admin 응답](../artifacts/dashboard-final-20261018-2328/account-dashboard-polls-2.json)을 보존했습니다.

## Git 전달 기록

독립 Architect 최종 승인 후 다음 commit으로 변경을 전달했습니다.

| Commit | 전달 내용 |
|---|---|
| `c93885a` | runtime 관측 API와 수명 회귀 |
| `6b612ff` | 대시보드 기능과 두 composition root |
| `cbdab9c` | QA 도구와 fixture |
| `17ba96a` | 검증 증거와 운영 문서 |

첫 main push는 `origin/main`의 `f5b83a3`에서 **`17ba96a5db32facecb792a1d0c64753e2687c071`**로 정상 완료됐으며 main만 push했습니다. 이 문서의 전달 기록을 저장하는 후속 docs commit이 추가될 수 있으므로 이 SHA를 원격의 영구적인 최종 SHA로 표시하지 않습니다. 로컬 main과 원격의 최종 일치 검증은 후속 전달 기록 commit 이후 Git 담당/coordinator가 다시 수행합니다. 이 Git 전달은 외부 HTTPS 배포나 WAN·장기간 운영 성능을 검증한 결과가 아닙니다.
