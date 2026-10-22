# 대시보드 UI 검증 — 2026-10-23

조회 전용 대시보드의 정보 위계·chart·룸 탐색 화면을 개선했습니다. 실제 브라우저 기본 **14개 그룹**, 추가 layout/history **20개 검사**가 통과했습니다. 실제 사용자 화면 `http://127.0.0.1:3000/admin`에서도 0룸 상태의 로그인과 5초 자동 갱신을 확인했습니다. 독립 Architect가 UI 변경·사용자 실행 확인·최종 문서를 검토하고 전체 APPROVED, 차단 0건으로 승인했습니다. 검증 소스는 QA manifest 기준이며 최종 전달 commit과 원격 SHA는 Git 이력 및 최종 응답에서 확인합니다.

## 변경과 참고 자료

운영 접속·룸 네 지표와 CPU/RSS 두 자원을 약 2:1로 묶고, compact toolbar와 type/surface 위계를 정리했습니다. 연결·처리량·CPU·RSS 4개 차트를 넓은 화면에서 2열, 좁은 화면에서 1열로 배치했습니다. chart plot 높이는 140px이며 실제 관측 기간에 따라 약 10초부터 최대 15분까지 x축을 조정하고 `HH:mm:ss`를 표시합니다. 단일 표본은 dot으로 표시합니다. 룸 표는 최대 440px 높이와 sticky header를 사용하며 일반 갱신은 scroll과 룸 button focus를 보존하고 페이지·필터 변경은 표 scroll을 초기화합니다. 로그인과 metadata drawer의 주행동·상태 위계도 정리했습니다.

사용자가 지정한 [VoltAgent/awesome-design-md](https://github.com/VoltAgent/awesome-design-md/tree/main/design-md)의 [Linear](https://github.com/VoltAgent/awesome-design-md/blob/main/design-md/linear.app/DESIGN.md), [Supabase](https://github.com/VoltAgent/awesome-design-md/blob/main/design-md/supabase/DESIGN.md), [Sentry](https://github.com/VoltAgent/awesome-design-md/blob/main/design-md/sentry/DESIGN.md) 문서에서 surface·spacing·데이터 우선 배치·focus/disabled·touch target 원칙을 참고했습니다. 이 자료는 marketing canvas에 대한 inspired interpretation이며 각 서비스의 공식 운영 제품 specification이 아닙니다. 이를 운영 화면에 적용한 구성과 기존 forest `#142b28`·teal `#74c7b2`·amber `#edc58a` palette 유지 결정은 Campfire의 설계 판단입니다. 한국어 system font와 self-only CSP를 유지하며 외부 font·CDN을 추가하지 않았습니다. [10월 19일 설계 메모](superpowers/specs/2026-10-19-dashboard-ui-design.md)에 범위와 보존 계약이 있습니다.

변경 source는 dashboard HTML/CSS/JS와 Node 제어 검사입니다. API·protocol·인증·게임 코드는 변경하지 않았습니다. 기존 DOM ID와 룸 첫 cell의 `[game span, room button]`, 5초 single-flight, OFF·hidden·logout·401·request generation 동작을 보존했습니다.

## 검증 출처

branch는 `codex/dashboard-ui-refresh`, 기준 HEAD는 `31958b67d7412979a0d67e1d53a6f48d8a6d8327`입니다. UI 변경은 dirty working tree에서 검사했으므로 HEAD만으로 검사 소스를 재현했다고 주장하지 않습니다. [기본 summary](../artifacts/dashboard-ui-20261023-074050/summary.json)의 65개 source manifest와 before/after hash가 실제 출처입니다. source aggregate는 `c76f4a4b7b1e359ecaaf580c85d49ab0dec03a0009dfdae91243ec216648c5fe`로 실행 전후 동일했습니다. 추가 검사에서도 source와 server binary가 같았습니다. 문서와 artifact 자체는 source aggregate에 포함하지 않습니다.

| 실행 binary | SHA-256, 실행 전후 동일 |
|---|---|
| server | `b1338640176776557099605aeaf7046fbd25979b9fea6ffc20551f6459ed34f7` |
| account server | `49573e8a53f3d0a08af1fe170ec701eee43ef66813d7f77930c109b24425a9ff` |
| Godot | `c7cccbf8fb143e34e02fd6521e09be2c2b974f0d5db080b19071c9c570718ccf` |

기본 browser 실행은 다음과 같습니다. runner는 binary를 build하지 않습니다.

```sh
python3 tools/validate_dashboard.py --suite browser --output artifacts/dashboard-ui-20261023-074050
```

[10월 23일 Node 원본 로그](../artifacts/dashboard-ui-20261023-074050/root-verification-logs/node-20261023.log)는 **14개 scenario 통과**입니다. [보존된 dashboard unit 로그](../artifacts/dashboard-ui-20261023-074050/root-verification-logs/unit.log)의 **12 passed**는 앞선 구현 단계 결과이며 이번 재개에서 새로 실행한 Rust 검사로 표현하지 않습니다. 앞선 coordinator 도구 출력에는 HTTP 5개 통과와 server/account 두 build exit 0가 있으나 해당 raw 로그 세 파일은 현재 없습니다. 이를 복원하거나 존재하지 않는 원본 링크를 만들지 않았습니다. 보존된 파일과 누락 범위는 [로그 출처 index](../artifacts/dashboard-ui-20261023-074050/root-verification-log-index.json)에 있습니다. 이번 실제 HTTP 제공 자산 세 개는 [추가 검사](../artifacts/dashboard-ui-20261023-074050/additional-ui/summary.json)와 [사용자 서버 검사](../artifacts/dashboard-ui-20261023-074050/production-served-assets.json)에서 각각 source SHA와 일치했습니다.

## 실제 브라우저와 제어 fixture

별도 loopback 서버에서 production Godot adapter로 **104개 실제 룸**을 유지했습니다. [기본 14개 그룹](../artifacts/dashboard-ui-20261023-074050/browser-checks.json)은 첫 100행·다음 4행·이전 페이지, game/private 필터·정확한 룸 검색·metadata drawer, 인증·read-only method·보안 header·API privacy allowlist와 화면 상태를 확인합니다. [fixture 출처](../artifacts/dashboard-ui-20261023-074050/real-room-fixture.json), [desktop](../artifacts/dashboard-ui-20261023-074050/dashboard-desktop-viewport.png), [mobile](../artifacts/dashboard-ui-20261023-074050/dashboard-mobile-viewport.png), [private drawer](../artifacts/dashboard-ui-20261023-074050/dashboard-private-drawer.png)를 보존했습니다.

[추가 20개 검사 summary](../artifacts/dashboard-ui-20261023-074050/additional-ui/summary.json)는 실패 0건입니다. 실제 Chromium geometry와 SVG transform을 적용한 글자 크기를 측정했으며 source의 font-size 값만 읽은 결과와 구분합니다.

| 검사 | 실제 결과 |
|---|---|
| 1440px / 390px SVG label 최소 표시 크기 | 14px / 13.5px, label bbox 겹침 각각 0건 |
| 조작 영역 | 검사한 target의 44px 기준 미달 0건 |
| 100행 표 | viewport 높이 440px, 내부 scroll 높이 8245px, header는 sticky |
| 390px 가로 범위 | body/root 390px, 표 client 356px / scroll 814px로 표 내부에 가로 scroll 제한 |
| 실제 5초 갱신 | sample 11→16, 표 scrollTop 300과 동일 룸 button focus 유지 |
| 페이지·필터 전환 | 다음 페이지 4행, 페이지·필터 변경 후 표 scrollTop 0 |

[1440px geometry](../artifacts/dashboard-ui-20261023-074050/additional-ui/layout-1440.json)·[PNG](../artifacts/dashboard-ui-20261023-074050/additional-ui/actual-layout-1440.png), [390px geometry](../artifacts/dashboard-ui-20261023-074050/additional-ui/layout-390.json)·[PNG](../artifacts/dashboard-ui-20261023-074050/additional-ui/actual-layout-390.png)에 원본이 있습니다. sticky header의 화면 Y는 표 viewport Y와 같았습니다.

이력 0/1/11개 표본·15분 경계·gap/null·CPU 0→125는 **실제 브라우저에서 fetch 응답을 제어한 fixture**입니다. 15분을 실시간 대기하거나 해당 CPU를 OS에서 발생시킨 결과가 아닙니다. [제어 assertion 8개](../artifacts/dashboard-ui-20261023-074050/additional-ui/controlled-history-assertions.json)와 [11표본 화면](../artifacts/dashboard-ui-20261023-074050/additional-ui/controlled-history-eleven.png), [gap/null/multicore 화면](../artifacts/dashboard-ui-20261023-074050/additional-ui/controlled-history-gap-null-multicore.png)을 보존했습니다. 단일 dot·초기 시간 범위·실제 시각 label·15분 범위·누락 구간 단절·미측정과 정상 0·100% 초과 표시를 확인합니다.

polling race도 JavaScript 제어를 포함합니다. OFF는 진행 응답 완료를 허용하고 이후 polling을 막으며, visibility event는 hidden abort와 visible refresh를 확인합니다. logout 후 늦은 실제 응답은 화면을 복구하지 못합니다. 401·warming-up은 명시적인 synthetic response입니다. 이 검사는 물리 OS 입력이나 실제 OS background 전환을 주장하지 않습니다.

## 사용자 실행 화면과 비밀 보관

사용자 서버 `http://127.0.0.1:3000/admin`은 테스트 서버와 분리했습니다. 104개 테스트 룸을 이 서버에 주입하지 않았습니다. [최종 visible 확인](../artifacts/dashboard-ui-20261023-074050/production-visible-verification-final.json)에서 **실제 0룸**, 연결됨·visible·자동 갱신 ON 상태였고 sample은 **225→230**으로 증가했습니다. 키 입력란은 비었고 localStorage/sessionStorage 각 0개, cookie도 비었습니다. [실제 사용자 화면](../artifacts/dashboard-ui-20261023-074050/production-visible-zero-rooms-final.png)을 보존했습니다. 이 확인 시각은 2026-10-23 07:43:19 KST이며 이후의 상시 가용성을 보장하는 기록은 아닙니다.

키는 detached 실행 manager의 RAM·서버 환경·browser RAM에서만 사용하며 일반 파일·CLI 인자·URL·storage·환경 dump에 기록하지 않습니다. manager에는 키를 반환하는 명령이 없으며 제어 FIFO는 0600입니다. 이 운영 보조 도구와 임시 코드는 제품에 포함하지 않습니다. 브라우저 검사끼리는 state directory와 Chromium profile을 분리합니다.

[최종 비밀 scan](../artifacts/dashboard-ui-20261023-074050/production-secret-scan-final.json)은 사용자 화면 관련 242개 파일에서 실제 키 누출 0건입니다. 기본 검사 27개·추가 검사 21개 파일도 누출 0건이며, 이 수치는 각 검사 범위이므로 서로 더해 고유 파일 수로 해석하지 않습니다. [runtime log scan](../artifacts/dashboard-ui-20261023-074050/runtime-log-scan.json)은 4개 log에서 예상하지 않은 오류 0건입니다. browser의 의도한 wrong-key 401은 별도입니다. 기본·추가 검사가 만든 owned process는 정리됐고 사용자 실행 화면은 유지했습니다. 종합 출처는 [QA rollup](../artifacts/dashboard-ui-20261023-074050/qa-rollup.json)에 있습니다.

## 검증 한계와 검토 상태

이번 범위에는 게임 코드 변경이 없어 8명·16명 부하와 native 계정 전체 회귀를 다시 실행하지 않았습니다. 해당 근거는 [2026-10-18 검증](dashboard-verification-2026-10-18.md)이며 이번 UI의 fresh 실행과 구분합니다. account binary hash 일치는 계정 native 전체 회귀를 다시 실행했다는 뜻이 아닙니다. 외부 HTTPS 배포·WAN 성능·장기간 운영·대규모 CCU도 이번 검사에 포함하지 않습니다.

독립 Architect가 source·QA 증거·사용자 실행 확인·최종 보고서를 검토하고 전체 APPROVED, 차단 0건으로 승인했습니다. 이 승인은 외부 HTTPS 배포나 Git 전달 완료를 의미하지 않습니다. 검증 소스는 QA manifest 기준이며 최종 전달 commit과 원격 SHA는 Git 이력 및 최종 응답에서 확인합니다.
