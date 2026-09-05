# 데이터 기반 첫 구현 상태

기준일 2026-09-05. 전체 서버 목표는 진행 중이며 아이템 행동 게이트는 닫혀 있다.

## 구현한 범위

- `src/wire`: signed VarInt/VarLong의 읽기·쓰기·길이. Java의 비정규 표현 수용, 마지막 byte의 상위 bit 절삭과
  최대 길이 continuation 뒤 추가 byte가 있을 때의 오류 구분. 할당·외부 dependency 없음.
- `src/nbt`: 13 binary tag ID, UTF-16 값과 Java modified UTF-8, named/network root, heterogeneous list wrapper와
  wrapper 모양 compound escaping, 중복 key 마지막 값, Java float/double zero·NaN 및 equality,
  quota·요청 allocation·512 depth·출력 길이 제한, 실패 시 slice/추가 출력 rollback.
- 한 개 Rust library package, Rust 1.96.0, unsafe 금지, 외부 Rust dependency 0개.
- 공식 JAR/source/resource/registry/packet 발견 목록과 lock→metadata→bundler→inner JAR→report provenance 검증 도구.

## 실제 실행한 검증

Windows x86_64, Rust1.96.0/Java25에서 다음을 실행했다.

| 검증 | 결과와 범위 |
| --- | --- |
| `cargo test --locked --all-targets` | 24통과: NBT18 + wire6. Java oracle1개는 명시적 ignored |
| `cargo test --locked --release --all-targets --timings` | 동일24통과. foundation release 검증이며 서버 부하 benchmark 아님 |
| `cargo test --locked --test wire_java_oracle -- --ignored --nocapture` | 공식26.3-pre-2 Java VarInt/VarLong15,420사례 실제 비교 통과 |
| `cargo clippy --locked --all-targets -- -D warnings` | 통과 |
| `cargo fmt --all --check` | 통과 |
| `python -m unittest discover -s tools/tests -v` | 10통과, 입력 무결성·누락 source·stale report regression 포함 |
| inventory `--refresh-reports`, `--check` | 공식 generator 실행과5035Java/9893resources/95registries/7053entries/259packets 최신성 확인 |

정확성 리뷰어는 별도 공식 JVM probe와 Rust review harness로 MUTF8·혼합 list·512/513 깊이·중복 key·NaN을 확인했다.
NBT equality에서 발견한 NaN/±0 문제를 수정했다. 최적화 리뷰어가 메모리 예산·추상화·의존성과 inventory provenance를 독립 확인했고,
보고서를 버전/JAR에 묶지 못하던 도구 문제를 regression test와 함께 수정했다.
리뷰 자료는 형제 `Roadmap/reviews/`에 있다.

## 남은 범위와 의도한 API 경계

NBT binary는 전체 BASE-NBT가 아니다. SNBT·NBT path·의미 연산/visitor/skip·압축·registry/typed schema·component·migration은 남아 있다.
현재 named root API는 이름을 보존·검증하며, 원본 이름을 건너뛰는 Vanilla 디스크 편의 함수와 구분된다.
호출자별 disk fallback/oversized UTF 정책은 해당 소비자 구현에서 따로 대조한다.

디코더 allocation 예산은 요청한 backing allocation 누계이며 allocator의 실제 retained capacity·전체 RSS 보증이 아니다.
writer 예산은 추가된 논리 출력 bytes이고 기존 Vec capacity를 줄이는 정책이 아니다. 이후 worker/connection memory 예산에 결합해야 한다.
compound의 정렬 Vec·private ordinal은 초기 구현 선택이며 hot-path 소비자가 생긴 뒤 조회/변경/메모리 비용을 측정한다.

commit `b8dfca1`의 [CI run 33948100994](https://github.com/Love0118/Arrow-MC/actions/runs/33948100994)에서
Linux x86_64/ARM64·macOS ARM64·Windows x86_64 네 native host의 format/clippy/debug24/release24 테스트와 Python10 테스트가 통과했다.
각 host triple 확인 단계도 성공했다. Java oracle는 CI에서 ignored이며 위의 Windows 로컬 명시적 실행 결과로만 입증한다.
최초 workflow는 YAML 구문으로 job 시작 전 실패했고 `b8dfca1`에서 수정하여 성공했다.
현재 서버 실행기·gameplay·멀티코어 tick/청크 성능은 구현·검증하지 않았다.
소스를 참조해 독립 설계한 구현이며 [출처 정책](provenance-policy.md)을 따른다. clean-room이나 법적 무위험은 주장하지 않는다.
