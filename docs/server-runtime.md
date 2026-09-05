# 서버 실행·청크 section·병렬 준비

이 문서는 status/ping 서버와 청크 section 기반의 상세 경계를 기록한다. 추가한 인증·configuration과 예약 tick은
[로그인 구현 문서](login-configuration.md)에 있다. Play·월드 생성·조명·청크 ticket/load 전체·게임 tick 실행은 후속 작업이다.
아이템의 NBT/component 선행 조건을 서버 전체의 대기 조건으로 확대하지 않고 이 경로들을 병렬 개발한다.

## 실제 TCP 서버

```powershell
cargo run --release -- --bind 127.0.0.1 --port 25565 --description "Arrow MC"
```

`--help`는 bind/port/description/max-players/max-connections/timeout-seconds/connection-bytes/io-workers를 안내한다.
표시 버전은 공식 `26.3 Pre-Release 2`, protocol은1073742158이다. 온라인 인원은 실제 구현 범위에 맞게0이다.
status는 클라이언트 protocol 숫자가 달라도 응답하고, ping-first와 중복 요청 처리도 고정 서버의 순서를 따른다.
snapshot 미설정의 동일 버전 login은 로그인 불가 메시지로 종료한다. 검증된 서비스를 설정하면 실제 로그인으로 이어지고,
다른 버전과 transfer 요청은 해당 handshake 경로의 진단을 사용한다.

연결마다 하나의 async task가 socket 읽기·쓰기를 소유한다. 프레임 길이는3-byte VarInt, 내부 정수는 별도5-byte VarInt 규칙이다.
handshake 고정 입력 버퍼787bytes와 다음 상태의 작은 프레임 한계로 입력을 제한한다.
서버 상태 JSON은 시작할 때 한 번 만들고 공유하며 매 연결에 재생성하지 않는다.

기본 최대 연결256개, 전체 교환 시간30초, 연결당 양방향 application traffic256KiB다.
deadline은 byte마다 초기화하지 않는다. 선언된 프레임과 응답 길이를 처리 전에 계상한다.
통신량 한계는 RSS 한계가 아니다. 실제 상주 비용은 task 수·고정 future/socket 상태·공유 응답 및 OS 네트워크 버퍼를 함께 봐야 한다.
종료는 accept를 멈추고 task를 취소·join하여 socket을 회수한다. I/O worker 기본값은 가용 CPU와2 중 작은 수다.

Tokio1.53.1의 net/io-util/time/sync/signal/rt-multi-thread/macros만 사용한다.
`select!`는 shutdown·accept·task 완료를 명확히 다루기 위해 사용하며 자체 async polling framework를 만들지 않는다.
serde_json1.0.151로 JSON escape를 처리하고 custom derive 모델을 추가하지 않는다.
인증·native crypto까지 포함한 현재 Cargo lock은127개 registry package를 모든 플랫폼·선택 의존성까지 잠근다.
실제 컴파일 subset은 host와 feature에 따라 다르다.
[원문 라이선스 고지](../third_party/rust/README.md)는 lock hash와 함께 보존한다.

## 청크 section 저장과 wire 값

`world::section`은4096개 block state ID와64개 biome ID를 처리한다. 좌표는 각각16³/4³이고 linear index는 `x + side*(z + side*y)`다.
single-value는 별도 단일 값, indirect는 palette+packed words, direct는 registry ID words다.
block indirect bits는4..8, biome은1..3이며 direct bits는 전달받은 registry 크기에서 구한다.
변경할 때 palette를 불필요하게 축소하지 않고 명시적인 repack에서 linear 첫 등장 순서로 정리한다.

64-bit word를 넘어 값을 나누어 저장하지 않으며 각 word의 padding을 구분한다.
이 버전의 section payload에는 **non-empty block count와 fluid block count의 두 short**가 있고,
palette 뒤의 long 배열에는 별도 length prefix가 없다. 기존 버전의 형식을 가정하지 않는다.

registry ID 범위는 전달받은 연속 ID 공간을 검증한다. air/fluid 판정과 실제 registry 데이터는 호출자 책임이다.
두 count의 범위도 검사하지만 ID만으로 실제 블록 물성을 발명하지 않는다.
잘못된 palette index/ID는 읽기에서 즉시 거부하고 무시되는 padding은 정규화한다.
Java의 일부 늦은 오류 발생과 비정규 입력을 그대로 재출력하는 것까지 보장하는 API는 아니다.

`prepare_section`은 사전 확보한 출력 Vec에 준비한 bytes를 추가하며 heap scratch를 만들지 않는다.
최대 출력16,646bytes를 미리 확보하고 전체 입력·count 검사를 통과한 뒤 추가한다.
single/indirect/direct 저장, palette 성장·repack·비정규 header/padding과 실제 공식 클래스의 출력·변경 반환값을 비교했다.

## 실제 병렬 작업과 메모리 수명

`runtime::CpuPool`은 고정 수의 CPU thread가 실제 `prepare_section` 작업을 수행한다.
임의 closure executor나 per-world pool을 제공하지 않는다. status 응답처럼 작은 I/O 작업을 이 pool에 보내지 않는다.

1. `try_reserve_section`이 slot과 요청 backing bytes33,286을 먼저 확보한다.
2. 확보한 입력16,640bytes를 호출자가 직접 채우고 미리 확보한 출력16,646bytes와 함께 제출한다.
3. worker는 입력을 처리하고 immutable 결과를 만든다. 서로 다른 section의 완료 순서는 달라도 된다.
4. 소비자는 필요 순서로 각 task 결과를 받고 현재 world epoch/revision을 확인한 뒤 사용해야 한다.
5. 취소·오류·ready 결과 보관 중에도 permit을 유지하며 buffer가 해제된 뒤 반환한다.

slot 수는 예약·queued·running·ready·보관된 completion 전체를 포함한다.
worker가 소비자의 결과 수령을 기다리지 않으므로 한 worker·가득 찬 queue·취소와 shutdown에서도 drain이 진행된다.
buffer를 permit과 분리해서 소유할 수 있는 API는 제공하지 않는다. 다른 예산으로 복사하거나 전송한다면 그 소비자가 비용을 책임진다.
worker stack은 각2MiB이며 queue/control/Arc·allocator metadata 및 OS 메모리는 payload 예산과 별도다.

`SectionKey`는 identity와 revision을 보존하며 `world::preparation::SectionPreparationOwner`가 실제 소유한
section 변경과 epoch·generation·revision 검증을 담당한다. 오래된 완료 결과는 cache에 들어가지 않는다.
게임 tick 적용 단계, I/O+CPU 전체 thread 예산 통합은 아직 남아 있다.
이 pool의 완료는 section 준비 병렬화의 검증이며 같은 tick의 상호작용을 병렬화했다는 뜻이 아니다.

소유자의 admission·cache 회수와 압축·configuration 데이터 경계는 [접속 준비 문서](connection-preparation.md)에 있다.

## 검증과 측정

- 실제 TCP·자식 실행 파일 테스트12개와 UTF-8 unit test. 단편화 handshake, 합쳐 보낸 request/ping,
  음수 ping payload, 버전/transfer/login 경로, 순서·한계·deadline·종료를 검사한다.
- 공식 status codec 관찰59줄 재실행 일치, hostname byte sequence222,973개 UTF-16 길이 대조 일치.
- section 일반18개와 실제 JAR oracle:324개 출력·310개 변경 반환값 대조.
- runtime public8개와 지연 worker를 직접 제어하는 internal5개: 출력 bytes, 취소, ready 결과 보관,
  permit 수명, 순서,1-worker 종료를 검사한다.

```powershell
cargo test --locked --all-targets
$env:ARROW_MC_JAVA_REFERENCE_ROOT = 'E:\projects\Arrow MC\Decompile'
cargo test --test chunk_section_java_oracle -- --ignored --nocapture
cargo test --release --test chunk_section_bench -- --ignored --nocapture
cargo run --release --example prepare_sections -- 2 8192 8 257
python tools/collect_rust_notices.py --check
```

성능 예제는 synthetic section 입력과 선언한 registry 크기를 사용한다.
인수는 worker 수·section 수·동시 작업 상한·palette 크기이며 worker0은 동기 대조 경로다.
측정한 버퍼 bytes·worker stack과 전체 프로세스 RSS를 구분하고, 처리량과 queue 포함 지연을 함께 보고한다.
실제 서버 tick p99·월드 생성 속도나 네 플랫폼 성능 향상을 이 수치로 주장하지 않는다.
현재54회 실행과 처리량·지연·메모리·컴파일 비용은 [로컬 측정 보고서](benchmarks/README.md)에 보존했다.
검수·실행·CI 근거는 [foundation 상태](foundation-status.md)와 로컬 `Roadmap/reviews/`에 있다.
