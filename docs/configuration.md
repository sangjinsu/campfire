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
| `--guest-ttl-ms` | 86400000 | 발급 시점부터 토큰 TTL |
| `--reconnect-grace-ms` | 30000 | 단절/교체 자리 예약 |
| `--auth-timeout-ms` | 5000 | 첫 인증 메시지 기한 |
| `--cleanup-interval-ms` | 100 | 만료 session/rate key 정리 주기 |
| `--max-frame-bytes` | 16384 | JSON 메시지/frame 상한 |
| `--input-rate-per-second` | 60 | 연결별 게임 입력 token bucket |
| `--input-burst` | 30 | 연결별 순간 burst |
| `--room-input-capacity` | 1024 | 룸 bounded input queue |
| `--reliable-capacity` | 64 | 연결별 reliable event queue |
| `--request-cache-capacity` | 128 | 연결별 응답 재전송 cache |
| `--max-input-seq-jump` | 10000 | 현재 generation의 마지막 수신 번호에서 허용하는 도약 |
| `--max-sessions` | 10000 | 전체 살아 있는 세션 상한 |
| `--max-connections` | 4096 | 전체 연결 상한 |
| `--max-unauthenticated-connections` | 256 | 미인증 연결 상한 |
| `--max-rooms` | 1024 | 임시 생성 예약 포함 룸 상한 |
| `--max-rate-keys` | 4096 | 발급 주체별 token bucket key 상한 |
| `--guest-rate-per-second` | 2 | 발급 주체별 초당 허용량 |
| `--guest-burst` | 10 | 발급 주체별 burst |
| `--global-guest-rate-per-second` | 100 | 서버 전체 발급 초당 허용량 |
| `--global-guest-burst` | 200 | 서버 전체 발급 burst |
| `--rate-key-ttl-ms` | 60000 | 최근 발급 key 만료 |
| `--write-timeout-ms` | 2000 | 하나의 소켓 send future에 적용하는 재확인 기한 |
| `--max-write-timeouts` | 2 | 동일 send future의 연속 기한 초과 횟수; 초과 후 연결 종료 |
| `--max-abuse-violations` | 5 | 개인 크기/빈도 위반 종료 기준 |

0, 잘못된 타입, 알 수 없는 옵션, duration/정수 overflow 및 runtime 유한 상한을 넘긴 설정은 시작 전에 거부한다. `Config::validate()`가 in-process와 CLI에 같은 규칙을 적용한다. 현재 안전 상한은 정원 4096, capacity/frame/cache/session/connection/room/rate-key 계열 1048576, rate/burst/위반 횟수 1000000, duration 366일이다. frame 설정은 최소 256 bytes다. tick은 1~1000, snapshot은 1~tick이다. 입력 번호 도약은 0보다 큰 u64다. 제한을 높이면 프로세스 메모리 예산도 늘어난다.

룸 생성 settings의 framework 필드는 `capacity`(1~서버 capacity), `public`(boolean, 기본 true)다. 이 필드를 제거한 나머지를 게임 factory에 전달한다. arena는 `speed`(0.1~100, 기본 5), `boundary`(1~1000, 기본 10), `goal_radius`(0.1~boundary, 기본 0.5)만 허용한다. arena의 좌표 범위는 양 축 `[-boundary,boundary]`, 목표는 `(boundary/2,0)`이다. 설정 생략은 `{}`로 표현한다.

`arena`는 기본 Cargo feature다. 전용 서버는 composition root에서 필요한 crate/factory만 의존하고 등록한다. `cargo build -p campfire-server --no-default-features`는 arena 코드가 제외된 바이너리를 빌드하며 이 바이너리는 실행 시 등록 가능한 게임이 없다는 오류를 반환한다. `cargo build -p campfire-server --no-default-features --features arena`는 arena를 명시적으로 포함한다. `--game`은 이미 컴파일한 factory의 등록 ID를 고르는 옵션이며 동적 코드를 적재하지 않는다.

WebSocket writer는 `write_buffer_size(0)`와 유한한 `max_write_buffer_size(16MiB)`를 사용한다. 하나의 reliable 메시지를 보내는 future는 기한이 한 번 지났다고 폐기하지 않으며, 같은 future를 유지하다 설정한 기한 횟수까지 완료되지 않으면 해당 연결을 종료한다. 기본 설정의 총 대기 상한은 한 send당 약 4초다. 게임 모듈이 생성하는 공개 state도 이 송신 buffer 예산을 고려해야 한다.

발급 빈도의 주체는 서버가 받은 실제 peer IP(`ConnectInfo`)다. client의 forwarded header는 신뢰하지 않는다. 신뢰하는 reverse proxy 뒤에서는 운영자가 검증한 peer 정보 adapter를 구성해야 하며, 그렇지 않으면 여러 사용자가 같은 proxy 주체의 발급 한도를 공유할 수 있다.

`Campfire::metrics()`는 현재 세션/연결/룸/rate-key 수, 입력 및 snapshot 수, 개인 위반과 공유 queue 과부하, queue 깊이 최댓값, tick 지연/처리 시간, 입력 대기/처리 시간 최댓값을 제공한다. 바이너리는 약 10초마다 이 값을 로그에 기록한다. 별도 metrics HTTP endpoint는 제공하지 않는다.
