# Arrow MC

Minecraft Java Edition **26.3**의 공식 서버를 소스로 조사하면서 **독립적으로 설계하는 Rust 서버 구현체**입니다.
Java 디컴파일본과 실제 실행 결과를 동작 기준으로 삼고, Pumpkin에서는 검증한 구현 아이디어와 최적화를 선별적으로 참고합니다.
출처와 코드 재사용 조건은 [출처 정책](docs/provenance-policy.md)에 기록합니다.

현재 단계는 **실행 가능한 서버와 기반 기능 구현 진행 중**입니다. Java 서버 목록의 status/ping에 실제 TCP로 응답합니다.
청크 section palette/packed storage와 제한된 CPU worker의 병렬 section 준비도 구현했습니다.
검증된 snapshot을 지정하면 online 로그인·암호화·압축과 실제 registry/tag configuration 전송까지 수행합니다.
현재 버전 Anvil 청크의 병렬 읽기·decode·resident 예산 전환과 block/fluid 예약 tick의 복원·영역 연산도 구현했습니다.
청크 요청의 소유권 검증·canonical section 준비와 Vanilla 청크 후보/ACK·전송 batch 소유자도 추가했습니다.
여섯 heightmap, 실제 시야 경계·차이 계산, chunk/light·control packet 인코딩과 실제 TCP 순서 검증도 추가했습니다.
**실제 spawn 준비와 Play·월드 생성·게임 tick 실행은 아직 미완료**입니다.
현재 실행 방법과 검증 경계는 [로그인·configuration·예약 tick](docs/login-configuration.md)에 기록합니다.
[청크 저장 로딩](docs/chunk-storage.md)과 [예약 tick 복원](docs/saved-ticks.md)에 추가 경로와 검증 범위를 기록합니다.
[로드된 청크 소유자](docs/chunk-loading-owner.md)와 [청크 전송 준비](docs/chunk-sender.md)는 활성화·실제 socket 연결의 남은 경계도 구분합니다.
[Heightmap·시야·chunk wire](docs/chunk-wire-heightmap-view.md)에 현재26.3 형식과 실행 근거를 정리했습니다.
[설계 기준](docs/architecture.md)과 [Vanilla/Pumpkin 비교·최적화 계획](docs/optimization-plan.md)에 지원 범위,
동기·비동기 실행 경계, 가져올 최적화와 직접 가져오지 않을 동작을 정리했습니다.

지원 목표는 **Linux ARM64/x86_64, Apple Silicon macOS, Windows x86_64**입니다.
청크 로딩과 tick의 병렬 처리량·지연 개선을 우선하며, 이를 위한 **측정 가능하고 제한된 RAM 증가를 허용**합니다.
불필요한 추상화와 빌드 의존성을 줄이고, 바닐라 패킷 순서·게임 의미·view-distance 2..32 전체 범위를 유지합니다.
tick 병렬화는 초기에 단일 스레드 대조 경로와 함께 개발할 핵심 항목입니다.

아이템 동작은 NBT·registry·typed component 122종과 중첩 stack 데이터 기반을 모두 검증한 뒤 구현합니다.
[전체 구현 계약](docs/implementation-contract.md)에 선행 게이트와 1~3명 독립 리뷰어 운영 원칙이 있습니다.

## Rust 기반 개발

Rust `1.96.0`, 단일 package의 library와 서버 실행 파일을 사용합니다.
네트워크에는 필요한 기능만 켠 Tokio, 인증에는 최소 기능 reqwest, protocol crypto에는 OpenSSL을 사용합니다.
버전과 embedded native 코드를 포함한 [의존성 고지](THIRD_PARTY_NOTICES.md)를 고정합니다.
NBT·wire·section kernel·CPU pool 자체에는 추가 외부 framework를 도입하지 않았습니다.
`src/nbt`는 모든 binary tag·Java modified UTF-8·mixed list·named/network root·자원 제한을 처리합니다.
`src/snbt`는 현대 SNBT parser·compact/pretty writer와 UTF-16 진단을 처리합니다. [범위·자원 정책·대조 근거](docs/snbt.md)를 별도로 기록합니다.
`src/nbt/path`와 `src/nbt/predicate`는 경로 조회·생성·변경·삭제와 bounded 비교를 처리하고, 여섯 NumericTag 변환도 제공합니다.
[NBT 경로 범위와 API 차이](docs/nbt-path.md)를 구분해 기록합니다. `src/wire`는 VarInt/VarLong을 처리합니다.
디스크 NBT 압축과 현재 청크용 block-state/biome registry는 구현했습니다.
범용 NBT ops·전체 typed registry/component와 item gameplay는 아직 구현 전입니다.
로컬 디코더 메모리 예산은 요청한 backing allocation의 누계이며 RSS 상한을 뜻하지 않습니다. writer는 추가 출력 길이를 제한합니다.

```powershell
cargo test --locked --all-targets
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all --check
$env:ARROW_MC_JAVA_REFERENCE_ROOT = 'E:\projects\Arrow MC\Decompile'
cargo test --test wire_java_oracle -- --ignored --nocapture
```

마지막 명령은 로컬 공식 JAR의 클래스와 실제 대조합니다. 일반 테스트에서는 명시적으로 ignored이며 별도 실행 결과만 대조 성공 근거로 사용합니다.
CI는 네 native target에서 foundation debug/release 테스트를 실행합니다. 이 CI의 통과는 완성된 서버의 플랫폼 검증을 뜻하지 않습니다.

## 현재 서버 실행

```powershell
cargo run --release -- --bind 127.0.0.1 --port 25565
```

Java 서버 목록에서 상태·설명·ping을 확인할 수 있습니다. 위 명령은 snapshot 미설정이므로 로그인은 종료합니다.
검증된 snapshot·별도 manifest hash를 지정하는 [로그인 실행 방법](docs/login-configuration.md)을 사용하면 configuration까지 진행합니다.
연결별 읽기·쓰기 순서, 연결 수, 프레임 크기, 전체 교환 시간과 통신량을 제한하며 종료 시 task/socket을 회수합니다.
기본 I/O worker는 최대2개입니다. [실행·청크·worker 범위](docs/server-runtime.md)에서 설정과 검증 경계를 확인할 수 있습니다.

청크 section 병렬 준비를 실제 codec으로 실행하는 예제는 다음과 같습니다.

```powershell
cargo run --release --example prepare_sections -- 2 8192 8 257
```

인수는 worker 수·section 수·동시 작업 상한·palette 크기입니다. worker0은 동기 대조 경로입니다.
이 예제는 section 준비의 처리량·지연·버퍼 예산을 측정합니다. 월드 tick 병렬화나 플레이 가능한 월드의 부하 시험은 아닙니다.

## 작업 공간

```text
E:\projects\Arrow MC\          # 작업 공간 컨테이너, Git 루트 아님
├── Arrow MC\                 # 이 Git 저장소: 구현, 도구, 설계 문서
├── Decompile\                # 공식 서버 JAR, 디컴파일본, 전용 CodeGraph
├── Pumpkin MC\               # 고정 커밋으로 체크아웃한 참조용 클론
└── Roadmap\                  # 구현 예정/완료 목록과 이식 기록
```

Git 명령은 내부 `Arrow MC`에서 실행합니다. 형제 디렉터리 세 개는 로컬 자료이며 이 저장소의 push에 포함되지 않습니다.
각 코드 프로젝트에 독립적인 CodeGraph 인덱스를 사용합니다. 상위 작업 공간을 인덱싱하지 않습니다.

## 고정 기준

- Minecraft: `26.3-pre-2`, Java 25, protocol `1073742158`, world data `5018`.
- 2026-09-05 확인 시점에는 26.3 정식 버전이 없어 공식 최신 프리릴리스를 사용합니다.
- Vineflower: `1.12.0`.
- Pumpkin: `8d0d0d311778cb0aecb5fc957d571a38f286fda0`, submodule 포함.
- 출처와 검증 결과: [기준 보고서](docs/reference-baseline.md), [버전 잠금 파일](references.lock.json).

## 전체 요소와 의존성 목록

형제 `Roadmap/catalog/`에 source/resource/registry/packet 발견 목록과 데이터·아이템, 월드·틱, 서버·게임플레이 catalog를 둡니다.
공식 보고서의 122 components·1,658 items·1,286 blocks·161 entities를 포함하며 파일 존재와 실제 구현 완료를 구별합니다.

```powershell
python tools/generate_vanilla_inventory.py --refresh-reports
python tools/generate_vanilla_inventory.py --check
```

공식 데이터 생성기를 실행하며 게임 서버를 시작하지 않습니다. JAR과 보고서의 버전·해시를 연결하고 5,035 Java·9,893 bundled resources·
95 built-in registries/7,053 entries·259 packets를 추적합니다. 상세 component/item/command/entity 분류는 catalog의 별도 검증 자료와 함께 사용합니다.

## 재현

Python 3.12+, Java 25 JDK(`java`, `javap`), Git, PowerShell 7, CodeGraph 1.6.0이 필요합니다.
Python 외부 패키지는 사용하지 않습니다. 디컴파일 JVM 최대 힙은 6 GiB입니다.

```powershell
Set-Location 'E:\projects\Arrow MC\Arrow MC'
pwsh -File tools/Sync-References.ps1
python -m unittest discover -s tools/tests -v
```

스크립트는 자신의 위치를 기준으로 형제 디렉터리를 찾습니다. 공식 메타데이터와 서버 JAR의 SHA-1,
내부 JAR·라이브러리와 디컴파일러의 SHA-256을 검사합니다. 기존 소스는 덮어쓰지 않고 검사하며,
기존 Pumpkin의 추적 파일이 수정되어 있으면 중단합니다.

직접 디컴파일하거나 기존 소스만 검사할 수도 있습니다.

```powershell
python tools/prepare_minecraft.py                       # 소스가 없는 버전의 최초 생성
python tools/prepare_minecraft.py --verify-existing     # 기존 소스 검사
codegraph sync 'E:\projects\Arrow MC\Decompile'
python tools/verify_reference_index.py
```

Java 파일 전체를 실제 인덱스 파일 목록과 대조하며 `ai/goal/target` 포함 여부와 대표 심볼을 검사합니다.
파일 목록 검증은 각 메서드의 동작 정확성이나 그래프의 모든 호출 관계를 보증하는 검사는 아닙니다.

## 26.3 정식 버전으로 변경

```powershell
python tools/prepare_minecraft.py --version latest
codegraph sync 'E:\projects\Arrow MC\Decompile'
python tools/verify_reference_index.py
```

`latest`는 공식 manifest에서 26.3 계열의 정식 버전을 우선 선택하고, 없으면 최신 프리뷰를 선택합니다.
성공한 뒤 `references.lock.json`을 갱신합니다. 이미 생성된 같은 버전이면 `--verify-existing`을 함께 사용합니다.
특정 버전은 `--version 26.3`처럼 지정합니다. 자동 모니터링이나 자동 버전 변경은 설정되어 있지 않습니다.

버전 변경 시 이전 소스를 보존하고 protocol/data version, 패킷, 레지스트리, tick/AI 변경점을 비교한 후
형제 `Roadmap`과 기준 보고서를 갱신합니다. 재생성이 필요하면 기존 버전 디렉터리를 먼저 별도 보관합니다.
