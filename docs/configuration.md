# 실행 설정

`cargo run -p campfire-server -- --help`가 실제 CLI 목록을 출력한다. 값은 모두 `--option VALUE`로 지정하며 `--game ID`를 반복하면 등록 목록에 추가하고, 다른 옵션은 마지막 값이 적용된다. `--game arena --game arena-b`는 같은 정적 arena 모듈을 서로 격리된 두 게임으로 등록한다.

CLI가 환경 변수보다 우선한다. 환경 변수 이름은 옵션의 `-`를 `_`로 바꾸고 `CAMPFIRE_`를 앞에 붙인다. 예: `--capacity 8` ↔ `CAMPFIRE_CAPACITY=8`. 등록 게임은 `CAMPFIRE_GAMES=arena,arena-b`이며 명시적 `--game` 목록이 이를 대체한다. `RUST_LOG`는 tracing 필터다.

| CLI 옵션 | 기본값 | 의미 |
| --- | ---: | --- |
| `--bind` | `127.0.0.1:3000` | 숫자 IP와 port; 공개 노출은 TLS reverse proxy 사용 |
| `--game` | `arena` | 등록 게임 ID; 1~64 ASCII 영숫자/`_`/`-`, 중복 금지 |
| `--capacity` | 16 | 룸 생성 정원의 서버 상한 |
| `--tick-hz` | 30 | 1~1000Hz |
| `--snapshot-hz` | 15 | 1~tick-hz |
| `--guest-ttl-ms` | 86400000 | 게스트 TTL 및 계정 session lifetime 상한 |
| `--reconnect-grace-ms` | 30000 | 단절/교체 자리 예약 |
| `--auth-timeout-ms` | 5000 | 첫 WS 인증 메시지 기한 및 계정 verifier 실행 기한 |
| `--cleanup-interval-ms` | 100 | 만료 session/rate key 정리 주기 |
| `--max-frame-bytes` | 16384 | JSON 메시지/frame, 계정 body·credential·claims 상한 |
| `--input-rate-per-second` | 60 | 연결별 input·command 공유 token bucket |
| `--input-burst` | 30 | 연결별 순간 burst |
| `--room-input-capacity` | 1024 | input queue와 별도 command FIFO 각각의 상한 |
| `--reliable-capacity` | 64 | 연결별 reliable event queue |
| `--request-cache-capacity` | 128 | 연결별 응답 cache 및 membership별 명령 결과 FIFO 상한 |
| `--max-input-seq-jump` | 10000 | 현재 generation의 마지막 수신 번호에서 허용하는 도약 |
| `--max-sessions` | 10000 | 전체 살아 있는 세션 상한 |
| `--max-connections` | 4096 | 전체 연결 상한 |
| `--max-unauthenticated-connections` | 256 | 미인증 WS·진행 중 HTTP verifier의 공유 상한 |
| `--max-rooms` | 1024 | 임시 생성 예약 포함 룸 상한 |
| `--max-rate-keys` | 4096 | 발급 주체별 token bucket key 상한 |
| `--guest-rate-per-second` | 2 | guest·account 공유 발급 주체별 초당 허용량 |
| `--guest-burst` | 10 | guest·account 공유 발급 주체별 burst |
| `--global-guest-rate-per-second` | 100 | guest·account 공유 서버 전체 발급 초당 허용량 |
| `--global-guest-burst` | 200 | guest·account 공유 서버 전체 발급 burst |
| `--rate-key-ttl-ms` | 60000 | 최근 발급 key 만료 |
| `--write-timeout-ms` | 2000 | 하나의 소켓 send future에 적용하는 재확인 기한 |
| `--max-write-timeouts` | 2 | 동일 send future의 연속 기한 초과 횟수; 초과 후 연결 종료 |
| `--max-abuse-violations` | 5 | 개인 크기/빈도 위반 종료 기준 |

0, 잘못된 타입, 알 수 없는 옵션, duration/정수 overflow 및 runtime 유한 상한을 넘긴 설정은 시작 전에 거부한다. `Config::validate()`가 in-process와 CLI에 같은 규칙을 적용한다. 현재 안전 상한은 정원 4096, capacity/frame/cache/session/connection/room/rate-key 계열 1048576, rate/burst/위반 횟수 1000000, duration 366일이다. frame 설정은 최소 256 bytes다. tick은 1~1000, snapshot은 1~tick이다. 입력 번호 도약은 0보다 큰 u64다. 제한을 높이면 프로세스 메모리 예산도 늘어난다.

룸 생성 settings의 framework 필드는 `capacity`(1~서버 capacity), `public`(boolean, 기본 true)다. 이 필드를 제거한 나머지를 게임 factory에 전달한다. arena는 `speed`(0.1~100, 기본 5), `boundary`(1~1000, 기본 10), `goal_radius`(0.1~boundary, 기본 0.5)만 허용한다. arena의 좌표 범위는 양 축 `[-boundary,boundary]`, 목표는 `(boundary/2,0)`이다. 설정 생략은 `{}`로 표현한다.

`arena`는 기본 Cargo feature다. 전용 서버는 composition root에서 필요한 crate/factory만 의존하고 등록한다. `cargo build -p campfire-server --no-default-features`는 arena 코드가 제외된 바이너리를 빌드하며 이 바이너리는 실행 시 등록 가능한 게임이 없다는 오류를 반환한다. `cargo build -p campfire-server --no-default-features --features arena`는 arena를 명시적으로 포함한다. `--game`은 이미 컴파일한 factory의 등록 ID를 고르는 옵션이며 동적 코드를 적재하지 않는다.

WebSocket writer는 `write_buffer_size(0)`와 유한한 `max_write_buffer_size(16MiB)`를 사용한다. 하나의 reliable 메시지를 보내는 future는 기한이 한 번 지났다고 폐기하지 않으며, 같은 future를 유지하다 설정한 기한 횟수까지 완료되지 않으면 해당 연결을 종료한다. 기본 설정의 총 대기 상한은 한 send당 약 4초다. 게임 모듈이 생성하는 공개·비공개 state와 명령 결과도 이 송신 buffer 예산을 고려해야 한다.

발급 빈도의 주체는 서버가 받은 실제 peer IP(`ConnectInfo`)다. client의 forwarded header는 신뢰하지 않는다. 신뢰하는 reverse proxy 뒤에서는 운영자가 검증한 peer 정보 adapter를 구성해야 하며, 그렇지 않으면 여러 사용자가 같은 proxy 주체의 발급 한도를 공유할 수 있다.

`Campfire::metrics()`는 현재 세션/연결/룸/rate-key 수, 입력 및 snapshot 수, 개인 위반과 공유 queue 과부하, queue 깊이 최댓값, tick 지연/처리 시간, 입력 대기/처리 시간 최댓값을 제공한다. 바이너리는 약 10초마다 이 값을 로그에 기록한다. 관리자 키를 설정하면 [대시보드](dashboard.md)의 인증된 cache 조회 API로도 확인할 수 있다.

## 관리자 대시보드

`CAMPFIRE_ADMIN_TOKEN`은 CLI 옵션이 없는 env-only secret이다. 미설정·빈 문자열은 OFF이며 `/admin`과 하위 경로가 모두 404다. nonempty 값은 32–256바이트 ASCII graphic 문자(`!`–`~`)여야 한다. 공백·제어 문자·Unicode·잘못된 길이는 서버 시작 전에 거부하며 자동 trim하지 않는다. `openssl rand -hex 32`는 유효한 64자 값을 만든다. 값을 terminal 출력·명령 인자·URL·로그에 넣지 말고 비밀 관리 도구를 통해 서버 환경과 로그인 입력에 공급한다.

기본 서버와 `--no-default-features --example account_server` 모두 이 변수를 읽는다. binary는 dashboard HTML/CSS/JS를 포함하므로 별도 정적 파일 서버가 필요하지 않다. 정적 shell은 공개하고 `/admin/api/status`, `/rooms`, `/history`는 정확한 Bearer 관리자 키를 요구한다. 게임 runtime token·계정 credential·cookie·query는 관리자 인증이 아니다. 서버를 재시작하면 키 변경이 반영되고 이력·boot ID가 초기화된다. [전체 운영 지침](dashboard.md)을 참고한다.

## 계정·명령·비공개 상태의 제한

확장은 새 `Config` 공개 필드나 CLI 옵션을 추가하지 않습니다. 기존 `Config { ... }` literal과 guest 설정을 유지하며 위 limits를 공유합니다. 기본 CLI는 guest-only이며 계정 검증기는 호스트 코드에서 `Campfire::with_authenticator()`로 제공합니다. [계정 통합 예제](reuse.md#기존-계정-검증기-연결하기)를 참고하세요.

- HTTP 계정 교환의 body와 credential, 검증기가 반환하는 claims의 직렬화 크기는 각각 `max_frame_bytes` 이내여야 합니다. issuer는 1~128 bytes, subject는 1~256 bytes이며 공백만 있는 값은 거부합니다.
- 계정 verifier와 미인증 WebSocket은 `max_unauthenticated_connections` permit을 공유합니다. 발급 rate bucket과 `max_sessions`도 guest/account가 공유합니다. verifier는 global state mutex 밖에서 실행하며 timeout·취소·실패 후 permit을 반환합니다.
- `AccountGrant::valid_for`는 **인증 요청 시작 시점 기준** 남은 유효기간입니다. session deadline은 그 시점부터 `min(valid_for, guest_ttl)`로 정합니다. 0이나 허용 duration 상한을 넘는 grant는 거부합니다. 실제 credential의 만료·권한 검증은 호스트 책임입니다.
- 동일 `(game_id, issuer, subject)`의 재발급은 살아 있는 session의 player ID와 room을 유지하면서 token을 교체합니다. 이전 token은 무효화합니다. 기존 재접속 예약은 늘리지 않으며 새 token이 더 일찍 만료하면 예약도 단축합니다.
- command FIFO는 input queue와 별도로 `room_input_capacity`개를 수용합니다. queue가 가득 차면 `SERVER_BUSY`이며 sequence를 소비하지 않습니다. actor가 처리한 terminal 결과는 membership별 `request_cache_capacity`개를 보관합니다. 이 상한은 연결의 request cache와 별개로 적용됩니다.
- `private_snapshot()`과 명령 결과는 trusted game output입니다. runtime은 결과 이력의 항목 수를 제한하지만 게임이 반환하는 임의 JSON의 전체 메모리를 격리하지 않습니다. 결과와 공개·비공개 상태는 작고 bounded하게 유지하세요.

기본 설정에서는 membership마다 최근 terminal 결과 128개만 full sync에 포함합니다. 퇴출된 sequence도 다시 실행하지 않지만 그 결과를 복구할 수는 없습니다. 과금·거래처럼 영속 처리가 필요한 작업에는 호스트의 저장·중복 처리 정책이 별도로 필요합니다.
