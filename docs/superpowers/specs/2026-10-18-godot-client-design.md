# Campfire Godot 플레이 클라이언트 설계

## 범위와 계약
Godot 4.7.2 프로젝트는 `examples/godot`에 두고 F5/F6 및 CLI로 실행한다. 기존 Rust 서버 권한 2D arena를 사용한다. 경계 ±10, 목표 (5,0), 이동 speed 5, 위치와 공동 점수는 서버가 소유하며 Rust 게임 규칙은 바꾸지 않는다. 게스트 발급, game별 공개 방 생성·목록·입장, 8/16명 정원, WASD/방향키 이동, 수동 재접속 후 자동 resume와 full sync가 필요하다. GUI와 실제 Godot headless 클라이언트는 `scripts/campfire_client.gd`를 공유한다.

`CampfireClient` Node의 메서드는 `connect_guest(base_url,game_id)`, `request_rooms(cursor=null)`, `create_room(capacity=16)`, `join_room(id,instance)`, `leave_room()`, `logout()`, `disconnect_for_test()`, `reconnect()`, `send_direction(Vector2)`이며 bool을 반환한다. signals는 status_changed, authenticated, rooms_received, room_entered, room_resumed, snapshot_received, room_left, room_closed, error_received다. `last_error_command:String`은 error_received 직전에 해당 요청 command type을 기록하며 local/no-id 오류는 빈 문자열이다. UI는 오류 signal handler 안에서 이를 읽어 같은 pending 조작만 해제한다. Snapshot과 목록 항목은 protocol의 필드 이름을 그대로 유지한다. 방 식별자는 실제 UUID와 instance이며 별도 숫자 코드를 만들지 않는다. Token은 adapter 메모리에만 남기며 UI와 로그에 출력하지 않는다.

## 화면과 상호작용
주제는 Campfire 파티 테스트 코트다. forest blue (#19383E) 작업대가 pale green (#EAF1EA) 서버 월드를 감싼다. 나의 불빛 링은 #E8A75E, 목표는 #ECD77E, 다른 참가자는 청록·녹색 계열, 연결이 끊긴 예약자는 회색이다. 중앙 좌표 코트와 목표/참가자 링에 시각적 특징을 모으고 반복적인 카드 장식은 쓰지 않는다. 한글은 Apple SD Gothic Neo와 Noto Sans CJK KR를 앞세운 SystemFont fallback을 사용한다.

기본 1280×800, 최소 900×600이다. 헤더는 연결 상태를 표시한다. 왼쪽 320px 스크롤 도구에는 URL/game, 게스트 연결·복구, 공개 방 생성 정원, 페이지별 목록 선택, 입장·퇴장과 예약 설명을 둔다. 오른쪽 코트에는 점수, 참가자/예약 수, 처리 입력 ack·server tick·snapshot seq가 있다. 전체 방/instance 식별자는 줄바꿈하며 가리지 않는다. URL/game은 disconnected에서만 편집하고 진행 중 동작은 중복 클릭하지 못하게 한다. 텍스트 입력 focus와 application focus 상실 시 이동을 멈춘다. 위쪽 키는 월드 +y이며 그릴 때 y를 반전한다. 클라이언트가 자체적으로 위치를 진행하지 않는다.

게스트 종료는 token을 폐기한다. 명시적인 연결 끊기 시험은 같은 게스트와 예약 정보를 보존하고 재접속 시 자동 resume한다. 발급 실패, 만실, 변경된 방, 예약 만료는 다음 동작을 설명한다. 단절 뒤 마지막 snapshot을 남겨두되 정지/예약 상태를 명확히 표시한다.

## 검증
실제 main scene을 인스턴스화하는 SceneTree 테스트로 버튼 상태, 레이아웃, 서버 snapshot 표시, 실제 key event, 텍스트 focus 및 창 focus 상실을 검증한다. Adapter 단위 테스트와 실제 8/16개 Godot 프로세스 통합 검증은 분리한다. 그래픽 viewport 캡처는 frame_post_draw를 기다리고 제한 시간 안에 종료한다. DummyRenderer 이미지는 시각 검증 근거로 삼지 않는다. 구현과 통합 후 fresh Architect 검토를 받는다.
