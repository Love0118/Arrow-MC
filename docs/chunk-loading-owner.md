# 로드된 청크의 소유 경계

`world::loading::ChunkLoadingOwner`는 실제 disk read 요청과 canonical resident를 소유한다.
`ChunkStore`의 읽기·decode 결과를 현재 요청과 연결하고, 같은 공용 CPU pool로 높이 전체의 section bytes를 준비한다.
light engine·POI·entity·block/fluid tick 활성화는 아직 수행하지 않는다. 저장 status `FULL`은 송신·spawn 준비 완료가 아니다.

## 요청과 게시

`request`는 새 읽기 요청, 이미 진행 중인 요청, 현재 resident를 구분한다. 반복 요청은 allocation이나 generation을 늘리지 않는다.
요청 generation과 world epoch는 overflow를 사전 검사한다. 요청 취소·world reload 후의 늦은 완료가 새 slot을 덮어쓰지 않는다.
누락·unavailable·실패의 현재 요청은 `finish_without_chunk`로 종료하고 재요청할 수 있다. 파일 누락을 기본 청크로 만들지 않는다.

`LoadingReadRequest`와 `LoadingSectionTask`는 owner마다 한 번 생성한 작은 `Arc` identity를 공유한다.
private completion은 이 identity를 보유하므로 숫자 epoch·generation·좌표가 같은 다른 owner의 결과도 거부한다.
disk completion은 실제 `ChunkStore`의 height·registry/configuration context와 현재 pending key도 검증한다.
낮은 수준의 임의 `ChunkDecodeOutput`를 branded completion으로 만드는 공개 생성자는 없다.
호출자는 읽을 `ChunkStore`를 선택하며, 이 구조가 저장 디렉터리의 권한이나 외부 파일의 진위를 인증하는 것은 아니다.

실패한 게시에는 원래 completion과 CPU lease가 남아 재시도할 수 있다. resident와 metadata 예산을 확보한 후에만 게시하고 CPU 예약을 반환한다.
상태 변경은 synchronous owner에서 처리한다. I/O/CPU 작업의 실제 종료 전까지 buffer 예약은 기존 작업이 유지한다.

## Canonical 저장 뷰

- 요청 좌표에 게시하고 저장 좌표 불일치를 `Relocation`으로 보고한다. raw NBT의 모든 좌표를 일괄 수정하지 않는다.
- 유효 높이의 중복 section은 마지막 항목을 선택한다. block/sky light는 각각 마지막으로 존재하는 layer를 선택한다.
- 실제 resident 안의 누락 section은 공유 singleton air/plains container로 채운다. 높이 밖 light도 보존하며 skylight 없는 dimension은 sky layer를 노출하지 않는다.
- 원래 NBT·palettes·light는 resident 하나에 보관한다. canonical row는 원래 section의 index만 저장한다.
  별도 `SectionPreparationOwner`에 같은 palette를 다시 상주시키지 않는다.

요청 좌표는 고정 기준의 generation 여유를 포함한 `±2,097,061` 범위다.
Java의 `abs(i32::MIN)` overflow로 그 값이 통과하는 우발적 경로는 Arrow에서 명시적으로 거부한다.
이 정책은 일반적인 유효 world 좌표나 view-distance 범위를 줄이는 최적화가 아니다.

## 준비와 메모리

section 준비는 이미 승인된 CPU 입력 buffer만 채운다. 결과를 `accept_prepared`로 검증한 후 반환하는 `PreparedSection`은
owner를 빌리므로 사용하는 동안 remove/reload할 수 없으며 CPU lease도 유지한다.
`LoadingSectionTask::wait`는 blocking 호출이다. async 실행에서는 `try_take`를 사용해 owner의 실행 기회를 보장해야 한다.
앞으로 실제 transport에 연결할 때 이 borrow를 느린 socket 대기 동안 잡아 두지 않고, 승인된 전송 소유권으로 넘겨야 한다.

`LoadingLimits`는 pending/resident slot 수와 metadata backing bytes를 제한한다. slot 배열과 최대 256개의 canonical row를 먼저 승인한다.
canonical 구성의 고정 scratch는 6 KiB, 최대 row Vec는 8 KiB다. 실제 capacity를 계상하며 default singleton은 heap palette를 만들지 않는다.
resident 데이터·공유 registry·CPU buffers는 각각 기존 예산에 속한다. 소유권 제어용 `Arc`와 allocator·stack·OS 비용은 RSS와 별도로 본다.

## 현재 검증

실제 서로 다른 region 파일과 같은 숫자 key를 사용하는 두 owner, unload/reload 후 지연 완료,
context 불일치·예산 실패·counter 고갈·중복 section/lights·누락 기본 section·foreign preparation을 검사했다.
낮은 I/O 계층의 취소 시험과 별개로 owner의 수요/게시 전이가 기존 resident와 lease를 유지하는지 확인한다.
정확성·자원 리뷰 두 역할이 이 소유권 경계를 검수했다. 전체 native 결과는 [구현 상태](foundation-status.md)에 따로 기록한다.

`inspect_chunk` 예제는 이제 실제 요청→disk→canonical owner→공유 CPU 준비를 실행한다. 실행 인수는 [저장 로딩 문서](chunk-storage.md)와 같다.
공식 registry와 직접 만든 1-section raw 파일을 Overworld 높이로 읽은 실행에서 기본 section까지 포함한 **24 sections /192 bytes**를 준비했다.
CPU peak 예약29,360,310 bytes, resident2,574 bytes, owner metadata984 bytes였다. 1,475µs는 로컬 단일 표본이며 서버 성능 비교가 아니다.
이 경로의 원본 기록은 로컬 `Roadmap/reviews/inspect-loading-owner-example.json`이다.
