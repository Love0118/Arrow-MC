# 청크 heightmap·시야·패킷 연결 기반

실제 청크 소비자를 위해 여섯 heightmap, 전체 시야2..32의 추적 차이, clientbound chunk/light 패킷을 구현했다.
공식 `26.3-pre-2` 소스와 실행 관찰에 근거한 독립 Rust 구현이다. 이 기능들을 실제 world/ticket/light/player 활성화와
연결하는 서버 coordinator는 아직 남아 있다. 데이터·wire 구현 성공을 Play 준비 완료로 표시하지 않는다.

## Heightmap과 현재 registry

`world::heightmap`의 `HeightmapKind`는 공식 ID0..5를 공유한다. 저장 및 worldgen용 두 종류와 live용 네 종류를 처리하며,
clientbound producer가 선택하는 종류는 **WORLD_SURFACE(1), MOTION_BLOCKING(4), MOTION_BLOCKING_NO_LEAVES(5)**다.
현재 판정은 옛 `blocksMotion`/Leaves 클래스 검사 대신 configuration의 두 block tag와 air/fluid 정보를 사용한다.

heightmap을 도입한 format v2는 검증된 configuration tag와 static block ID domain을 결합했다. 현재 format v3는 이 의미를 유지하며 조명용 binary metadata를 추가한다. block별 두 tag bit를 기존 state flag byte에
합치므로 추가 state 배열이 없다. 전체35,723상태를 실제 `Heightmap.Types.isOpaque`와 대조했다. block entity type49개의
이름/ID도 같은 신뢰 문맥에서 제공하지만, block entity의 실제 `getUpdateTag` 행동은 이 데이터로 대신하지 않는다.
v1/v2는 과거 로컬 참조로 보존하고 현재 loader는 v3의 다섯 데이터 파일을 검증한다. [준비 명령과 현재 manifest](chunk-storage.md)를 사용한다. 이 schema 갱신만으로 light engine·world/Play 완료를 주장하지 않는다.

`HeightmapSource`는 canonical section을 고정 배열의 참조로 빌린다. palette 복사나 추상적인 ChunkAccess 구현체를 만들지 않는다.
높이 값은 minY 상대값256개를 padded64-bit words에 보관한다. 높이384에서는9bits/37words, map당296bytes다.
높이가 커져도 지원 범위 안에서 map당 최대512bytes이며 요청/실제 capacity를 예산에 포함한다.

- 같은 길이의 저장 long 배열은 padding·범위를 포함한 raw bits를 보존한다. 길이가 맞지 않으면 해당 map을 다시 계산한다.
- 현재 status가 요구하는 누락 map만 계산하고, 이미 저장된 인식 가능한 다른 종류도 유지한다. 여러 누락 map은 한 column 스캔에서 함께 계산한다.
- prime은 Vanilla처럼 매칭 상태가 없는 column의 기존 값을 그대로 둘 수 있다. 무조건0으로 초기화하는 별도 의미를 넣지 않는다.
- increment update는 top 아래 빠른 반환, 새 top, 제거 후 하향 검색과 반환값을 보존한다. prime/update에는 추가 heap 할당이 없다.
- 실제 ordinary chunk의 air-only section 조회와 `Blocks.AIR` prime 예외를 구분한다. debug world의 가상 지형은 구현 범위가 아니다.

`HeightmapSet::from_canonical`은 실제 저장 NBT를 읽어 이 규칙으로 복원한다. map은 resident와 별도 예산을 가진다.
현재 동기 kernel이며 mutable world의 block 변경 callback이나 별도 heightmap CPU job에는 아직 연결하지 않았다.
기존 공유 pool의 section 준비 경로는 그대로 사용한다.

## 시야 추적

`world::view`는 server/client의 signed 요청을2..32 안의 유효 거리로 제한한다. 실제 모양은 축별 경계 보정 후
엄격한 거리 제곱 비교를 사용하는 곡선 경계다. 단순 정사각형으로 대체하지 않는다. simulation distance와 ticket 범위는 별도다.

`ViewDifference`는 heap allocation 없이 Enter/Leave를 순차 생성한다. 겹친 범위의 mixed 순서와
떨어진 위치 이동의 전체 Leave→Enter 순서를 보존한다. 큰 teleport에서 두 위치 사이 세계 전체를 순회하지 않는다.
최대 작업 범위는 겹침133² cells, 떨어진 이동2×67² cells이며 같은 view에는 스캔이 없다.

`PlayerView`는 center 변경의 SetCenter→차이 이벤트→최종 view 교체 순서를 유지한다. 반경만 바뀌면 center를 다시 보내지 않는다.
외부 효과를 승인한 뒤에만 현재 이벤트를 acknowledge한다. queue가 가득 차면 같은 이벤트를 재시도하고,
전이가 끝날 때까지 관련 world/readiness callback을 미뤄야 한다. 지연된 일부 효과와 이전 view가 공존하는 상태는
Arrow의 명시적 역압 처리이며 Vanilla의 같은 tick 중간 상태와 같다는 주장이 아니다.
Enter는 추적 진입이다. 실제 game send-ready가 확인되어야 sender에 pending으로 등록한다.

## 실제 chunk/light 패킷

`server::chunk_packet`은 packet ID와 body를 인코딩한다. framing·compression·encryption은 기존 transport 책임이다.
chunk data에는 heightmap의 **종류→long 배열 map**, 전체 section byte array, block entity 정보가 들어간다.
heightmap wire를 NBT로 가정하지 않는다. low-level codec은6종을 표현하고 caller가 정한 map iteration 순서를 유지한다.
Java decoder의 EnumMap과 runtime producer의 HashMap이 항상 같은 byte 순서를 갖는다고 주장하지 않는다.

light의 네 BitSet은 **VarInt byte count + little-endian bit bytes**이며 뒤의0bytes를 정규화한다. long 배열 형식이 아니다.
low-level light payload 길이0..2048 수용과 실제 DataLayer producer의2048bytes를 구분한다.
implicit empty, 없는 layer, 실제 할당된2048byte 전부0인 layer는 다른 표현이다. byte 값을 스캔해 임의로 empty mask로 바꾸지 않는다.

block entity는 packedXZ byte, Y short, 검증된 type ID, optional network compound를 인코딩한다.
None은 End0, 명시적인 빈 compound는 그대로 인코딩한다. 실제 producer는 빈 update tag를 None으로 바꾼다.
디스크의 block_entities NBT를 그대로 update tag라고 전달하지 않는다.

`encoded_len`은 입력과 정확한 전체 길이를 검사하고 `encode`는 승인된 출력 Vec를 한 번 할당한다.
NBT 소비에 필요한 `nbt::network_encoded_len`만 추가했으며 임시 NBT Vec·범용 codec framework는 도입하지 않았다.
깊이 최대512의 고정 frame으로 mixed list·MUTF-8·출력 제한과 오류 순서를 검사한다.
큰 packet kernel은 앞으로 실제 producer의 공유 CPU 작업/소유권과 연결해야 한다. source·encoded output·delivery 복사본의
동시 보관 비용은 각각 계상하며 개별 Vec 상한을 전체 프로세스 RSS 상한으로 취급하지 않는다.

Start/Finish/Forget/CacheCenter/CacheRadius는 고정11byte stack buffer로 인코딩한다.
`delivery_bytes`는 기존 queue의 Data buffer를 복사하지 않고 빌리며 control intent도 실제 bytes로 변환한다.
queue는 실제 write 완료 뒤에만 전진하고 실패·취소 때 연결과 queue를 함께 폐기한다.

## 검증 범위

| 경로 | 실제 근거 |
| --- | --- |
| Heightmap | ProtoChunk10시나리오/363연산의 전체70snapshot,6종×256조회·raw words 및 모든35,723상태 predicate 대조 |
| View | Java 관찰6,182행: 전체 grid124,062 membership, 수치 경계3,528행, 순서 있는 diff112개 포함 |
| Chunk codec | 실제 registry 문맥의 공식 constructor/codec78사례,231byte golden, 실제 TCP packet 순서 |
| NBT sizing | 기존 binary 검증과 합쳐24개, 깊이·출력 한계별 실제 writer 길이/오류 비교 |
| Root 조합 | view→control bytes와 Forget queue의 Full/retry2개. center/radius는 수동 sink 조합이며 production coordinator 검증은 아님 |

TCP 검증은 실제 section encoding→chunk packet→sender queue→공유 CPU ConnectionTransport→socket을 연결한다.
server의 Play 상태 활성화나 전체 light/world producer를 구동한 시험은 아니다.
정확성·최적화/추상화 독립 리뷰 두 역할이 마지막 root 조합까지 검수했다.

v2 registry와 실제 raw Anvil inspector 실행에서는4개 heightmap/1,184bytes를 계산했고 (0,0)의 종류1/3/4/5 높이는16이었다.
동일 실행의24sections는192bytes였다. 원본은 로컬 `Roadmap/reviews/inspect-heightmap-example.json`에 있다.
전체 테스트·native CI 상태는 [구현 상태](foundation-status.md)에 별도로 기록한다.
