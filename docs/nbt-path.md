# NBT 수치·predicate·경로 기반

기준은 Java Edition `26.3-pre-2`다. 원문과 직접 만든 공식 API 관찰에서 의미를 확인하고,
기존 소유형 `nbt::Tag` 위에 구체적인 Rust 함수와 경로 node를 별도로 설계했다.
별도 NBT 모델, Java class hierarchy, 전역 `Rc`/`Arc` tree, path plugin framework는 도입하지 않았다.

## 수치와 predicate

`Tag::as_byte/as_short/as_int/as_long/as_float/as_double`는 여섯 NumericTag 변환을 제공하며 비숫자는 `None`이다.
정수 narrowing은 하위 bit를 유지한다. float/double의 byte·short·int 변환은 floor 후 Java의 포화 cast를 적용한다.
FloatTag의 long 변환은 truncate, DoubleTag의 long 변환은 floor다. 현재 `Mth.floor`는 `Math.floor` 후 cast를 사용하며
과거 버전의 cast 후 wrapping decrement와 다르다. long→float는 double을 경유하지 않아 이중 반올림 차이를 피한다.
원래 tag 값의 signed zero를 임의로 바꾸지 않으며 NaN payload 보존까지 약속하는 API는 아니다.

`nbt::predicate::CompareBudget`는 비교와 경로 탐색이 함께 쓰는 작업 예산이다.
compound predicate는 expected key의 부분 일치이고, partial list는 실제 list의 길이 조건을 지키면서
각 expected 원소가 actual 어디엔가 있으면 된다. 이미 일치한 actual 원소를 소비하지 않는다.
따라서 `[1,1]`은 `[1,2]`와 부분 일치하지만 `[1]`에는 길이 때문에 맞지 않는다.
빈 expected list는 빈 actual list에만 맞는다. strict list는 내부 compound도 정확히 비교한다.
배열은 ListTag와 다른 타입이며 NaN equality와 ±0 구분을 유지한다.

비교는 명시적인 DFS 작업 스택을 사용한다. 예산 소진을 `false`로 바꾸지 않고 오류로 반환한다.
기본 작업량은1,000,000 units, 비교 scratch는1MiB이며 이는 Java 문법 제한이나 전체 RSS 보증이 아니다.
경로 내부의 여러 비교는 작업 counter와 실제 요청한 scratch 할당 누계를 공유한다.

## 경로와 소유권

`nbt::path::Path`는 여섯 구체적인 node와 소비한 원본 UTF-16 문자열을 저장한다.

| 표기 | 의미 |
| --- | --- |
| `name`, `"name"`, `'name'` | named child |
| `[n]` | list 또는 primitive array의 index |
| `[]` | 모든 collection 원소 |
| `[{...}]` | compound predicate와 맞는 ListTag 원소 |
| `name{...}` | predicate와 맞는 named child |
| `{...}` | 첫 node에서 root compound 검사 |

`parse`와 `parse_utf16(input,start,limits)`는 path 하나와 절대 UTF-16 cursor를 반환한다.
ASCII space 앞에서 끝나며 tab/LF는 unquoted 이름에 들어갈 수 있다. trailing dot도 원본처럼 소비한다.
quoted 경로 이름은 Brigadier 규칙을 쓰고, pattern 안의 compound는 SNBT 규칙을 쓴다.
SNBT의 `\n`·`\u` escape를 경로 이름에 그대로 허용하지 않는다.

읽기 `get`은 기존 값을 빌려주는 `Selection::Borrowed`와 primitive array 원소의 분리된 숫자 `Detached`를 반환한다.
`count_matching`은 전체 선택 개수를 구한다. 결과가 비는 단계에서 get는 nothing-found, count는0이다.
반복 `[]`의 오류 prefix가 마지막 wildcard 위치를 사용하는 동작도 보존한다.

`get_or_create`는 누락된 부모만 생성하고 변경 가능한 기존 값의 참조 또는 detached 숫자를 반환한다.
`set`, `insert`, `remove`는 결과 순서와 변경 개수를 유지하는 동기 연산이다.
중간 오류가 발생해도 앞서 만든 부모나 성공한 변경을 되돌리지 않는다. 예를 들어 `a[0].b` 생성 실패 뒤에도 `{a:[]}`는 남는다.
numeric array는 모든 NumericTag를 폭에 맞게 변환하며 SNBT typed-array literal 문법의 제한과 구분한다.
wildcard set의 보고 개수와 최종 byte 차이가 같다고 가정하지 않는다.

값 공급 함수는 소유 `Tag`를 반환한다. 공급 함수 자체의 할당은 호출자 책임이며 라이브러리는 받은 값을
추가하기 전에 retained backing allocation을 검사한다. 서로 다른 mutable tree branch를 같은 Java 객체로 별칭화하는 모델은 제공하지 않는다.
Java의 immutable 숫자·문자열·End 객체 cache identity도 Rust 슬롯 주소로 복제하지 않는다.
기존 참조를 변경했을 때 root에 보이는 효과와 set source/대상 사이의 복사 격리는 별도로 검증한다.

## 명시적인 경계와 남은 소비자

- 입력 기본2Mi UTF-16 units, 경로 node4,096개, 내부 요청 allocation32MiB, 누적 후보·작업 각각1,000,000개.
  node 제한은 Arrow 자원 정책이다. Vanilla set/insert의 source depth 검사512와 혼동하지 않는다.
- `is_too_deep`는 시작 depth를 포함하여512 이상인지 검사한다. get/create에는 같은 Vanilla source-depth gate를 임의로 추가하지 않는다.
- Java의 빈 path mutation/lone `[` unchecked 예외는 Rust의 `InvalidPath` 오류로 반환한다. 성공이나 no-op으로 위장하지 않는다.
- 런타임 compound/list는 End를 담을 수 있다. binary writer는 roundtrip이 불가능한 End-containing container를
  `UnexpectedEnd`로 거부한다. Java가 이런 구성값에서 생성하는 잘린/손실된 bytes까지 일치한다고 주장하지 않는다.
- storage의 live 값, entity/block의 serialized draft, loot의 lazy copy는 서로 다른 적용 경계다.
  라이브러리의 부분 변경을 실제 서버에서 언제 공개·저장하는지는 각 소비자 구현에서 검증한다.
- command message의 Component 구조·style·context/autocomplete, NbtOps/codec·압축·visitor/skip·registry/component 기반은 별도 미완료다.

## 공식 대조 자료

`tests/fixtures/nbt_path.tsv`에는 직접 선택한 공식 API 관찰3,050개를 넣었다.
path2900·predicate129·source-depth10·End binary11 사례다. 변경 개수·root-after·선택 type/순서·supplier 호출·UTF-16 위치·
translation key와 인수를 검사한다. Java unchecked11개와 immutable identity20개·소유 supplier 별칭1개는
정확히 같은 API라고 숨기지 않고 위 경계로 분리하여 명시적으로 검사한다.

공식 서버처럼 `SharedConstants`를 먼저 초기화한 뒤 `TagParser`를 만든다. 순서를 뒤집으면 SNBT grammar가
기본 Brigadier exception provider를 미리 잡아 일부 오류의 translation metadata가 달라진다.
초기 harness와 수정 전후 관찰은 로컬 `Roadmap/reviews/`에 보존하며, 세 사례의 metadata만 바뀌고 값·cursor·변경 상태는 같았다.

수치 oracle는100,634개 NumericTag의603,804개 변환을, predicate oracle는41,616개 nullable pair의
strict/partial/exact124,848개 결과를 실제 고정 JAR와 비교했다. 유한한 관찰 수를 전체 서버 동등성 증명으로 확대하지 않는다.

```powershell
cargo test --locked --test reference_nbt_path
$env:ARROW_MC_JAVA_REFERENCE_ROOT = 'E:\projects\Arrow MC\Decompile'
cargo test --test nbt_foundation_java_oracle -- --ignored --nocapture
python tools/export_nbt_path_fixtures.py --check
```

일반 테스트는 저장된 TSV를 사용하고 Java 없이 실행한다. live oracle는 기본 `ignored`이며 명시적으로 실행한 결과만 근거다.
fixture exporter는 로컬 연구·검수 JSON의 hash를 기록하고 새 필드·버전 불일치·중복 ID를 거부한다.
실제 구현 검수·자원 실패·네 플랫폼 결과는 [foundation 상태](foundation-status.md)에 기록한다.
`DATA-004` 소비자 통합·전체 `BASE-NBT`·`DATA-GATE-ITEMS` 완료를 뜻하지 않는다.
