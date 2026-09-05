# 소스 탐색 결과와 최적화 계획

기준: Vanilla `26.3-pre-2`, Pumpkin `8d0d0d311778cb0aecb5fc957d571a38f286fda0`, 2026-09-05.
네트워크, 청크·메모리, tick·AI, 플랫폼·빌드의 네 조사로 CodeGraph 진입점과 현재 원문을 확인했다.
**정적 조사 완료 / Rust 구현·게임 장애 재현·성능 측정 미실행** 상태다.
아래의 채택은 구현 방향을 뜻하며 이미 빨라졌거나 RAM이 줄었다는 의미가 아니다.

제품 제약·지원 플랫폼·실행 경계는 [architecture.md](architecture.md), 진행 상태는 형제 `Roadmap/README.md`에서 관리한다.

## 먼저 확정한 것

- Linux ARM64/x86_64, macOS ARM64, Windows x86_64의 네 플랫폼을 대상으로 한다.
- 청크 로딩·tick의 멀티코어 처리량과 지연 개선을 우선하고, 이를 위한 **상한이 있는 RAM 증가를 허용**한다.
  메모리는 처리량·p99 개선 대비 비용으로 평가한다. 불필요한 crate·generic·macro·plugin 계층은 늘리지 않는다.
- tick 병렬화는 **P1~P2의 핵심 개발 항목**이다. 단일 스레드 경로는 비교 기준과 fallback이다.
- 바닐라 view-distance **2..32** 전체를 유지한다. **0.01..64 chunks/tick** 전송률은 별도 계약이다. 반경 64 확장은 이번 범위에 없다.
- 패킷의 인과 순서, 같은 tick의 상호작용과 입력 가시성, RNG 의미를 변경하는 코드는 성능 최적화로 그대로 채택하지 않는다.
- Vanilla의 순차 실행기·대기 구조·내부 자료구조를 그대로 유지할 의무는 없다. 호환 대상은 관찰 가능한 결과와 필요한 의존 순서다.

## tick 루프 병렬화에 대한 판단

Pumpkin은 세계·플레이어·엔티티·block entity와 block/fluid/random tick을 병렬 실행한다.
이는 중요한 최적화 전략이며 Arrow MC도 초기부터 병렬 tick을 구현·검증한다.
다만 현재 소스의 모든 루프를 그대로 가져오는 것은 별도의 결정이다.

구체적으로 Pumpkin은 예약 block/fluid tick 입력을 정렬한 뒤 32개 단위 `par_chunks`에서 월드 callback을 호출한다.
정렬된 두 작업 중 앞 작업이 상태를 쓰고 뒤 작업이 그 상태를 읽는 경우, batch가 동시에 실행되면 뒤 작업이 이전 값을 읽을 수 있다.
서로 독립적인 작업에는 이런 문제가 없지만, 청크가 다르다는 사실만으로 이웃 갱신·공유 RNG·인벤토리 의존성이 사라지지는 않는다.
이것은 소스에서 확인한 **순서 보장 차이와 호환성 위험**이며 이번 조사에서 특정 게임 장애를 재현한 결과는 아니다.

근거: [Pumpkin 입력 정렬](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-world/src/level.rs:594>),
[병렬 상태 변경](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin/src/world/mod.rs:1907>),
[Vanilla 예약 tick drain](<E:/projects/Arrow MC/Decompile/sources/26.3-pre-2/net/minecraft/world/ticks/LevelTicks.java:126>),
[Vanilla 단계 순서](<E:/projects/Arrow MC/Decompile/sources/26.3-pre-2/net/minecraft/server/level/ServerLevel.java:390>).

첫 병렬 경로는 선택한 tick 종류의 읽기/쓰기·이웃·RNG·전역 상태 범위를 보수적으로 정의한다.
원래 논리 순서의 의존 관계를 유지하면서 독립적인 준비 작업을 동시에 실행하고, 상호작용 영역은 같은 순서로 실행한다.
동적 전파 범위를 알 수 없는 작업은 상태 변경 전에 소유자 경로로 보낸다. 같은 tick의 앞선 변경을 읽도록 하며,
오래된 snapshot의 결과를 순서대로 commit하는 것만으로 해결했다고 판단하지 않는다.
부분 변경 후 동일 tick을 다시 실행하거나 경계 효과를 다음 tick으로 미루는 방법도 기본 해법으로 사용하지 않는다.

고밀도 상호작용에서는 작업이 하나의 순차 묶음으로 합쳐질 수 있다. 분산된 부하와 밀집 부하 모두에서 scheduler 비용·확장성·RAM을 측정한다.
처음부터 범용 transaction/MVCC/rollback 엔진을 만들지 않는다. 대조 실행의 실제 접근 trace는 범위 모델의 회귀 검사에 사용하되,
몇 번 관측한 접근 목록만으로 앞으로의 모든 입력이 독립적이라고 판단하지 않는다.

## 가져올 최적화와 바꿀 부분

| ID | 방향·시점 | 확인한 근거 | Arrow MC 적용 및 통과 조건 |
| --- | --- | --- | --- |
| OPT-01 | tick 병렬화 **방향 채택**, P1~P2 | Pumpkin `world/mod.rs:1507-1596,1907-1987`; Vanilla `ServerLevel.java:390-455,829-855` | 의존성 단위 병렬 tick. P1 prototype은 선택한 실제 tick의 독립/상호작용 사례로 검증한다. 승객·hopper·redstone·유체·spawn/despawn·동일 target 경쟁은 각 기능 도입 때 대조 범위를 확대한다. 전체 mutable loop의 무검증 이식은 제외한다. |
| OPT-02 | CPU/I/O 분리·제한된 generation **수정 채택**, P1 | Pumpkin `main.rs:44-59`, `schedule.rs:101-144,1312-1324` | 공용 CPU pool 하나와 전체 admission 예산. per-Level pool·무제한 결과 큐는 복사하지 않는다. 작업 개수뿐 아니라 입력·출력·scratch bytes, 취소 후 실제 해제까지 계측한다. |
| OPT-03 | single-value·균일 light **채택**, packed/dense/mixed **초기 비교**, P1 | Vanilla `PalettedContainer.java:75-91`, `Strategy.java:25-59`, `DataLayer.java:86-98`; Pumpkin `palette.rs:157-206`, `format/mod.rs:810-840` | 단일값에서는 배열 지연 할당. 4096-entry 4-bit payload는 2 KiB, Pumpkin indexed는 4 KiB/dense는 8 KiB이다. 이는 payload 산술이며 RSS 실측이 아니다. packed를 기준으로 활성 chunk의 indexed/dense, generation draft와 보관 packed의 혼합을 비교한다. 추가 RAM이 병렬 처리량·지연을 개선하면 채택할 수 있다. |
| OPT-04 | count·활성 section mask **수정 채택**, P2 | Pumpkin `palette.rs:126-150,428-446`, `level.rs:523-545` | 재균일화·불필요한 section 스캔 제거를 검증한다. 매 변경의 전체 index 재매핑·`shrink_to_fit`은 보류. mask 무효화·지원 차원 높이·RNG 소비를 유지한다. |
| OPT-05 | compressor·scratch 재사용, 큰 codec CPU 분리 **수정 채택**, P1~P2 | Pumpkin `packet_encoder.rs:85,130-174,209-218`, `net/java/mod.rs:704-809` | 초기 연결당 한 framing 작업으로 순서를 단순화하고 연결 간 병렬 처리한다. buffer retained capacity와 worker 수의 곱을 측정한다. 작은 packet은 inline, 큰 작업만 offload하는 경계를 실측한다. |
| OPT-06 | 병렬 chunk encode·epoch 검사 **수정 채택**, P2 | Pumpkin `chunk_sender.rs:156-184,239-265`; Vanilla `PlayerChunkSender.java:50-111` | immutable snapshot과 명시적 revision으로 encode하고 원래 후보 순서로 조립한다. batch 전체 queue 용량 예약 성공 후 sent ledger 갱신. `Arc` pointer 동일성만으로 내용 불변을 가정하지 않는다. |
| OPT-07 | 단계 의존 generation·load 중복 억제 **수정 채택**, P2~P3 | Pumpkin `chunk_holder.rs:5-17`, `schedule.rs:854-924`, `file_manager.rs:52-94,142-164`; Vanilla `ChunkMap.java:513-536` | 필요한 단계·이웃 의존성과 in-flight 생명주기를 구분한다. Vanilla status/radius/ticket·unload 의미를 포팅하고 ready job만 실행한다. watcher 수만으로 모든 청크 생명주기를 대체하지 않는다. |
| OPT-08 | pending write 병합·region별 I/O **채택/수정 채택**, P2~P3 | Vanilla `IOWorker.java:129-168`; Pumpkin `file_manager.rs:257-318`, `anvil.rs:621-627,799-815` | 동일 chunk pending write의 최신 snapshot 병합과 read-your-write 유지. region별 쓰기 소유권, 다른 region의 제한된 병렬 I/O. snapshot generation·저장 성공·durable flush를 별도 추적한다. |
| OPT-09 | 작은 goal 목록·control slot·pathfinding scratch **수정 채택**, P2~P4 | Pumpkin `goal_selector.rs:11-59`, `pathfinder/mod.rs:39-54,104-105,149-153`; Vanilla `GoalSelector.java:24-43`, `WalkNodeEvaluator.java:30-48` | 작은 Vec/배열과 재사용 heap/node buffer 평가. instance identity·stable removal·priority·tie-break 유지. 모든 AI의 순서·탐색 한계·RNG를 Pumpkin 값으로 바꾸지 않는다. |
| OPT-10 | phase에 맞는 위치/AABB cache **수정 채택**, P2 | Pumpkin `world/mod.rs:1489-1509,1555-1565` | 실제로 필요한 읽기 시점의 값만 재사용한다. player tick 이전 snapshot을 이후 충돌에 무심코 재사용하지 않는다. 변경·teleport·collision fixture와 retained bytes를 비교한다. |
| OPT-11 | 고정 데이터 공유·변경분 생성 **방향 채택**, P1 | Pumpkin `pumpkin-data/src/lib.rs:14-41`, `pumpkin-macros/Cargo.toml:8-18` | P1에는 codec·팔레트·접속에 필요한 최소 packet/block/biome ID·registry schema를 준비한다. 전체 registry/datapack/reload는 P2에서 확대한다. 입력 해시가 같으면 재생성을 생략하고 동일 내용 파일은 다시 쓰지 않는다. 대형 Rust 생성 코드와 compact blob의 빌드·시작 시간·RSS를 비교한다. |
| OPT-12 | 작은 빌드 범위·portable profile **채택**, P0~P1 | Pumpkin root `Cargo.toml:3-21,113-116,221-225`, server `Cargo.toml:33,113-119`, `flake.nix:59-60` | 초기 server 빌드 범위를 명시하고 plugin VM·WASM/WASI·Bedrock·웹 스택을 제외한다. Cargo 기본 release와 thin LTO를 비교한다. fat LTO/1 unit·native CPU·allocator 변경을 기본 강제하지 않는다. |

표의 Pumpkin 축약 경로는 `crates/pumpkin/src/`, `crates/pumpkin-world/src/`, `crates/pumpkin-protocol/src/`의 해당 파일을 뜻한다.
정확한 경로는 아래 원문 링크와 형제 조사 보고서에 있다. 값별 count, mask, 캐시, SIMD 등은 메모리를 추가할 수 있으므로
유용해 보인다는 이유만으로 모두 켜지 않는다. 각 기능의 기준선과 비교해 남길 최적화를 결정한다.
snapshot·double buffer·넉넉한 worker scratch·여러 in-flight chunk는 병렬성 확보 비용으로 허용되는 초기 실험이다.
예산 안에서 처리량·tail latency가 개선되면 최소 메모리 표현보다 우선할 수 있다. 초기부터 여러 저장소 backend를 위한 범용 계층을 만들 필요는 없다.

대표 원문: [generation pool](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-world/src/chunk_system/schedule.rs:135>),
[palette](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-world/src/chunk/palette.rs:126>),
[Vanilla packed 전략](<E:/projects/Arrow MC/Decompile/sources/26.3-pre-2/net/minecraft/world/level/chunk/Strategy.java:25>),
[packet encoder](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-protocol/src/java/packet_encoder.rs:85>),
[chunk encode](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin/src/net/chunk_sender.rs:239>),
[region I/O](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-world/src/chunk/io/file_manager.rs:257>),
[Vanilla write 병합](<E:/projects/Arrow MC/Decompile/sources/26.3-pre-2/net/minecraft/world/level/chunk/storage/IOWorker.java:129>),
[goal selector](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin/src/entity/ai/goal/goal_selector.rs:11>),
[pathfinder](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin/src/entity/ai/pathfinder/mod.rs:39>),
[생성 데이터](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-data/src/lib.rs:14>),
[빌드 profile](<E:/projects/Arrow MC/Pumpkin MC/Cargo.toml:113>).

## 그대로 가져오지 않을 동작

| 항목 | 소스에서 확인한 차이·위험 | 처리 방향 |
| --- | --- | --- |
| priority queue 추월·queue Full 유실 | Pumpkin normal/priority 각 4096 items, priority 우선 drain. `try_enqueue_packet_data`는 Full을 debug만 기록한다. chunk commit은 enqueue 성공을 반환받지 않고 sent 상태를 갱신한다. | 연결별 ordered queue, byte 예산, 전체 batch 예약. 임의 추월·조용한 drop·실패한 enqueue의 sent 처리는 제외. |
| 청크 전송률·준비 청크 보충 | Pumpkin 0.1..500, Vanilla 0.01..64. Pumpkin은 가까운 미준비 청크 때문에 빈 quota를 더 먼 ready 청크로 채울 수 있다. | Vanilla ACK/NaN/quota·후보 선택·batch 경계를 포팅한다. |
| 무제한 ingress·player tick당 64 packet 처리 | Pumpkin 수신 `SegQueue`, player tick에서 적용. Vanilla의 packet-processing 단계와 다를 수 있다. | ingress bytes·개수 제한과 원본 적용 단계 유지. rate limit와 buffered memory 상한을 분리한다. |
| random tick/AI RNG·AI 순서 변경 | Pumpkin `rand::random`/`rand::rng`; Vanilla에는 월드 LCG, 엔티티 RNG, behavior의 level RNG 소비가 존재한다. | 알고리즘·소유자·초기 상태·호출 횟수와 AI 단계 순서를 보존한다. world seed만 같으면 모든 live RNG가 같다고 가정하지 않는다. |
| 정렬 후 mutable tick 전체 병렬화 | 앞서 설명한 same-tick 의존성 차이. | OPT-01의 독립성 검증과 순서 있는 작업 묶음으로 재구성한다. |
| watcher 기반 수명·dirty 선행 해제 | Pumpkin ticket에 lifetime/id TODO, 저장 전에 dirty를 false로 만드는 경로, lock drain 자체가 flush는 아닌 경로가 있다. | Vanilla ticket 종류·만료·저장 revision을 보존하고 실패/동시 mutation/종료 시험. load 오류는 Vanilla의 분류·보고·fallback 조건대로 처리한다. |
| 전체 region backing buffer 상주·item 수만의 queue 제한 | 작은 Bytes slice가 큰 region buffer를 붙잡거나 가변 크기 batch가 queue 한 항목이 될 수 있다. | compressed/raw/decoded/snapshot/scratch/cache의 retained bytes 합산, 작업 이전 admission·cache 회수 정책. |
| Anvil header 선행 갱신·다른 저장 형식 기본화 | Pumpkin in-place 재배치 경로는 payload보다 header를 먼저 쓰며 rollback/corruption 관련 주석이 있다. | 최초 저장 형식은 Vanilla Anvil + `.mcc`. crash atomicity를 단순 write 순서만으로 보장했다고 주장하지 않고 장애 복구를 시험한다. |

근거: [queue 포화](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin/src/net/java/mod.rs:510>),
[송신 drain](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin/src/net/java/mod.rs:723>),
[chunk ledger](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin/src/net/chunk_sender.rs:271>),
[Vanilla chunk sender](<E:/projects/Arrow MC/Decompile/sources/26.3-pre-2/net/minecraft/server/network/PlayerChunkSender.java:114>),
[dirty 저장](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-world/src/chunk/io/file_manager.rs:360>),
[ticket TODO](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-world/src/chunk_system/chunk_loading.rs:91>),
[Anvil 쓰기](<E:/projects/Arrow MC/Pumpkin MC/crates/pumpkin-world/src/chunk/format/anvil.rs:351>).

## 공통 채택 시험

| 축 | 동일하게 고정할 조건 | 비교·실패 판단 |
| --- | --- | --- |
| 동작 | baseline 버전, save/seed, 캡처한 live RNG 초기 상태·입력 trace, view/simulation 설정 | tick 단계·상태 변화·RNG draw·예약 tick·연결별 packet trace. packet 순서를 정렬해서 차이를 지우지 않는다. 가변 timestamp 등은 필드별로 근거를 기록해 정규화한다. |
| 병렬성 | 같은 작업을 1/2/4/8/허용 N workers, 전체 thread 예산과 CPU 제한 기록 | 완료 순서 지연 주입, same-tick 최신 상태 읽기, serial fallback 전환, 독립/밀집 활동. 동일 작업량의 처리량·CPU time·speedup 및 scheduler 비용. |
| RAM | 동일 save·플레이어 trace·거리, generated/ungenerated와 중첩/분산 플레이어 분리 | steady/peak RSS, live chunk/entity/connection당 bytes, 큐·결과·scratch·snapshot·cache retained bytes, unload 후 회수 시간. 범위를 축소해 얻은 수치는 불합격. |
| 부하·실패 | 느린 client/disk, login storm, 이동·teleport·dimension 변경, shutdown·write 오류 | packet 무손실·순서, 오래된 결과 차단, dirty revision 유지, read-your-write, 저장 후 재시작·복구. 취소한 실행 작업의 메모리도 실제 해제까지 계산. |
| 지연 | 동일한 tickrate·부하·warm-up·측정 구간·반복 횟수 | tick p50/p95/p99·최장 stall, 작업 대기·save·chunk-ready 지연. 20 TPS 설정의 50 ms 예산과 catch-up/freeze/step/sprint 동작을 함께 확인. |
| 플랫폼 | 네 native runner, 고정 toolchain/target/features/profile/OS/CPU | debug/release 수치·codec fixture, 실제 TCP·파일 I/O·종료·복구. cross compile만으로 실행 검증 완료 처리하지 않는다. |
| 빌드 | cold/warm dependency cache를 분리, clean/no-change/leaf/common-type/generated-data 변경 | wall time·peak build RAM·link time·binary 크기·재컴파일 범위. `cargo build --locked --timings` 보고서와 runtime 결과를 함께 기록. |

압축 결과는 backend/레벨에 따라 bytes가 달라질 수 있다. 복원된 payload, 필드 의미, 전환 프레임 경계와 packet 순서로 판정하며
디스크 레이아웃·팔레트 번호도 의미 동등성 규칙을 미리 정한다. 반대로 관찰 가능한 게임 결과 차이를 정규화로 숨기지 않는다.
성능 기준값과 개선율은 첫 실행 가능한 P1 기준선에서 측정한다. 현재 수치를 만들어 넣지 않는다.

## 추가 조사 범위

이번에는 protocol registry 전체·signed chat/bundle 예외, 모든 Brain/AI, redstone·유체·worldgen·lighting의 전체 경로와
모든 저장 복구 경우를 완주하지 않았다. 네 플랫폼 실행이나 Pumpkin 전체 성능도 측정하지 않았다.
각 기능 착수 시 Java의 정확한 호출 관계·추가 경로를 조사하고 작은 검증 단위로 로드맵을 확장한다.

로컬 상세 보고서: `Roadmap/research/network.md`, `chunks-memory.md`, `multicore.md`, `platform-build.md`.
이 계획과 architecture는 구현 Git에 저장되며, 상세 조사와 진행표는 기존 합의대로 형제 Roadmap에 로컬 보관한다.
