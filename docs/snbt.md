# SNBT 데이터 기반

`src/snbt`는 잠금 기준 `26.3-pre-2`의 현대 SNBT를 기존 `nbt::Tag`로 읽고 출력한다.
Java 소스와 직접 만든 JVM 입력의 관찰 결과로 규칙을 확인한 뒤 Rust parser와 formatter를 별도로 설계했다.
별도 AST, parser framework, 외부 Rust dependency는 도입하지 않았다.

## API와 표현

| API | 계약 |
| --- | --- |
| `parse`, `parse_utf16` | 전체 입력을 읽고 남은 비공백 입력을 거부 |
| `parse_prefix` | UTF-16 slice의 값과 소비한 UTF-16 unit 수를 반환; 뒤 명령 인수는 남김 |
| `parse_compound`, `parse_compound_utf16` | 전체 입력의 최상위 compound 요구 |
| `write` | compact tag 표현을 UTF-16 Vec에 추가 |
| `write_pretty` | 기본 4-space 들여쓰기와 구조 경로별 key 우선순위를 반영한 표현을 추가 |

`&str` 편의 함수는 UTF-16 변환 비용을 parser의 할당 예산에 포함한다. 이미 UTF-16인 호출자는
직접 slice를 전달할 수 있다. 문자열 값과 오류 위치는 lone surrogate를 포함한 Java UTF-16 의미를 유지한다.
prefix 입력을 잘라 전달한 경우 반환 cursor와 진단의 source span은 그 slice 기준이다.

정수의 signed/unsigned suffix와 bit width, radix, 문법 토큰 사이 공백, 숫자 underscore,
float/zero 처리, mixed list, typed array, 중복 compound key의 마지막 값, trailing comma,
`bool()`·`uuid()` 및 `\x`·`\u`·`\U`·`\N` escape를 처리한다.
Java의 일부 불완전한 builtin suffix rollback과 UUID segment 처리도 관찰 결과에 맞춘다.
문자 이름과 hex digit은 [Unicode 16 기반 조회](unicode-data.md)를 사용한다.

출력은 모든 `Tag`에서 parse roundtrip을 보장하는 형식이 아니다. `End`, nonfinite float/double,
빈 compound key는 Java visitor의 출력과 parser 수용 범위가 다르다. compact와 pretty의 `End` 출력도 다르다.
키는 UTF-16 순서로 정렬하고 숫자 출력은 Java의 유효 자릿수·지수 표기 경계를 맞춘다.

## 오류와 자원 경계

오류는 거친 `ErrorKind`, UTF-16 위치, 선택적인 translation key와 구조화된 인수를 가진다.
세부 진단은 parser 한 곳에 저장하고 recursive return에는 작은 실패 정보만 전달한다.
입력 전체나 오류 문자열을 매 재귀 단계마다 복제하지 않는다. 실제 command/chat 번역·styling과
최종 메시지 구성은 이후 소비자 구현의 범위다.

`Diagnostic::write_argument(input, output, max_output_units)`는 source span을 확인하고 UTF-16 인수 하나를
제한된 출력에 추가한다. 반환 `bool`로 인수 없음과 빈 문자열 인수를 구별하며 실패 시 기존 출력으로 복원한다.
span은 해당 parser에 전달한 slice 기준이다. 원래 입력과 다른 내용을 전달했는지까지 인증하는 API는 아니다.

| 기본 제한 | 값 | 의미 |
| --- | --- | --- |
| 입력 | 2,097,152 UTF-16 units | prefix 뒤 미소비 입력도 포함 |
| 할당 | 33,554,432 bytes | decoded/scratch Vec의 요청 backing allocation 누계 |
| 깊이 | 512 | list·compound·builtin call의 Arrow stack/admission 정책 |
| 출력 | 4,194,304 UTF-16 units | 한 writer 호출이 추가하는 길이 |

SNBT의 깊이 512는 Vanilla 문법 자체의 고정 상한이 아니다. 공식 Java는 충분한 JVM stack에서
600단계 사례도 허용했다. Arrow는 기본 Rust test thread stack에서 512단계 list·compound·builtin을
검증하고 그보다 큰 설정은 거부한다. binary NBT의 별도 깊이 계약과 혼동하지 않는다.

할당 예산은 allocator metadata·stack·전체 RSS의 상한이 아니다. writer 실패 시 기존 출력 내용은
유지하지만 늘어난 Vec capacity는 남을 수 있다. 이후 worker/connection 메모리 예산과 결합해야 한다.
compound는 한 번 정렬하여 중복 key를 정리하고, pretty 출력은 기존 정렬 entry와 작은 고정 경로 상태를
사용한다. 입력 폭만큼 별도 map을 만들거나 key 집합을 매번 복제하지 않는다.

## 검증 자료와 재현

`tests/fixtures/snbt.tsv`는 직접 선택한 synthetic 입력과 실제 공식 API 관찰 결과 7,018개다.
입력·문자열 출력·오류 인수는 UTF-16 hex로 저장하며 숫자 type과 float raw bits를 보존한다.
JAR, Java 구현 본문, Mojang registry/asset dump는 포함하지 않는다.

- parser 7,005사례: 성공·실패·typed value·UTF-16 cursor·translation key와 0/1 인수의 실제 UTF-16 내용.
- compact 출력 2,063사례: 일반 값과 직접 구성한 특수 tag 포함.
- 별도 depth 관찰 6사례: Java 문법과 Arrow 자원 정책의 차이를 명시적으로 확인.
- 실제 Java float writer 대조 71,168개와 pretty formatter 38개: Windows에서 명시적으로 실행하여 일치.
- 기본 테스트에서 live Java oracle는 `ignored`이며 실행하지 않은 oracle를 성공으로 계산하지 않는다.

```powershell
cargo test --locked --all-targets
$env:ARROW_MC_JAVA_REFERENCE_ROOT = 'E:\projects\Arrow MC\Decompile'
cargo test --test snbt_writer -- --ignored --nocapture
cargo test --test snbt_pretty -- --ignored --nocapture
python tools/export_snbt_fixtures.py --check
python tools/generate_unicode_names.py --check
```

fixture exporter의 원본 관찰 JSON과 자체 Java probe는 로컬 형제 `Roadmap/reviews/`에 있다.
일반 Rust 테스트는 저장된 TSV만 사용하므로 Java와 로컬 참조가 없는 네 native CI host에서도 실행한다.
exporter의 `--check`는 로컬 원본 관찰 자료가 필요하며 일반 CI에 넣지 않는다.

이 검증은 현재 사례와 명세의 구현 근거이며 입력 전체에 대한 형식적 동등성 증명이 아니다.
NBT path·predicate/ops·visitor/skip·압축·schema/registry·migration·component와 소비자별 진단은 남아 있다.
SNBT 구현으로 `DATA-GATE-ITEMS`나 전체 `BASE-NBT`를 완료 처리하지 않는다.
