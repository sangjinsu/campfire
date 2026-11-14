<!-- 제목은 커밋과 같은 형식으로 씁니다. 예: perf(runtime): encode shared snapshot state once -->

## 연결된 이슈

Closes #

## 변경 내용

<!-- 무엇을 왜 바꿨는지. 구현 방식 중 리뷰어가 알아야 할 선택이 있으면 함께 적습니다. -->

## 영향 범위

- [ ] JSON 프로토콜 변경 (`docs/protocol.md` 갱신)
- [ ] 공개 Rust API 변경 (`Config`, `Game`, `GameModule` 등)
- [ ] CLI 옵션·기본값 변경 (`docs/configuration.md` 갱신)
- [ ] Godot client 수정 필요
- [ ] 해당 없음

## 검증

<!-- 실행한 명령과 결과를 적습니다. 실행하지 않은 항목은 체크하지 말고 이유를 적습니다. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo clippy --workspace --all-targets --locked -- -D warnings`
- [ ] `cargo test --workspace --all-targets --locked`
- [ ] 동작이 바뀌는 변경이면 테스트를 추가하거나 수정했습니다
- [ ] transport·room·session 또는 Godot client를 건드렸으면 Runtime validation을 실행했습니다
      (`gh workflow run runtime-validation.yml --ref <branch>`, run 링크: )

## 측정값

<!-- 성능 변경일 때만. 전후 수치와 측정 명령, 빌드 종류(debug/release)를 적습니다. -->

## 남은 일

<!-- 이 PR에서 다루지 않은 것, 후속 이슈 번호. 없으면 "없음". -->
