# 로그인·configuration·예약 tick 구현

`26.3-pre-2`의 실제 TCP 로그인에서 RSA 응답 검증, online session 조회, 암호화·압축 전환,
LoginAcknowledged와 configuration registry/tag 전송까지 연결했다. **현재 configuration은 실제 spawn 준비를 기다린다.**
월드 생성·저장 청크 load·lighting·player publication이 없으므로 FinishConfiguration/Play를 완료하지 않는다.
아이템 기반 게이트와 전체 Vanilla 목표는 계속 진행 중이다.

## 실행

Windows는 MSVC C++ 도구와 다음 helper로 고정 native Perl/NASM을 준비한다. Unix는 C compiler·make·Perl이 필요하다.
첫 OpenSSL 빌드는 일반 Rust 재컴파일보다 오래 걸리며 helper는 도구를 형제 `Decompile/tools`에 보관한다.
프로세스 환경만 바꾸고 종료 시 복구하며 전역 PATH를 변경하지 않는다.

```powershell
& ./tools/Prepare-WindowsBuild.ps1 -CargoArguments @('build', '--locked', '--release')
```

검증된 configuration 준비 실행에서 **별도 기록한** manifest fingerprint를 사용한다.
검사 대상 manifest에서 기대값을 다시 계산하는 것은 신뢰 검증이 아니다.

```powershell
./target/release/arrow-mc --bind 127.0.0.1 --port 25565 `
  --configuration-snapshot '../Decompile/bootstrap/26.3-pre-2' `
  --configuration-manifest-sha256 '<준비 실행에서 기록한 SHA-256>'
```

기본은 `--online-mode true`, `--compression-threshold 256`, `--accepts-transfers false`다.
`--online-mode false`를 명시한 경우에만 offline 이름 UUID를 사용한다. 인증 오류의 자동 offline fallback은 없다.
snapshot과 기대 hash를 함께 지정하지 않으면 login 관련 설정은 거부된다. 둘을 모두 생략한 기본 실행은 기존 status/ping 경로다.
`Server::bind_with_login`은 library 소비자에게 같은 연결 경로를 제공한다.

## 상태·패킷·인증

Hello의 UUID는 인증 근거로 사용하지 않는다. 이름·배열·문자열의 byte/UTF-16 제한과 malformed UTF-8,
profile property의 key별 묶음 순서까지 공식 codec에 대조했다. LoginFinished는 실제 profile 뒤에 별도 session UUID를 보낸다.
session UUID는 현재 connection 집합이 공유하고, 마지막 socket이 없어지는 maintenance 단계에서 초기화한다.
두 maintenance 사이의 재접속은 기존 UUID를 유지한다. 실제 game tick loop와의 조율은 후속 범위다.

OpenSSL의 RSA1024 PKCS#1 v1.5 EVP 연산과 AES128/CFB8, IV=secret, signed SHA1 server hash를 사용한다.
primitive나 padding 알고리즘을 새로 만들지 않았다. 암호화는 인증 HTTP 시작 전에 활성화되며 frame length까지 감싼다.
압축 설정은 이전 framing으로 쓰고 성공 경계 뒤 새 모드로 전환한다. 같은 방향의 cipher state를 복제하거나 병렬 적용하지 않는다.
실패·취소 뒤 복구할 수 없는 write/cipher 상태는 socket과 함께 폐기한다.

인증은 공유 HTTP client에서 제한된 수의 요청만 수행한다. 기본 connect/read 5초, 전체 요청 10초, body 최대 1 MiB다.
response는 읽는 도중 제한하고, 느린 body도 전체 deadline을 연장하지 않는다. redirect와 proxy를 끄며 TLS 검증을 유지한다.
authlib의 queried name/profile UUID·property 의미, null과 service 오류를 구별한다. 인증 대기 중 잘못된 login 입력도 처리하고
종료된 세션의 늦은 결과를 공개하지 않는다. 이 작업의 테스트는 **로컬 mock service**만 사용했으며 실제 계정 인증을 수행하지 않았다.

입장 정책은 bounded in-memory UUID ban → whitelist/operator → IP ban → 정원/bypass 순서다.
operator의 whitelist 우회와 정원 우회는 별도다. configuring socket을 실제 world player로 세지 않는다.
현재 공개된 player는 0이며 Play 입장·동일 UUID world 퇴장 대기와 정책 재검사는 실제 world owner 통합에서 마무리한다.
정책 파일 저장·관리 명령은 미구현이고, 만료 날짜 표시 문자열은 해당 소비자가 공급한다.
만료 ban은 직접 무시하고 IP는 typed address로 비교하므로 원문의 만료 contains/get 오류와 IPv6 문자열 분리 오류를 재현하지 않는다.

configuration은 brand → 실제 features → known packs → 32 synchronized registries → tags 순서다.
known-pack 응답 전체가 일치하면 해당 entry의 NBT만 생략하며, 그 외에는 full contents를 보낸다.
현재 설정되지 않은 server links/resource pack/code of conduct 작업은 구현한 기본 분기에 포함하지 않는다.
현재 task와 맞지 않는 응답은 거부하며 keepalive는 실제 socket 대기 중에도 동작한다.
부분 frame·cipher·CPU 결과 수신 상태를 유지하여 timer 때문에 입력을 잃지 않는다. `false` shutdown 알림은 종료를 뜻하지 않는다.

## CPU·메모리·컴파일 비용

하나의 CPU pool이 section·packet compression·큰 AES body·RSA를 처리한다. per-world/per-login pool이나 임의 closure executor를 추가하지 않았다.
slot·입력/출력 backing bytes를 먼저 확보하고 취소된 작업도 실제 buffer 해제까지 계상한다.
완료 데이터와 cipher state는 connection 소유자에게 이동하며 borrowed async receive를 취소해도 진행 중 결과를 잃지 않는다.
필요한 작은 frame prefix만 I/O 흐름에서 처리한다.

기본 I/O worker는 CPU 수와 2 중 작은 값이며 CPU worker는 `max(1, 가용 CPU - I/O worker)`다.
`--cpu-workers`, `--cpu-jobs`(기본 64), `--cpu-bytes`(기본 128 MiB)로 조정한다.
`--max-login-connections` 기본 8은 status 연결과 별도로 비싼 login/configuration 체류를 제한한다.
CPU 128 MiB는 전체 서버 RAM 상한이 아니다. codec/직렬화 임시값에는 admitted connection마다 보수적 32 MiB를 별도로 계상한다.
snapshot·native TLS/OpenSSL/압축 내부 상태·thread stack·allocator/OS 비용도 별도다. 이 수치는 요청한 storage 정책이며 RSS 측정값이 아니다.

큰 known-pack 응답은 문자열 전체를 보관하지 않고 검증 후 snapshot 목록과 비교한다.
profile은 소유권을 이동하고 이미 보낸 Hello/LoginFinished buffer는 즉시 해제한다.
정상 registry data는 immutable snapshot을 공유하며 한 번에 다음 packet 하나만 만든다.
로그인·configuration에 status의 30초 전체 교환 deadline/256 KiB 평생 traffic 제한을 적용하지 않는다.

새 의존성은 OpenSSL, 최소 TLS 기능의 reqwest, typed 응답 모델용 serde다. HTTP/2·HTTP 압축·cookie·일반 JSON helper를 켜지 않는다.
HTTP TLS는 Windows Schannel, Apple Security Framework, Linux OpenSSL이며 protocol crypto는 네 플랫폼에서 vendored OpenSSL을 사용한다.
Rust `rsa` crate의 미해결 private-key timing 문제 때문에 이를 채택하지 않았다.
[RustSec 근거](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)와 [EVP 동작](https://docs.openssl.org/3.5/man3/EVP_PKEY_decrypt/)을 확인했다.
원본 고지는 embedded OpenSSL을 포함해 [127개 lock package](../third_party/rust/README.md)에 보존한다.

## 예약 block/fluid tick

`world::ticks::ScheduledTickOwner`는 한 world의 block/fluid queue와 공유 sub-tick counter를 소유한다.
청크 내부는 trigger time 우선, due 청크 사이에는 priority/suborder 우선으로 선택한다.
중복 예약도 Vanilla처럼 counter를 소비하며 signed 정수 계산은 Java wrapping을 보존한다.
수집을 끝낸 뒤 실행할 항목을 소유자에게 하나씩 반환하므로 같은 phase에 새로 예약한 due-now 항목을 몰래 실행하지 않는다.
block 단계에서 예약한 fluid 항목은 뒤의 fluid 수집에 포함될 수 있다.

청크 등록·tick 가능 상태·queue/선택 목록 예산을 검사하고, 한 phase의 기본 실행 상한 65,536을 보존한다.
공간 부족 시 명시적으로 실패하며 tick을 조용히 버리지 않는다. hash는 중복 확인에만 쓰고 실행 순서로 쓰지 않는다.
직접 동기 queue가 실제 block/fluid 행동이나 병렬 game tick을 대신하는 것은 아니다.
SavedTick 복원·완전 동률의 Java heap/hash-map 역사·영역 clear/copy는 필수 후속 구현이다.
좌표 정렬로 동률 결과를 임의로 바꾸지 않는다. 상세 작업은 로컬 `Roadmap/research/scheduled-tick-oracle/compatibility-next.md`에 있다.

실제 JAR의 332개 trace와 16개 queue 테스트를 대조했다. 동기 schedule/collect/dispatch 단일 Windows 측정에서
256/4,096/65,536개 작업의 p50은 약 28.5 µs/571.9 µs/11.65 ms였으며 마지막 크기의 보관 backing은 15,775,744 bytes였다.
독립 리뷰어의 같은 범위 측정은 마지막 크기 약 13.95 ms였다. 이 수치는 게임 TPS·병렬 tick 가속을 입증하지 않는다.

전체 테스트·native CI와 독립 검수 결과는 [구현 상태](foundation-status.md)에 기록한다.
