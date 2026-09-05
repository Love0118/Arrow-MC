# 청크 기반 기능의 로컬 측정

## 초기 block/sky 조명

Windows x86_64/Ryzen 5 9600X/Rust1.96.0 release에서 8개 독립 영역·총32청크를 같은 입력으로 비교했다.
세 번씩 순서를 바꿔 측정한 중앙값은 inline198.64ms, pool1 worker230.67ms, 2 workers110.93ms, 4 workers61.68ms였다.
이 fixture에서 한 worker의 제출·재개 비용은 inline보다 컸고, 네 worker는 약3.22배 빨랐다.
1,775,616개 block/sky 비교 값이 모든 실행에서 일치했다. 처리량·지연은 전체 서버 TPS나 Vanilla 성능 비교가 아니다.

source/registry 생성·pool 시작·최종 값 비교·최종 결과/pool 해제는 측정 밖이다. engine 생성, 64단위 재제출,
완료 전달과 임시 kernel 자료구조의 해제는 측정에 포함한다. CPU의 보수적 예약151,230,784bytes와
worker별2MiB stack은 별도이며 RSS는 측정하지 않았다.

[12개 원시 실행·해시·재현 조건](lighting-windows-summary.json)에 실제 측정 코드와 뒤이은 metadata 설명 수정의 해시를 구분했다.
고정 local v3 registry와 `ARROW_MC_JAVA_REFERENCE_ROOT`를 준비한 뒤 재현한다.

```powershell
cargo test --locked --release --test lighting_benchmark -- --ignored --nocapture --test-threads=1
```

## Section 준비

2026-09-05 Windows x86_64, Ryzen5 9600X(6 cores/12 logical CPUs), Rust1.96.0 release에서 실행했다.
각 조합마다32,768 sections를3번 준비한54개 실행이며 아래 처리량·p95는 반복의 중앙값이다.
입력은 미리 만든8개 synthetic section을 순환하며 generation은 측정 밖, worker 입력 복사·admission·queue·결과 수령은 측정 안이다.
동기 경로는 같은 원본을 직접 빌린다. 실제 block registry·worldgen·서버 tick 성능을 측정한 결과가 아니다.

| CPU worker | 1-value section/s | 256-value section/s | 257-value section/s | 256-value의 queue 포함 p95 |
| ---: | ---: | ---: | ---: | ---: |
| 0: 직접 동기 | 345,619 | 133,458 | 289,765 | 10µs |
| 1 | 311,067 | 113,828 | 290,992 | 47µs |
| 2 | 568,679 | 224,372 | 487,113 | 54µs |
| 4 | 712,741 | 396,870 | 704,841 | 76µs |
| 8 | 387,848 | 416,602 | 390,243 | 159µs |
| 12 | 376,044 | 449,815 | 359,738 | 228µs |

256-value fixture의12 worker는 직접 동기 대비3.37배,1 worker pool 대비3.95배 처리량이다.
그러나 queue 지연도 증가하며 single/direct fixture는4 worker 이후 느려졌다.
간단한 section마다 작은 작업을 보내는 비용과 job 단위·동시 작업 수를 실제 청크 소비자 통합에서 다시 측정해야 한다.
최대 core 수나 최대 queue를 무조건 기본값으로 고르는 근거로 사용하지 않는다.

동시 작업 상한은 `max(workers,1)*4`였다.12 worker/48slot에서 요청 buffer 예약 최대1,597,728bytes,
process working set 관찰 최대6,615,040bytes였다.2MiB씩인 worker stack의 주소 공간 예약25,165,824bytes는 RSS와 다른 수치다.
working set은 OS에서 표본으로 읽은 값이며 모든 순간·다른 환경의 최대 RSS를 보증하지 않는다.
원본 fixture133,120bytes, latency 수집 Vec·pool control·allocator/OS 비용도 payload budget과 구분한다.

벤치마크의 checksum은 전체 payload hash가 아니라 길이와 양끝 일부 byte의 **표본 fingerprint**다.
전체 bytes 동등성은 runtime 통합 테스트에서 별도로 검증한다. 초기 전체 checksum 실험은 한 thread의 hash 비용이
결과를 지배해 준비 작업 비교로 쓰지 않았고 로컬 `target/runtime-benchmark/`에 원본을 남겼다.

- [54개 원시 실행](section-preparation-windows-runs.json)
- [조합별 요약](section-preparation-windows-summary.json)
- 실행: `cargo run --release --example prepare_sections -- 4 32768 16 256`
- 다른 palette와 worker 수에도 같은 입력 수로 반복한다. 한 번의 수치나 표본 fingerprint만으로 결과 정확성을 판정하지 않는다.

## Section 저장 payload

별도 `chunk_section_bench`에서 container stack72bytes, uniform heap0bytes,16-value heap2,112bytes,
17-value2,864bytes,256-value5,120bytes,257-value8,192bytes를 확인했다.
4→5 bits 성장 시 기존2,112와 교체2,864의 동시 payload4,976bytes를 예산에 포함한다.
이는 Vec capacity 기반 payload이며 allocator metadata·전체 chunk·light·queued snapshot·RSS를 포함하지 않는다.
읽기 비용도 dense 배열보다 컸으므로 이 표현이 모든 접근 부하에서 빠르다고 결론 내리지 않는다.

## 서버 빌드 비용

같은 Windows/Rust 환경, 비어 있는 별도 Cargo target·따뜻한 registry/OS cache에서 단일 측정했다.
Tokio/JSON 의존성을 포함한 서버 binary debug 최초7.355s, 변경 없음0.089s,
`server/protocol.rs`의 timestamp만 갱신한 재컴파일0.999s, release 최초8.204s였다.
debug/release executable은 각각1,370,112/698,368bytes이며 debug 심볼 파일·다른 산출물은 별도다.
실제 코드 변경·다른 플랫폼·cold download·peak build RAM의 측정값은 아니다.
원본 시간·source hash·Cargo timing 경로는 로컬 `Roadmap/reviews/server-build-cost.json`에 있다.
