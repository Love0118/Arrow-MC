# 전체 구현 목표와 선행 기반 게이트

목표는 **바닐라 소스를 기반으로 동작을 조사하되 독립적으로 설계한 Rust 구현체**로 고정된 Vanilla 26.3 서버의 모든 기능을 구현·검증하는 것이다.
소스 기반 분석·구현은 사용자가 명시적으로 허용했다. 정식 26.3 릴리스가 제공되면 기준을 갱신하고
변경된 항목도 같은 완료 기준으로 추적한다. 현재는 `26.3-pre-2`다. 일부 테스트가 통과했다는 이유로 목표를 작은 기능 집합으로 축소하지 않는다.
사용자의 추가 라이선스 조건도 완료 기준이다. 원문 본문을 기계적으로 번역하지 않고 독립 구현·출처·배포 조건을 검토한다.
자세한 정책은 [provenance-policy.md](provenance-policy.md)를 따른다. 엄격한 clean-room이나 법적 무위험을 주장하지 않는다.

## 누락을 추적하는 세 종류의 목록

1. **실제 입력 목록**: JAR 7,761 classes와 디컴파일 Java 5,035개, bundled resources 9,893개를 대조한다.
   공식 데이터 생성기의 registry 95개/등록 항목 7,053개와 protocol packet 259개도 보존한다.
   생성 명령은 `python tools/generate_vanilla_inventory.py`; 공식 보고서 재생성은 `--refresh-reports`, 최신성 검사는 `--check`다.
2. **의미·의존성 catalog**: 형제 `Roadmap/catalog/data-items.md`, `world-ticks.md`, `server-gameplay.md`에서 기능을
   foundation·소비 기능·tick 민감 계약·오류/저장/네트워크 동작으로 나누고 선행 조건과 검증을 기록한다.
3. **구현 증거**: 각 항목에 Rust 경로/심볼, source/version, 검증 명령/fixture, 독립 리뷰와 알려진 차이를 연결한다.
   파일·registry의 존재나 Rust 타입 선언만으로 기능 완료를 판정하지 않는다. 미연결·미확인 항목은 남은 작업이다.

동적 레지스트리·datapack·tag·recipe·loot·advancement·structure·trade 등은 정적 registry dump만으로 다 담기지 않는다.
resource inventory와 실제 로더/실행 경로를 함께 확인한다. library·bootstrap·데이터 생성기·테스트 보조 파일도 목록에서 조용히 삭제하지 않는다.

## 아이템보다 먼저 완성할 데이터 기반

| 게이트 | 완성해야 하는 범위 | 소비 기능 진입 조건 |
| --- | --- | --- |
| BASE-NBT | 모든 tag, named/network root, Java modified UTF-8와 UTF-16, mixed list wrapper, quota/depth/오류, numeric/copy 연산, SNBT·path·필요 변환·압축 입출력 | 바이너리만 통과하면 전체 NBT 완료가 아니다. 저장·명령·component codec에 필요한 범위를 전부 검증한다. |
| BASE-REGISTRY | 이름·숫자 ID, holder/tag 참조, 동적 lookup 문맥, datapack schema·로딩/검증·재로드·실패 진단 | 실제 26.3 등록 ID와 참조 해결을 검증한다. Pumpkin numeric ID를 복사하지 않는다. |
| BASE-CODEC | 저장용·네트워크용 codec의 구분, 제한과 오류 전파, optional/default/recursive 표현, registry-aware fallback | 범용 Java codec class hierarchy 복제보다 필요한 구체적 타입·함수로 의미를 구현한다. |
| BASE-COMPONENT | 등록된 122개 component의 완전한 payload와 codec, 기본 prototype·sparse patch, inherit/set/remove, 비교·copy 격리·transient/persistent·network 차이 | 빈 unit struct, 입력 버림, End로 대체하는 placeholder는 미구현이다. 각 component별 실제 round-trip·오류·중첩·default/patch 사례가 필요하다. |
| BASE-STACK-DATA | 비활성 ItemStack 값의 ID·count·empty·중첩 stack·component patch, 최대값 검증·copy·equality·persistence/wire | 실제 item 사용·제작·container transaction과 구분한다. 모든 데이터 기반 완료 후 item gameplay 소비 기능을 구현한다. |

components에는 bundle/container/charged projectile/use remainder처럼 stack 값을 다시 포함하는 타입이 있다.
따라서 BASE-COMPONENT와 BASE-STACK-DATA는 서로 연결된 **데이터 모델 묶음**으로 함께 구현한다.
이는 아이템 행동을 먼저 만드는 예외가 아니다. 사용·소모·공격·제작·인벤토리 거래는 데이터 기반 게이트 뒤에 둔다.

Pumpkin에는 NBT와 많은 component 구현이 이미 있으므로 전체가 없다고 단정하지 않는다.
현재 비교에서 일부 payload가 unit struct로 읽기를 버리거나 End를 쓰는 경우, 등록 이름·ID 차이, 비교가 항상 false인 경로가 확인되었다.
각각의 실제 완성도를 검사하며 참고할 부분만 가져온다. 자세한 증거는 data-items catalog와 foundation review 보고서에 있다.

## 다른 기능에도 적용할 선행 조건

| 소비 기능 | 선행 기반과 지켜야 할 계약 |
| --- | --- |
| redstone·repeater·comparator·observer·piston·sculk | 예약 tick의 시간/priority/sub-tick·중복·추가 예약, neighbor update 순서, block state·shape·signal·block entity, chunk boundary·같은 tick 읽기/쓰기 |
| fluid·random tick·성장 | level RNG/위치 LCG, 물/용암 tick 지연·neighbor와 schedule, chunk status·활성 범위 |
| worldgen·청크 병렬 로드 | registry/codec·좌표·RNG/noise·상태 단계/이웃 DAG·ticket·palette/light·저장 revision·완료 결과 공개 경계 |
| entity·AI·combat | entity 값·synced data·attributes/effects/damage, 물리/충돌·소유 RNG·goal/brain memory 순서·passenger, item 데이터 기반 |
| inventory·recipe·loot·trade | 전체 item/component 데이터, slot/menu synchronization·transaction, 조건/context codec·registry/tag·RNG, 저장·packet 순서 |
| network login/config/play | VarInt와 framing 구분, 문자열/NBT/typed codec, registry snapshot, 압축/암호화/상태 전이 barrier·순서·역압 |
| save/reload/migration | 완전한 저장 데이터·NBT/component codecs, version·dirty/saved revision·read-your-write·실패 분류·durable flush |

## 구현과 리뷰 운영

- 작업 중 독립 리뷰어 **1~3명**을 유지한다. 현재는 정확성 리뷰어와 최적화·불필요한 추상화 리뷰어 두 역할이다.
- 구현 에이전트에도 실제 파일 소유 범위를 배정한다. 리뷰어만 배치하고 구현을 중단하지 않는다.
- 리뷰어는 구현과 별도로 Vanilla 근거·JVM oracle·회귀 사례를 확인하고 결과를 기록한다. 리뷰 통과를 구현자의 자기보고로 대신하지 않는다.
- 참조 소스는 읽기 전용으로 사용하며 CodeGraph 초기화/동기화는 공유 checkout에서 root가 조정한다.
- 한 응집된 Rust package의 구체적인 enum/함수로 시작한다. per-tag trait object, 범용 DFU/ECS/DI, 불필요한 crate와 custom macro는 선행 도입하지 않는다.
- async는 대기·입출력 조율, CPU 함수는 제한된 worker, 상태 변경은 필요한 의존 순서와 소유권을 따른다.
- 병렬 청크·tick 성능을 위해 예산 내 RAM 증가는 허용하며 컴파일 시간·binary 크기·실행 비용을 함께 측정한다.

## 완료 판정

각 기능은 실제 구현, 필요한 선행 게이트, 정상/오류/경계/저장/packet/tick 대조, 독립 리뷰, 해당 플랫폼 검증을 근거로 완료한다.
동일 구현을 다시 읽어 만든 round-trip만으로 Vanilla와의 일치를 증명하지 않는다. 실제 공식 JAR oracle 또는 독립적인 명세 fixture를 사용한다.
대표 기능의 통과를 모든 component/item/block/entity 완료로 확대하지 않는다.

전체 목표는 네 플랫폼의 온전한 서버 동작과 모든 catalog 항목의 구현·검증 근거가 확보될 때만 완료한다.
Rust 서버 실행기, gameplay 또는 큰 기반의 일부가 남아 있으면 목표를 계속 활성 상태로 유지한다.
