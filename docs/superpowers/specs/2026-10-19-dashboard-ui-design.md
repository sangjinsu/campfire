# Campfire 대시보드 UI 개선 설계

상태: 2026-10-19 구현을 위한 설계 메모. 기준 branch는 `codex/dashboard-ui-refresh`, base는 `31958b6`이다. 이 문서는 검증 결과나 최종 승인 기록이 아니다. 화면의 최종 치수와 구조는 frontend 변경을 고정한 뒤 실제 화면과 대조한다.

2026-10-23 갱신: frontend를 고정한 뒤 실제 브라우저 검증을 수행했다. 최종 chart plot 높이는 140px, 룸 표 viewport 최대 높이는 440px이며 넓은 화면 2열 chart·좁은 화면 1열 구성을 확인했다. 실행 결과와 증거, 독립 Architect 최종 승인 결과는 [UI 검증 보고서](../../dashboard-ui-2026-10-23.md)에 기록한다. 아래 검증 계획은 10월 19일 작성 당시 계획으로 보존한다.

## 목적과 적용 범위

운영자가 접속·룸 현황, 프로세스 자원, 시간에 따른 변화를 빠르게 읽고 많은 룸을 탐색하도록 정보 위계와 밀도를 정리한다. 조회 전용 기능과 [기존 대시보드 승인 계약](2026-10-18-dashboard-design.md)을 유지한다. API·protocol·인증·게임 동작·관리 변경 명령은 이번 변경의 대상이 아니다.

### 참고 자료와 적용 판단

참고 모음은 [VoltAgent/awesome-design-md의 design-md](https://github.com/VoltAgent/awesome-design-md/tree/main/design-md)다. 연구 담당자가 확인한 다음 문서의 원칙을 참고한다.

| 참고 문서 | 가져올 원칙 |
|---|---|
| [linear.app/DESIGN.md](https://github.com/VoltAgent/awesome-design-md/blob/main/design-md/linear.app/DESIGN.md) | surface 위계, 절제된 accent, 4/8 단위 spacing, 읽기 쉬운 type와 focus |
| [supabase/DESIGN.md](https://github.com/VoltAgent/awesome-design-md/blob/main/design-md/supabase/DESIGN.md) | 데이터 우선 배치, 1px border와 명도 구분, 의미를 구분하는 chart color, 8px spacing |
| [sentry/DESIGN.md](https://github.com/VoltAgent/awesome-design-md/blob/main/design-md/sentry/DESIGN.md) | 약 1.5 line-height, 주행동 우선순위, focus/disabled 상태, mobile 약 44px 조작 영역 |

이 자료는 marketing canvas를 바탕으로 한 inspired interpretation이며 해당 서비스의 공식 운영 제품 specification이 아니다. 위 원칙을 Campfire 운영 화면에 적용하는 구성·치수·색상 선택은 우리 설계 판단이다. 기존 forest `#142b28`, teal `#74c7b2`, amber `#edc58a`, 한국어 system font를 유지한다. self-only CSP를 유지하고 CDN·외부 font·외부 asset을 추가하지 않는다.

## 화면 구성

- **상단:** 제목·연결 상태·최종 갱신 시각을 먼저 읽게 하고, 수동 갱신·5초 자동 갱신·로그아웃을 compact toolbar에 정리한다. 숫자·단위·설명의 type 위계를 구분하고 panel의 겹친 장식을 줄인다.
- **현재 지표:** 접속·룸에 관한 기존 네 지표와 CPU/RSS 두 자원을 약 2:1 비율의 그룹으로 묶는다. 운영 수치와 프로세스 자원의 관계를 드러내되 모든 값의 기존 의미와 null 표현을 보존한다.
- **이력:** 연결·처리량·CPU·RSS 4개 차트를 넓은 화면에서 2열, 좁은 화면에서 1열로 배치한다. 운영 현황과 자원 변화를 각각 읽게 하고 실제 관측 기간을 표시한다.
- **룸 목록:** 필터와 페이지 상태를 표 가까이에 모은다. 표 viewport는 약 440px 높이로 제한하고 header는 sticky로 유지한다. 가로 넘침은 표 내부 scroll로 처리한다. 갱신 때 keyboard focus·선택·표 scroll 위치가 불필요하게 초기화되지 않도록 한다.
- **로그인과 상세:** 로그인은 키 입력과 접속 행동의 위계를 분명히 한다. drawer는 룸 식별 정보와 현재 metadata를 구분하고 닫기·focus 상태를 읽기 쉽게 한다.

모든 기존 DOM ID를 보존한다. 특히 룸 행 첫 cell의 `[game span, room button]` 구조를 유지한다. 이 구조에 의존하는 기존 제어 검사와 실제 브라우저 조작을 함께 보존한다.

## Chart 표시 계약

이력 저장은 기존 최근 15분·최대 900개의 실제 표본 계약을 유지한다. 화면의 x축은 관측 시작 직후부터 실제 표본 범위를 읽을 수 있도록 최소 약 10초에서 최대 15분까지 적응한다. 시각은 `HH:mm:ss`로 표시한다. 단일 표본도 dot으로 보여 주고 여러 표본이 쌓이면 실제 시간 간격을 반영한다.

누락 구간을 0이나 복제값으로 채우지 않고 선을 끊는다. null은 미측정으로 남기고 CPU 0%와 구분한다. CPU는 논리 코어 1개=100%이므로 100% 초과도 잘라내지 않는다. chart의 표시 범위 조정으로 이력 보관 기간·서버 sample·수집 간격을 바꾸지 않는다. 15분 전체가 관측되지 않았으면 관측됐다고 표현하지 않는다.

## 보존할 동작

관리자 키는 browser 메모리에만 보관하며 입력 후 입력창을 비운다. URL·storage·cookie·로그에 저장하지 않는다. 기존 5초 polling과 single-flight를 유지한다. 자동 갱신 OFF는 향후 polling을 중단하며 진행 요청의 완료를 허용한다. hidden/pagehide는 진행 요청을 취소한다. logout·401은 키와 화면을 비우고 request generation 검사를 통해 늦은 응답이 데이터를 복구하지 못하게 한다.

필터·검색·100개 keyset pagination, private 룸 metadata 범위, paused·초기 수집·빈 상태·오류·stale 구분도 유지한다. 변경 method나 관리 권한을 추가하지 않는다.

## 검증 계획과 완료 조건

아래 항목은 **검증 예정**이다. 실행한 명령·출력·artifact·최종 source와 binary provenance를 받은 뒤 별도 검증 보고서에 결과를 기록한다.

| 검증 | 확인할 내용 |
|---|---|
| Node 제어 검사 | 기존 9개 회귀, chart 0/1/11개 표본, 15분 경계, gap/null/0과 100% 초과 |
| 자산 제공 | 자산을 binary에 다시 포함하도록 build하고 serve된 HTML/CSS/JS hash와 고정한 소스를 대조 |
| 실제 브라우저 | 실제 104룸, 100+4 pagination, 필터/private/drawer, polling race, keyboard/focus, 표 scroll 유지 |
| 좁은 화면 | 390px viewport에서 실제 typography·조작 영역·2열에서 1열 전환·표 내부 scroll 확인 |
| 사용자 실행 대시보드 | `http://127.0.0.1:3000/admin`의 실제 0룸 상태에서 로그인과 5초 자동 갱신 확인. 가상 룸을 주입하지 않음 |
| 독립 검토 | 최종 diff·실행 증거·보고서를 Architect가 검토 |

게임 코드 변경이 없는 UI 범위이므로 8명·16명 및 native 계정 전체 부하 검사를 반복하지 않는다. 해당 기존 근거는 [2026-10-18 대시보드 검증](../../dashboard-verification-2026-10-18.md)에 있으며 이번 변경의 fresh 실행 결과와 구분한다. 외부 HTTPS 배포나 WAN·장기간 운영 검증을 이번 UI 검사 결과로 주장하지 않는다.

검증 artifact와 문서에는 관리자 키·환경 변수 dump를 기록하지 않는다. 실행 주소에는 키를 포함하지 않는다.
