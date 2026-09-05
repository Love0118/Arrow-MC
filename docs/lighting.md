# 조명 데이터·계산·게시 경계

고정 기준은 Minecraft `26.3-pre-2`, DataVersion `5018`이다. 현재 구현은 block/sky 조명의 값·저장소·전파 커널과,
선택한 불변 청크 영역의 초기 재계산 또는 저장 조명 복원을 공용 CPU pool에서 수행해 게시하는 경로다.
**저장 복원은 선택 domain의 유한한 initialization/light transaction이다. 일반 `ThreadedLevelLightEngine`
callback dispatcher·1,000-task marker·live chunk status/flag·ticket·Play 연결은 아직 완료하지 않았다.**

커널·초기 통합의 `5e14ec7`와 resident 예산 이전의 `e0b4593`은 각각 당시 네 native 플랫폼 검증을 마쳤다.
현재 저장 복원·queued-first snapshot 배치는 실제 Java 대조와 독립 검수를 진행했으며 전체/native 결과는 별도로 기록한다.
배치별 결과는 [구현 상태](foundation-status.md)에 기록한다.

## 책임과 호출 경로

| 경로 | 현재 책임 | 이 경로가 보장하지 않는 것 |
| --- | --- | --- |
| `world/storage/registry/lighting.rs` | 고정 v3의 state별 조명 값, cached face ID, ordered face-pair 판정 | 일반 collision shape·동적 world shape 구현 |
| `world/lighting/source.rs` | 불변 section/source identity와 canonical 원래 saved row·status·flag 조회 | owned terrain에 저장 provenance 생성, ticket eligibility 자동 결정 |
| `world/lighting/layer.rs`, `storage.rs` | DataLayer 표현, section 지원 수, queued/updating/visible 상태, snapshot·COW·알림 | 모든 chunk 로딩 callback과 저장 writer |
| `world/lighting/queue.rs`, `block.rs` | check/decrease/increase 큐와 block light 전파 | 실제 world mutation event의 자동 입력 |
| `world/lighting/sources.rs`, `sky.rs` | 열별 sky source와 sky 전파·enable/disable·재개 | terrain heightmap으로 sky source를 대체하는 동작 |
| `world/lighting/work.rs` | fresh 재계산 또는 원래 saved row 복원·조건부 전파, 단계별 중단과 두 snapshot 선택 | 일반 Threaded wrapper의 task history·live chunk 상태 변경 |
| `runtime/lighting.rs` | 공용 CPU 계산·중단·재제출, 완료본의 resident 선예약 후 CPU 환급 | 별도 per-world executor, 소비자 예산 없는 완료 cache |
| `world/lighting/owner.rs` | 현재 선택 영역과 canonical topology 검사 후 resident 완료본 보유 | ticking/send-sync/spawn/Play readiness |
| `server/light_snapshot.rs` | 완료된 queued-first 선택을 packet mask/update로 빌림. 독립 visible 경로도 유지 | raw disk light를 검증 없이 전송 가능 상태로 승격 |

위 경로는 모두 `src/` 아래다. 일반 실행 흐름은 다음과 같다.

1. `ChunkLoadingOwner`가 실제 청크를 canonical resident로 보유한다.
2. fresh 경로는 `LightingDomain::begin`/`try_reserve_canonical_lighting`, 저장 복원은 `begin_restore`/`try_reserve_canonical_lighting_restore`를 사용한다. 두 경로 모두 kernel 최대 비용과 source metadata를 먼저 예약한 뒤 청크 목록을 포착한다.
3. `PendingLighting::submit(max_units)`로 제출하고 `LightingTask::wait` 또는 `wait_mut`로 결과를 받는다.
4. 아직 끝나지 않은 결과는 `progress`를 확인하고 `into_pending`으로 같은 예약 아래 재제출한다. 필요하면 `request_growth`로 원래 총예산 안의 큐·scratch 증설을 요청한다. 오류를 완료로 처리하지 않는다.
5. 두 layer가 모두 끝나면 `LightingDomain::accept(owner, completion, &resident_budget)`가 domain identity·현재 source·sky 모드를 먼저 검사하고 완료본의 resident 비용을 승인한다. 성공한 뒤 CPU slot과 예약 bytes를 반환한다.
6. `ready(owner)`의 `ReadyLighting`을 `PacketLightSnapshot::from_ready`에 넘겨 packet을 만든다. 실제 send-sync와 Play 상태는 별도 소비자가 확인해야 한다.

## v3 데이터와 조명 입력

현재 [준비 명령](chunk-storage.md)은 `ExportBlockStateData.java`와 `ExportLightingData.java`로 initialized `WorldLoader`의
공개 state/shape API를 관찰한다. 기본 snapshot은 로컬 `Decompile/bootstrap/26.3-pre-2-block-states-v3`,
독립 기대 manifest SHA-256은 `19c81b4f667315d5981385cbab154e31b4e0ece899d171afb6fad51caa4a4a39`다.
JAR·configuration manifest·선택한 vanilla pack과 파일별 hash를 확인한다. v3는 bundle schema 버전이며 DataVersion을 바꾼 값이 아니다.

`lighting.bin`은 35,723개 state의 16-byte material과 377개 canonical face의 142,129개 ordered pair를 담는다.
헤더를 포함해 589,351 bytes이며 SHA-256은 `bc76b729fd0c7c9c93f00063281623936f7ff335ad00780983c57e0e003d4d30`이다.
검수자는 exporter를 별도 경로에서 다시 실행해 같은 bytes를 확인했다. face를 통합하기 전 427개 runtime 표현에 대해서도
182,329개 ordered pair가 같은 판정을 낸다는 근거를 확인했다. runtime은 shape 객체나 state² 표를 보관하지 않는다.

`LightingSource::from_canonical`은 resident의 `Arc`와 section index만 유지한다. palette를 복사하지 않으며 로딩 owner가
청크를 제거해도 마지막 source가 해제될 때까지 실제 resident 예약이 남는다. 같은 Y의 마지막 terrain을 읽고, 존재하는 청크의
누락 section은 AIR로 취급한다. source 목록에서 빠진 청크는 owner에 데이터가 있어도 unavailable이다.

available 청크의 높이 밖 조회는 lighting AIR, unavailable 청크의 조회는 BEDROCK이다. 현재 실제 metadata에서
AIR·VOID_AIR·CAVE_AIR의 조명 값과 빈 face는 같다. 이는 조명 조회만의 대체 근거이며 일반 block identity를 합치는 정책이 아니다.
chunk 좌표는 ±2,097,061, 입력 block Y는 아래·위 padding을 남기는 `[-2032, 2032)` 범위로 제한한다.

`from_sections`는 **생산자가 먼저 메모리를 승인해 만든** owned section을 받는다. 생성 뒤 전달된 한도는 이전 allocation을
소급해 승인하지 않는다. 입력 Vec의 spare capacity와 section backing을 검사하고 중복 좌표·잘못된 section 수·ID·non-empty/fluid count를 거부한다.
이 source는 canonical owner의 최신성을 주장하지 않는 독립 입력이다.

## 값과 visible snapshot

`DataLayer`는 implicit uniform과 materialized 2,048-byte 배열을 구분한다. implicit zero는 empty이지만,
이미 배열로 만들어진 all-zero layer는 data다. `fill`·`materialize`·`copy`와 packet mask가 이 표현 차이를 보존한다.
독립 DataLayer의 넓은 정수 API와 실제 storage의 밝기 `0..15` 경계도 구별한다.

`LightSectionStorage`의 `Empty/LightOnly/LightAndData`는 밝기 값과 다른 section 지원 상태다. 데이터 section과 주변 26개 section의
지원 수를 반영하고, queued override·수정 중 값·visible snapshot을 분리한다. block의 없는 layer는 0이며 sky의 없는 layer는
위쪽 layer와 source/top 경계에 따라 조회하므로 동일한 기본값으로 합치지 않는다.

여러 위치의 변경은 알림 합집합과 필요한 COW layer를 먼저 확보한 뒤 적용한다. allocation 실패 시 예전 값과 표현을 유지한다.
visible 게시가 실패하면 기존 snapshot과 미게시 변경을 보존한다. 이미 게시된 알림도 소비자가 명시적으로 확인하기 전까지 남는다.
오래된 snapshot은 자신이 참조하는 layer와 index/top/body 예약을 유지한다.

## 전파와 중단·재개

block engine은 checks→decreases→increases와 저장소 조정·visible 게시 단계를 구분한다. 큐에 들어갈 출력과 실제 바뀔 layer를
승인하기 전 입력 작업을 버리지 않는다. `SourceStamp`와 `StorageStamp`가 중간 재개 대상의 identity를 고정한다.
source 열거는 section Y, local Y/Z/X 순서다. source를 없애거나 약한 값으로 바꾸는 경로도 감소와 재증가로 검증한다.

skylight의 source는 256개 열의 차단 edge cache다. source가 없는 곳의 `MIN/MAX` sentinel, 두 면의 합집합,
raw dampening, 빈 section 아래의 수평 연결을 처리한다. terrain heightmap과 같은 값으로 가정하지 않는다.
`populate_budgeted`와 enable cursor는 완료 전에 propagation 또는 partial publication으로 넘어가지 않는다.

독립 검수에서 sky enable의 COW 실패 뒤 위쪽만 15로 공개되는 오류를 발견했다. 현재는 미완료 enable 동안
다른 작업과 게시를 `Busy`로 막고, 여유가 생긴 뒤 같은 cursor에서 이어간다. 큐가 부족하면 필요한 capacity를 명시하고
새 큐·scratch를 이전 버퍼와 함께 승인한 뒤 재개한다. 고정 총예산 자체가 작으면 상위에서 취소·재구성해야 한다.

`LightingWork::new`는 새 storage에서 초기 relighting을 수행한다. section 지원·sky sources·enable/populate·block sources와 두 전파를
차례로 진행하며 중간 엔진 snapshot은 외부 완료본으로 노출하지 않는다. `step(max_units)`의 한 단위는 propagation entry일 수도,
한 청크의 256-column scan일 수도 있다. **작업 단위 상한은 wall-clock 지연 상한이 아니다.**

## 저장 조명의 선택 domain 복원

`LightingSource::saved_light(address)`는 canonical resident의 원래 `StoredSection` row 목록과 저장 status·flag를 빌린다.
중복 Y와 terrain 밖 row도 보존하며 owned terrain에는 저장 정보를 만들어 반환하지 않는다.
`LightingWork::new_restore`는 각 청크의 첫 적용 가능한 layer에서 retain을 켠 뒤 원래 row 순서로 block, sky를 staging한다.
없는 필드는 기존 pending 값을 지우지 않는다. 나중에 존재하는 값만 같은 Y·kind를 대체한다.
sky 없는 dimension에서는 sky 설치를 생략하지만2048-byte 길이 검증은 앞선 decoder에서 이미 수행한다.

선택 영역의 source cache를 먼저 초기화하고 최종 non-air terrain의 support를 만든 뒤 block/sky UPDATE를 수행한다.
initialize POST에서는 각 청크에 대해 저장 status가 `LIGHT/SPAWN/FULL`이고 flag가 true일 때 enable을 켠 다음 retain을 해제한다.
이 조건을 만족하지 않은 청크만 source 전파를 수행하고 다시 두 engine을 UPDATE한다. 저장 배열의 개수나 완전성을 검사해
reuse 조건을 새로 낮추지 않으며 false flag도 앞서 staging한 저장 배열을 버리지 않는다.

최종 `CompletedLighting::block/sky`는 gameplay visible 선택이고 `packet_block/packet_sky`는 `LightDataSnapshot`의
queued 우선, 그다음 visible 선택이다. 지원 없는 queued layer는 `hasLightWork=false`·retain 해제 뒤에도 남을 수 있다.
그 layer를 gameplay visible로 승격하거나 packet-facing 선택에서 버리지 않는다. packet 순회 범위 밖의 저장 row도
data snapshot에는 남지만 실제 packet encoder의 dimension+padding 범위를 넓히지는 않는다.

복원 중 저장2048-byte 배열은 원래 resident 데이터와 engine이 승인한 layer backing이 함께 살아 있다.
마지막 data snapshot의 index/body 비용도 capture 전에 승인하고 실패하면 같은 private phase와 기존 자료를 유지한다.
두 종류의 snapshot이 모두 확보되기 전 완료본을 만들지 않는다. resident 비용은 visible과 data snapshot 양쪽을 포함하며
공유 layer의 보수적 중복 계산을 허용한다. 추가 generic executor나 per-world pool은 없다.

저장 flag/status는 불변 source provenance다. 이 작업이 live chunk의 flag를 false/true로 변경하거나 status/future를 완료하는 것은 아니다.
일반 Threaded PRE/POST task dispatcher와 marker 순서, live mutation·ticket/send-sync 통합은 별도 책임으로 남는다.

## 독립 실행과 공용 CPU의 예산

| 소유자·설정 | 승인하는 메모리 | 분리해서 남겨야 하는 비용 |
| --- | --- | --- |
| `SourceLimits` | source metadata Vec·canonical section index·직접 받은 palette backing | shared registry, 원래 resident lease, stamp Arc, 생산자가 이미 만든 입력 |
| `BlockLightLimits`, `SkyLimits` | checks/FIFO/index와 sky source·계획 scratch | source·layer storage |
| lighting `StorageLimits` | section/column/알림/COW/visible/data snapshot metadata와 layer 예약 | source·엔진 큐·CPU thread stack |
| `LightingLimits::reservation_bytes` | `LightingWork` 본체와 설정된 모든 coordinator/engine/storage allowance의 보수적 합 | source 자체는 별도 합산. shared registry·resident와 allocator/OS/native 비용은 분리 |
| `ResidentLightingBudget` | 완료 후 남는 source metadata·owned section·visible/data snapshot backing과 보수적 제어 비용 | 이미 해제한 kernel 큐·scratch·설정 최대치, 기존 canonical resident·shared registry |
| `PacketLightSnapshot`의 `control_bytes` | block/sky update descriptor 두 Vec | borrowed layer payload, 완전 packet output, delivery·framing 복사본 |

독립 호출자는 `LightingWork::new` 또는 `new_restore` 전에 `reservation_bytes()`에 해당하는 외부 admission을 직접 확보해야 한다.
각 lower-level 큐와 storage 한도는 그 자료구조의 유지 비용을 제한하며, 자동으로 공용 CPU의 aggregate budget을 확보하지 않는다.
독립 `CompletedLighting`에서 snapshot을 복제하면 storage 내부 lease가 살아 있지만 별도 외부 admission 정책도 호출자가 유지해야 한다.

canonical 공용 실행의 `CpuPool::try_reserve_canonical_lighting`은 **kernel 최대 예약 + `SourceLimits.metadata_bytes`**를
source index를 포착하기 전에 확보한다. 기존 resident의 palette는 복사하지 않고 별도 resident lease를 유지한다.
생산자가 이미 만든 source를 받는 `try_reserve_lighting`은 **kernel 최대 예약 + `source.heap_bytes()`**를 CPU lease에 포함한다.
이 경우에도 생산자가 입력을 만들기 전 확보해야 했던 admission 책임은 없어지지 않는다. 두 경로 모두 새 engine/queue/storage는
예약 후 worker에서 만든다.

pending·running·paused·ordinary error와 아직 이전하지 않은 complete 결과는 같은 CPU lease를 유지한다.
완료본은 `LightingCompletion::try_adopt(&ResidentLightingBudget)`에 성공한 뒤 `ResidentLighting`의 별도 예약 아래 보유한다.
runtime snapshot getter는 crate-private이며 완료본·resident·`ReadyLighting`에서 외부로 snapshot/source를 복제해 예약 수명과 분리할 수 없다.

runtime의 한 번 제출은 `MAX_LIGHTING_SLICE_UNITS = 64`로 제한한다. 큰 요청도 64단위까지만 실행하고 아직 남은 결과는 다시 제출한다.
각 단위의 비용이 다르므로 이 제한은 지연 시간 보장이 아니다. `request_growth`는 고정 크기의 요청만 저장하며 실제 큐·계획의 allocation은
다음 worker 제출에서 이전 backing과 새 backing을 함께 승인해 수행한다.

`LightingTask`를 취소하거나 소비자를 버려도 진행 중인 slice가 소유한 버퍼를 먼저 환급하지 않는다. worker가 slice를 끝내고
결과가 버려질 때 payload를 해제한 뒤 lease를 반환한다. 이전 전 완료본이나 이전에 실패한 완료본을 보관하면 CPU slot도 계속 차지한다.
성공적으로 이전한 완료본은 CPU slot을 차지하지 않는다. 여러 domain은 같은 `ResidentLightingBudget` 또는 그 clone을 공유해야
합계 상주량을 제한할 수 있다. 각 domain에 독립 예산을 만들면 aggregate 한도가 되지 않는다.

uniform layer도 materialize될 수 있는 2,048 bytes와 제어 비용을 예약하지만 실제 backing은 아직 없을 수 있다.
`heap_bytes`·`retained_bytes`·reserved layer bytes는 각 API가 설명한 범위의 수치다. 이를 더한 값도 process RSS나 allocator peak의 측정값은 아니다.

## 완료본의 resident 이전

`LightingCompletion::resident_bytes()`는 완료 후 실제로 도달 가능한 source와 visible/data snapshot의 backing을 기준으로
보수적인 승인량을 계산한다. source metadata와 owned palette, snapshot 본체·index/top Vec의 실제 capacity,
layer payload·Arc·budget 제어 비용을 포함한다. uniform layer도 잠재적인 2,048-byte payload를 예약하며,
공유된 제어 객체는 결과나 layer마다 다시 계산할 수 있다. 계산은 allocation이나 layer materialization 없이 수행한다.

이미 해제한 engine 큐·sky source cache·scratch·working storage 배열과 설정상의 kernel 최대치는 resident 비용에서 제외한다.
canonical palette는 기존 `ResidentChunk`의 `Arc`와 원래 resident lease를 그대로 유지하며 복사하거나 중복 이전하지 않는다.
shared registry도 기존 수명 관리에 남는다. `CompletedLighting::retained_bytes()`와 runtime의 `resident_bytes()`는
이 경계를 구분하며 후자는 resident wrapper와 공유 ledger 제어 비용까지 포함한다.

이전 순서는 **현재 owner 검증 → 목적지 bytes/results 승인 → 기존 payload 소유권 이동 → CPU 예약 반환**이다.
목적지 mutex를 놓은 뒤 CPU ledger를 환급하므로 두 mutex를 동시에 잡지 않는다. source·palette·layer payload 복사는 없다.
미완료·용량 부족·산술 overflow는 원래 완료본과 CPU lease를 반환한다. `LightingDomain::accept` 실패에서도 현재 요청을
유지하므로 공간이 실제로 생긴 뒤 같은 완료본으로 재시도할 수 있다. 무제한 자동 재시도나 우선순위 scheduler는 추가하지 않았다.

`e0b4593`의 당시 fixture에서는 CPU 예약 **8,392,584 bytes**가 resident 승인량 **120,156 bytes**로 이전됐다.
현재 추가된 data snapshot 전의 역사적 수치이며 현재 완료본 크기나 RSS 감소율·처리량 측정으로 사용하지 않는다.
현재 저장 복원 배치의 동일 runtime 검사에서는 CPU8,392,608bytes→resident237,484bytes를 관측했다.
visible/data snapshot이 함께 참조하는 layer도 보수적으로 계산한 승인량이다. 근거는 로컬 `Roadmap/reviews/saved-lighting-resident-size.log`다.
성공 뒤 `max_jobs = 1`인 같은 CPU pool에서 packet 작업을 실행할 수 있다. 다만 다른 running/paused 작업이나
아직 이전하지 못한 완료본은 여전히 CPU 예산을 사용하므로 coordinator는 네트워크용 slot과 bytes 여유를 함께 남겨야 한다.

## 현재 owner 확인과 packet 연결

`LightingDomain::begin`은 이전 결과를 즉시 취소하고 새 domain을 만든다. source capture나 CPU admission이 실패해도 이전 결과를
계속 최신으로 제공하지 않는다. 완료 수락에는 현재 요청의 source identity와 canonical owner revision, sky 모드가 모두 맞아야 한다.
canonical owner의 새 게시·제거·reload는 **처음 source에서 빠져 있던 이웃의 변경까지** 이전 source를 무효화한다.

resident revision만으로 availability/ticket 변경을 전부 알 수는 없다. caller가 선택한 available 목록이나 eligibility가 달라지면
`begin` 또는 `cancel`로 domain도 갱신해야 한다. 다른 world/domain·이전 요청·중간 결과·이미 수락한 중복은
resident 승인 전에 거부하며 실패 결과의 lease를 분리하지 않는다.

청크 제거 뒤 `ready(owner)`가 `None`이 되어도 domain이 보관한 stale 완료본의 메모리는 자동으로 환급되지 않는다.
명시적인 `cancel`, 새 `begin`, domain 해제가 resident 결과와 그 source의 원래 청크 lease를 해제한다.
서로 겹치는 여러 domain의 우선순위와 취소는 호출자가 조정해야 한다.

`ready(owner)`는 resident 완료본을 가진 domain과 canonical owner를 함께 빌린다. `ReadyLighting`은 coherent block/sky 완료본이지만
chunk status·send-sync·spawn·Play 준비 완료 표식은 아니다. `PacketLightSnapshot::from_ready`는 선택 domain 밖 청크를 allocation 전에 거부한다.
독립 `PacketLightSnapshot::new`는 caller가 같은 revision의 snapshot을 선택했다는 조건을 별도로 충족해야 한다.

packet 범위는 dimension의 아래·위 section 하나씩을 더한 최대 258개이며 mask 네 개는 각각 고정 33 bytes다.
allocated layer payload는 빌리고 uniform 값은 scalar로 넘겨 마지막 packet output 안에서만 확장한다. 임시 2,048-byte 배열이나 layer clone을 만들지 않는다.
필터의 `None`은 전부 포함, `Some(empty)`는 미포함이다. `from_ready`와 독립 `from_data`는 고정된 queued-first 선택을,
기존 독립 `new`는 visible snapshot을 사용한다. 독립 호출자는 각 선택에 맞는 source fence와 예약 수명을 유지해야 한다.
storage를 packet으로 연결한 성공과 실제 전송의 인과관계·BE update tag·world readiness 검증은 별도 완료 조건이다.

## 현재 검증 근거

기존 커널·초기 통합 수치는 `5e14ec7`까지의 근거다. 해당 source의 [CI33973360687](https://github.com/Love0118/Arrow-MC/actions/runs/33973360687)는
네 native 플랫폼의 debug/release 각각612통과·37선택제외를 확인했으며 새 resident 이전을 검증한 CI가 아니다.
resident 이전의 `e0b45930dca1da4ac9d66b6b37cf7cfe5369e692`는 [CI33974887856](https://github.com/Love0118/Arrow-MC/actions/runs/33974887856)에서
네 native debug/release 각각630통과·37선택제외를 확인했다. 이 성공도 현재 저장 복원 배치의 CI가 아니다.
현재 저장 복원 배치는 Windows 전체 debug/release 각각655통과·38선택제외, all-target strict Clippy·format을 통과했다.
실제 새 Java oracle도 debug/release와 독립 재실행을 통과했다. 새 source commit/native CI는 전달 기록에서 별도로 확인한다.
아래 수치는 실행 범위와 단위가 서로 다르므로 합산해 전체 기능 완료율을 만들지 않는다.

| 범위 | 실제 근거 | 한계 |
| --- | --- | --- |
| v3 material/face | 35,723 state·377 face·142,129 ordered pair, 427 runtime 표현의182,329 pair, exporter 독립 재실행 bytes 일치 | 전체 collision API나 모든 변경 가능한 shape 소비자 검증 아님 |
| source/소유권 | 일반13개와 실제 AIR 변형 metadata1개 통과. resident 제거 후 lease·미포함 이웃의 topology 변경·reload/foreign owner·admission 검증 | 실제 ticket eligibility 변화의 자동 연결 아님 |
| block kernel | Java34 snapshot·1,548 layer·6,340,608 nibble 대조를 정확성 리뷰어가 재실행 | 내부 hash 작업 횟수·순서 전체 동등성 아님 |
| sky source | Java2,008 snapshot·514,048 column·update 반환2,004개 | 실제 world status/Threaded callback 시험 아님 |
| sky 전파 | 9청크를 사용하는2시나리오·22공개단계·2,497 layer·10,227,712 nibble | 서버 전체 TPS·큰 world 충분성의 증거 아님 |
| packet bridge | 일반7개, 실제 block 계산→chunk packet→bounded queue→기존 TCP 경로 및 leased payload·표현·mask 경계 | Play 입장이나 현재 CPU/domain 통합 최종 검수와 별개 |
| v3 기존 소비자 | `world_storage_chunk_oracle`와 `world_heightmap_java_oracle`의 v3 기본값 변경 후 선택 실행2개 통과 | 새 lighting 전체 oracle를 대신하지 않음 |
| 초기 work/owner 통합 | work8·owner7, source metadata 선예약·동일 domain/revision·실제 canonical→공용 CPU→packet→TCP 검증, 두 독립 리뷰 통과 | 전체 world status·Play coordinator 미구현 |
| 공용 CPU lifecycle | 일반5·결정적 gate4개, constructed/running·queued·ready 취소와 wait 재개, 예산·작업 보존·worker 증설 검증, 두 독립 리뷰 통과 | 64단위는 wall-clock 지연 상한이 아님 |
| 초기 block/sky 조합 | 실제 초기화한 Java `LevelLightEngine`의 2영역·216개 전체 layer·884,736 nibble을 무제한/7단위 실행 각각과 대조. 7단위 재개4,656회. Debug·release와 독립 재실행 통과 | fresh initial relighting이며 저장 light 복원·Threaded callback 시험 아님 |
| 후속 resident 비용 helper | `world_lighting_retention`5개와 내부 arithmetic3개 debug/release 통과. uniform/allocated-zero·COW·빈 snapshot·sky top·spare capacity·같은 Arc 중복·overflow·오래된 source 수명 검증. 두 독립 검수 통과 | helper fixture의 `CompletedLighting`120,084bytes는 runtime resident wrapper 등의72bytes를 제외한 값. 별도 설정 최대37,750,312bytes와 source2,456bytes는 위 CPU fixture와 합치지 않음 |
| 이전 resident runtime | `runtime_resident_lighting`6개와 resident ledger overflow 내부1개, e0b4593에서 검증. 완료본 이전 후 CPU slot/bytes 반환, 목적지 실패·재시도·block-only/sky 결과의 상주 합계 | 당시8,392,584→120,156bytes는 새 data snapshot을 포함하지 않음. RSS·속도 측정 아님 |
| 후속 resident owner·TCP | `world_lighting_owner`10개 로컬 통과. 실제 Anvil→공용 CPU→resident→packet→queue→TCP를 `max_jobs = 1`로 실행. accept 직후와 packet write 뒤 CPU slot/bytes0, reader/write 동안 resident1개·정확한 charge 유지, cancel 뒤0 | owner/status·Play 완료 아님. 한 byte 부족 실패의 동일 완료본 재시도·공유 예산 압력·unload 후 stale domain 수명도 검증 |
| 저장 복원 실제 Java | 23 transaction을 무제한/7단위로 대조. visible13,148,160nibble, queued-first1,724layer/7,061,504nibble,1,865재개. 단일22개와 서로 다른 reuse 조건의 이웃2청크1개. malformed array16개 |138개 Java phase 관측은 Rust private phase와 직접 대조한 수치가 아님. 일반 Threaded dispatcher·임의 다중 청크 상태 조합·Play 동등성 아님 |

원문을 읽은 뒤 독립 Rust 자료구조·API와 공개 Java API observer를 작성했다. Java/Pumpkin 본문 복사 또는 clean-room 작업이라고 주장하지 않는다.
공식 JAR·bulk data·관측 출력은 형제 로컬 `Decompile`에 남기며 배포 코드에 넣지 않는다.

근거 보고서:

- [독립 정확성 검수](../../Roadmap/reviews/lighting-correctness.md)
- [독립 자원·추상화 검수](../../Roadmap/reviews/lighting-optimization.md)
- [Skylight 실제 결과](../../Roadmap/research/lighting-sky-results.md)
- [Threaded 통합의 후속 계약](../../Roadmap/research/lighting-next-prerequisites.md)
- [저장 light 복원·queued 우선순위·PRE/POST 계약](../../Roadmap/research/lighting-saved-restore-contract.md)
- [resident 조명 예산 이전 계약](../../Roadmap/research/lighting-resident-budget-contract.md)
- [저장 복원 실제 Java 결과](../../Roadmap/research/lighting-restore-oracle-results.md)

공식 관측은 실제 `WorldLoader`·`ProtoChunk`·`LevelLightEngine` 등을 호출한다. 게임 서버나 사용자 계정 접속을 실행한 검증으로 확대하지 않는다.
선택 oracle 재현에는 `ARROW_MC_JAVA_REFERENCE_ROOT`와 현재 v3 snapshot의 독립 hash를 사용한다. 변경한 영역의 target만 실행한다.

```powershell
$env:ARROW_MC_JAVA_REFERENCE_ROOT = 'E:\projects\Arrow MC\Decompile'
cargo test --locked --test world_lighting_source
cargo test --locked --test world_block_light_java_oracle -- --ignored --nocapture
cargo test --locked --test lighting_sources_java_oracle --test lighting_sky_java_oracle -- --ignored --nocapture
cargo test --locked --test world_lighting_restore_java_oracle -- --ignored --nocapture
```

남은 범위는 일반 `ThreadedLevelLightEngine` PRE/POST dispatcher·pending marker, live chunk flag/status 갱신,
실제 world mutation·ticket/readiness coordinator, 전체 world send-sync·Play 연결이다. native 결과는 source commit별로 기록한다.
현재 측정으로 일반적인 메모리 최솟값이나 서버 TPS·p99 개선을 주장하지 않는다.
