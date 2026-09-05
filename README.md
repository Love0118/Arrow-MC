# Arrow MC

Minecraft Java Edition **26.3**의 공식 서버 동작을 기준으로 처음부터 작성하는 Rust 서버 프로젝트입니다.
Java 디컴파일본을 동작 기준으로 삼고, Pumpkin에서는 검증한 로직과 최적화만 선별적으로 참고합니다.

현재 단계는 **참조 소스 준비와 1차 최적화 조사 완료**입니다. Rust 서버 런타임과 게임 기능은 아직 구현하지 않았습니다.
[설계 기준](docs/architecture.md)과 [Vanilla/Pumpkin 비교·최적화 계획](docs/optimization-plan.md)에 지원 범위,
동기·비동기 실행 경계, 가져올 최적화와 직접 가져오지 않을 동작을 정리했습니다.

지원 목표는 **Linux ARM64/x86_64, Apple Silicon macOS, Windows x86_64**입니다.
청크 로딩과 tick의 병렬 처리량·지연 개선을 우선하며, 이를 위한 **측정 가능하고 제한된 RAM 증가를 허용**합니다.
불필요한 추상화와 빌드 의존성을 줄이고, 바닐라 패킷 순서·게임 의미·view-distance 2..32 전체 범위를 유지합니다.
tick 병렬화는 초기에 단일 스레드 대조 경로와 함께 개발할 핵심 항목입니다.

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
