# 구현 출처와 라이선스 원칙

2026-09-05 사용자 요구: 바닐라 로직의 기계적 번역으로 라이선스 문제가 생기지 않도록 한다.
완전한 서버 구현 목표를 유지하되 구현 방법과 배포 자료의 출처를 함께 관리한다.
이후 사용자가 **소스 기반 분석·구현을 명시적으로 허용**했다. 소스 참고 자체를 중단 사유로 삼지 않으며,
이 허용을 제3자 코드의 라이선스 면제나 무위험 보증으로 해석하지 않는다.
최종 확인한 개발 목표는 **소스 기반으로 동작을 이해하고, 독립적인 Rust 구현체로 만드는 것**이다.
소스 참조를 막는 clean-room 전제를 도입하지 않는다. 작성 경로와 직접 재사용 여부를 구별해 기록한다.

## 확인한 공식 조건

Mojang은 난독화 제거 후에도 EULA·Usage Guidelines가 그대로 적용된다고 안내한다.
현재 내부 JAR의 `META-INF/LICENSE`도 Minecraft EULA와 Microsoft Services Agreement를 가리킨다.
EULA는 Minecraft 코드·콘텐츠의 배포를 제한하며 원본 코드의 상당 부분을 포함하지 않는 독창적인 Mod를 구분한다.
난독화가 없다는 사실을 오픈소스 라이선스나 기계적 번역의 배포 허가로 간주하지 않는다.

출처: [Mojang 난독화 제거 안내](https://www.minecraft.net/en-us/article/removing-obfuscation-in-java-edition),
[Minecraft EULA](https://www.minecraft.net/en-us/eula), [Usage Guidelines](https://www.minecraft.net/en-us/usage-guidelines).

Pumpkin 참조 커밋에는 GPL-3.0 라이선스 문서가 있다. 실제 코드를 복사·수정·링크해 배포할 때에는
해당 파일과 의존성의 조건·고지·소스 제공 및 라이선스 호환성을 확인해야 한다. 단순 출처 링크만으로 조건을 충족했다고 판단하지 않는다.
현재 Arrow MC의 Rust foundation은 Pumpkin dependency나 복사한 Pumpkin 모듈을 포함하지 않는다.
[고정 Pumpkin LICENSE](https://github.com/Pumpkin-MC/Pumpkin/blob/8d0d0d311778cb0aecb5fc957d571a38f286fda0/LICENSE).

기능·프로토콜 규칙과 구체적인 코드 표현의 구별이 중요하다. 예를 들어 미국 저작권법은 §102(b)에서
아이디어·절차·작동 방법 등을 보호 범위와 구분하고, §101은 번역을 파생 저작물의 예로 든다.
적용 관할, 계약, 실제 코드와 배포 방식에 따른 판단이 필요하며 이 문서는 법적 무위험 보증이 아니다.
[U.S. Copyright Office — Chapter 1](https://www.copyright.gov/title17/92chap1.html).

## 작업 방식

1. 공개 형식·인터페이스와 로컬 공식 프로그램의 관찰 결과를 기능 명세·입출력 fixture로 기록한다.
2. 소스 참조가 필요하면 사실·규칙·예외를 확인하고 출처를 기록한다. Java 메서드 본문·주석·고유한 표현을 복사하거나
   이름과 문법만 바꾸어 Rust로 옮기는 방법은 기본 구현 경로에서 제외한다.
3. Rust 자료구조·알고리즘 구성·오류 API·병렬화는 요구사항에 맞게 독립적으로 설계한다. 동일한 동작과 wire 값이 필요하더라도
   Java class 구조·동기화 방식·내부 구현 전체를 그대로 재현할 이유는 없다.
4. 구현별로 읽은 참조, 작성 방법, 직접 가져온 코드·데이터·의존성 여부, oracle/테스트 근거를 기록하고 독립 리뷰어가 확인한다.
5. 직접 재사용할 오픈소스 코드는 파일별 조건과 프로젝트 라이선스 호환성이 해결된 경우에만 도입한다.
   이를 해결하지 않은 코드를 우선 넣고 나중에 고지를 추가하는 순서를 사용하지 않는다.
6. JAR·디컴파일 Java·원본 assets·대량의 생성된 Mojang 데이터는 현재처럼 형제 로컬 참조에 두며 Git 배포물에 포함하지 않는다.
   서버가 필요로 하는 내장 데이터의 배포/사용 방법도 별도 출처·권한 항목으로 관리한다. 로컬 추출만으로 모든 이용이 허용된다고 주장하지 않는다.

## clean-room 표현

엄격한 clean-room은 원문 분석 담당과 구현 담당의 정보 접근을 분리하고 그 절차를 입증하는 방식이다.
현재 에이전트들은 디컴파일 소스에 접근했으므로 이 작업을 **clean-room이라고 부르지 않는다**.
원문을 봤다는 이유만으로 독립 구현이 자동으로 불가능하다고 단정하지도 않는다.
현재 방침은 출처가 기록된 독립 설계와 동작 대조이며, source exposure와 실제 작성 경로를 숨기지 않는다.

## 현재 foundation 출처 기록

| 파일 | 작성 경로 | 직접 가져온 코드/의존성 | 검토 한계 |
| --- | --- | --- | --- |
| `src/wire/mod.rs` | VarInt/VarLong의 byte 의미를 확인한 후 slice API·오류·길이 기반 writer를 별도 작성 | 작성 에이전트 보고와 원문 비교에서 pasted body/comment 없음, 외부 dependency 없음 | source-exposed 구현. 형식상 필요한 bit 연산·상수 유사성은 존재하며 법적 판정은 아님 |
| `src/nbt/mod.rs`, `read.rs`, `write.rs` | NBT 규칙·Java oracle 확인 후 UTF-16 값·정렬 Vec compound·transactional reader/writer·Rust 자원 예산을 별도 설계 | 작성 에이전트 보고에서 pasted body/comment 없음, Pumpkin dependency 없음 | mixed-list wrapper·quota·수치 의미는 호환 목적의 원문/관찰 기반이며 clean-room 아님 |
| `src/nbt/numeric.rs`, `predicate.rs`, `path/*` | 현재 NumericTag·NbtUtils·NbtPathArgument와 실제 API 관찰에서 규칙을 확인한 후 반복형 비교·소유 참조 BFS·공유 예산·오류/해제를 독립 설계 | 원문 본문·주석 복사나 범용 Java class hierarchy 도입 없음 | Java 객체 별칭·unchecked 예외·End binary 출력과 다른 경계를 명시하며 전체 서버 소비자 동등성은 별도 검증 |
| `src/snbt/*` | 현재 grammar·직접 만든 JVM 사례를 확인하여 구체적인 UTF-16 parser, 작은 오류 메타데이터와 formatter를 별도 작성 | parser framework·Java 본문 복사·외부 Rust dependency 없음 | source-guided 구현이며 모든 새 소비자의 동작 검증을 대신하지 않음 |
| `src/unicode_names/*`, `third_party/unicode/*` | 공식 Unicode16 데이터에서 자체 생성기가 compact binary를 생성, 독립 lookup을 Java25와 대조 | Unicode 데이터는 Unicode License v3로 포함하며 전체 고지·URL·checksum 보존 | JDK 소스·table·oracle 출력은 생성 입력 아님. 바이너리 배포 시 고지 동봉 필요 |
| `src/server/*`, `src/main.rs` | 공식 packet/listener/UTF·crypto/auth 규칙·실제 JVM 관찰을 바탕으로 단일 socket owner와 연결 제한·종료·configuration을 별도 설계 | Java/Pumpkin 본문 복사 없음. Tokio/serde_json/OpenSSL/reqwest/serde 사용, native 코드를 포함한 의존성 고지 수집 | online 검증은 local mock service에서 수행. 실제 account·spawn/Play와 전체 게임 서버 동등성은 미완료 |
| `src/world/section*`, `preparation.rs`, `src/runtime/*` | PalettedContainer/SimpleBitStorage/section wire 규칙·실행 관찰에 근거한 compact enum·packed words·사전 할당 kernel과 fixed CPU worker·permit 수명 별도 설계 | 원문 본문 복사나 Pumpkin pool 직접 도입 없음. kernel/pool 추가 framework 없음 | section owner의 revision 검증과 저장 결과의 resident 이전을 구분. 실제 game tick은 후속 검증 |
| `src/world/storage/*` | RegionFile·NbtIo·NbtOps·SerializableChunkData·공식 압축 API의 동작을 조사하고 borrowed codec·bounded I/O와 pull reader를 독립 설계 | Java/JDK method body 복사 없음. flate2/zlib-rs·lz4_flex·xxhash-rust 사용 및 고지 | 현재5018 읽기만 지원. 일부 내부 palette 압축 차이 명시, DFU·world 활성화·저장 writer 미완료 |
| `src/world/ticks*` | 공개 tick container·heap/map의 실제 관찰에서 동률·pending·lazy query 규칙을 확인해 bounded queue/index를 독립 설계 | Java/fastutil body·주석 복사나 Java collection 계층 도입 없음 | source-exposed 구현이며 실제 block/fluid callback·durable 저장 연결은 별도 |
| `src/world/loading.rs` | SerializableChunkData·ChunkHolder와 실제 경계 관찰에 근거해 opaque request·canonical index·borrowed 준비 결과를 독립 설계 | Java/Pumpkin 본문 복사·별도 pool·crate·dependency 없음 | canonical resident와 staged light이며 실제 world 활성화·send-ready 완료가 아님 |
| `src/server/chunk_sender.rs` | PlayerChunkSender와 fastutil/Guava 공개 동작을 실제 실행해 quota·동률 규칙을 확인한 후 bounded pending/selection/delivery를 독립 설계 | 원문 method body·주석을 복사하지 않고 별도 Rust 자료구조 작성, 추가 dependency 없음 | 호출자가 실제 readiness와 packet body를 제공. encoder/TCP 조합은 검증했으나 실제 world producer·Play 상태는 미연결 |
| `src/world/heightmap.rs`, `view.rs` | Heightmap·ChunkTrackingView·ChunkMap의 동작과 실제 ProtoChunk/geometry 관찰에 근거해 packed column·borrowed source·streamed difference를 독립 설계 | Java method body 복사·범용 world trait·추가 dependency 없음 | ordinary world 데이터 kernel이며 light/ticket/player 활성화는 별도 |
| `src/server/chunk_packet.rs`, `src/nbt/size.rs` | 현재 registry-aware stream codec과 기존 독립 NBT writer 규칙에서 borrowed packet model·정확한 크기 계산·고정 control buffer를 별도 작성 | 원문 codec class hierarchy나 본문을 복제하지 않음, 새 dependency 없음 | 실제 TCP 조합은 검증하되 block entity update 행동·world readiness/Play는 후속 |
| `tools/oracles/ExportBlockStateData.java`, `prepare_block_state_data.py` | 직접 작성한 helper가 공식 registry의 public API를 호출하고 hash로 연결한 최소 palette metadata를 출력 | Java method body·공식 bulk data를 저장소에 넣지 않음. 출력은 로컬 Decompile에 보관 | exporter source 공개와 생성된 Mojang 데이터의 배포 권한을 동일하게 취급하지 않음 |
| `tests/*` | 직접 작성한 작은 binary 사례 및 로컬 공식 API 호출 oracle | JAR·디컴파일 파일을 저장하지 않음. fixture는 작은 입력·관찰 결과 | 일치 검사는 정확성 근거이며 저작권 허가를 대신하지 않음 |

코드 리뷰는 복사 여부·표현·의존성을 검토하는 공학적 절차다. 프로젝트 전체의 법적 적합성을 보증하는 절차로 표시하지 않는다.
직접 복제/번역된 표현이나 권한이 불명확한 포함 데이터가 발견되면 해당 부분을 공개 변경에서 제외하고 해결한다.

현재 제3자 데이터 고지는 [THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md)에 모으고,
Unicode 데이터의 구체적인 출처와 검증은 [unicode-data.md](unicode-data.md)에 기록한다.
