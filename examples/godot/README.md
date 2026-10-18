# Campfire Godot 플레이 예제

Godot 4.7.2의 production scene과 `CampfireClient`를 함께 제공하는 샘플입니다. **Guest Arena**에서 이동과 공동 점수를, **Account Inventory**에서 계정 로그인·수집 명령·내 inventory를 확인합니다. 공개 방 생성·목록·입장·퇴장과 재접속은 두 모드가 공유합니다.

## 실행

저장소 루트에서 서버를 시작합니다. Rust 1.96 이상과 Godot 4.7.2가 필요합니다.

```sh
cargo run -p campfire-server -- --bind 127.0.0.1:3000 --game arena --capacity 16
godot --editor --path examples/godot
```

Godot에서 F5로 실행하거나 `scenes/main.tscn`을 연 뒤 F6으로 실행합니다. 기본 Server URL은 `http://127.0.0.1:3000`, Game ID는 `arena`입니다. 서버의 등록 게임 ID와 같아야 합니다. 편집기 없이 실행하거나 두 번째 클라이언트를 띄울 수도 있습니다.

```sh
godot --path examples/godot
godot --path examples/godot --position 80,80
```

첫 창에서 **게스트로 연결 → 최대 인원 8명 또는 16명 → 방 만들고 입장**을 누릅니다. 두 번째 창에서 **게스트로 연결 → 새로고침 → 방 선택 → 선택한 방 입장**을 누르면 같은 코트에서 만납니다. 목록 선택은 서버가 반환한 실제 `room_id`와 `room_instance_id`를 사용하며, 전체 식별자는 목록 tooltip과 현재 방 정보에서 확인할 수 있습니다. 빈 방 목록에서는 새 방을 만들면 됩니다. 방 목록과 방 식별자는 왼쪽 패널을 스크롤해 볼 수 있습니다.

코트를 클릭한 뒤 **WASD 또는 방향키**로 움직입니다. 금빛 목표 (5,0)에 먼저 닿으면 공동 점수가 한 번 증가합니다. 불빛 링은 나, 색 점은 다른 참가자, 회색은 재접속을 기다리는 예약 참가자입니다. 텍스트 입력란에 focus가 있거나 창이 focus를 잃으면 이동 입력을 멈춥니다. 위쪽 입력은 서버의 +y이며 화면에는 위쪽으로 표시합니다.

위치와 점수는 서버가 계산합니다. 클라이언트가 입력을 보낸 것과 서버가 처리한 것은 다르며, 하단 **입력 처리 확인**은 snapshot의 `last_processed_input_seq`입니다. 클라이언트는 위치를 먼저 예측해 이동시키지 않습니다.

## 재접속

**연결 끊기 (시험)**은 네트워크를 끊되 같은 게스트와 방 정보를 메모리에 보관합니다. **같은 게스트 재접속**을 누르면 새 WebSocket에서 인증하고 예약이 남아 있는 경우 자동으로 `resume`하여 최신 full sync를 받습니다. 기본 예약 시간은 30초입니다. 끊긴 동안 참가자는 정지하며 이전에 미처리한 입력을 재생하지 않습니다.

예약이 만료되면 목록을 새로고침해 다시 입장하거나 새 방을 만듭니다. **방 나가기**는 자리를 즉시 반납합니다. **게스트 종료**는 서버에 logout을 요청하고 로컬 token·방 정보를 폐기하므로 이후 새 게스트로 연결해야 합니다. Logout 응답이 확인되면 서버 자리 해제도 확인된 것입니다. 응답 없이 연결이 닫히거나 timeout이 발생해도 명시한 종료 의사에 따라 로컬 정보는 폐기하지만, 서버의 처리 완료를 확인한 것으로 간주하지 않습니다. 이 경우 서버 자리는 예약 만료까지 남을 수 있습니다. 일반 연결 끊기와 오류는 재접속용 자격증명을 보존합니다. 앱을 종료하면 token을 저장하지 않으므로 같은 게스트 복구를 할 수 없습니다. 서버가 재시작되면 세션과 방도 사라집니다. URL/Game ID/profile은 연결이 끊긴 상태에서 편집합니다. 편집하는 즉시 이전 token·방·개인 상태·미확인 명령을 폐기하며 **재접속**은 비활성화됩니다. 새 context에서 다시 로그인하세요.

토큰은 adapter 메모리에서만 취급하고 화면이나 로그에 표시하지 않습니다. 외부 서버는 HTTPS/WSS를 사용합니다. 다른 `game_id`의 방은 목록과 입장에서 격리됩니다.

## 계정 Inventory

개발용 verifier는 환경 변수의 두 credential을 Alice/Bob으로 구분합니다. 실제 IdP·DB 연동은 [호스트 계정 검증기](../../docs/reuse.md#기존-계정-검증기-연결하기)를 구현해야 합니다. 아래 값은 각자의 임의 값으로 바꾸고 terminal의 환경 변수에만 둡니다.

```sh
export CAMPFIRE_DEMO_ALICE_CREDENTIAL='choose-a-local-alice-value'
export CAMPFIRE_DEMO_BOB_CREDENTIAL='choose-a-different-local-bob-value'
# Optional: export CAMPFIRE_DEMO_BIND='127.0.0.1:3101'
cargo run -p campfire-server --no-default-features --example account_server
```

두 Godot 창에서 **Account Inventory**를 선택합니다. 기본 주소는 `http://127.0.0.1:3001`, game ID는 `inventory`이며 optional bind를 바꿨다면 주소도 맞춥니다. 한 창은 Alice 값을, 다른 창은 Bob 값을 masked credential란에 넣고 **계정 로그인**합니다. 입력란은 제출 즉시 비워집니다. 첫 창에서 방을 만들고 두 번째 창에서 **새로고침 → 방 선택 → 선택한 방 입장**합니다.

**wood / stone / key** 수집은 `game.command`를 한 번 보냅니다. 공용 수집 횟수는 같고, 내 inventory와 내 명령 결과는 계정마다 다릅니다. Inventory 모드는 이동 input을 보내지 않습니다. 결과를 기다리는 동안 새 수집 버튼은 잠깁니다. **미확인 명령 같은 내용으로 재확인**은 보관된 seq·payload를 재전송하며 이미 처리된 행동을 중복 실행하지 않습니다. 응답 timeout으로 연결이 끊기면 먼저 **같은 계정 재접속**으로 최신 상태와 retained 결과를 복원합니다.

**연결 끊기 (시험)**은 화면의 개인 상태·결과를 지우고 같은 membership의 복구 metadata를 메모리에 유지합니다. 같은 계정 credential로 다시 로그인하면 token이 회전하고 살아 있는 player·room·inventory를 복구합니다. 다른 계정으로 로그인하거나 logout·방 퇴장·context 편집을 하면 이전 개인 상태와 명령 metadata를 폐기합니다. 결과 보관 이력이 퇴출되어 `COMMAND_RESULT_UNAVAILABLE`가 나오면 결과를 성공으로 추정하지 않으며, 방에서 나가 새 membership으로 복구해야 합니다.

`CampfireClient`를 재사용할 때는 disconnected에서 `set_game_profile("inventory")` 후 `connect_account(url, game_id, credential)`를 호출합니다. `send_game_command(payload)`, `retry_pending_command()`, `has_pending_game_command()`, `can_retry_pending_command()`와 `private_state_changed`, `command_result_received`, `command_pending_changed` signal을 사용할 수 있습니다. `player_id`, `private_state`, `command_results`, `last_processed_command_seq`, `pending_command_seq`는 자신의 상태만 나타냅니다. profile은 schema 선택이고 game ID는 서버 namespace이므로 별개입니다.

## 검증

`CampfireClient`는 GUI와 실제 Godot headless 테스트가 공통으로 사용하는 adapter입니다. 단위 검증과 실제 scene smoke 검증은 다음과 같습니다.

```sh
godot --headless --editor --path examples/godot --import --quit
godot --headless --path examples/godot --script tests/client_unit.gd
godot --headless --path examples/godot --script tests/ui_smoke.gd
```

계정 transport와 실제 native Godot 창 두 개를 검증하는 stdlib runner입니다. 별도 임의 포트의 서버를 시작하고 랜덤 credential을 환경 변수로만 주입합니다. 기존 서버·창은 건드리지 않으며 자신의 모든 자식 프로세스를 종료합니다. macOS GUI 세션이 필요하고 `--headless`로 대체하지 않습니다. output은 새 디렉터리여야 합니다.

```sh
python3 -u tools/validate_godot_accounts.py --godot /opt/homebrew/bin/godot \
  --output artifacts/godot-accounts-local
```

실제 scene의 Control signal로 로그인·방·수집·재확인·재접속을 실행하며 OS 클릭을 주장하지 않습니다. 응답 유실은 socket 응답을 실제로 버려 재현합니다. 자연 6초 timeout/resume 검사와, transport request lock만 해제하는 별도 fixture를 사용한 같은 seq·payload 재확인 버튼 검사를 구분해 기록합니다. PNG는 양쪽 실제 renderer의 `frame_post_draw` 이후 저장하며 JSON에는 PID·display driver·검사·공용/private 상태와 source/binary SHA를 남깁니다. credential과 token은 저장하지 않습니다.

실제 두 native 계정 86개 검사와 계정 기능 추가 후 Arena 8명·16명 회귀 결과는 [계정 클라이언트 검증 보고서](../../docs/godot-accounts-2026-10-18.md)에 있습니다.

아래 Python 표준 라이브러리 runner는 서버를 빌드하고 임시 포트에서 실행한 뒤 **실제 Godot 프로세스 8개 및 16개**를 각각 띄웁니다. 기본 20초 시나리오와 별도의 실제 scene 버튼·키 입력·재접속 검증을 수행하고 마지막에 모든 자식 프로세스를 정리합니다.

```sh
python3 tools/validate_godot.py --godot /opt/homebrew/bin/godot --counts 8 16 --duration 20
```

실제 실행 결과와 검증 범위는 [Godot 검증 기록](../../docs/godot-verification.md)을 참고하세요. runner는 검증 환경에서 게스트 발급 burst/rate를 100으로 늘리고 재접속 예약을 2초로 줄여 만료를 확인합니다. 이는 사용자용 서버 기본값을 바꾸지 않습니다. 그래픽 세션이 없는 CI에서는 `--headless-ui`를 사용하되 화면 품질 검증은 별도로 진행해야 합니다.

실제 Godot 그래픽 viewport를 캡처하고 자동 종료하는 명령입니다. 저장 디렉터리를 먼저 만드세요. `--capture-delay`와 `--quit-after-seconds`는 초 단위입니다. headless DummyRenderer 캡처는 시각 검증 근거로 사용하지 않습니다.

```sh
mkdir -p artifacts/godot
godot --path examples/godot --rendering-method gl_compatibility -- \
  --server=http://127.0.0.1:3000 --game=arena --auto-create=16 \
  --capture="$PWD/artifacts/godot/ui-court.png" --capture-delay=2 --quit-after-seconds=3
```

화면 기본 크기는 1280×800이며 최소 900×600입니다. `.godot/` 캐시는 커밋하지 않으며 GDScript `.uid`는 버전 관리합니다. 처음 실행할 때 캐시를 생성할 수 있습니다. 한글은 macOS의 Apple SD Gothic Neo, Noto Sans CJK KR 등의 시스템 font를 사용하므로 Linux에서 한글이 표시되지 않으면 해당 font를 설치하세요.
