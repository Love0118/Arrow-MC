# 예약 tick 복원과 영역 연산

`world::ticks::ScheduledTickOwner`에 pending `SavedTick`, unpack/pack, 영역 clear/copy와 완전 동률의 실행 이력을 구현했다.
실제 block/fluid 행동과 병렬 game tick 실행은 아직 연결하지 않았다. 이 문서의 persistence는 메모리상 저장 표현의 복원·추출을 뜻한다.
chunk NBT의 block/fluid type codec, durable Anvil writer와 unload/save 통합은 남아 있다.

## 관찰 가능한 순서

- 저장 목록은 chunk별로 걸러 pending 상태로 보관한다. pending도 count와 중복 판정에 포함되지만 unpack 전 실행하지 않는다.
- unpack의 sub-order는 각 chunk의 `-N..-1`이다. signed delay·time 계산은 Java wrapping을 보존한다.
- 중복 saved 항목은 둘 다 enqueue될 수 있다. 첫 중복을 poll하면 나머지가 있어도 dedup identity가 제거되는 원본 동작을 구분한다.
- 완전 동률은 좌표로 새로 정렬하지 않는다. Java heap의 삽입·제거와 scheduling map의 충돌·resize·순회 삭제 이력이 선택 순서에 영향을 준다.
  해당 계약만 private heap/index로 구현했고 범용 collection API로 확대하지 않았다.
- pack은 pending의 원래 delay·순서를 먼저 기록하고, 이어 live heap 항목만 sub-order와 안정적인 source index로 정렬한다.
  pending은 이 정렬에 포함하지 않는다. clear는 queued·selected·이미 반환한 항목에 적용되며 pending에는 적용하지 않는다.
- copy는 source snapshot을 먼저 모으고 중복은 first-wins로 처리한다. 자기 영역 복사도 원본 snapshot을 사용한다.
  destination 여유를 확인한 뒤 적용하므로 예산 실패로 일부만 복사하지 않는다. live sub-tick counter는 소비하지 않는다.

`will_tick_this_phase`는 원본의 lazy set 때문에 변경 가능한 조회 API다. 이미 만들어진 set의 clear 후 stale membership을 임의로 고치지 않는다.
saved 중복이 많은 경우에는 set이 빌 때 남은 전체 tick suffix를 반복 스캔하면 이차 비용이 발생한다.
`RemainingCounts`가 현재 남은 distinct identity와 개수를 별도로 관리하여 빈 set을 distinct 목록으로 재구축한다.
조회 답은 여전히 원래 lazy set에서 구한다. count로 직접 답하는 방식은 중복 poll·clear 의미를 바꾸므로 사용하지 않는다.

## 예산과 비용

`retained_heap_bytes`는 각 backing Vec의 실제 capacity × element size 합이며 빈 채 보관 중인 buffer도 포함한다.
owner의 selected 목록·dedup·두 scheduling index와 chunk별 두 domain queue·heap 순회 scratch·pending 목록을 계상한다.
새 count index의 64-bit 요청 비용은 선택 상한 `S`에 대해 `24*S + 8*(2*next_power_of_two(S))` bytes다.
최대 선택 상한 `S=65,536`에서는 **2,621,440 bytes**다. 실제 할당 capacity가 요청보다 크면 실제 값을 계상한다.

드문 pack/copy용 scratch를 처음부터 약 14 MiB 확보하던 중간 설계는 제거했다. 필요한 때에 필요한 개수만 승인하며
buffer 교체 중에는 old+new 비용을 함께 확보한다. `release_operation_scratch`로 queue를 건드리지 않고 반환할 수 있다.
pack 출력은 호출자가 사전에 확보해야 하며 owner가 출력 Vec를 몰래 늘리지 않는다.
이 수치는 allocator metadata·stack을 포함한 RSS가 아니다.

distinct `k`개로 다시 만든 set이 비려면 최소 `k`개의 identity 제거가 필요하다. 이 때문에 반복 중복 suffix 전체 스캔을 피한다.
2개/4개 identity를 번갈아 넣은 2,048개 선택 항목에서 전체 rebuild entry 수가 선택 개수 이하인지 검증했다.
모든 hash 입력의 상수 시간이나 실제 gameplay TPS 향상을 주장하지 않는다.

## 검증과 재현

실제 고정 JAR에서 기존 live 332관찰과 persistence 398관찰을 비교했다. pending 중복·재예약·negative order,
heap 동률·wrapped map 순회·lazy clear·copy/self-copy·반복 identity 조회를 포함한다.
일반 회귀 테스트에서 원자적 admission 실패, scratch 해제, phase reset 및 iterator/index 불변식도 검사했다.
정확성·최적화 두 독립 리뷰가 최종 count index와 자원 수명을 확인했다.

```powershell
$env:ARROW_MC_JAVA_REFERENCE_ROOT = 'E:\projects\Arrow MC\Decompile'
cargo test --locked --release --test world_ticks_java_oracle -- --ignored --nocapture
cargo test --locked --release --test world_ticks_persistence_java_oracle -- --ignored --nocapture
```

공식 소스는 동작 조사에 참조했고 Rust는 독립 설계했다. Java 클래스의 method body를 기계 번역하지 않았다.
다음 작업은 저장 NBT·실제 chunk 수명과 연결한 뒤 tick callback의 같은 tick 의존성·RNG를 유지하는 병렬 실행 경로다.
