# 접속 데이터와 병렬 section 결과 준비

고정 기준은 `26.3-pre-2`, protocol `1073742158`이다. 이 구현은 현재 status 서버 다음에 필요한
압축·configuration 데이터와 section 결과 수명 관리를 제공한다. 로그인 인증, configuration session,
실제 spawn 준비와 Play 전환은 아직 연결하지 않았다. 빈 registry나 가짜 spawn으로 접속 완료를 표시하지 않는다.

## 패킷 압축

`server::compression`은 packet ID를 포함한 payload의 프레임 encode/decode와 압축 threshold 변경을 처리한다.
바깥 frame body는 최대 2,097,151 bytes, 압축 경로의 복원 데이터는 최대 8 MiB다. 두 한계는 끝값을 포함한다.
threshold 미만은 DataLength 0으로 보내며 수신 DataLength 0에는 압축 threshold 검사를 적용하지 않는다.
Java가 요청한 출력 길이를 채운 뒤 읽지 않는 데이터까지 더 검사해 정상 수용 여부를 바꾸지 않도록 대조했다.

연결마다 작은 `CompressionState`를 두고, 큰 `CompressionScratch`는 제한된 CPU worker가 재사용한다.
scratch 내부 backend heap은 worker 준비 비용이다. 호출자의 Vec 증가 예산과 별개이며 전체 RSS 상한으로 표시하지 않는다.
codec 오류는 입력 위치·추가 출력 길이를 원복하지만 이미 요청한 할당은 누계 예산에서 차감된 상태를 유지한다.

`prepare_threshold`는 이전 압축 모드로 Login SetCompression을 준비한다. 반환된 guard가 상태를 독점하고
`write_threshold`가 socket을 소유해 전체 쓰기가 성공한 뒤 새 모드를 활성화한다. peer 수신 확인을 기다리는 경계는 아니다.
쓰기가 시작된 뒤 실패·취소되면 socket을 닫고 상태를 재사용할 수 없게 한다. 준비 실패와 시작 전 guard 폐기는 기존 모드를 유지한다.
이전에 queue에 넣은 packet과의 순서는 향후 연결 소유자가 보장해야 한다.

flate2 `1.1.10`의 zlib-rs backend를 선택했다. 고정 실제 Java 경계 132개에서 이 backend는 모두 일치했고
비교한 miniz_oxide backend는 11개가 달랐다. 실제 JVM 양방향 대조는 payload 129개와 한계 거부 4개를 통과했다.
이 선택은 의미 보존이 우선이다. 단일 로컬 표본에서 128 KiB 복원은 zlib-rs가 더 느렸으며 모든 작업의 가속을 주장하지 않는다.

별도 빈 Cargo target·따뜻한 dependency cache의 최소 driver 단일 측정은 miniz/zlib 각각 빌드 3.819/3.699초,
실행 파일 193,536/267,776 bytes였다. inline scratch 8,240/8,432 bytes는 backend heap을 제외한다.
전체 서버 build·RSS·네 플랫폼 성능과 구별한다. 원본은 로컬 `Roadmap/reviews/compression-validation.json`이다.

## 실제 section 변경과 완료 결과

`world::preparation::SectionPreparationOwner`는 loaded chunk와 실제 `Section` palette/count를 소유한다.
chunk/section/pending/cache 수와 source heap 예산을 먼저 정한다. 변경은 ID·count·예산 검사를 통과한 후 수행하며,
실제 값이 바뀐 경우에만 revision을 올린다. count 변경도 결과를 무효화하고, 값이 같은 변경은 cache를 유지한다.

요청은 먼저 준비 수요를 표시한다. `drive`는 pool permit을 확보한 입력 버퍼에 현재 palette를 직접 기록하므로
admission 전에 별도 snapshot을 할당하지 않는다. 진행 중 여러 변경은 최신 revision의 준비 수요로 합쳐진다.
모든 ready task를 확인하여 느린 이전 task가 다른 section 완료 처리를 막지 않는다.

결과는 epoch·chunk generation·section 위치·revision이 현재 소유 데이터와 일치할 때만 cache에 공개한다.
unload/reload 후 늦게 도착한 결과도 버린다. 취소한 실행의 permit은 실제 buffer 해제까지 유지한다.
cache completion도 pool 예산을 보유하며, 자체 cache가 새 작업 admission을 막으면 오래된 cache를 회수한다.
회수만으로 자동 재준비를 반복하지 않으며 소비자가 필요할 때 다시 요청할 수 있다.

cache 조회는 빌린 bytes를 제공한다. 이는 client 전송 확인이나 디스크 저장 완료가 아니다.
실제 game-state tick 의존성과 청크 ticket/world generation/lighting은 후속 범위다.

## 로컬 configuration snapshot

```powershell
python tools/prepare_configuration_data.py
# 아래 값은 위 검증된 준비 실행이 출력한 값으로 설정한다. manifest에서 읽어 만들지 않는다.
$env:ARROW_CONFIGURATION_MANIFEST_SHA256 = '<Trusted manifest SHA-256 출력값>'
cargo test --locked --test configuration_data -- --ignored --nocapture
```

준비 도구는 잠금 metadata·bundler·inner JAR·library 해시를 확인하고, 독립 작성 Java helper로 공식 resource loader와
network element codec을 호출한다. 서버 socket이나 world를 만들지 않는다. 기본 출력은 형제
`Decompile/bootstrap/26.3-pre-2/`이며 기존 디렉터리를 덮어쓰지 않는다. 다시 생성할 때는 `--output`으로 새 하위 경로를 지정한다.
bulk 데이터와 JAR은 Git 배포에 포함하지 않는다.
manifest에는 실행 경로도 기록하므로 재생성마다 fingerprint가 달라질 수 있다. opt-in 테스트의 기본값은 최초 검증한
로컬 snapshot을 고정하며 재생성한 경우 위 환경변수로 새로 신뢰한 값을 전달한다.

출력에는 manifest, registry entry 순서·ID, full network NBT, tags, static member domain,
known packs와 enabled features가 있다. 현재 vanilla-only pack 구성을 지원한다. core pack fingerprint의 의미는
압축을 푼 pack 내용 hash가 아닌 **source JAR SHA-256**이며 manifest에 그 종류를 기록한다.

`ConfigurationSnapshot::load`는 검증된 준비 실행에서 호출자가 별도 보관한 manifest SHA-256을 먼저 대조한다.
manifest 안의 해시를 바꿔 새 데이터를 신뢰시키는 것은 허용하지 않는다. 버전·protocol·JAR 크기/해시·순서 있는
pack fingerprint도 별도 기대값과 대조한다. 이 기대 manifest hash를 검사할 파일에서 다시 읽어 만드는 것은 신뢰 검증이 아니다.
파일 크기/총량/개수/할당 admission을 먼저 검사하고 각 파일 digest, 중복·누락 entry, 연속 protocol ID,
tag member 범위와 full NBT 소비를 검증한다. metadata의 보수적 byte 배수와 한 번의 NBT scratch를 계상하며
이 admission 값은 측정 RSS가 아니다. 신뢰 시작점은 별도 검증한 로컬 준비 실행이며 배포 서명 체계는 제공하지 않는다.

읽은 데이터는 immutable 객체 전체를 공유하도록 설계했으며 entry마다 lock/refcount를 추가하지 않는다.
클라이언트가 요청한 known-pack 목록 전체에 정확히 응답하면 해당 pack의 entry contents를 생략할 수 있다.
부분 응답·순서 차이·알 수 없는 pack이면 전체 contents로 돌아간다. 원본 entry bytes는 항상 남아 있다.

실제 준비 결과는 synchronized registry 32개·entry NBT 432개·tagged registry 15개다.
manifest를 포함한 실제 입력은 439개 파일·1,384,869 bytes, 보관하는 network NBT는 147,864 bytes였다.
Windows x86_64 release 단일 측정의 첫 프로세스 load는 34.135 ms, 같은 프로세스 재호출은 28.100 ms였다.
OS cache는 통제하지 않았고 cold disk·RSS·동시 접속 benchmark로 해석하지 않는다.
이는 해당 접속 데이터의 준비·검증 범위다. 전체 typed registry/component/recipe codec과 item 기반 완성을 뜻하지 않는다.
실제 접속은 인증·프로토콜 전환·클라이언트 응답·spawn 주변 청크 준비 조건을 함께 구현한 뒤 검증한다.
