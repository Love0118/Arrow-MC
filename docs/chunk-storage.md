# 현재 버전 청크 저장 로딩

`26.3-pre-2` / DataVersion `5018`의 Anvil 파일을 실제로 읽고, 공용 CPU pool에서 압축·NBT·section을 decode한 뒤
별도 resident 예산으로 소유권을 옮긴다. `examples/inspect_chunk.rs`와 `tests/chunk_storage_pipeline.rs`는 이 경로를
network section encoding까지 연결한다. 이후 [로드된 청크 소유자](chunk-loading-owner.md)를 추가해 요청 identity와 canonical view를 검증한다.
아직 서버의 live world·ticket·spawn에 연결하지 않았으며 저장 writer도 없다.

## 데이터와 동작

`world::storage::registry::ChunkRegistrySnapshot`은 공식 API로 생성한 block 1,286개, global state 35,723개,
biome 67개의 ID·default·property·air/fluid 정보를 공유한다. block direct palette 폭은 **16 bits**다.
현재 disk block-state CODEC는 문자열 또는 `{id,properties}`이며 옛 `{Name,Properties}` 형식으로 가정하지 않는다.
property 오류의 개별 default, palette entry 오류의 air/plains 복구, primitive array와 숫자 list 수용을 공식 실행으로 구분했다.
packed long stream의 Float/Double 변환은 boxed `Number.longValue`의 truncation/saturation을 사용한다.

독립적으로 전달한 manifest SHA256을 먼저 검증하고 configuration manifest와 source JAR의 hash·크기를 연결한다.
block/property domain과 state ID/flag를 압축된 배열로 보관하며 청크마다 registry를 복사하지 않는다.
이는 청크 palette용 metadata 구현이다. 전체 typed registry·datapack reload·122 components 구현 완료를 뜻하지 않는다.

`region`은 8 KiB header와 작은 record header만 읽고 선택한 chunk payload를 연다. 음수 좌표와 외부 `.mcc`,
일부 extent/length 오류의 공식 읽기 우선순위를 구분한다. 전체 region buffer를 상주시키지 않는다.
파일 누락·구조상 unavailable·실제 I/O/decode 실패를 구분하며, 실패를 빈 청크 생성으로 바꾸지 않는다.
동시에 외부 프로그램이 파일 내용을 덮어쓰는 경우의 일관된 snapshot은 보장하지 않는다.

gzip(1), zlib(2), raw(3), Java LZ4 block(4)을 지원한다. LZ4 frame 형식과는 다르다.
NBT 소비자는 한 compound root에서 멈춘다. Java의 compressed refill·8 KiB buffering·primitive/bulk read 경계를 맞춰
아직 읽지 않은 trailer를 강제로 검증하지 않는다. 필요한 LZ4 block은 전체 checksum 검증 후 노출한다.
고정 4 KiB scanner가 root 경계를 수집하고 기존 NBT decoder를 한 번 호출하므로 반복 재파싱을 하지 않는다.
이름을 보존하는 일반 named NBT API와 달리 disk root 이름은 불투명 bytes로 skip한다.

`StoredChunkDraft`는 원래 NBT를 보존하면서 section palette·light·status·좌표·시간을 추출한다.
중복 section Y와 아직 활성화하지 않은 entity/tick/structure 데이터도 버리지 않는다. light 배열은 2,048 bytes를 검증한다.
사용하지 않는 palette entry는 내부 압축으로 제거할 수 있다. 셀별 state/biome·count·light 의미는 유지하지만
불필요한 disk palette의 순서나 bit 폭을 그대로 보관한다는 계약은 아니다.

현재 `5018`만 decode한다. 이전/누락 DataVersion은 upgrade 필요, 미래 버전은 미지원으로 반환한다.
DFU, 좌표를 포함한 tick/entity/structure의 개별 복원·활성화, 중복 section의 live 적용,
light engine 등록과 postprocessing은 후속 단계다. 요청 위치의 canonical 게시와 relocation 진단은 owner에서 구현했다.

## 비동기와 소유권

1. `ChunkStore::read`가 I/O slot을 먼저 확보한 후 blocking 파일 읽기를 시작한다.
2. payload 길이를 확인하고 CPU job slot과 byte allowance를 확보한 뒤 compressed buffer를 할당한다.
3. 실제 I/O closure가 permit과 buffer를 보유한다. async await 취소가 진행 중인 파일 읽기의 예산을 먼저 반환하지 않는다.
4. 읽은 buffer를 같은 공용 CPU pool로 이동해 decode한다. 디스크 대기가 CPU worker를 점유하지 않는다.
5. ready 결과도 CPU lease를 보관한다. `try_adopt`는 resident allowance를 먼저 확보한 다음 CPU lease를 반환한다.
   실패하면 원래 결과와 lease를 돌려주므로 이미 준비한 chunk를 재시도할 수 있다.

`ChunkReadKey`는 epoch·좌표·generation을 전달한다. **adoption은 메모리 이전만 수행한다.**
현재 world와의 key 일치, 늦은 결과 거부, ticket 유효성, 저장 좌표 검사 및 live publication은 이후 소유자가 처리해야 한다.
section 준비의 기존 revision 검증과 새 disk loading의 검증 범위를 혼동하지 않는다.
`ChunkLoadingOwner`는 이 중 요청 identity/context와 늦은 결과, 좌표 relocation·canonical resident 게시를 구현했다.
실제 ticket engine·gameplay 활성화는 남아 있으며 낮은 수준 `try_adopt` 자체의 계약은 그대로다.

## 메모리와 의존성

기본 입력 cap은 compressed 8 MiB, inflated 8 MiB, NBT 요청 allocation 16 MiB, typed decode 4 MiB다.
gzip/zlib/raw 작업의 예약은 **28 MiB + compressed bytes**, LZ4는 최대 8 MiB workspace를 추가한다.
LZ4는 먼저 최대 비용을 승인하고 실제 필요한 block 크기만 할당한다.
다른 작업의 예약이 없고 job slot 4개가 비어 있는 128 MiB CPU pool에서는
일반 작업 4개의 compressed 합계가 16 MiB 이하이면 동시에 들어간다.
cap은 Arrow의 명시적인 자원 정책이며 바닐라가 허용하는 모든 파일의 최대 크기라는 주장은 아니다.

resident charge는 NBT 누적 요청 allocation과 typed backing capacity를 합친 보수적 값이다.
registry의 별도 loader admission, 압축 backend 상태·worker stack·제어 객체·allocator·OS 비용은 별도이며 전체 RSS 상한이 아니다.
초기 v1 registry bundle은558,073bytes/loader admission71,433,344bytes였다. 현재 v3 bundle의 입력 크기는 아래 기록으로 구분한다. registry 비용을 청크마다 반복하지 않는다.

단일 Cargo package·기존 CPU pool을 유지했다. `lz4_flex 0.14.0`의 safe/checked decode와
`xxhash-rust 0.8.18`의 xxh32만 추가했다. 고정 외부 의존성은 129개이며 루트 프로젝트를 포함한 lock은 130 packages다.
원본 고지는 `third_party/rust/`에 보존한다.
새 범용 codec framework, crate 분리, per-world thread pool은 추가하지 않았다.

## 재현과 검증

공식 bundle은 로컬 `Decompile`에만 두며 저장소에 배포하지 않는다. 준비 도구는 독립 작성한 공식 API 호출 helper를 실행한다.

```powershell
$configHash = '105626403604b8a2500181c9c27bd6abeab093df23d3f65db91d16245dc8f198'
python tools/prepare_block_state_data.py --configuration-manifest-sha256 $configHash
$registryHash = '19c81b4f667315d5981385cbab154e31b4e0ece899d171afb6fad51caa4a4a39'
cargo run --locked --release --example inspect_chunk -- `
  '../Decompile/bootstrap/26.3-pre-2-block-states-v3' $registryHash $configHash `
  'PATH/TO/WORLD/region' 0 0 -64 384
```

현재 hash는 **format v3**다. configuration-bound heightmap predicate·49개 block entity type ID에 조명용 `lighting.bin`을 추가했다. v1/v2는 과거 로컬 참조로 보존하지만 현재 loader와 oracle 기본 입력은 v3다. Minecraft/DataVersion은 여전히 `26.3-pre-2`/`5018`이며 bundle schema 버전과 혼동하지 않는다.

runtime manifest는 정확히 `blocks.json`, `biomes.json`, `export-metadata.json`, `block-entity-types.json`, `lighting.bin` 다섯 데이터 파일을 승인한다. 현재 이 다섯 파일은1,168,014bytes, manifest13,931bytes를 포함한 입력은1,181,945bytes다. JSON 입력의128배 admission과 binary reader의 비용은 요청 예산이며 실제 RSS 상한이 아니다.

`lighting.bin`589,351bytes는 `ARLITE3\0`, little-endian state/face count, state당16bytes의 emission/dampening/flag/6방향face ID, ordered face-pair bitset으로 구성된다. 현재35,723state·377canonical faces·142,129ordered pair를 포함한다. 자체 `ExportLightingData.java`는 initialized WorldLoader의 공개 state/shape API를 호출하며, descriptor로 통합된 runtime shape variant들에도182,329ordered pair의 동등성을 확인한다. face descriptor 자체는 runtime 데이터 파일이 아니며 provenance에 digest/크기/검증 수만 남긴다.

v3 실제 metadata의 AIR0·VOID_AIR18649·CAVE_AIR18650은 emission0/dampening0/empty-face로 동일하다. available chunk의 높이 밖 lighting lookup에 AIR를 쓰는 근거이며 일반 block identity가 같다는 뜻은 아니다. unavailable chunk는 BEDROCK88의 emission0/dampening15와 별개다. 실제 registry assertion은 `tests/world_lighting_source.rs`의 선택 실행으로 기록한다.
snapshot을 갱신하면 신뢰할 수 있는 준비 명령의 stdout으로 hash를 함께 갱신해야 한다. 기존 출력 directory는 덮어쓰지 않으므로 재생성 시 `--output`으로 `Decompile/bootstrap` 아래 새 경로를 지정한다. 생성 provenance가 달라지면 같은 논리 데이터라도 manifest hash는 달라질 수 있다. oracle override는 `ARROW_BLOCK_STATE_SNAPSHOT`, `ARROW_BLOCK_STATE_MANIFEST_SHA256`, `ARROW_CONFIGURATION_MANIFEST_SHA256`을 함께 사용한다.
예제는 파일 읽기만 수행하며 누락 파일을 생성하거나 world를 활성화하지 않는다.

- 실제 공식 JAR: region 52 fixtures의 두 API 104관찰, chunk 72사례, 전체 registry ID/default/state 조회를 대조했다.
- chunk 72사례에는 collection 표현 20개와 boxed Float/Double 경계 22개를 포함한다. 전체 셀·count·light·metadata를 비교한다.
- 공식 압축 NBT 소비 기록 988사례의 성공/실패 및 blob hash가 일치했다. 개별 예외 문자열 전체 동등성을 주장하지 않는다.
- 실제 blocking I/O·CPU를 지연시킨 취소 시험과 resident 실패/재시도, 음수 region 좌표부터 실제 section encoding까지 검증했다.
- 초기 실제 공식 registry와 직접 만든 raw Anvil 예제: 1 section, network 8 bytes, CPU peak charge 29,360,310 bytes,
  resident charge 2,574 bytes였다. 플레이 가능한 월드 시험은 아니다.
  현재 예제는 canonical owner를 거쳐 누락 기본값을 포함한24sections/192bytes를 준비한다. 별도 결과는 owner 문서에 기록한다.

합성 24-section zlib 청크 12개를 같은 입력으로 한 번씩 읽은 Windows release 측정:

| CPU workers / I/O slots | 전체 시간 | 청크/초 | CPU peak 예약 bytes | 12개 resident charge |
| --- | ---: | ---: | ---: | ---: |
| 1 / 1 | 9.305 ms | 1,289.6 | 29,360,805 | 3,172,656 |
| 2 / 2 | 3.026 ms | 3,966.2 | 58,721,573 | 3,172,656 |
| 4 / 4 | 1.922 ms | 6,242.2 | 117,443,144 | 3,172,656 |

따뜻한 로컬 filesystem cache에서 assertion을 포함한 단일 표본이다. 실제 월드 TPS, p99, cold disk나 네 플랫폼의 가속률을 대표하지 않는다.
정확성·최적화 독립 리뷰 두 역할이 최종 코드와 자원 수명을 검수했다. 원시 근거는 로컬 `Roadmap/reviews/`에 있다.
