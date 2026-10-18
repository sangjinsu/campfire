# 다른 Rust 서버에서 Campfire 사용하기

`campfire-runtime`을 library crate로 가져오면 기존 Axum 0.8 서버에 게스트·룸·WebSocket 기능을 합성할 수 있습니다. 새 게임은 `GameModule`/`Game`으로 구현합니다. 다른 웹 프레임워크에서는 Campfire를 별도 서비스로 실행하고 [HTTP/JSON WebSocket 프로토콜](protocol.md)로 연결할 수 있습니다.

## 적용 범위 먼저 확인하기

| 상황 | 현재 지원하는 방식 |
|---|---|
| 지속적인 이동 입력과 공개 상태를 사용하는 게임 | 게임 규칙을 `Game`으로 구현하고 runtime의 세션·룸·tick·snapshot을 재사용합니다. |
| 기존 Axum 0.8 애플리케이션 | `Campfire::router()`가 반환하는 `Router<()>`를 기존 router와 합성합니다. |
| 다른 Rust 웹 프레임워크 | Campfire 서버를 별도 프로세스·서비스로 두고 기존 프로토콜을 사용합니다. |
| 기존 계정, 영속 DB, 플레이어별 비밀 정보 | 해당 통합과 API 확장은 별도 구현 범위입니다. |

현재 runtime은 **플레이어당 tick 사이에 받은 최신 유효 입력 하나**를 적용합니다. 여러 카드 선택·거래·공격 명령을 도착 순서대로 모두 실행해야 하는 게임에는 별도 명령 처리 설계가 필요합니다. `Game::snapshot()`에는 수신 플레이어 인자가 없으며 같은 공개 게임 상태를 참가자에게 보냅니다. 수신자별 입력 처리 번호는 따로 제공하지만, 비밀 패 등 플레이어별 비공개 게임 view를 만드는 callback은 없으므로 해당 기능은 공개 API 확장이 필요합니다.

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

Campfire는 `/health`, `/v1/games/{game_id}/guests`, `/v1/games/{game_id}/ws`를 등록합니다. 기존 애플리케이션의 동일 route와 충돌하지 않게 구성하세요. 기존 router가 `Router<AppState>`라면 `.with_state(app_state)`로 필요한 state를 제공한 뒤 `Router<()>`를 합성합니다. 같은 state 타입을 요구하는 계약은 [Axum Router 문서](https://docs.rs/axum/0.8.9/axum/struct.Router.html#method.merge)에서 확인할 수 있습니다.

[`into_make_service_with_connect_info::<SocketAddr>()`](https://docs.rs/axum/0.8.9/axum/struct.Router.html#method.into_make_service_with_connect_info)는 게스트 발급 제한에 사용할 실제 peer IP를 제공합니다. 이를 생략해도 연결은 가능하지만 발급 주체가 모두 `unknown`으로 처리되어 같은 bucket을 공유합니다. `X-Forwarded-*` header는 자동으로 신뢰하지 않습니다. TLS reverse proxy 뒤에서 원래 사용자 IP를 사용하려면 운영 환경에 맞는 신뢰된 peer 정보 구성이 필요합니다.

예제처럼 root에 `merge`하면 기본 Godot 클라이언트와 바로 연결됩니다. 현재 Godot adapter는 URL의 path prefix를 지원하지 않으므로 `/campfire` 아래에 `nest`하는 구성에는 별도 client 또는 proxy 설정이 필요합니다.

## 자신의 게임 모듈 만들기

공개 계약은 [`game.rs`](../crates/campfire-runtime/src/game.rs)에 있고, 전체 구현 예제는 [`ArenaModule`](../crates/campfire-arena/src/lib.rs)입니다.

| 계약 | 구현할 내용 |
|---|---|
| `GameModule::create` | 룸별 `Game`을 생성합니다. `settings`를 검증하고 `GameContext`의 정원과 tick 간격을 사용합니다. |
| `player_joined` / `player_left` | 참가자 추가와 자리 해제를 게임 상태에 반영합니다. |
| `player_connection_changed` | 연결 상태 변경을 반영합니다. 단절된 참가자의 정지 등 게임 동작을 정합니다. |
| `validate_input` / `apply_input` | 입력을 검증하고 tick에 적용할 입력 상태를 갱신합니다. |
| `tick` | 전달받은 고정 `Duration`만큼 게임 상태를 진행합니다. |
| `snapshot` | 참가자들이 공유할 공개 게임 상태를 `serde_json::Value`로 반환합니다. |

Framework의 룸 설정인 `capacity`와 `public`은 runtime이 처리한 뒤 factory에 전달할 `settings`에서 제거합니다. 게임은 최종 정원을 `GameContext::capacity`에서 읽고 나머지 게임별 설정을 검증합니다. 생성한 모듈을 `Campfire::register_game()`으로 등록하면 됩니다. ID는 1~64자의 ASCII 영숫자·`-`·`_`를 허용하며 중복 등록은 거부합니다.

게임 callback은 동기 함수이며 신뢰하는 Rust 코드로 실행됩니다. blocking I/O와 긴 계산을 피하고, 외부 데이터는 별도 task에서 준비하는 구조가 필요합니다. runtime은 지연을 관측하지만 실행 중인 callback을 강제 중단하거나 OOM·process abort를 격리하지 않습니다.

게임 crate를 composition root의 의존성에 추가하고 필요한 모듈을 등록하세요. 기본 CLI의 `--game arena --game arena-b`는 같은 arena factory를 두 ID로 등록합니다. 새 규칙을 동적으로 읽어오는 옵션은 아니므로 다른 규칙은 crate와 등록 코드를 추가하고 다시 빌드합니다.

## 다른 프레임워크와 계정 시스템

Campfire를 별도 서비스로 실행하면 Rust 서버의 웹 프레임워크와 관계없이 클라이언트가 게스트 발급·WebSocket·룸 프로토콜을 사용할 수 있습니다. 공개 endpoint와 메시지 예제는 [프로토콜](protocol.md)에 있습니다.

현재 `session`, `room`, `transport` 구현과 룸 actor의 control/input API는 crate 외부에 공개되어 있지 않습니다. 따라서 Actix Web 등 다른 프레임워크에서 내부 룸 actor만 직접 호출하는 adapter를 만들려면 공개 API를 확장해야 합니다. `Campfire::router()`는 자체 게스트 인증 흐름을 제공하며 기존 계정·권한·DB를 자동으로 연결하지 않습니다. 게임 모듈 계약에는 특정 transport 타입이 없지만, runtime crate의 현재 HTTP/WebSocket transport는 Axum에 의존합니다.

현재 세션·방은 단일 노드 메모리에 있으며 재시작하면 초기화됩니다. 영속 저장소와 분산 directory는 별도 설계 범위입니다. 설정 가능한 상한과 발급 제한은 [설정 문서](configuration.md)를 참고하세요.

## 의존성 관리

위 path dependency 예제는 **Campfire 저장소 전체**를 유지하는 방식입니다. 하위 crate만 복사하면 `workspace.package`와 `workspace.dependencies` 상속을 해석할 수 없습니다. 팀 프로젝트에서는 저장소 경로를 고정하거나, 원격 push 후 Cargo Git dependency에 검증한 commit의 `rev`를 지정해 재현성을 관리할 수 있습니다. [Cargo 의존성 문서](https://doc.rust-lang.org/cargo/reference/specifying-dependencies.html)에서 path·Git 지정 방법을 확인하세요.

현재 이 가이드는 로컬 path dependency를 기준으로 합니다. crates.io 공개 배포나 프레임워크별 공식 adapter 설치를 전제로 하지 않습니다.
