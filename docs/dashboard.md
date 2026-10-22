# 서버 대시보드

Campfire binary에 포함된 조회 전용 화면입니다. 연결·세션·공개/비공개 룸·재접속 예약석, game별 요약, 프로세스 CPU/RSS와 최근 15분 전역 이력을 한 탭에서 확인합니다. 별도 Node 서비스·DB·CDN이 필요하지 않습니다. 룸 종료나 kick 기능은 제공하지 않습니다.

UI 개선과 실제 브라우저·사용자 실행 화면의 검증 범위는 [2026-10-23 UI 검증 보고서](dashboard-ui-2026-10-23.md)에 있습니다.

## 활성화와 로그인

기본값은 OFF입니다. 관리자 키를 서버 환경 변수 `CAMPFIRE_ADMIN_TOKEN`으로 공급하고 서버를 시작하세요. 아래 명령은 키를 출력하지 않습니다. 자신의 비밀 관리 도구로 동일한 값을 브라우저 로그인 입력에 공급하세요. shell tracing(`set -x`)을 사용하지 않습니다.

```sh
export CAMPFIRE_ADMIN_TOKEN="$(openssl rand -hex 32)"
cargo run -p campfire-server -- --bind 127.0.0.1:3000 --game arena --game arena-b
```

`http://127.0.0.1:3000/admin`을 열고 관리자 키를 입력합니다. 키는 32–256바이트 ASCII graphic 문자만 허용하며 빈 값은 OFF입니다. 잘못된 nonempty 값은 startup error입니다. 프로세스를 재시작해야 key 변경이 반영됩니다. 게임 참가용 runtime token과 계정 credential은 이 키를 대신할 수 없습니다. 개발용 계정 서버도 같은 환경 변수로 `/admin`을 활성화합니다.

입력 직후 password 필드를 비웁니다. token은 탭 메모리에만 남고 URL·localStorage·sessionStorage·cookie에 저장하지 않습니다. 로그아웃·401은 키와 화면 데이터를 지웁니다. 페이지 새로고침 후에는 다시 로그인합니다. 관리자 키는 여러 관리자에게 같은 관측 권한을 주는 shared secret이며 SSO나 사용자별 권한 체계가 아닙니다.

## 표시 값의 의미

| 표시 | 의미와 단위 |
|---|---|
| WebSocket 연결 / 세션 | 열린 연결 수 / 살아 있는 세션 수. 재접속 예약으로 세션이 연결보다 많을 수 있음 |
| 연결 / 정원 / 예약 | actor가 보유한 현재 seat 수. 연결+예약은 정원 이하 |
| CPU | 논리 코어 1개=100%. 여러 코어 사용 시 100% 초과 가능. cgroup quota 대비 비율이 아님 |
| RSS | API는 bytes, 화면은 MiB(1,048,576 bytes). 현재 프로세스의 실제 상주 메모리 |
| `—` | 첫 CPU 표본·측정 실패 등 null. 정상 idle의 CPU 0%와 구분 |
| 처리량 | 누적 counter 차이 / 실제 표본 경과시간. 첫 표본·counter 감소는 null |
| tick / snapshot sequence | 각 룸의 최신 관측 번호. 전체 룸 tick 합계를 서버 Hz로 표시하지 않음 |
| 누적 / 최댓값 | 이 프로세스 실행 이후 값. 재시작 시 초기화 |

collector는 1초마다 cache를 교체합니다. HTTP는 cache만 조회하고 game callback이나 룸 actor 응답을 기다리지 않습니다. runtime atomic metrics와 actor summary는 한 트랜잭션이 아니므로 전이 순간 합계가 일시적으로 다를 수 있습니다. 룸/cache와 성공 후의 자원 표본은 서버에서 3초를 넘으면 stale입니다. 자원 sampler가 최초 표본 자체를 발행하지 못하면 시작 후 3초를 넘긴 상태는 unavailable과 null이며, 첫 CPU 간격을 기다리는 warming-up과 구분합니다. 자원 sampler가 실패·정체되어도 룸 수집은 계속합니다. 현재 status와 rooms의 age는 조회 시 다시 계산하고, history의 process age·state는 **해당 표본 수집 당시 값**으로 보존합니다.

이력은 최근 15분과 최대 900개를 동시에 적용하며 실제 수집 표본만 저장합니다. 누락 구간을 0이나 복제값으로 채우지 않고 그래프의 선도 끊습니다. 방별 시계열은 저장하지 않습니다. 재시작은 새로운 boot ID와 빈 이력으로 시작합니다. 미측정 p95·HTTP 오류율을 만들어 표시하지 않습니다.

5초 자동 갱신과 수동 갱신은 중첩하지 않습니다. 자동 갱신 OFF는 미래 polling을 중지하며 현재 진행 중인 응답은 완료할 수 있습니다. hidden/pagehide와 로그아웃은 진행 요청을 abort하고 늦은 응답은 generation 검사로 무시합니다. 다시 보이면 자동 갱신 ON인 탭만 갱신합니다. paused와 연결 실패, 빈 룸, 초기 수집, 서버 stale를 구분하며 browser 자체의 갱신 지연은 마지막 성공 이후 10초를 기준으로 표시합니다.

## API와 비공개 범위

| 경로 | 조회 |
|---|---|
| `/admin/api/status` | 현재 지표, game별 합계, 자원과 freshness |
| `/admin/api/rooms` | game·공개 설정·룸 ID 검색, 최대 100개의 keyset 페이지 |
| `/admin/api/history` | boot ID와 `after_seq`를 사용한 최대 900개 증분 이력 |

API는 GET/HEAD에 동일한 Bearer 인증을 적용합니다. 인증 누락·불일치·중복 header는 401, 잘못된 cursor는 400, 변경 method는 405입니다. 모든 admin 응답은 `Cache-Control: no-store`, self-only CSP, frame 차단, no-referrer, nosniff를 사용합니다. shell·JS·CSS는 키 입력 화면을 위해 공개됩니다.

관리자는 private 룸의 존재·정원·연결·예약·tick·instance metadata를 볼 수 있습니다. JSON allowlist는 game settings/payload/public state/private state, command 결과, 계정 claims/credential, player ID를 포함하지 않습니다. `/admin` 데이터는 관리자에게만 제공하고 응답을 공개 cache에 저장하지 마세요. 전체 envelope와 필터 계약은 [승인 설계](superpowers/specs/2026-10-18-dashboard-design.md)에 있습니다.

## 외부 HTTPS reverse proxy

다음은 **설정 예시**이며 외부 TLS 배포의 실행 증거가 아닙니다. Campfire를 `127.0.0.1:3000`에 bind하고, DNS를 소유한 실제 domain으로 바꾼 Caddyfile을 사용합니다.

```caddyfile
campfire.example.com {
    reverse_proxy 127.0.0.1:3000
}
```

Caddy는 domain을 알고 DNS가 해당 서버를 가리키며 80/443 접근이 가능하면 HTTPS 인증서를 자동 관리합니다. WebSocket과 `/admin`을 같은 origin에서 root 경로로 전달하세요. backend 3000 포트는 loopback에 유지하고 방화벽에서 외부 접근을 차단합니다. 별도 host의 proxy라면 private network와 신뢰된 firewall 규칙을 명시적으로 구성합니다. [공식 reverse proxy 안내](https://caddyserver.com/docs/quick-starts/reverse-proxy)의 조건을 확인하세요.

이 예시는 access log를 켜지 않습니다. 운영상 access log를 추가할 때 Caddy의 기본 민감 header redaction을 유지하고 `log_credentials`를 활성화하지 마세요. `Authorization`·Cookie·Set-Cookie를 수집하는 debug/추가 proxy·APM 설정도 배제합니다. [Caddy log 공식 문서](https://caddyserver.com/docs/caddyfile/directives/log)는 Authorization 등 header의 기본 redaction을 설명합니다. 관리자 키를 query·URL·CLI argument로 전달하지 않습니다. 외부 script/font/asset 허용이나 CORS 허용을 추가할 필요가 없습니다.

`localhost`용 Caddy 인증서는 내부 CA 신뢰 설정이 필요할 수 있습니다. 인증서 신뢰 오류와 sandbox의 loopback `EPERM`은 다른 원인입니다. QA 도구의 권한 문제를 해결하려고 운영 TLS 검증을 비활성화하지 마세요. 이 저장소의 검증은 별도 명시가 없는 한 loopback HTTP이며 실제 외부 DNS·인증서·방화벽 배포를 확인하지 않습니다.

## 재현

기존 설치된 Godot와 gstack browse를 사용합니다. runner는 binary를 빌드하지 않으며 지정한 새 artifact 디렉터리만 생성합니다. 원본 서버와 기존 Godot 창에는 signal을 보내지 않습니다.

```sh
cargo build -p campfire-server --locked --offline
cargo build -p campfire-server --no-default-features --example account_server --locked --offline
python3 tools/validate_dashboard.py --suite all --output artifacts/dashboard-new-run
```

Cargo 작업과 측정을 겹치지 마세요. `--suite browser|load|accounts`로 분리할 수 있습니다. browser suite는 production Godot adapter로 104개 실제 룸을 만들고 gstack으로 실제 HTTP 화면을 확인합니다. race·visibility·warming-up 경계는 명시적인 JavaScript 제어 fixture이며 물리 OS 입력을 주장하지 않습니다. load는 동일 조건의 OFF/ON 각각 8·16개 실제 Godot 프로세스를 실행합니다. accounts는 기존 transport/두 native scene 전체 회귀를 재사용하면서 admin cache를 조회합니다. source와 binary hash가 실행 전후 같아야 성공입니다.
