# 청크 후보 선택과 전송 소유권

`server::chunk_sender`는 Vanilla `PlayerChunkSender`의 pending 후보, f32 quota·ACK와 순서 있는 delivery queue를 구현한다.
실제 Play socket이나 chunk packet encoder에는 아직 연결하지 않았다. `Start`·`Finish`·`Forget`은 typed 전송 의도이며,
`Data`는 호출자가 공급한 완전한 packet bytes다. 빈 청크 본문을 만들어 기능 완료를 대신하지 않는다.

## 선택과 ACK

시작 요청률은9 chunks/tick, 첫 batch의 미응답 상한은1이다. ACK 이후 상한은10이며,
요청률은0.01..64로 제한하고 NaN은0.01로 처리한다. outstanding batch가 없어도 ACK를 처리하고 quota를1로 재설정한다.
quota는 f32 연산과 cap을 보존한다. 미응답 상한에 도달했을 때 새 quota를 누적하지 않는다.

remote 연결은 quota만큼 가까운 후보를 먼저 고른 뒤 준비 여부를 확인한다. 가까운 미준비 후보를 더 먼 준비 완료 청크로 보충하지 않는다.
memory 연결의 별도 분기는 quota 개수보다 많은 준비 청크를 보내 quota가 음수가 될 수 있다.
같은 거리의 선택은 pending hash table의 삽입/삭제/resize 이력과 Guava의 선택 순서에 영향을 받는다.
원래 stream의 순회 방향과 unstable top-K를 직접 관찰했으며 새 좌표 tie-break를 넣지 않는다. 거리의 Java i32 wrapping도 대조했다.

`begin_tick`은 증가하는 실제 tick ID를 한 번 받는다. 반환된 `TickPlan`에서 admission을 재시도해도 quota가 다시 누적되지 않는다.
plan을 버리면 이미 발생한 tick 누적은 남지만 delivery나 pending 삭제는 발생하지 않는다.
이 API는 동기적으로 사용하며 socket 또는 worker를 기다리는 동안 sender의 mutable borrow를 유지하지 않는다.

## 준비 상태와 실패

`SendReadyChunk`를 공급하는 호출자는 실제 ticking chunk와 send-sync 완료, 현재 snapshot에 해당하는 packet body를 확인해야 한다.
section worker 완료 또는 disk `FULL`만으로 이 조건을 만족하지 않는다. 향후 world/tracking과 연결할 때 이 책임을 실제 상태로 충족해야 한다.

queue는 전체 `Start → Data… → Finish`의 span·payload 비용을 확보하고 복사한 뒤 pending 제거·quota·미응답 수를 변경한다.
Full·byte cap·allocation 실패는 명시적으로 반환하고 부분 batch나 조용한 packet 유실을 만들지 않는다.
한 번 승인한 batch는 마지막 Finish까지 buffer를 소유한다. `front_packet`을 빌리고 실제 쓰기가 완료된 뒤 `packet_written`을 호출한다.
write 실패 시 `fail`로 queue를 닫고 보관 buffer를 회수하며, 호출자는 실제 연결도 종료해야 한다.
일부를 쓴 batch를 살아 있는 socket에서 다시 보내지 않는다. 실제 transport 취소 경로는 해당 소비자를 연결한 후 별도 검증한다.

pending을 제거하는 drop에는 Forget이 필요 없고, pending이 없는 살아 있는 player의 drop은 Forget을 enqueue한다.
Forget도 이미 승인한 batch를 추월하지 않는다. 죽은 player 분기와 queue admission 실패를 구분한다.

## 비용과 검증

pending table·rehash spare·선택/sort scratch는 시작 시 `SenderLimits.control_bytes`로 승인한다.
remote top-K는 최대64개와 고정128-entry scratch를 사용한다. memory 분기의 전체 후보 배열도 max_pending 기준으로 승인한다.
delivery의 group metadata·span·payload는 `DeliveryLimits.max_bytes`로 계상하며 실제 Vec capacity를 검사한다.
원래 snapshot bytes와 전송 복사본이 함께 있는 동안에는 각 소유자의 예산에 모두 포함된다. 이 값은 RSS 상한이 아니다.
범용 실행기·새 CPU pool·의존성·crate는 추가하지 않았다.

일반14개 테스트와 실제 고정 JAR 대조173개 선택 사례·475개 ACK/tick 관측을 통과했다.
선택 사례에는65×65 전체 반경32 footprint와 최대64 후보, 반복 trim·동률·이력 변화가 포함된다.
리뷰에서 K=1일 때 더 가까운 후보를 놓치는 오류를 발견해 수정하고 회귀 검증했다.
정확성 및 최적화·추상화 두 독립 리뷰 역할이 최종 상태와 자원 수명을 확인했다.

```powershell
$env:ARROW_MC_JAVA_REFERENCE_ROOT = 'E:\projects\Arrow MC\Decompile'
cargo test --locked --test server_chunk_sender
cargo test --locked --test server_chunk_sender_java_oracle -- --ignored --nocapture
```

공식 oracle는 공개 메서드와 번들 dependency API에 직접 만든 입력을 주고, 도달 가능한 rate 상태 일부를 명시적으로 구성한다.
실제 server world/TCP의 end-to-end 동작이나 처리량 개선을 이 검증으로 주장하지 않는다. view/ticket engine과 chunk packet codec·transport 연결은 후속 단계다.
