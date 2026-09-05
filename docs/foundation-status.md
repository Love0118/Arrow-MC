# 서버·데이터 기반 구현 상태

기준일 2026-09-05. 전체 서버 목표는 진행 중이며 아이템 행동 게이트는 닫혀 있다.

## 현재 실행 경로

실제 Java status/ping 서버, section palette/packed storage, 고정 worker의 병렬 section 준비를 구현했다.
packet compression·section 변경/완료 소유자·실제 configuration snapshot 준비와 검증을 추가했다.
[접속 준비 범위](connection-preparation.md)에 구현·자원·남은 통합 경계를 기록했다.
[로그인·configuration·예약 tick](login-configuration.md)에는 이번 실제 접속 경로와 실행 방법이 있다.
로그인 인증부터 configuration까지 연결했고, Play·spawn·월드 생성·게임 tick 실행은 남아 있다.
이후 실제 chunk 저장 로딩·canonical owner·heightmap·view·chunk packet을 추가했다.
직전 source commit `15e689d`는 네 native 플랫폼에서 debug/release 각각521개와 Clippy·format을 통과했다.
각 profile의 선택29개는 기본 CI에서 제외하며, 실제 Java 실행 근거는 아래 단계별 기록에서 구분한다.

commit `7a3e90e`의 [CI run33953216241](https://github.com/Love0118/Arrow-MC/actions/runs/33953216241)에서 네 native 플랫폼
전부 architecture·format·Clippy·debug198·release198 테스트가 통과했다. Python tooling·Unicode 재생성과 Linux의
lock 기반 의존성 고지 검사도 성공했다. 최초 `d86860f`는 Unix CLI helper의 불필요한 `mut` lint로 실패했고,
Windows 전용 scope로 수정해 재검증했다. 실행·section 병렬 준비의 성공이며 로그인·게임플레이 완성은 아니다.

Tokio1.53.1·serde_json1.0.151·flate2 1.1.10(zlib-rs)·sha2 0.10.9와 OpenSSL0.10.81·reqwest0.13.4·serde1.0.228을 고정한다.
단일 package의 library+binary를 유지하고 section·compression·큰 cipher body·RSA·청크 저장 decode가 같은 고정 pool을 공유한다.
저장 압축의 최소 기능 lz4_flex0.14.0·xxhash-rust0.8.18을 추가했다.
현재 고정 외부 의존성129개의 [원문 의존성 고지](../third_party/rust/README.md)를 수집·검사했다.
이전의 외부 의존성0개는 아래 초기 데이터 기반 snapshot에 해당하며 현재 서버 package 전체의 수치가 아니다.

## 조명 전파·공용 CPU·게시 경계 추가

[block/sky 조명](lighting.md)의 state/face metadata, DataLayer·section storage, 전파와 초기 재계산을 구현했다.
canonical source를 포착하기 전에 CPU metadata 예산을 확보하고, source palette는 기존 resident와 공유한다.
중단·실패·취소·완료까지 같은 예약을 유지하며, 현재 domain과 canonical revision에 맞는 두 layer만 packet에 제공한다.
실제 Anvil→canonical resident→공용 CPU→조명 승인→chunk packet→기존 TCP 경로를 검증했다.

Windows 전체 debug/release 각각 **612개 통과·37개 선택 제외**, strict all-target Clippy·format,
Python **57개 통과·2개 제외**와 고정 의존성 **129개** 고지 검사를 통과했다.
8개 lighting/packet/registry 선택 oracle, 갱신한 chunk·heightmap oracle2개와 새 combined lighting oracle를 별도 실행했다.
combined oracle는 실제 Java `LevelLightEngine`의 2영역·216개 전체 layer·884,736 nibble을 무제한/7단위 실행과 각각 대조했다.
독립 정확성·자원/추상화 리뷰 두 역할이 kernel, worker 취소·복구, source domain과 packet 수명을 검수했다.
첫 source `9a15a52`의 [CI33972450784](https://github.com/Love0118/Arrow-MC/actions/runs/33972450784)에서
Linux ARM64/x86_64와 macOS ARM64는 debug/release 각각612개·37선택제외를 통과했다.
Windows debug도 통과했지만 release는 기존 저장 취소 테스트의 완료 counter/실제 결과 전달 사이 경쟁에서 실패했다.
counter를 결과 전달 완료로 가정한 테스트의 동기화 문제였다. test-only gate로 결과 전달 직전 취소 시 예약이 남음을
검증하고, 실제 receiver에 결과가 있는 경우에만 즉시 환급을 확인하도록 수정했다. production 동작은 바꾸지 않았다.
수정한 storage debug/release 각각2개와 해당 release 테스트100회 반복을 통과했다.
수정 source `5e14ec702b2817a9542cb5c538a6426552645362`의
[CI33973360687](https://github.com/Love0118/Arrow-MC/actions/runs/33973360687)는 **success**다.
네 native 플랫폼 모두 debug/release 각각612통과·0실패·37선택제외와 format·Clippy·tooling을 통과했다.
네 host의 exact cache 복원도 성공했다. 이전 실패를 성공으로 덮지 않고 `Roadmap/reviews/lighting-ci-{9a15a52,5e14ec7}*`에 구분한다.

새 dependency·crate·world trait·per-world CPU pool은 추가하지 않았다. 기존 native 의존성 cache가 있는 로컬 all-target compile은
debug30.05s/release53.42s였다. 테스트 포함 총65.08s/87.72s이며 clean build나 동일 소스 성능 개선 수치가 아니다.
로그·집계는 로컬 `Roadmap/reviews/lighting-{debug,release}.log`와 대응 summary JSON에 있다.

[32청크 초기 조명 benchmark](benchmarks/lighting-windows-summary.json)는 같은 입력을 세 번씩 측정해
inline198.64ms, pool1 worker230.67ms, 2 workers110.93ms, 4 workers61.68ms 중앙값을 기록했다.
모든 실행의 비교 값1,775,616개가 일치했고, 보수적 공용 예약151,230,784bytes와 별도 worker stack을 기록했다.
이 fixture에서는 1 worker 비용이 더 컸으며, 4 workers는 inline 대비 약3.22배였다. 전체 TPS·p99·RSS 또는 Vanilla 대비 속도 주장이 아니다.

별도 Windows 프로세스에서 같은 benchmark 전체를 관측한 [메모리 표본](benchmarks/lighting-windows-memory.json)은
OS 보고 peak working set47,333,376bytes, private memory 표본 최대42,803,200bytes였다.
registry/source 준비와 전체 실행·검증을 포함한 값이며 worker별 분리 측정·resident 예산 상한을 뜻하지 않는다.

저장 light 재사용·PRE/POST callbacks·ticket/status·실제 world mutation과 Play는 남아 있다.
`5e14ec7`까지의 완료본은 보관하는 동안 CPU slot을 유지했다. 후속 resident 이전은 아래에 별도로 기록한다.
겹치는 domain의 전역 조정과 실제 ticket/readiness 연결은 남아 있다.

## 완료 조명의 resident 예산 이전

완료 조명이 CPU 슬롯을 모두 차지하면 worker가 비어 있어도 packet 작업을 제출하지 못하는 문제를 해결했다.
`LightingDomain::accept(owner, completion, &resident_budget)`는 기존 source/domain 검사를 먼저 수행하고,
`ResidentLightingBudget`에 목적지 비용을 확보한 뒤 완료 payload를 이동해 CPU 슬롯과 예약을 반환한다.
목적지가 한 byte 부족해도 원래 completion·CPU 예약·현재 요청을 보존하므로 같은 결과를 재시도할 수 있다.

snapshot의 실제 metadata capacity·body·제어 객체와 layer별 보수적 payload allowance, source backing을 계산한다.
uniform layer의 잠재2KiB를 보존하며 이미 해제된 작업 큐의 최대량은 resident 비용에 넣지 않는다.
shared registry·canonical 청크 palette는 복사하지 않고 원래 lease를 유지한다. 계산과 이전 과정에 새 payload allocation은 없다.
공유 `ResidentLightingBudget`을 사용해야 여러 domain의 합산량이 제한된다.

CPU 슬롯 하나의 실제 canonical→조명→resident 이전→packet→TCP 시험과 예산 부족/재시도·unload 후 수명·COW·overflow를 검증했다.
예제 한 건은 CPU 예약8,392,584bytes에서 resident 청구120,156bytes로 바뀌었다. 이 값은 해당 fixture의 admission 비용이며 RSS가 아니다.
새 테스트 포함 로컬 debug/release 전체 각각630개·37선택제외와 strict Clippy·format을 통과했다.
source `e0b45930dca1da4ac9d66b6b37cf7cfe5369e692`를 beta에 전달했고,
[CI33974887856](https://github.com/Love0118/Arrow-MC/actions/runs/33974887856)에서 네 native 플랫폼 모두
debug/release 각각630통과·0실패·37선택제외와 format·Clippy·tooling·의존성 고지 검사를 통과했다.
이전 `5e14ec7`의612개 성공과 구분한 원본·집계는 로컬 `Roadmap/reviews/resident-lighting-ci-e0b4593*`에 있다.

## 저장 조명의 초기화·재계산 선택과 packet 데이터

`LightingDomain::begin_restore`와 `LightingWork::new_restore`가 canonical resident의 원래 저장 row 순서를 읽는다.
필요한 retain·block/sky queue를 준비하고 source/support·UPDATE·enable/retain 해제·조건부 전파를 진행한다.
재사용 조건은 저장 status가 Light/Spawn/Full이고 lightCorrect가 true인 경우다. 배열이 완전한지 새 조건을 넣지 않고,
재계산할 청크의 저장 배열도 먼저 staging한다. source의 저장 status/flag는 원본으로 유지한다.

`LightDataSnapshot`은 packet/저장 조회에서 queued를 visible보다 먼저 선택하며, 지원 section이 없는 queued-only layer도 보존한다.
게임 내 밝기는 기존 visible snapshot을 사용한다. implicit zero·allocated-zero 차이를 packet mask에서도 유지한다.
완료 전에 양쪽 snapshot의 allocation을 승인하며, 실패하면 부분 완료를 노출하지 않는다.
실패 시 Vec보다 예약이 먼저 반환되던 storage 임시값 정리 순서도 수정했다.

실제 Java와23개 복원 transaction을 비교했다. 인접 두 청크의 재사용/재계산이 다른 경우를 포함하며,
무제한·7단위 실행의 합계13,148,160 visible nibble·1,724 packet layer/7,061,504 nibble과1,865회 재개가 일치했다.
16개 잘못된 저장 배열 길이도 실제 Java/Rust에서 거부했다. Java의138개 phase 관찰은 Rust 내부 callback timing 검증으로 확대하지 않는다.
두 독립 리뷰와 실제 restore→resident 이전→packet→한 CPU 슬롯의 TCP 회귀를 통과했다.

로컬 전체 debug/release 각각655통과·38선택제외, strict Clippy·format을 확인했다. 이 source의 native CI는 별도로 기록한다.
대표 resident 시험의 현재 CPU 예약은8,392,608bytes, resident allowance는237,484bytes다.
게임용 visible과 packet용 데이터가 참조하는 같은 layer도 보수적으로 각각 청구하므로 e0b4593의120,156bytes와 구분한다.
실제 layer payload를 두 번 복사한 값이나 RSS 측정은 아니다.

선택 영역의 불변 복원 transaction을 구현했으며, 임의 Threaded 작업의1,000개 batch·priority·pending marker,
mutable chunk lightCorrect/status 게시·ticket/send-sync·전체 Play coordinator는 남아 있다.

## Heightmap·시야·chunk packet 추가

[여섯 heightmap·전체 view2..32·chunk/light wire](chunk-wire-heightmap-view.md)를 구현했다.
registry·heightmap·view·packet의 구현 에이전트와 좁은 NBT sizing 소비자 작업을 병행했으며,
정확성 및 최적화·추상화 두 독립 리뷰가 마지막 조합까지 검수했다.
실제 TCP 시험은 section bytes→chunk packet→sender queue→공유 CPU transport→socket과 control 순서를 검증한다.
이는 실제 world producer·light fence·Play 상태 활성화를 대신하지 않는다.

Windows 전체 debug521/release521통과(각선택29제외), all-target Clippy·format과 Python57개 중55통과/2제외를 확인했다.
commit `15e689d`의 [CI run33963649619](https://github.com/Love0118/Arrow-MC/actions/runs/33963649619)에서
네 native 플랫폼 전부 architecture·format·Clippy·debug521/release521(각선택29제외)와 tooling을 통과했다.
실제 Java 대조는 view6,182행, heightmap70snapshots 및 모든35,723predicate,
chunk packet78사례와231byte golden이다. v2 registry의 독립 hash로 이전72개 chunk 저장 oracle도 다시 통과했다.
실제 inspector에서 heightmap4개/1,184bytes와24sections/192bytes를 확인했다.

새 runtime dependency나 crate는 없고, registry의 tag 판정은 기존 state flag byte에 통합했다.
원본 소스·JAR·생성된 bulk Mojang 데이터는 로컬 참조에 남기고 독립 API 호출 helper와 검증 코드만 배포한다.
기존 native/Rust artifact가 있는 로컬 cache에서 이번 all-target compile은 debug26.53s/release34.06s였다.
이는 빈 target의 최초 빌드나 속도 향상 측정이 아니다. 로그와 집계는 로컬 `Roadmap/reviews/heightmap-view-packet-build-validation.json`에 있다.

같은 native image·dependency key의 캐시를 네 host 모두 정확히 복원했다. 로그에는 Arrow만 다시 빌드했고 OpenSSL/의존성 재컴파일은 없었다.
cache hit에서 정리·재저장을 건너뛰는 경로도 확인했다. 아래는 이전 `8cccaa3`의 첫 miss와 `15e689d`의 hit를 각각 한 번 관찰한 CI job 전체 시간이다.

| Native host | 첫 miss | 정확한 hit | 복원 시간 |
| --- | ---: | ---: | ---: |
| Linux ARM64 | 283s | 149s | 3s |
| Linux x86_64 | 341s | 146s | 5s |
| macOS ARM64 | 439s | 148s | 10s |
| Windows x86_64 | 1,294s | 348s | 19s |

소스·테스트 범위가481→521로 달라졌고 host 간·반복 표본을 통제한 동일 소스 benchmark가 아니다.
일반 clean build나 runtime/TPS 향상으로 해석하지 않는다. raw log hash·cache ID·step 시간은 로컬 `Roadmap/reviews/heightmap-ci-cache-15e689d.json`에 있다.

## 로드된 청크 owner·chunk sender 추가

[청크 owner](chunk-loading-owner.md)는 현재 수요와 실제 읽기/준비 결과를 owner identity로 연결하고,
좌표 relocation·중복 section/light·누락 기본 section을 canonical view로 제공한다.
[chunk sender](chunk-sender.md)는 f32 rate/ACK·동률 후보 선택과 전체 batch 승인 후 상태 변경을 구현한다.
구현 에이전트 두 역할과 정확성·최적화 독립 리뷰 두 역할이 최종 코드까지 확인했다.

일반 owner12·내부3·sender14개, 실제 sender Java 선택173·ACK/tick475개 관측을 통과했다.
commit `8cccaa3`의 [CI run33961682865](https://github.com/Love0118/Arrow-MC/actions/runs/33961682865)에서
네 native 플랫폼 모두 architecture·format·Clippy·debug481/release481(각선택26제외)와 tooling을 통과했다.
Windows 로컬 동일 전체 검증도 통과했다.
실제 inspect_chunk는 공식 registry·직접 만든 Anvil 파일에서 canonical24sections/192bytes를 준비했다.
raw 저장 읽기·canonical resident·현재 game send-ready를 구분하며, 실제 월드 활성화·Play socket은 아직 연결하지 않았다.

반복 CI의 native dependency 재빌드 비용을 줄이기 위해 공식 actions/cache를 고정 SHA로 사용한다.
native runner/architecture/target·host image·toolchain·lock·manifest·native helper·workflow 설정이 같을 때만 재사용한다.
모든 검사는 항상 실행하고 cache miss의 성공한 job에서 `cargo clean --locked --package arrow-mc`로 Arrow 산출물을 제거한 뒤
의존성만 보존한다. 다른 key로의 fallback은 사용하지 않는다. 최초 네 플랫폼 cache miss·정리·저장이 모두 성공했고,
저장된 캐시4개의 합계는1,649,632,556bytes였다. 이후 정확한 key 복원 결과와 단일 CI 시간 비교는 위 새 묶음에 기록했다.
workflow는 SHA를 검증한 actionlint1.7.12로 YAML·표현식·Action 입력을 확인했다. 외부 shellcheck/pyflakes는 사용하지 않았다.
이번 로컬 all-target compile 시간은 debug21.80s/release38.32s였다. 기존 native/Rust artifact가 있는 cache의 증분 값이며
최초 빌드나 성능 향상 수치가 아니다. 새 runtime 의존성이나 crate 분리는 없다.

## 청크 저장 로딩·예약 tick 복원

[Anvil 로딩](chunk-storage.md)과 [SavedTick](saved-ticks.md)을 실제 구현했다. 저장·압축·registry·NBT 소비자·tick의
구현 에이전트를 병행했고, 정확성 및 최적화·추상화 독립 리뷰 두 역할이 마지막 수정까지 확인했다.
청크 저장은 DataVersion5018의 read-only 경로다. live world/ticket/lighting/spawn·Play, DFU, durable 저장은 남아 있다.
SavedTick은 메모리상 복원/pack과 clear/copy를 제공하며 실제 NBT tick type codec·gameplay callback은 아직 연결하지 않았다.

commit `cf158de`의 [CI run33960174302](https://github.com/Love0118/Arrow-MC/actions/runs/33960174302)에서
네 native 플랫폼 모두 architecture·format·Clippy·debug452/release452(각선택24제외)를 통과했다.
Windows 로컬 동일 전체 검증과 CI의 Python tooling·Unicode 재생성·원문 고지 검사도 통과했다.
Python52개 중50개 통과/2개 조건부 제외, 고정 외부 의존성129개 원문 고지 검사도 통과했다.
공식 대조는 chunk72사례, 압축 NBT 소비988사례, live tick332·saved tick398·heap129관찰이며,
registry는35,723 states·1,286 blocks·67 biomes 전체 ID/default/property를 조회했다. 기본 CI의 ignored와 별도 실제 실행 근거를 구분한다.

리뷰에서 numeric collection의 codec 수용 누락, boxed Float/Double 변환, 실제 I/O 취소의 buffer 수명과
중복 saved tick의 반복 lazy-set 재구축 비용을 확인하고 수정했다. pack/copy scratch는 실제 필요 시에만 승인한다.
합성12청크의1/2/4-worker release 단일 측정은9.305/3.026/1.922ms, resident charge는동일3,172,656bytes였다.
실제4개 job의 peak CPU 예약117,443,144bytes를 확인했다. TPS·RSS·cold-disk·전 플랫폼 성능 개선의 근거로 확대하지 않는다.
실제 공식 registry를 사용한 inspect_chunk 예제로 파일→decode→resident→section bytes 경로도 확인했다.

기존 native dependency와 일부 Rust artifact가 있는 cache에서 전체 target의 이번 Cargo compile 시간은
debug23.55s/release29.04s였다. 이후 변경 없는 release binary build는 process0.324s(Cargo0.14s),
실행 파일8,007,168bytes였다. 빈 target의 최초 컴파일·메모리 측정은 아니며 이전 최초 build와 속도 비교하지 않는다.
원본 로그와 집계는 로컬 `Roadmap/reviews/storage-ticks-build-validation.json`에 있다.

## 실제 로그인·configuration·예약 tick 추가

로그인·configuration·crypto/auth·공유 CPU·예약 tick·입장 정책·native 도구의 구현 역할과 정확성/최적화 독립 리뷰2역할로 진행했다.
public Server를 실제 TCP로 구동하여 local mock online 인증·암호화·압축·verified UUID·전체 configuration 순서를 검증했다.
미검증 계정은 encrypted disconnect로 거부하고 offline fallback을 사용하지 않았다. 별도 release 실행 파일은 명시적 offline 모드에서
실제 고정 snapshot의32registry/432entries/15tagregistry 전송을 확인했다. 두 경우 모두 spawn 준비 전 FinishConfiguration을 보내지 않았다.

Java 실대조: packet scalar312,405개, login213개, crypto/auth 각1개 oracle suite, scheduled tick332trace.
configuration fixture77개는70개 serverbound 관찰과7개 clientbound 관찰이며, Rust는 그중 구현한6개 clientbound codec을 비교한다.
FinishConfiguration은 관찰만 했고 구현 완료로 표시하지 않는다. 실제 snapshot TCP에서도 known-pack omission/full fallback의432payload와 tags를 검증했다.
Windows 로컬 전체332debug/332release·선택16제외, Clippy·format·Python39개 중37통과/2조건부제외,
새 실제 JVM·snapshot6개 suite의 release 명시적 실행을 통과했다.
최초 `c6572a2` CI의 Linux x86_64는 Rust 검증을 통과한 뒤 offline 고지 검사의 다른 플랫폼 package cache 누락으로 실패했다.
검사 전에 `cargo fetch --locked`를 수행하도록 수정하고, 같은 오류의 Cargo stderr를 보이는 회귀 검증을 추가했다.
수정 commit `c4e4348`의 [CI run33957804418](https://github.com/Love0118/Arrow-MC/actions/runs/33957804418)에서
네 native host 모두 architecture·format·Clippy·debug332/release332(선택16제외)를 통과했다.
Python tooling·Unicode 재생성과 전체 플랫폼127package의 원문 고지 검사도 성공했다. 이는 로그인/configuration/live tick 묶음의 검증이다.

리뷰에서 session UUID reset 시점·false shutdown 알림·profile property 묶음 순서·CPU 대기 중 read timeout·
보낸 buffer 장기 보관 문제를 수정했다. known-pack 문자열 전체 보관을 없애고 검증 가능한 byte bound로 반복 decoding을 줄였다.
6.29MB malformed 응답의 현재 실제 소비 경로 p50은1µs,8.39MB 유효 multibyte 입력은4.13ms였다.
단일 Windows release/따뜻한 CPU cache 측정이며 일반 네트워크/TPS·전 플랫폼 가속을 의미하지 않는다. 긴 필드의 검증 CPU 비용은 남는다.

OpenSSL3.6.3·Perl5.42.2.1·NASM3.02를 사용했고 release build 출력에서 assembly 활성화를 확인했다.
새 native/HTTP 의존성을 처음 포함한 전체 release target 컴파일은 Cargo 기준6분12초였다. 기존 Rust artifact가 있어 완전한 fresh-target 측정은 아니다.
동일 cache의 release binary build0.172s/변경 없음0.141s/main timestamp 재컴파일2.164s, 실행 파일7,958,528bytes였다.
이전 작은 server binary보다 의존성·초기 build 비용이 커졌다. 빌드 peak RAM은 측정하지 않았다.
원시 근거는 로컬 `Roadmap/reviews/login-build-cost.json`, `login-binary-smoke.json`과 두 검수 보고서에 있다.

## 압축·section 변경·configuration 데이터 추가

구현 에이전트 세 역할과 정확성·최적화 리뷰어 두 역할로 진행했다. NBT 묶음은 닫고 실제 접속 준비와
chunk 변경·병렬 결과 공개에 작업을 분산했다. 압축 기본12개·owner16개·configuration 합성9개 테스트를 추가했다.
고정 Java 압축132개와 실제 JVM 양방향133개를 통과했고, 실제 configuration32registry/432entry/15tagregistry를 읽었다.
외부 manifest hash 없이 자체 descriptor만 검사하던 문제를 독립 리뷰에서 재현해 수정했으며,
tags 삭제·core payload 변조 후 자체 hash를 재작성한 두 사례가 원래 신뢰값에서 거부됨을 확인했다.
두 리뷰 역할의 현재 범위 차단 사항은 해결됐다. Python32개 중30개 통과, 선택 Unicode·Windows symlink 권한2개는 제외했다.
구현 commit `d5730fc`의 [CI run33955242307](https://github.com/Love0118/Arrow-MC/actions/runs/33955242307)에서
네 native host 모두 architecture·format·Clippy·debug235/release235(선택9제외)를 통과했다.
Python tooling·Unicode 재생성·Linux 의존성 고지 검사도 통과했다. 정확한 commit의 job 결과와 test count를 로컬 리뷰 자료에 보존했다.

현재 Windows 단일 server build 측정은 빈 target/따뜻한 registry·OS cache에서 debug 최초8.367s,
변경 없음0.080s, server module timestamp 재컴파일0.991s, release 최초8.887s였다.
debug/release 실행 파일은1,383,424/697,344bytes다. 사용하지 않는 library 경로는 executable link에서 제거될 수 있다.
이전 server 묶음의7.355/0.089/0.999/8.204s와 단일 표본만 비교하여 성능 회귀·향상을 단정하지 않는다.
build peak RAM은 측정하지 않았고, 원본·source hash는 로컬 `Roadmap/reviews/connection-build-cost.json`에 보존했다.

## NBT 경로 기반 추가

현재 `src/nbt`에 여섯 NumericTag 변환, 반복형 predicate/exact comparison, 여섯 node의 NBT 경로
parse/get/count/create/set/insert/remove를 추가했다. 상세한 ownership·부분 변경·End binary 정책 차이는 [NBT 경로 문서](nbt-path.md)에 있다.
공식 path 관찰3,050개와 수치603,804개·predicate124,848개 실제 JVM 대조를 통과했다.
전체 로컬 debug/release154개, Clippy·format, Python22개 통과이며 선택 oracle5개/1개는 기본 실행에서 제외한다.
Java unchecked예외11개·immutable identity20개·소유 supplier별칭1개·End binary11개는 동일하다고 숨기지 않고 명시적인 API 경계로 검증한다.

거부된20,000단계 factory 값의 재귀 해제로 stack overflow가 발생하던 문제를 수정했다.
`Tag::drop_iterative`와 소유 임시값 guard는 이미 비워진 container slot을 사용해 추가 할당·unsafe 없이 해제한다.
정확성·최적화 리뷰어가 경로·공유 예산·부분 복사·오류 후 해제를 독립 확인했다.
commit `0e872c6`의 [CI run33952148949](https://github.com/Love0118/Arrow-MC/actions/runs/33952148949)에서 네 native host의
format·Clippy·debug154·release154와 Python tooling이 통과했다.

NBT의 남은 범위를 서버 전체의 선행 조건으로 확장하지 않는다. 사용자 피드백에 따라 실제 TCP status/ping 서버,
청크 section palette/packed storage, 제한된 shared CPU worker를 독립 구현 경로로 병행한다.
아이템 행동 게이트는 유지하며 병렬 tick 전체·청크 로딩 전체가 구현됐다고 표시하지 않는다.

## 구현한 범위

- `src/wire`: signed VarInt/VarLong의 읽기·쓰기·길이. Java의 비정규 표현 수용, 마지막 byte의 상위 bit 절삭과
  최대 길이 continuation 뒤 추가 byte가 있을 때의 오류 구분. 할당·외부 dependency 없음.
- `src/nbt`: 13 binary tag ID, UTF-16 값과 Java modified UTF-8, named/network root, heterogeneous list wrapper와
  wrapper 모양 compound escaping, 중복 key 마지막 값, Java float/double zero·NaN 및 equality,
  quota·요청 allocation·512 depth·출력 길이 제한, 실패 시 slice/추가 출력 rollback.
- `src/snbt`: 현대 SNBT 전체 문법을 대상으로 한 UTF-16 parser, compact/pretty writer, translation key·동적 인수,
  별도 입력·할당·출력 제한. [SNBT 범위와 대조 자료](snbt.md)에 관찰 사례와 API 경계를 기록했다.
- `src/unicode_names`: 공식 Unicode16 데이터에서 만든 읽기 전용 1,352,711-byte lookup. 전체 Java25 이름·대문자·hex digit 대조,
  [출처와 고지](unicode-data.md) 보존. 입력마다 heap map을 만들지 않는다.
- 초기 SNBT snapshot은 한 개 Rust library package, Rust1.96.0, unsafe 금지, 외부 Rust dependency0개였다.
- 공식 JAR/source/resource/registry/packet 발견 목록과 lock→metadata→bundler→inner JAR→report provenance 검증 도구.

## 이전 SNBT batch의 실제 실행 검증

Windows x86_64, Rust1.96.0/Java25에서 다음을 실행했다.

| 검증 | 결과와 범위 |
| --- | --- |
| `cargo test --locked --all-targets` | 70통과, live Java oracle3개는 명시적 ignored |
| `cargo test --locked --release --all-targets --timings` | 동일70통과. foundation release 검증이며 서버 부하 benchmark 아님 |
| `cargo test --locked --test wire_java_oracle -- --ignored --nocapture` | 공식26.3-pre-2 Java VarInt/VarLong15,420사례 실제 비교 통과 |
| SNBT frozen oracle | 실제 JVM 관찰7,018개: parser7,005개의 typed value·cursor·오류 key/인수, compact2,063개, 깊이 정책6개 통과; 항목 간 중복 있음 |
| SNBT live writer oracle | Java float71,168개와 pretty38개 실제 비교 통과 |
| Unicode live oracle | 이름 범위1,114,112 code points·canonical294,579개·lookup786,584건·BMP hex65,536 units·비ASCII uppercase1,113,984개, 불일치0 |
| `cargo clippy --locked --all-targets -- -D warnings` | 통과 |
| `cargo fmt --all --check` | 통과 |
| `python -m unittest discover -s tools/tests -v` | 19통과, opt-in Unicode oracle1개 skipped. 별도 opt-in 실행의6개 테스트는 전부 통과 |
| Unicode 생성기·SNBT fixture exporter `--check` | 고정 데이터 hash·재생성 일치,7,018개 fixture 최신성 통과 |
| inventory `--refresh-reports`, `--check` | 공식 generator 실행과5035Java/9893resources/95registries/7053entries/259packets 최신성 확인 |

정확성 리뷰어는 별도 공식 JVM probe와 Rust review harness로 MUTF8·혼합 list·512/513 깊이·중복 key·NaN을 확인했다.
NBT equality에서 발견한 NaN/±0 문제를 수정했다. 최적화 리뷰어가 메모리 예산·추상화·의존성과 inventory provenance를 독립 확인했고,
보고서를 버전/JAR에 묶지 못하던 도구 문제를 regression test와 함께 수정했다.
리뷰 자료는 형제 `Roadmap/reviews/`에 있다.

SNBT 구현에서도 정확성·최적화 리뷰 역할 두 개를 유지했다. 별도 JVM 입력에서 발견한 오류 cursor·literal 후보 진단 차이를 수정하고
전체 corpus로 재검증했다. 확장된 오류 객체의 재귀 stack 비용 문제는 작은 내부 실패 값과 parser 한 곳의 진단으로 해결했다.
512단계 list·compound·builtin은 기본 test thread stack에서 검증했다. 진단 인수의 잘못된 span·출력 제한과 rollback도 확인했다.

최초 SNBT commit `4c099c8`의 [native CI](https://github.com/Love0118/Arrow-MC/actions/runs/33950228901)에서는
Linux x86_64·Windows x86_64와 tooling이 통과했지만 Linux/macOS ARM64의 debug512단계 쓰기 경로에서 stack overflow가 발생했다.
writer도 내부 실패를 offset·kind만 갖는 작은 값으로 변경하고 공개 오류는 경계에서 한 번 구성하도록 수정했다.
Windows debug assembly에서 compact 재귀 frame은3,768→600bytes, pretty compound 재귀 경로는3,848→1,160bytes로 감소했다.
이는 ARM 실행 검증을 대신하지 않으며, parsing·writing·drop을 분리한 기본 stack 회귀 검증을 추가했다.
수정 전후91,000개 출력·오류·rollback 비교가 일치했다.
수정 commit `397bb04`의 [CI run 33950454773](https://github.com/Love0118/Arrow-MC/actions/runs/33950454773)은
네 native host 전부에서 architecture 확인·format·Clippy·debug70·release70 테스트와 tooling을 통과했다.
Linux/macOS ARM64의 512단계 parsing·compact/pretty writing·drop 및 513 거부·실패 rollback을 실제 native host에서 확인했다.

## 현재 library 빌드 비용

초기 SNBT commit `4c099c8`의 Windows x86_64, Ryzen5 9600X, Rust1.96.0에서 별도 빈 Cargo target directory로 한 번 측정했다.
Cargo process 전체 경과 시간은 debug library 최초0.901s, 변경 없음0.052s, `src/lib.rs`의 timestamp만 갱신한
재컴파일0.324s, release library 최초0.887s였다. OS file cache는 비우지 않았다.
debug/release `rlib`는 각각8,083,862/3,208,740bytes였다. 이는 배포 executable 크기나 build peak RAM 측정이 아니다.
서버 코드·다중 플랫폼·실제 코드 변경의 빌드 비용을 대표하지 않으며 단일 측정 원본은 로컬 `Roadmap/reviews/snbt-build-cost.json`에 있다.

writer stack 수정 `397bb04`에서 같은 절차로 다시 측정한 값은 debug 최초0.919s, 변경 없음0.056s,
timestamp 재컴파일0.349s, release 최초0.881s다. debug/release `rlib`는8,116,878/3,208,722bytes였다.
이는 수정 후 측정값이며 단일 표본 간 차이로 성능 향상·회귀를 결론 내리지 않는다. 원본은 `Roadmap/reviews/snbt-build-cost-final.json`이다.

## 남은 범위와 의도한 API 경계

NBT binary와 SNBT는 전체 BASE-NBT가 아니다. path·numeric/predicate와 현재 chunk 소비자의 압축 읽기는 구현했고,
범용 ops/visitor/skip·전체 registry/typed schema·component·migration 및 저장 writer는 남아 있다.
현재 named root API는 이름을 보존·검증하며, 원본 이름을 건너뛰는 Vanilla 디스크 편의 함수와 구분된다.
호출자별 disk fallback/oversized UTF 정책은 해당 소비자 구현에서 따로 대조한다.

디코더 allocation 예산은 요청한 backing allocation 누계이며 allocator의 실제 retained capacity·전체 RSS 보증이 아니다.
writer 예산은 추가된 논리 출력 bytes이고 기존 Vec capacity를 줄이는 정책이 아니다. 이후 worker/connection memory 예산에 결합해야 한다.
compound의 정렬 Vec·private ordinal은 초기 구현 선택이며 hot-path 소비자가 생긴 뒤 조회/변경/메모리 비용을 측정한다.

이전 binary/wire batch인 commit `b8dfca1`의 [CI run 33948100994](https://github.com/Love0118/Arrow-MC/actions/runs/33948100994)에서
Linux x86_64/ARM64·macOS ARM64·Windows x86_64 네 native host의 format/clippy/debug24/release24 테스트와 Python10 테스트가 통과했다.
각 host triple 확인 단계도 성공했다. Java oracle는 CI에서 ignored이며 위의 Windows 로컬 명시적 실행 결과로만 입증한다.
최초 workflow는 YAML 구문으로 job 시작 전 실패했고 `b8dfca1`에서 수정하여 성공했다.
이전 SNBT batch 시점에는 서버 실행기·gameplay·멀티코어 tick/청크 성능을 구현·검증하지 않았다.
소스를 참조해 독립 설계한 구현이며 [출처 정책](provenance-policy.md)을 따른다. clean-room이나 법적 무위험은 주장하지 않는다.
