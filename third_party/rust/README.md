# Rust 의존성 고지

고정된 `Cargo.lock`의 모든 플랫폼 registry package에서 라이선스 고지를 원본 bytes 그대로 보존한다.
소스 구현을 vendor하는 디렉터리는 아니다. 바이너리 배포 시 필요한 고지를 배포물에 함께 포함한다.
각 package의 전체 조건은 아래 원문 고지에서 확인하며 이 목록을 프로젝트 전체의 법적 적합성 인증으로 해석하지 않는다.

| Package | SPDX 선언 | 원문 고지 |
| --- | --- | --- |
| bytes 1.12.1 | MIT | [LICENSE](bytes-1.12.1/LICENSE) |
| errno 0.3.14 | MIT OR Apache-2.0 | [LICENSE-APACHE](errno-0.3.14/LICENSE-APACHE), [LICENSE-MIT](errno-0.3.14/LICENSE-MIT) |
| itoa 1.0.18 | MIT OR Apache-2.0 | [LICENSE-APACHE](itoa-1.0.18/LICENSE-APACHE), [LICENSE-MIT](itoa-1.0.18/LICENSE-MIT) |
| libc 0.2.189 | MIT OR Apache-2.0 | [LICENSE-APACHE](libc-0.2.189/LICENSE-APACHE), [LICENSE-MIT](libc-0.2.189/LICENSE-MIT) |
| memchr 2.8.3 | Unlicense OR MIT | [COPYING](memchr-2.8.3/COPYING), [LICENSE-MIT](memchr-2.8.3/LICENSE-MIT) |
| mio 1.2.3 | MIT | [LICENSE](mio-1.2.3/LICENSE) |
| pin-project-lite 0.2.17 | Apache-2.0 OR MIT | [LICENSE-APACHE](pin-project-lite-0.2.17/LICENSE-APACHE), [LICENSE-MIT](pin-project-lite-0.2.17/LICENSE-MIT) |
| proc-macro2 1.0.107 | MIT OR Apache-2.0 | [LICENSE-APACHE](proc-macro2-1.0.107/LICENSE-APACHE), [LICENSE-MIT](proc-macro2-1.0.107/LICENSE-MIT) |
| quote 1.0.47 | MIT OR Apache-2.0 | [LICENSE-APACHE](quote-1.0.47/LICENSE-APACHE), [LICENSE-MIT](quote-1.0.47/LICENSE-MIT) |
| serde 1.0.229 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde-1.0.229/LICENSE-APACHE), [LICENSE-MIT](serde-1.0.229/LICENSE-MIT) |
| serde_core 1.0.229 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde_core-1.0.229/LICENSE-APACHE), [LICENSE-MIT](serde_core-1.0.229/LICENSE-MIT) |
| serde_derive 1.0.229 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde_derive-1.0.229/LICENSE-APACHE), [LICENSE-MIT](serde_derive-1.0.229/LICENSE-MIT) |
| serde_json 1.0.151 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde_json-1.0.151/LICENSE-APACHE), [LICENSE-MIT](serde_json-1.0.151/LICENSE-MIT) |
| signal-hook-registry 1.4.8 | MIT OR Apache-2.0 | [LICENSE-APACHE](signal-hook-registry-1.4.8/LICENSE-APACHE), [LICENSE-MIT](signal-hook-registry-1.4.8/LICENSE-MIT) |
| socket2 0.6.5 | MIT OR Apache-2.0 | [LICENSE-APACHE](socket2-0.6.5/LICENSE-APACHE), [LICENSE-MIT](socket2-0.6.5/LICENSE-MIT) |
| syn 3.0.5 | MIT OR Apache-2.0 | [LICENSE-APACHE](syn-3.0.5/LICENSE-APACHE), [LICENSE-MIT](syn-3.0.5/LICENSE-MIT) |
| tokio 1.53.1 | MIT | [LICENSE](tokio-1.53.1/LICENSE) |
| tokio-macros 2.7.2 | MIT | [LICENSE](tokio-macros-2.7.2/LICENSE) |
| unicode-ident 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 | [LICENSE-APACHE](unicode-ident-1.0.24/LICENSE-APACHE), [LICENSE-MIT](unicode-ident-1.0.24/LICENSE-MIT), [LICENSE-UNICODE](unicode-ident-1.0.24/LICENSE-UNICODE) |
| wasi 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | [LICENSE-APACHE](wasi-0.11.1+wasi-snapshot-preview1/LICENSE-APACHE), [LICENSE-Apache-2.0_WITH_LLVM-exception](wasi-0.11.1+wasi-snapshot-preview1/LICENSE-Apache-2.0_WITH_LLVM-exception), [LICENSE-MIT](wasi-0.11.1+wasi-snapshot-preview1/LICENSE-MIT) |
| windows-link 0.2.1 | MIT OR Apache-2.0 | [license-apache-2.0](windows-link-0.2.1/license-apache-2.0), [license-mit](windows-link-0.2.1/license-mit) |
| windows-sys 0.61.2 | MIT OR Apache-2.0 | [license-apache-2.0](windows-sys-0.61.2/license-apache-2.0), [license-mit](windows-sys-0.61.2/license-mit) |
| zmij 1.0.23 | MIT | [LICENSE-MIT](zmij-1.0.23/LICENSE-MIT) |

재생성: `python tools/collect_rust_notices.py`; 검사: 같은 명령에 `--check`.
Cargo registry cache가 비어 있으면 고정된 package 다운로드가 필요하다. `sources.json`은 lock과 각 고지의 SHA-256을 기록한다.
