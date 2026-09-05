# Rust 의존성 고지

고정된 `Cargo.lock`의 모든 플랫폼 registry package에서 라이선스 고지를 원본 bytes 그대로 보존한다.
소스 구현을 vendor하는 디렉터리는 아니다. 바이너리 배포 시 필요한 고지를 배포물에 함께 포함한다.
포함된 OpenSSL 라이브러리, Text::Template 빌드 도구, tracing-core의 spin 구현에는 별도 원문 고지도 보존한다.
각 package의 전체 조건은 아래 원문 고지에서 확인하며 이 목록을 프로젝트 전체의 법적 적합성 인증으로 해석하지 않는다.

| Package | SPDX 선언 | 원문 고지 |
| --- | --- | --- |
| atomic-waker 1.1.2 | Apache-2.0 OR MIT | [LICENSE-APACHE](atomic-waker-1.1.2/LICENSE-APACHE), [LICENSE-MIT](atomic-waker-1.1.2/LICENSE-MIT), [LICENSE-THIRD-PARTY](atomic-waker-1.1.2/LICENSE-THIRD-PARTY) |
| base64 0.22.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](base64-0.22.1/LICENSE-APACHE), [LICENSE-MIT](base64-0.22.1/LICENSE-MIT) |
| bitflags 2.13.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](bitflags-2.13.1/LICENSE-APACHE), [LICENSE-MIT](bitflags-2.13.1/LICENSE-MIT) |
| block-buffer 0.10.4 | MIT OR Apache-2.0 | [LICENSE-APACHE](block-buffer-0.10.4/LICENSE-APACHE), [LICENSE-MIT](block-buffer-0.10.4/LICENSE-MIT) |
| bumpalo 3.20.3 | MIT OR Apache-2.0 | [LICENSE-APACHE](bumpalo-3.20.3/LICENSE-APACHE), [LICENSE-MIT](bumpalo-3.20.3/LICENSE-MIT) |
| bytes 1.12.1 | MIT | [LICENSE](bytes-1.12.1/LICENSE) |
| cc 1.4.5 | MIT OR Apache-2.0 | [LICENSE-APACHE](cc-1.4.5/LICENSE-APACHE), [LICENSE-MIT](cc-1.4.5/LICENSE-MIT) |
| cfg-if 1.0.4 | MIT OR Apache-2.0 | [LICENSE-APACHE](cfg-if-1.0.4/LICENSE-APACHE), [LICENSE-MIT](cfg-if-1.0.4/LICENSE-MIT) |
| core-foundation 0.10.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](core-foundation-0.10.1/LICENSE-APACHE), [LICENSE-MIT](core-foundation-0.10.1/LICENSE-MIT) |
| core-foundation-sys 0.8.7 | MIT OR Apache-2.0 | [LICENSE-APACHE](core-foundation-sys-0.8.7/LICENSE-APACHE), [LICENSE-MIT](core-foundation-sys-0.8.7/LICENSE-MIT) |
| cpufeatures 0.2.17 | MIT OR Apache-2.0 | [LICENSE-APACHE](cpufeatures-0.2.17/LICENSE-APACHE), [LICENSE-MIT](cpufeatures-0.2.17/LICENSE-MIT) |
| crypto-common 0.1.7 | MIT OR Apache-2.0 | [LICENSE-APACHE](crypto-common-0.1.7/LICENSE-APACHE), [LICENSE-MIT](crypto-common-0.1.7/LICENSE-MIT) |
| digest 0.10.7 | MIT OR Apache-2.0 | [LICENSE-APACHE](digest-0.10.7/LICENSE-APACHE), [LICENSE-MIT](digest-0.10.7/LICENSE-MIT) |
| displaydoc 0.2.7 | MIT OR Apache-2.0 | [LICENSE-APACHE](displaydoc-0.2.7/LICENSE-APACHE), [LICENSE-MIT](displaydoc-0.2.7/LICENSE-MIT) |
| errno 0.3.14 | MIT OR Apache-2.0 | [LICENSE-APACHE](errno-0.3.14/LICENSE-APACHE), [LICENSE-MIT](errno-0.3.14/LICENSE-MIT) |
| fastrand 2.5.0 | Apache-2.0 OR MIT | [LICENSE-APACHE](fastrand-2.5.0/LICENSE-APACHE), [LICENSE-MIT](fastrand-2.5.0/LICENSE-MIT) |
| find-msvc-tools 0.1.12 | MIT OR Apache-2.0 | [LICENSE-APACHE](find-msvc-tools-0.1.12/LICENSE-APACHE), [LICENSE-MIT](find-msvc-tools-0.1.12/LICENSE-MIT) |
| flate2 1.1.10 | MIT OR Apache-2.0 | [LICENSE-APACHE](flate2-1.1.10/LICENSE-APACHE), [LICENSE-MIT](flate2-1.1.10/LICENSE-MIT) |
| foreign-types 0.3.2 | MIT/Apache-2.0 | [LICENSE-APACHE](foreign-types-0.3.2/LICENSE-APACHE), [LICENSE-MIT](foreign-types-0.3.2/LICENSE-MIT) |
| foreign-types-shared 0.1.1 | MIT/Apache-2.0 | [LICENSE-APACHE](foreign-types-shared-0.1.1/LICENSE-APACHE), [LICENSE-MIT](foreign-types-shared-0.1.1/LICENSE-MIT) |
| form_urlencoded 1.2.2 | MIT OR Apache-2.0 | [LICENSE-APACHE](form_urlencoded-1.2.2/LICENSE-APACHE), [LICENSE-MIT](form_urlencoded-1.2.2/LICENSE-MIT) |
| futures-channel 0.3.34 | MIT OR Apache-2.0 | [LICENSE-APACHE](futures-channel-0.3.34/LICENSE-APACHE), [LICENSE-MIT](futures-channel-0.3.34/LICENSE-MIT) |
| futures-core 0.3.34 | MIT OR Apache-2.0 | [LICENSE-APACHE](futures-core-0.3.34/LICENSE-APACHE), [LICENSE-MIT](futures-core-0.3.34/LICENSE-MIT) |
| futures-task 0.3.34 | MIT OR Apache-2.0 | [LICENSE-APACHE](futures-task-0.3.34/LICENSE-APACHE), [LICENSE-MIT](futures-task-0.3.34/LICENSE-MIT) |
| futures-util 0.3.34 | MIT OR Apache-2.0 | [LICENSE-APACHE](futures-util-0.3.34/LICENSE-APACHE), [LICENSE-MIT](futures-util-0.3.34/LICENSE-MIT) |
| generic-array 0.14.7 | MIT | [LICENSE](generic-array-0.14.7/LICENSE) |
| getrandom 0.4.3 | MIT OR Apache-2.0 | [LICENSE-APACHE](getrandom-0.4.3/LICENSE-APACHE), [LICENSE-MIT](getrandom-0.4.3/LICENSE-MIT) |
| http 1.5.0 | MIT OR Apache-2.0 | [LICENSE-APACHE](http-1.5.0/LICENSE-APACHE), [LICENSE-MIT](http-1.5.0/LICENSE-MIT) |
| http-body 1.1.0 | MIT | [LICENSE](http-body-1.1.0/LICENSE) |
| http-body-util 0.1.5 | MIT | [LICENSE](http-body-util-0.1.5/LICENSE) |
| httparse 1.10.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](httparse-1.10.1/LICENSE-APACHE), [LICENSE-MIT](httparse-1.10.1/LICENSE-MIT) |
| hyper 1.11.1 | MIT | [LICENSE](hyper-1.11.1/LICENSE) |
| hyper-tls 0.6.0 | MIT/Apache-2.0 | [LICENSE-APACHE](hyper-tls-0.6.0/LICENSE-APACHE), [LICENSE-MIT](hyper-tls-0.6.0/LICENSE-MIT) |
| hyper-util 0.1.20 | MIT | [LICENSE](hyper-util-0.1.20/LICENSE) |
| icu_collections 2.3.0 | Unicode-3.0 | [LICENSE](icu_collections-2.3.0/LICENSE) |
| icu_locale_core 2.3.0 | Unicode-3.0 | [LICENSE](icu_locale_core-2.3.0/LICENSE) |
| icu_normalizer 2.3.0 | Unicode-3.0 | [LICENSE](icu_normalizer-2.3.0/LICENSE) |
| icu_normalizer_data 2.3.0 | Unicode-3.0 | [LICENSE](icu_normalizer_data-2.3.0/LICENSE) |
| icu_properties 2.3.0 | Unicode-3.0 | [LICENSE](icu_properties-2.3.0/LICENSE) |
| icu_properties_data 2.3.0 | Unicode-3.0 | [LICENSE](icu_properties_data-2.3.0/LICENSE) |
| icu_provider 2.3.1 | Unicode-3.0 | [LICENSE](icu_provider-2.3.1/LICENSE) |
| idna 1.1.0 | MIT OR Apache-2.0 | [LICENSE-APACHE](idna-1.1.0/LICENSE-APACHE), [LICENSE-MIT](idna-1.1.0/LICENSE-MIT) |
| idna_adapter 1.2.2 | Apache-2.0 OR MIT | [LICENSE-APACHE](idna_adapter-1.2.2/LICENSE-APACHE), [LICENSE-MIT](idna_adapter-1.2.2/LICENSE-MIT) |
| ipnet 2.12.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](ipnet-2.12.1/LICENSE-APACHE), [LICENSE-MIT](ipnet-2.12.1/LICENSE-MIT) |
| itoa 1.0.18 | MIT OR Apache-2.0 | [LICENSE-APACHE](itoa-1.0.18/LICENSE-APACHE), [LICENSE-MIT](itoa-1.0.18/LICENSE-MIT) |
| js-sys 0.3.105 | MIT OR Apache-2.0 | [LICENSE-APACHE](js-sys-0.3.105/LICENSE-APACHE), [LICENSE-MIT](js-sys-0.3.105/LICENSE-MIT) |
| libc 0.2.189 | MIT OR Apache-2.0 | [LICENSE-APACHE](libc-0.2.189/LICENSE-APACHE), [LICENSE-MIT](libc-0.2.189/LICENSE-MIT) |
| linux-raw-sys 0.12.1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | [COPYRIGHT](linux-raw-sys-0.12.1/COPYRIGHT), [LICENSE-APACHE](linux-raw-sys-0.12.1/LICENSE-APACHE), [LICENSE-Apache-2.0_WITH_LLVM-exception](linux-raw-sys-0.12.1/LICENSE-Apache-2.0_WITH_LLVM-exception), [LICENSE-MIT](linux-raw-sys-0.12.1/LICENSE-MIT) |
| litemap 0.8.3 | Unicode-3.0 | [LICENSE](litemap-0.8.3/LICENSE) |
| log 0.4.34 | MIT OR Apache-2.0 | [LICENSE-APACHE](log-0.4.34/LICENSE-APACHE), [LICENSE-MIT](log-0.4.34/LICENSE-MIT) |
| memchr 2.8.3 | Unlicense OR MIT | [COPYING](memchr-2.8.3/COPYING), [LICENSE-MIT](memchr-2.8.3/LICENSE-MIT), [UNLICENSE](memchr-2.8.3/UNLICENSE) |
| mio 1.2.3 | MIT | [LICENSE](mio-1.2.3/LICENSE) |
| native-tls 0.2.18 | MIT OR Apache-2.0 | [LICENSE-APACHE](native-tls-0.2.18/LICENSE-APACHE), [LICENSE-MIT](native-tls-0.2.18/LICENSE-MIT) |
| once_cell 1.21.4 | MIT OR Apache-2.0 | [LICENSE-APACHE](once_cell-1.21.4/LICENSE-APACHE), [LICENSE-MIT](once_cell-1.21.4/LICENSE-MIT) |
| openssl 0.10.81 | Apache-2.0 | [LICENSE](openssl-0.10.81/LICENSE), [LICENSE-APACHE](openssl-0.10.81/LICENSE-APACHE) |
| openssl-macros 0.1.1 | MIT/Apache-2.0 | [LICENSE-APACHE](openssl-macros-0.1.1/LICENSE-APACHE), [LICENSE-MIT](openssl-macros-0.1.1/LICENSE-MIT) |
| openssl-probe 0.2.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](openssl-probe-0.2.1/LICENSE-APACHE), [LICENSE-MIT](openssl-probe-0.2.1/LICENSE-MIT) |
| openssl-src 300.6.1+3.6.3 | MIT/Apache-2.0 | [LICENSE-APACHE](openssl-src-300.6.1+3.6.3/LICENSE-APACHE), [LICENSE-MIT](openssl-src-300.6.1+3.6.3/LICENSE-MIT), [openssl/LICENSE.txt](openssl-src-300.6.1+3.6.3/openssl/LICENSE.txt), [openssl/external/perl/Text-Template-1.56/LICENSE](openssl-src-300.6.1+3.6.3/openssl/external/perl/Text-Template-1.56/LICENSE) |
| openssl-sys 0.9.117 | MIT | [LICENSE-MIT](openssl-sys-0.9.117/LICENSE-MIT) |
| percent-encoding 2.3.2 | MIT OR Apache-2.0 | [LICENSE-APACHE](percent-encoding-2.3.2/LICENSE-APACHE), [LICENSE-MIT](percent-encoding-2.3.2/LICENSE-MIT) |
| pin-project-lite 0.2.17 | Apache-2.0 OR MIT | [LICENSE-APACHE](pin-project-lite-0.2.17/LICENSE-APACHE), [LICENSE-MIT](pin-project-lite-0.2.17/LICENSE-MIT) |
| pkg-config 0.3.34 | MIT OR Apache-2.0 | [LICENSE-APACHE](pkg-config-0.3.34/LICENSE-APACHE), [LICENSE-MIT](pkg-config-0.3.34/LICENSE-MIT) |
| potential_utf 0.1.6 | Unicode-3.0 | [LICENSE](potential_utf-0.1.6/LICENSE) |
| proc-macro2 1.0.107 | MIT OR Apache-2.0 | [LICENSE-APACHE](proc-macro2-1.0.107/LICENSE-APACHE), [LICENSE-MIT](proc-macro2-1.0.107/LICENSE-MIT) |
| quote 1.0.47 | MIT OR Apache-2.0 | [LICENSE-APACHE](quote-1.0.47/LICENSE-APACHE), [LICENSE-MIT](quote-1.0.47/LICENSE-MIT) |
| r-efi 6.0.0 | MIT OR Apache-2.0 OR LGPL-2.1-or-later | [AUTHORS](r-efi-6.0.0/AUTHORS) |
| reqwest 0.13.4 | MIT OR Apache-2.0 | [LICENSE-APACHE](reqwest-0.13.4/LICENSE-APACHE), [LICENSE-MIT](reqwest-0.13.4/LICENSE-MIT) |
| rustix 1.1.4 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | [COPYRIGHT](rustix-1.1.4/COPYRIGHT), [LICENSE-APACHE](rustix-1.1.4/LICENSE-APACHE), [LICENSE-Apache-2.0_WITH_LLVM-exception](rustix-1.1.4/LICENSE-Apache-2.0_WITH_LLVM-exception), [LICENSE-MIT](rustix-1.1.4/LICENSE-MIT) |
| rustls-pki-types 1.15.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](rustls-pki-types-1.15.1/LICENSE-APACHE), [LICENSE-MIT](rustls-pki-types-1.15.1/LICENSE-MIT) |
| rustversion 1.0.23 | MIT OR Apache-2.0 | [LICENSE-APACHE](rustversion-1.0.23/LICENSE-APACHE), [LICENSE-MIT](rustversion-1.0.23/LICENSE-MIT) |
| schannel 0.1.29 | MIT | [LICENSE.md](schannel-0.1.29/LICENSE.md) |
| security-framework 3.7.0 | MIT OR Apache-2.0 | [LICENSE-APACHE](security-framework-3.7.0/LICENSE-APACHE), [LICENSE-MIT](security-framework-3.7.0/LICENSE-MIT) |
| security-framework-sys 2.17.0 | MIT OR Apache-2.0 | [LICENSE-APACHE](security-framework-sys-2.17.0/LICENSE-APACHE), [LICENSE-MIT](security-framework-sys-2.17.0/LICENSE-MIT) |
| serde 1.0.228 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde-1.0.228/LICENSE-APACHE), [LICENSE-MIT](serde-1.0.228/LICENSE-MIT) |
| serde_core 1.0.228 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde_core-1.0.228/LICENSE-APACHE), [LICENSE-MIT](serde_core-1.0.228/LICENSE-MIT) |
| serde_derive 1.0.228 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde_derive-1.0.228/LICENSE-APACHE), [LICENSE-MIT](serde_derive-1.0.228/LICENSE-MIT) |
| serde_json 1.0.151 | MIT OR Apache-2.0 | [LICENSE-APACHE](serde_json-1.0.151/LICENSE-APACHE), [LICENSE-MIT](serde_json-1.0.151/LICENSE-MIT) |
| sha2 0.10.9 | MIT OR Apache-2.0 | [LICENSE-APACHE](sha2-0.10.9/LICENSE-APACHE), [LICENSE-MIT](sha2-0.10.9/LICENSE-MIT) |
| shlex 2.0.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](shlex-2.0.1/LICENSE-APACHE), [LICENSE-MIT](shlex-2.0.1/LICENSE-MIT) |
| signal-hook-registry 1.4.8 | MIT OR Apache-2.0 | [LICENSE-APACHE](signal-hook-registry-1.4.8/LICENSE-APACHE), [LICENSE-MIT](signal-hook-registry-1.4.8/LICENSE-MIT) |
| slab 0.4.12 | MIT | [LICENSE](slab-0.4.12/LICENSE) |
| smallvec 1.16.0 | MIT OR Apache-2.0 | [LICENSE-APACHE](smallvec-1.16.0/LICENSE-APACHE), [LICENSE-MIT](smallvec-1.16.0/LICENSE-MIT) |
| socket2 0.6.5 | MIT OR Apache-2.0 | [LICENSE-APACHE](socket2-0.6.5/LICENSE-APACHE), [LICENSE-MIT](socket2-0.6.5/LICENSE-MIT) |
| stable_deref_trait 1.2.1 | MIT OR Apache-2.0 | [LICENSE-APACHE](stable_deref_trait-1.2.1/LICENSE-APACHE), [LICENSE-MIT](stable_deref_trait-1.2.1/LICENSE-MIT) |
| syn 2.0.119 | MIT OR Apache-2.0 | [LICENSE-APACHE](syn-2.0.119/LICENSE-APACHE), [LICENSE-MIT](syn-2.0.119/LICENSE-MIT) |
| syn 3.0.5 | MIT OR Apache-2.0 | [LICENSE-APACHE](syn-3.0.5/LICENSE-APACHE), [LICENSE-MIT](syn-3.0.5/LICENSE-MIT) |
| sync_wrapper 1.0.2 | Apache-2.0 | [LICENSE](sync_wrapper-1.0.2/LICENSE) |
| synstructure 0.13.2 | MIT | [LICENSE](synstructure-0.13.2/LICENSE) |
| tempfile 3.27.0 | MIT OR Apache-2.0 | [LICENSE-APACHE](tempfile-3.27.0/LICENSE-APACHE), [LICENSE-MIT](tempfile-3.27.0/LICENSE-MIT) |
| tinystr 0.8.4 | Unicode-3.0 | [LICENSE](tinystr-0.8.4/LICENSE) |
| tokio 1.53.1 | MIT | [LICENSE](tokio-1.53.1/LICENSE) |
| tokio-macros 2.7.2 | MIT | [LICENSE](tokio-macros-2.7.2/LICENSE) |
| tokio-native-tls 0.3.1 | MIT | [LICENSE](tokio-native-tls-0.3.1/LICENSE) |
| tower 0.5.3 | MIT | [LICENSE](tower-0.5.3/LICENSE) |
| tower-http 0.6.11 | MIT | [LICENSE](tower-http-0.6.11/LICENSE) |
| tower-layer 0.3.3 | MIT | [LICENSE](tower-layer-0.3.3/LICENSE) |
| tower-service 0.3.3 | MIT | [LICENSE](tower-service-0.3.3/LICENSE) |
| tracing 0.1.44 | MIT | [LICENSE](tracing-0.1.44/LICENSE) |
| tracing-core 0.1.36 | MIT | [LICENSE](tracing-core-0.1.36/LICENSE), [src/spin/LICENSE](tracing-core-0.1.36/src/spin/LICENSE) |
| try-lock 0.2.5 | MIT | [LICENSE](try-lock-0.2.5/LICENSE) |
| typenum 1.20.1 | MIT OR Apache-2.0 | [LICENSE](typenum-1.20.1/LICENSE), [LICENSE-APACHE](typenum-1.20.1/LICENSE-APACHE), [LICENSE-MIT](typenum-1.20.1/LICENSE-MIT) |
| unicode-ident 1.0.24 | (MIT OR Apache-2.0) AND Unicode-3.0 | [LICENSE-APACHE](unicode-ident-1.0.24/LICENSE-APACHE), [LICENSE-MIT](unicode-ident-1.0.24/LICENSE-MIT), [LICENSE-UNICODE](unicode-ident-1.0.24/LICENSE-UNICODE) |
| url 2.5.8 | MIT OR Apache-2.0 | [LICENSE-APACHE](url-2.5.8/LICENSE-APACHE), [LICENSE-MIT](url-2.5.8/LICENSE-MIT) |
| utf8_iter 1.0.4 | Apache-2.0 OR MIT | [COPYRIGHT](utf8_iter-1.0.4/COPYRIGHT), [LICENSE-APACHE](utf8_iter-1.0.4/LICENSE-APACHE), [LICENSE-MIT](utf8_iter-1.0.4/LICENSE-MIT) |
| vcpkg 0.2.15 | MIT/Apache-2.0 | [LICENSE-APACHE](vcpkg-0.2.15/LICENSE-APACHE), [LICENSE-MIT](vcpkg-0.2.15/LICENSE-MIT) |
| version_check 0.9.5 | MIT/Apache-2.0 | [LICENSE-APACHE](version_check-0.9.5/LICENSE-APACHE), [LICENSE-MIT](version_check-0.9.5/LICENSE-MIT) |
| want 0.3.1 | MIT | [LICENSE](want-0.3.1/LICENSE) |
| wasi 0.11.1+wasi-snapshot-preview1 | Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT | [LICENSE-APACHE](wasi-0.11.1+wasi-snapshot-preview1/LICENSE-APACHE), [LICENSE-Apache-2.0_WITH_LLVM-exception](wasi-0.11.1+wasi-snapshot-preview1/LICENSE-Apache-2.0_WITH_LLVM-exception), [LICENSE-MIT](wasi-0.11.1+wasi-snapshot-preview1/LICENSE-MIT) |
| wasm-bindgen 0.2.128 | MIT OR Apache-2.0 | [LICENSE-APACHE](wasm-bindgen-0.2.128/LICENSE-APACHE), [LICENSE-MIT](wasm-bindgen-0.2.128/LICENSE-MIT) |
| wasm-bindgen-futures 0.4.78 | MIT OR Apache-2.0 | [LICENSE-APACHE](wasm-bindgen-futures-0.4.78/LICENSE-APACHE), [LICENSE-MIT](wasm-bindgen-futures-0.4.78/LICENSE-MIT) |
| wasm-bindgen-macro 0.2.128 | MIT OR Apache-2.0 | [LICENSE-APACHE](wasm-bindgen-macro-0.2.128/LICENSE-APACHE), [LICENSE-MIT](wasm-bindgen-macro-0.2.128/LICENSE-MIT) |
| wasm-bindgen-macro-support 0.2.128 | MIT OR Apache-2.0 | [LICENSE-APACHE](wasm-bindgen-macro-support-0.2.128/LICENSE-APACHE), [LICENSE-MIT](wasm-bindgen-macro-support-0.2.128/LICENSE-MIT) |
| wasm-bindgen-shared 0.2.128 | MIT OR Apache-2.0 | [LICENSE-APACHE](wasm-bindgen-shared-0.2.128/LICENSE-APACHE), [LICENSE-MIT](wasm-bindgen-shared-0.2.128/LICENSE-MIT) |
| web-sys 0.3.105 | MIT OR Apache-2.0 | [LICENSE-APACHE](web-sys-0.3.105/LICENSE-APACHE), [LICENSE-MIT](web-sys-0.3.105/LICENSE-MIT) |
| windows-link 0.2.1 | MIT OR Apache-2.0 | [license-apache-2.0](windows-link-0.2.1/license-apache-2.0), [license-mit](windows-link-0.2.1/license-mit) |
| windows-sys 0.61.2 | MIT OR Apache-2.0 | [license-apache-2.0](windows-sys-0.61.2/license-apache-2.0), [license-mit](windows-sys-0.61.2/license-mit) |
| writeable 0.6.4 | Unicode-3.0 | [LICENSE](writeable-0.6.4/LICENSE) |
| yoke 0.8.3 | Unicode-3.0 | [LICENSE](yoke-0.8.3/LICENSE) |
| yoke-derive 0.8.2 | Unicode-3.0 | [LICENSE](yoke-derive-0.8.2/LICENSE) |
| zerofrom 0.1.8 | Unicode-3.0 | [LICENSE](zerofrom-0.1.8/LICENSE) |
| zerofrom-derive 0.1.7 | Unicode-3.0 | [LICENSE](zerofrom-derive-0.1.7/LICENSE) |
| zeroize 1.9.0 | Apache-2.0 OR MIT | [LICENSE-APACHE](zeroize-1.9.0/LICENSE-APACHE), [LICENSE-MIT](zeroize-1.9.0/LICENSE-MIT) |
| zerotrie 0.2.5 | Unicode-3.0 | [LICENSE](zerotrie-0.2.5/LICENSE) |
| zerovec 0.11.8 | Unicode-3.0 | [LICENSE](zerovec-0.11.8/LICENSE) |
| zerovec-derive 0.11.6 | Unicode-3.0 | [LICENSE](zerovec-derive-0.11.6/LICENSE) |
| zlib-rs 0.6.7 | Zlib | [LICENSE](zlib-rs-0.6.7/LICENSE) |
| zmij 1.0.23 | MIT | [LICENSE-MIT](zmij-1.0.23/LICENSE-MIT) |

재생성: `python tools/collect_rust_notices.py`; 검사: 같은 명령에 `--check`.
Cargo registry cache가 비어 있으면 먼저 `cargo fetch --locked`를 실행한다. 수집과 검사는 offline으로 실행한다.
`sources.json`은 lock·Cargo package·각 고지의 SHA-256, 제공된 VCS revision, 원래 경로와 적용 범위를 기록한다.
r-efi의 `AUTHORS`는 전체 MIT 조건과 저작권 고지를 포함하는 원문이다. package의 SPDX 선언과 내부 구성요소의 조건은 다를 수 있다.
