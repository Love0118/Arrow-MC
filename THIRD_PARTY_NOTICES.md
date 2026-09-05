# Third-party notices

## Rust runtime dependencies

The server uses pinned Tokio and serde_json packages, flate2 with the zlib-rs
backend for protocol and storage compression, and sha2 for local snapshot integrity checks.
Storage also uses the safe decoder in lz4_flex and the xxh32 feature of xxhash-rust.
It also uses OpenSSL for protocol crypto, minimal native-TLS reqwest for session
verification, and serde for bounded typed authentication responses. The resolved
dependencies have individual notices in [third_party/rust/README.md](third_party/rust/README.md).
The collector retains the original license/copyright files and records their
hashes against Cargo.lock, including packages for other supported platforms.
Audited nested notices include the linked OpenSSL sources inside openssl-src,
its Text::Template build tool, and the spin implementation inside tracing-core.
Include the applicable notices when distributing binaries containing this code.

## Unicode data

`third_party/unicode/16.0.0/` contains Unicode 16.0.0 Character Database data.
The generated binary data in `src/unicode_names/data/` is derived from those files
and is included in binaries that use the Unicode name lookup.

These data are provided under Unicode License v3. The complete copyright and
permission notice is in [third_party/unicode/LICENSE.txt](third_party/unicode/LICENSE.txt)
and must accompany redistributed source/data or associated documentation,
including distributions of binaries containing the generated data.

Source URLs, checksums and regeneration instructions are recorded in
[docs/unicode-data.md](docs/unicode-data.md) and
[third_party/unicode/sources.json](third_party/unicode/sources.json).

No Minecraft JAR, decompiled Java implementation, Pumpkin module, or generated
Minecraft asset/registry dump is bundled by this repository. Local reference
and verification material is managed separately in the workspace.
The independently authored Java helpers `tools/oracles/ExportConfigurationData.java`
and `tools/oracles/ExportBlockStateData.java` invoke official APIs from a separately acquired local JAR. Their generated output
stays under the sibling `Decompile/bootstrap/` directory and is not bundled here.
