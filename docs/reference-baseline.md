# 참조 환경 기준 보고서

확인일: 2026-09-05 (Asia/Seoul). Minecraft 26.3 정식 버전은 아직 없으며,
[공식 manifest](https://piston-meta.mojang.com/mc/game/version_manifest_v2.json)의 최신 26.3 빌드는
[`26.3-pre-2`](https://www.minecraft.net/en-us/article/minecraft-26-3-pre-release-2)입니다. 당시 최신 stable은 `26.2`입니다.

## 다운로드와 디컴파일

| 항목 | 확인 값 |
| --- | --- |
| 공식 서버 크기 | 62,292,880 bytes |
| 서버 bundler SHA-1 | `1dcf227881b28b21cc1d03ba830273f0d2d26319` |
| 내부 서버 SHA-256 | `18d6ad2986227ea55eb18f8ee6929999a4c48c0bbd623c36af3d2f64d3180e4a` |
| JDK | Microsoft OpenJDK `25.0.3` |
| 디컴파일러 | Vineflower `1.12.0` |
| 내부 서버 클래스 수 | 7,761 (내부·익명 클래스 포함) |
| 최상위 클래스 / 생성 Java 파일 | 5,035 / 5,035 |
| 외부 참조 라이브러리 | 39개, 각 bundled SHA-256 검증 |
| protocol / world data version | `1073742158` / `5018` |

공식 bundler에서 `META-INF/versions.list`와 `META-INF/libraries.list`를 읽어 내부 JAR과 라이브러리를 추출했습니다.
서버 프로그램은 실행하지 않았으며 디컴파일에는 내부 서버 JAR을 입력하고 39개 라이브러리를 타입 해석용으로 제공했습니다.
Vineflower `--folder --log-level=WARN --thread-count=4`, Java 최대 힙 6 GiB를 사용했습니다.

경고/오류 로그와 `$VF:` 등 실패 표식이 없고, 최상위 클래스에 대응하는 소스 누락이 없습니다.
초기 확인 실행과 재현 스크립트의 새 디컴파일 실행에서 Java 5,035개 파일의 SHA-256이 모두 일치했습니다.
추출 리소스도 버전별 소스 디렉터리에 보존했습니다. Java 소스 전체 재컴파일과 실제 서버 동작 대조는 수행하지 않았습니다.

## 난독화 확인

JAR 내부에 `net/minecraft/server/MinecraftServer.class`, `ServerLevel.class`, `GoalSelector.class` 등
읽을 수 있는 클래스 경로가 있습니다. `javap -p`로 `GoalSelector`의 `addGoal`, `tickRunningGoals`,
`availableGoals` 등 메서드·필드 이름도 확인했습니다. 공식 metadata에는 `server_mappings` 다운로드가 없습니다.

따라서 이 빌드는 확인한 클래스·멤버 기준으로 이름 난독화 없이 배포되며 remapping 단계가 필요하지 않습니다.
이는 모든 원본 지역 변수·주석·소스 표현식이 복원된다는 의미는 아닙니다.
[Mojang의 난독화 제거 안내](https://www.minecraft.net/en-us/article/removing-obfuscation-in-java-edition)도 참고할 수 있습니다.

## CodeGraph 검증

- 프로젝트 루트: `E:\projects\Arrow MC\Decompile`.
- CodeGraph `1.6.0`, Java 파일 **5,035/5,035** 포함, 누락 0.
- `net/minecraft/world/entity/ai/goal/target/`의 **11/11** 파일 포함.
- 145,746 nodes. 전체 재인덱싱 후 pending changes/references 0, engine 경고 없음.
- `MinecraftServer`, `GoalSelector`, `NearestAttackableTargetGoal` 심볼 조회 성공.
- `.gitignore`에서 `sources/**/`와 Java를 명시적으로 포함하여 `target` 기본 제외를 해제했습니다.
- 디컴파일본을 Cargo `target` 아래에 저장하지 않았습니다. 바이너리·보고서·도구만 인덱싱 대상에서 제외합니다.

파일 전체 포함과 대표 심볼 확인을 검증했습니다. 모든 Java 구문의 해석 정확도와 모든 호출 관계의 완전성까지 검증한 것은 아닙니다.

## Pumpkin 기준

[`Pumpkin-MC/Pumpkin`](https://github.com/Pumpkin-MC/Pumpkin)의 전체 클론을 형제 `Pumpkin MC`에 준비했습니다.
커밋은 `8d0d0d311778cb0aecb5fc957d571a38f286fda0`으로 고정하고 detached HEAD로 사용합니다.
`crates/pumpkin-plugin-wit` submodule은 `1ad73fff1e0a9e21b99255816df5f99f6260c1b9`입니다.
원본 `LICENSE`는 GPL-3.0 문서이며 클론에 그대로 보존되어 있습니다. 현재 Arrow MC로 가져온 Pumpkin 구현은 없습니다.
별도 CodeGraph 인덱스와 서버 tick·AI 참조 조회를 확인했습니다. Pumpkin 자체의 빌드/테스트는 수행하지 않았습니다.

원시 검증 자료는 형제 `Decompile/reports/26.3-pre-2/`의 `provenance.json`,
`GoalSelector.javap.txt`, `decompiler.log`, `index-verification.json`에 있습니다.

## 준비 도구 검증과 제한

Python 단위 테스트 6개와 PowerShell `Sync-References.ps1`의 실제 실행을 통과했습니다.
PowerShell 파일 자체는 이 CodeGraph 버전에서 인덱싱되지 않아 직접 실행으로 확인했습니다.
구현 저장소의 Python 3개 파일은 인덱싱되어 변경 대기 없이 조회됩니다. 다만 빈 저장소에서 처음 생성한
인덱스의 engine metadata가 비어 있어 재인덱싱 권고가 남습니다. 전체 재빌드는 현재 MCP가 DB를 열고 있어
Windows 파일 잠금으로 실패했습니다. CodeGraph MCP 연결을 종료한 후 아래 명령으로 메타데이터를 갱신할 수 있습니다.
이 제한은 별도 검증을 완료한 Decompile·Pumpkin 인덱스에는 해당하지 않습니다.

```powershell
codegraph index 'E:\projects\Arrow MC\Arrow MC'
```
