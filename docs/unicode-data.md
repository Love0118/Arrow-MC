# Unicode 이름 데이터와 검증

`src/unicode_names`는 SNBT `\N{...}`에서 사용하는 Java 25
`Character.codePointOf`와 UTF-16 단위 `Character.digit(char, 16)`의 독립 Rust 구현이다.
문자 이름 인식은 Unicode **16.0.0** 전체를 대상으로 한다. SNBT 문법에서 허용하는
문자 제한은 parser가 별도로 적용한다.

Java 25의 Unicode 버전과 이름 생성·대소문자 처리 계약은
[공식 Character API](https://docs.oracle.com/en/java/javase/25/docs/api/java.base/java/lang/Character.html)에서 확인했다.
Unicode 표준의 Hangul/CJK 알고리즘 이름과 Java의 block+hex 이름은 다르므로 Java의
관찰 가능한 이름을 맞춘다. 예를 들어 `HANGUL SYLLABLES AC00`은 허용하고
`HANGUL SYLLABLE GA`는 허용하지 않는다.

## 출처와 배포 고지

원본 데이터는 Unicode Consortium의 공식 `UnicodeData.txt`, `Blocks.txt`,
`NameAliases.txt`, `SpecialCasing.txt`, `ReadMe.txt`이다. `third_party/unicode/sources.json`에
Unicode 버전, 공식 URL, 다운로드 일자와 SHA-256을 기록했다. 원본 파일은
`third_party/unicode/16.0.0`에 보관하며 이름 blob 등 파생 데이터도 같은 출처를 가진다.

[Unicode 이용 조건](https://www.unicode.org/copyright.html)은 `Public/`의 데이터 파일에
[Unicode License v3](https://www.unicode.org/license.txt)가 적용됨을 명시한다.
다운로드 당시의 전체 고지문은 `third_party/unicode/LICENSE.txt`에 포함했다.
원본·파생 데이터 또는 이를 포함하는 바이너리를 배포할 때 이 저작권·허가 고지문을
함께 제공해야 한다. Unicode 이름을 광고·보증에 사용하는 권한을 추정하지 않는다.

생성기는 Unicode에서 직접 받은 데이터만 입력으로 사용한다. OpenJDK 소스·테이블,
Minecraft JAR·디컴파일 소스, JVM이 출력한 이름 목록은 생성·배포 데이터의 입력이 아니다.
Rust 자료구조·검색·오류 반환 API와 Python 생성기는 직접 작성했다. Java 공식 문서와
로컬 JVM 실행은 동작 확인에 사용했으며, 이를 프로젝트 전체의 법적 적합성 보증이나
clean-room 인증이라고 표시하지 않는다.

## 데이터 표현과 비용

| 항목 | 표현·크기 |
| --- | --- |
| 명시적 이름 | 40,077개, 정렬된 ASCII blob 1,031,152 bytes |
| 이름 색인 | code point·offset의 little-endian 8-byte record, sentinel 포함 320,624 bytes |
| 범위 기반 이름 | 20개 범위, 총 254,502 code point; 범위·prefix 801 bytes |
| 비ASCII → ASCII ROOT 대문자 확장 | UCD에서 추출한 10개 매핑, 60 bytes |
| BMP 십진수 범위 | 37개 연속 10-digit 범위의 시작점, 74 bytes |
| 전체 내장 binary 데이터 | **1,352,711 bytes** |

`include_bytes!`로 읽기 전용 데이터를 포함한다. 거대한 Rust 상수 표현식이나 runtime
`HashMap` 초기화를 만들지 않는다. 조회는 정렬된 record의 이진 검색이며, 정규화에
128-byte stack buffer를 사용한다. UTF-16 입력의 양끝 U+0000..U+0020을 먼저 제거하므로
긴 padding은 이름 길이와 별개다. 이름 전체를 위한 heap 할당·lock·async·외부 Rust
의존성은 없다. 현재 가장 긴 명시적 이름은 88 bytes이며 생성기가 buffer 한계를 검증한다.

Java의 control 이름은 Unicode 1 이름 필드가 기준이고, U+0007은 `BEL`, U+1F514는 `BELL`이다.
이름 필드가 없는 세 control에는 UCD figment alias를 사용한다. NameAliases 전체를
추가 이름으로 허용하지 않는다. 미할당 code point나 다른 prefix·불필요한 선행 0은 거부한다.
`HIGH SURROGATES D800` 같은 결과는 유효한 Java UTF-16 단위이므로 `Some(0xD800)`을 반환한다.

2026-09-05 Windows x86_64, AMD Ryzen 5 9600X, Rust 1.96.0에서 모듈과 작은 driver만
`rustc -O`로 컴파일한 단일 측정은 **0.304 s**, 실행 파일은 **1,493,504 bytes**였다.
미리 만든 입력 여섯 종류를 각각 3,000,000회 반복한 warm lookup 단일 측정은
**55~67 ns/call**이었다. 이는 프로젝트 전체 cold/incremental build, 동시 부하, RSS,
다른 플랫폼 성능을 입증하는 수치가 아니다. driver는 로컬 `Roadmap/reviews/unicode-name-bench.rs`에 있다.

## 재생성과 검사

기본 생성·검사는 네트워크나 Java를 요구하지 않으며 기존 파일 내용이 같으면 다시 쓰지 않는다.

```powershell
python tools/generate_unicode_names.py
python tools/generate_unicode_names.py --check
python -m unittest discover -s tools/tests -p test_unicode_names.py -v
```

원본 복구가 필요하면 `--download`로 공식 URL에서 다운로드하고 고정 SHA-256이 일치할 때만 저장한다.
Unicode 버전·checksum을 바꾸는 행위는 명시적인 baseline 변경이며 자동으로 최신판을 가져오지 않는다.
현재 시점 이후 `license.txt` 고지 연도가 바뀌면 무조건 덮어쓰지 않고 hash 불일치로 중단한다.

실제 Java 25와 전체 대조하는 검사는 별도로 활성화한다. 기본 검사에서 skipped로 표시된
oracle을 성공으로 계산하지 않는다.

```powershell
$env:ARROW_MC_UNICODE_JAVA_ORACLE = '1'
python -m unittest discover -s tools/tests -p test_unicode_names.py -v
```

2026-09-05 Microsoft OpenJDK **25.0.3+9-LTS**에서 실제 실행하여 아래 모두 불일치 **0**을 확인했다.

| 검증 범위 | 실제 검사 수 |
| --- | ---: |
| U+0000..U+10FFFF의 생성된 이름/미할당 상태와 Java `getName` | 1,114,112 code point |
| 명명된 code point와 Java 자체 이름 왕복 | 294,579 |
| Rust/Java `codePointOf`: 전체 이름·lowercase/trim, 전체 aliases, BMP 입력·양끝, 잘못된 범위 표기 | 786,584 query |
| UTF-16 `Character.digit(char,16)` 전체 범위 | 65,536 code unit |
| 비ASCII 단일 code point ROOT uppercase의 ASCII 변환 여부 | 1,113,984 code point |

Rust unit test 5개와 Python test 6개가 통과했다. 이 증거는 이름·hex-digit backend에 대한
검증이며 SNBT parser 전체, NBT 전체 또는 서버 전체 기능 완료를 뜻하지 않는다.
네 지원 플랫폼의 native 실행 검증도 이 Windows 결과만으로 완료 처리하지 않는다.
