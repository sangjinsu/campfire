# Campfire Godot 플레이 예제

Godot 4.7.2로 기존 Rust arena에 접속하는 작은 플레이 클라이언트입니다. 새 게스트로 연결하고 공개 방을 만들거나 목록에서 방을 골라 입장할 수 있습니다. 실제 서버 snapshot에 담긴 나와 다른 참가자의 위치, 연결 상태, 공동 점수와 처리 완료 입력 번호를 표시합니다.

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

예약이 만료되면 목록을 새로고침해 다시 입장하거나 새 방을 만듭니다. **방 나가기**는 자리를 즉시 반납합니다. **게스트 종료**는 서버에 logout을 요청하고 로컬 token·방 정보를 폐기하므로 이후 새 게스트로 연결해야 합니다. Logout 응답이 확인되면 서버 자리 해제도 확인된 것입니다. 응답 없이 연결이 닫히거나 timeout이 발생해도 명시한 종료 의사에 따라 로컬 정보는 폐기하지만, 서버의 처리 완료를 확인한 것으로 간주하지 않습니다. 이 경우 서버 자리는 예약 만료까지 남을 수 있습니다. 일반 연결 끊기와 오류는 재접속용 자격증명을 보존합니다. 앱을 종료하면 token을 저장하지 않으므로 같은 게스트 복구를 할 수 없습니다. 서버가 재시작되면 세션과 방도 사라집니다. URL/Game ID를 바꾸려면 연결을 끊은 뒤 **게스트로 연결**하여 새 게스트를 발급받습니다. 재접속은 원래 게스트의 서버와 게임을 그대로 사용합니다.

토큰은 adapter 메모리에서만 취급하고 화면이나 로그에 표시하지 않습니다. 외부 서버는 HTTPS/WSS를 사용합니다. 다른 `game_id`의 방은 목록과 입장에서 격리됩니다.

## 검증

`CampfireClient`는 GUI와 실제 Godot headless 테스트가 공통으로 사용하는 adapter입니다. 단위 검증과 실제 scene smoke 검증은 다음과 같습니다.

```sh
godot --headless --editor --path examples/godot --import --quit
godot --headless --path examples/godot --script tests/client_unit.gd
godot --headless --path examples/godot --script tests/ui_smoke.gd
```

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
