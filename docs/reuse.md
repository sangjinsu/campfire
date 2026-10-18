# 다른 Rust 서버에서 Campfire 사용하기

`campfire-runtime`을 library crate로 가져오면 기존 Axum 0.8 서버에 게스트·계정 세션·룸·WebSocket 기능을 합성할 수 있습니다. 새 게임은 `GameModule`/`Game`으로 구현합니다. 다른 웹 프레임워크에서는 Campfire를 별도 서비스로 실행하고 [HTTP/JSON WebSocket 프로토콜](protocol.md)로 연결할 수 있습니다.

## 적용 범위 먼저 확인하기

| 상황 | 현재 지원하는 방식 |
|---|---|
| 지속적인 이동 입력과 공개 상태를 사용하는 게임 | 게임 규칙을 `Game`으로 구현하고 runtime의 세션·룸·tick·snapshot을 재사용합니다. |
| 기존 Axum 0.8 애플리케이션 | `Campfire::router()`가 반환하는 `Router<()>`를 기존 router와 합성합니다. |
| 다른 Rust 웹 프레임워크 | Campfire 서버를 별도 프로세스·서비스로 두고 기존 프로토콜을 사용합니다. |
| 기존 계정 | 호스트의 async `AccountAuthenticator`로 credential을 검증하고 game별 runtime token으로 교환합니다. |
| 단발 행동과 플레이어별 비밀 정보 | `validate_command`/`apply_command`와 `private_snapshot`을 구현합니다. |
| 영속 DB·분산 directory | 호스트의 별도 저장·운영 설계가 필요합니다. |

이동은 **플레이어당 tick 사이에 받은 최신 유효 입력 하나**를 적용합니다. 개별 처리가 필요한 행동은 별도 bounded FIFO의 `game.command`로 보내세요. `Game::snapshot()`은 모두가 공유하는 상태, `private_snapshot(player_id)`는 그 플레이어에게만 보낼 상태를 반환합니다. 명령 결과와 비공개 상태는 수신자별로 전달합니다.

## 기존 Axum 서버에 합성하기

아래 예제는 기본 arena를 사용하는 최소 서버입니다. Rust 1.96 이상을 사용하고, Campfire 저장소 전체를 `my-server`와 나란히 두세요.

```text
work/
├── campfire/
│   ├── Cargo.toml
│   └── crates/
└── my-server/
    ├── Cargo.toml
    └── src/main.rs
```

`my-server/Cargo.toml`:

```toml
[package]
name = "my-server"
version = "0.1.0"
edition = "2024"
rust-version = "1.96"

[dependencies]
campfire-runtime = { path = "../campfire/crates/campfire-runtime" }
campfire-arena = { path = "../campfire/crates/campfire-arena" }
axum = "0.8"
tokio = { version = "1", features = ["macros", "rt-multi-thread", "net", "signal"] }
```

`my-server/src/main.rs`:

```rust
use axum::{Router, routing::get};
use campfire_arena::ArenaModule;
use campfire_runtime::{Campfire, Config};
use std::{error::Error, net::SocketAddr};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let campfire = Campfire::new(Config::default())?;
    campfire.register_game("arena", ArenaModule)?;

    let app = Router::new()
        .route("/api/ping", get(|| async { "pong" }))
        .merge(campfire.router());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    let shutdown_runtime = campfire.clone();

    let result = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = tokio::signal::ctrl_c().await;
        shutdown_runtime.shutdown().await;
    })
    .await;

    campfire.shutdown().await;
    result?;
    Ok(())
}
```

`my-server`에서 `cargo check`로 컴파일하고 `cargo run`으로 실행하세요. `GET /api/ping`은 `pong`, `GET /health`는 `{"status":"ok"}`를 반환합니다. 이 서버가 켜져 있으면 [Godot 예제](../examples/godot/README.md)에서 기본 URL과 `arena`로 접속할 수 있습니다. 이미 3000 포트의 서버를 실행 중이면 먼저 다른 포트를 선택하세요.

`router()`는 만료 정리 task를 시작하므로 Tokio runtime 안에서 호출해야 합니다. 예제의 `#[tokio::main]`이 이를 제공합니다. Ctrl+C를 받으면 `Campfire::shutdown()`으로 WebSocket과 룸을 정리한 뒤 HTTP 서버 종료를 기다립니다. SIGTERM까지 처리하는 구성은 [기존 서버 실행 코드](../crates/campfire-server/src/main.rs)를 참고하세요.

### Router와 접속 정보

Campfire는 `/health`, `/v1/games/{game_id}/guests`, `/v1/games/{game_id}/sessions`, `/v1/games/{game_id}/ws`를 등록합니다. 기존 애플리케이션의 동일 route와 충돌하지 않게 구성하세요. 기존 router가 `Router<AppState>`라면 `.with_state(app_state)`로 필요한 state를 제공한 뒤 `Router<()>`를 합성합니다. 같은 state 타입을 요구하는 계약은 [Axum Router 문서](https://docs.rs/axum/0.8.9/axum/struct.Router.html#method.merge)에서 확인할 수 있습니다.

[`into_make_service_with_connect_info::<SocketAddr>()`](https://docs.rs/axum/0.8.9/axum/struct.Router.html#method.into_make_service_with_connect_info)는 게스트·계정 발급 제한에 사용할 실제 peer IP를 제공합니다. 이를 생략해도 연결은 가능하지만 발급 주체가 모두 `unknown`으로 처리되어 같은 bucket을 공유합니다. `X-Forwarded-*` header는 자동으로 신뢰하지 않습니다. TLS reverse proxy 뒤에서 원래 사용자 IP를 사용하려면 운영 환경에 맞는 신뢰된 peer 정보 구성이 필요합니다.

예제처럼 root에 `merge`하면 기본 Godot 클라이언트와 바로 연결됩니다. 현재 Godot adapter는 URL의 path prefix를 지원하지 않으므로 `/campfire` 아래에 `nest`하는 구성에는 별도 client 또는 proxy 설정이 필요합니다.

## 같은 프로세스의 대시보드 합성

`campfire-server::dashboard::{Dashboard, DashboardOptions}`가 env 검증·수집 task·정적 화면·인증 API를 제공합니다. `DashboardOptions::from_env()?`가 `Some(options)`일 때 `Dashboard::start(campfire.observer(), options)?`를 생성하고 기존 `campfire.router()`에 `dashboard.router()`를 merge합니다. handle은 serve 전체 기간 보유하고 serve의 정상·오류 종료 뒤 `dashboard.shutdown().await`를 호출한 다음 runtime을 정리하세요. OFF일 때는 runtime router만 사용합니다. 실행 가능한 전체 composition은 [기본 서버](../crates/campfire-server/src/main.rs)와 [계정 예제](../crates/campfire-server/examples/account_server.rs)에 있습니다.

관측 전용 `RuntimeObserver`는 runtime의 수명을 연장하지 않습니다. `snapshot()`은 보관된 룸 summary와 atomic metrics를 읽으며 게임 snapshot/private hook을 호출하지 않습니다. 별도 프로세스로 실행한 dashboard는 그 프로세스만 관측하므로 기존 게임 프로세스에 합성해야 합니다. [환경 변수·HTTP·HTTPS 운영 계약](dashboard.md)을 함께 적용하세요.

## 자신의 게임 모듈 만들기

공개 계약은 [`game.rs`](../crates/campfire-runtime/src/game.rs)에 있고, 전체 구현 예제는 [`ArenaModule`](../crates/campfire-arena/src/lib.rs)입니다.

| 계약 | 구현할 내용 |
|---|---|
| `GameModule::create` | 룸별 `Game`을 생성합니다. `settings`를 검증하고 `GameContext`의 정원과 tick 간격을 사용합니다. |
| `player_joined` / `player_left` | 참가자 추가와 자리 해제를 게임 상태에 반영합니다. |
| `player_joined_with_identity` | 초기 입장에서 검증된 `PlayerIdentity`를 받습니다. 기본 구현은 `player_joined`를 호출합니다. |
| `player_identity_changed` | 살아 있는 계정 session의 token rotation 시 identity·권한을 갱신합니다. 입장 hook은 중복 호출하지 않습니다. |
| `player_connection_changed` | 연결 상태 변경을 반영합니다. 단절된 참가자의 정지 등 게임 동작을 정합니다. |
| `validate_input` / `apply_input` | 입력을 검증하고 tick에 적용할 입력 상태를 갱신합니다. |
| `validate_command` / `apply_command` | 단발 행동을 검증·실행하고 요청자에게 돌려줄 결과를 만듭니다. 기본값은 `COMMAND_UNSUPPORTED`입니다. |
| `private_snapshot` | 수신자별 `Option<Value>`를 반환합니다. 기본값 `None`이면 `private_state`를 생략합니다. |
| `tick` | 전달받은 고정 `Duration`만큼 게임 상태를 진행합니다. |
| `snapshot` | 참가자들이 공유할 공개 게임 상태를 `serde_json::Value`로 반환합니다. |

Framework의 룸 설정인 `capacity`와 `public`은 runtime이 처리한 뒤 factory에 전달할 `settings`에서 제거합니다. 게임은 최종 정원을 `GameContext::capacity`에서 읽고 나머지 게임별 설정을 검증합니다. 생성한 모듈을 `Campfire::register_game()`으로 등록하면 됩니다. ID는 1~64자의 ASCII 영숫자·`-`·`_`를 허용하며 중복 등록은 거부합니다.

`apply_command`가 Err를 반환할 때는 게임 상태를 변경하지 않아야 합니다. runtime은 임의 Game mutation을 rollback할 수 없습니다. 명령·private hook도 기존 Game 구현에는 선택 사항입니다.

게임 callback은 동기 함수이며 신뢰하는 Rust 코드로 실행됩니다. blocking I/O와 긴 계산을 피하고, 외부 데이터는 별도 task에서 준비하는 구조가 필요합니다. runtime은 지연을 관측하지만 실행 중인 callback을 강제 중단하거나 OOM·process abort를 격리하지 않습니다.

게임 crate를 composition root의 의존성에 추가하고 필요한 모듈을 등록하세요. 기본 CLI의 `--game arena --game arena-b`는 같은 arena factory를 두 ID로 등록합니다. 새 규칙을 동적으로 읽어오는 옵션은 아니므로 다른 규칙은 crate와 등록 코드를 추가하고 다시 빌드합니다.

## 기존 계정 검증기 연결하기

`Campfire::new(config)`는 guest-only입니다. 실계정 연동은 기존 로그인 시스템을 소유한 호스트가 `AccountAuthenticator`를 구현하고 `Arc<dyn AccountAuthenticator>`를 `Campfire::with_authenticator(config, authenticator)`에 전달하는 방식입니다. 특정 IdP SDK나 사용자 DB를 Campfire가 설치하지 않습니다.

기존 async 인증 함수에 연결할 수 있는 최소 adapter입니다. `verify`에는 호스트 검증 함수를 지정합니다. 실제 network client나 DB pool을 보유한 호스트 타입에 trait을 직접 구현해도 됩니다. 공개 타입은 `campfire_runtime`에서 import할 수 있습니다.

```rust
use campfire_runtime::{
    AccountAuthenticator, AccountGrant, AuthError, Campfire, Config, ConfigError,
};
use std::{future::Future, pin::Pin, sync::Arc};

type Verification = Pin<Box<dyn Future<Output = Result<AccountGrant, AuthError>> + Send>>;

// Supply an adapter to your existing async authentication service.
struct HostAuthenticator {
    verify: fn(String, String) -> Verification,
}

impl AccountAuthenticator for HostAuthenticator {
    fn authenticate<'a>(
        &'a self,
        game_id: &'a str,
        credential: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<AccountGrant, AuthError>> + Send + 'a>> {
        (self.verify)(game_id.to_owned(), credential.to_owned())
    }
}

pub fn runtime_with_accounts(
    config: Config,
    verify: fn(String, String) -> Verification,
) -> Result<Campfire, ConfigError> {
    Campfire::with_authenticator(config, Arc::new(HostAuthenticator { verify }))
}
```

1. `authenticate(game_id, credential)`에서 기존 서비스의 token/session 검증을 비동기로 수행합니다. 서명·만료·대상 게임 권한을 확인하고 안정적인 issuer와 subject를 결정하세요. client가 보낸 identity·claims를 그대로 신뢰하지 않습니다.
2. 성공 시 `AccountGrant { identity: AccountIdentity { issuer, subject, claims }, valid_for }`를 반환합니다. credential 거부는 `AuthError::InvalidCredential`, 검증 서비스 장애는 `AuthError::Unavailable`입니다. `valid_for`는 **요청 시작 시점 기준** 유효기간이며 검증 중 걸린 시간만큼 권한 수명을 늘리면 안 됩니다.
3. 기존 Axum 합성 예제의 생성 부분을 `Campfire::with_authenticator(Config::default(), authenticator)?`로 바꿉니다. 나머지 game 등록·router·shutdown 흐름은 동일합니다.
4. client는 `/v1/games/{game_id}/sessions`에 credential을 보내 runtime token을 받은 뒤 기존 `session.authenticate`와 room 프로토콜을 사용합니다. guest endpoint도 계속 사용할 수 있습니다.

계정 key는 `(game_id, issuer, subject)`입니다. 살아 있는 동일 계정의 재발급은 기존 player ID와 room을 유지하고 token을 회전시킵니다. 이전 token·socket 작업은 거부하며 새 WebSocket에서 authenticate 후 resume해야 합니다. 동일 issuer/subject라도 다른 game은 독립된 session입니다. 기본 CLI는 guest-only이며 Godot **Account Inventory** UI는 credential 교환·private inventory·단발 명령을 제공합니다. 실제 provider 검증 설정은 호스트 애플리케이션에 둡니다.

`player_joined_with_identity`에서 `PlayerIdentity::Account`의 검증된 claims를 게임 내부 인벤토리·권한으로 반영하고, `player_identity_changed`에서 갱신하세요. issuer/subject/claims는 다른 참가자에게 자동 전송하지 않습니다. 게임의 공용 `snapshot()`에도 비밀을 넣지 마세요.

### 실행 가능한 개발용 계정 서버

[`account_server.rs`](../crates/campfire-server/examples/account_server.rs)는 두 credential을 검증하는 호스트와 inventory 게임을 함께 제공합니다. 실제 IdP를 대신하는 **개발용 verifier**이며 production 로그인 구현 예제가 아닙니다. credential은 환경 변수에서 읽고 로그에 출력하지 않습니다.

저장소 루트에서 서로 다른 임의의 개발용 값을 설정하고 실행하세요.

```sh
export CAMPFIRE_DEMO_ALICE_CREDENTIAL='local-alice-example'
export CAMPFIRE_DEMO_BOB_CREDENTIAL='local-bob-example'
cargo run -p campfire-server --no-default-features --example account_server
```

서버는 기본 `127.0.0.1:3001`에 game ID `inventory`를 등록합니다. 다른 로컬 서버와 포트가 겹치면 `CAMPFIRE_DEMO_BIND=127.0.0.1:3101`처럼 bind 주소를 설정할 수 있습니다. Godot 두 창에서 **Account Inventory**를 선택해 각각 Alice/Bob의 credential로 로그인하고 같은 방에 입장하면 공용 수집 횟수와 서로 다른 내 inventory를 볼 수 있습니다. [Godot 실행·adapter API·검증 runner](../examples/godot/README.md#계정-inventory)에 전체 조작법이 있습니다.

직접 HTTP/WebSocket 계약을 확인하려면 같은 환경 변수를 설정한 다른 terminal에서 Alice의 session을 발급합니다. 아래 curl은 runtime token을 응답으로 출력하므로 그 출력을 로그·스크린샷·공유 아티팩트에 저장하지 마세요.

```sh
curl -sS http://127.0.0.1:3001/v1/games/inventory/sessions \
  -H 'Content-Type: application/json' \
  --data "{\"credential\":\"$CAMPFIRE_DEMO_ALICE_CREDENTIAL\"}"
```

위 curl은 예제의 단순한 credential 문자열 기준입니다. 임의 특수문자가 있는 credential은 JSON encoder로 body를 만드세요. 응답 token으로 `ws://127.0.0.1:3001/v1/games/inventory/ws`에 접속한 뒤 아래 메시지를 순서대로 보냅니다. `<token>`을 실제 값으로 바꾸고, create 응답의 room·instance 값을 마지막 요청에 넣으세요.

```json
{"v":1,"type":"session.authenticate","request_id":"auth-1","token":"<token>"}
{"v":1,"type":"room.create","request_id":"create-1","settings":{"capacity":2,"public":true}}
{"v":1,"type":"game.command","request_id":"collect-1","room_id":"<room>","room_instance_id":"<instance>","command_seq":1,"payload":{"action":"collect","item":"wood"}}
```

공용 상태에는 수집 횟수, 자신의 `private_state`에는 inventory가 나타나며 명령 결과는 자신의 inventory 개수를 반환합니다. Bob은 같은 방에 들어가도 Alice의 inventory·결과를 받지 않습니다. 이 게임은 이동 입력 대신 command를 사용하는 예제입니다. 기존 arena의 guest·이동 확인은 [headless 예제](../crates/campfire-server/examples/headless.rs)와 Godot client를 사용하세요.

## 명령과 비공개 상태를 게임에 적용하기

`validate_command`와 `apply_command`는 room actor에서 동기 호출합니다. 검증 오류면 apply를 호출하지 않습니다. 성공·게임 오류 모두 terminal로 기록되므로 결과가 없어도 같은 `(room_instance_id, command_seq)`와 payload로 재시도하세요. 보관된 결과를 재생하고 동일 행동을 두 번 실행하지 않습니다. `SERVER_BUSY` 등 실행 전 거부는 번호를 소비하지 않으며, 같은 번호를 다른 payload에 재사용하면 충돌입니다.

membership마다 seq 1부터 연속 증가합니다. token rotation·재접속은 같은 membership의 이력을 유지하지만 leave 뒤 새 membership은 1부터 시작합니다. 보관 이력은 기본 128개이며 퇴출된 seq는 `STALE_COMMAND`로 거부하고 다시 실행하지 않습니다. 서버 재시작·membership 종료 이후의 영속 중복 방지는 호스트가 별도로 설계해야 합니다.

`private_snapshot(player_id)`은 수신자 전용 JSON 또는 None을 반환하세요. 공용 `state`와 별도 `private_state` 필드로 전송합니다. full sync(`room.entered`·`room.resumed`)에는 자신의 retained `command_results`와 `last_processed_command_seq`가 포함됩니다. 정기 snapshot에는 명령 번호와 현재 private state만 실립니다. client는 full sync 결과를 `(room_instance_id, command_seq)`로 중복 제거합니다. 자세한 실패·재시도 계약은 [프로토콜](protocol.md#단발-명령과-재시도)에 있습니다.

## 다른 프레임워크에서 연결하기

Campfire를 별도 서비스로 실행하면 Rust 서버의 웹 프레임워크와 관계없이 클라이언트가 게스트 발급·WebSocket·룸 프로토콜을 사용할 수 있습니다. 공개 endpoint와 메시지 예제는 [프로토콜](protocol.md)에 있습니다.

현재 `session`, `room`, `transport` 구현과 룸 actor의 control/input API는 crate 외부에 공개되어 있지 않습니다. 따라서 Actix Web 등 다른 프레임워크에서 내부 룸 actor만 직접 호출하는 adapter를 만들려면 공개 API를 확장해야 합니다. `Campfire::router()`는 게스트 발급과 호스트 verifier의 계정 교환을 제공합니다. 기존 계정·권한·DB 연결은 위 호스트 검증기를 통해 명시적으로 구성합니다. 게임 모듈 계약에는 특정 transport 타입이 없지만, runtime crate의 현재 HTTP/WebSocket transport는 Axum에 의존합니다.

현재 세션·방은 단일 노드 메모리에 있으며 재시작하면 초기화됩니다. 영속 저장소와 분산 directory는 별도 설계 범위입니다. 설정 가능한 상한과 발급 제한은 [설정 문서](configuration.md)를 참고하세요.

## 의존성 관리

위 path dependency 예제는 **Campfire 저장소 전체**를 유지하는 방식입니다. 하위 crate만 복사하면 `workspace.package`와 `workspace.dependencies` 상속을 해석할 수 없습니다. 팀 프로젝트에서는 저장소 경로를 고정하거나, 원격 push 후 Cargo Git dependency에 검증한 commit의 `rev`를 지정해 재현성을 관리할 수 있습니다. [Cargo 의존성 문서](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)에서 path·Git 지정 방법을 확인하세요.

현재 이 가이드는 로컬 path dependency를 기준으로 합니다. crates.io 공개 배포나 프레임워크별 공식 adapter 설치를 전제로 하지 않습니다.
