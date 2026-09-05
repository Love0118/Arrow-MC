# Third-party notices

## Rust runtime dependencies

The server uses pinned Tokio and serde_json packages, flate2 with the zlib-rs
backend for protocol compression, and sha2 for local snapshot integrity checks. Their resolved
dependencies have individual notices in [third_party/rust/README.md](third_party/rust/README.md).
The collector retains the original license/copyright files and records their
hashes against Cargo.lock, including packages for other supported platforms.
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
The independently authored Java helper in `tools/oracles/ExportConfigurationData.java`
invokes official APIs from a separately acquired local JAR. Its generated output
stays under the sibling `Decompile/bootstrap/` directory and is not bundled here.
