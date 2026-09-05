//! Opt-in differential checks against the pinned Vanilla palette/storage classes.
//!
//! Run with `ARROW_MC_JAVA_REFERENCE_ROOT` set to the sibling `Decompile` directory:
//! `cargo test --test chunk_section_java_oracle -- --ignored --nocapture`.
//! Java must support the locked server's class version. The test uses synthetic
//! identity registries and locally installed jars; it contains no Vanilla data.

use arrow_mc::world::section::{
    ContainerKind, MAX_SECTION_NETWORK_BYTES, PalettedContainer, Registry, SectionCounts,
    prepare_section,
};
use std::{env, fmt::Write, fs, path::Path, process::Command, time::SystemTime};

const ALLOCATION_LIMIT: usize = 4 * 1024 * 1024;

// This is an independently written API driver, not a translated implementation.
const ORACLE: &str = r#"
import io.netty.buffer.Unpooled;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.Arrays;
import java.util.HexFormat;
import net.minecraft.core.IdMapper;
import net.minecraft.network.FriendlyByteBuf;
import net.minecraft.util.SimpleBitStorage;
import net.minecraft.world.level.chunk.PalettedContainer;
import net.minecraft.world.level.chunk.Strategy;

class ChunkSectionOracle {
    static Integer[] values;
    static Strategy<Integer> strategy;
    static PalettedContainer<Integer> container;
    static int axisBits;
    static byte[] savedContainer;

    static void create(String kind, int registrySize, int initial) {
        values = new Integer[registrySize];
        IdMapper<Integer> registry = new IdMapper<>(registrySize);
        for (int id = 0; id < registrySize; id++) {
            values[id] = Integer.valueOf(id);
            registry.addMapping(values[id], id);
        }
        axisBits = kind.equals("blocks") ? 4 : 2;
        strategy = axisBits == 4 ? Strategy.createForBlockStates(registry)
                                : Strategy.createForBiomes(registry);
        container = new PalettedContainer<>(values[initial], strategy);
    }

    static int get(int index) {
        int mask = (1 << axisBits) - 1;
        return container.get(index & mask, index >> (2 * axisBits), (index >> axisBits) & mask);
    }

    static int set(int index, int value) {
        int mask = (1 << axisBits) - 1;
        return container.getAndSet(index & mask, index >> (2 * axisBits),
                                   (index >> axisBits) & mask, values[value]);
    }

    static void compact() {
        container = PalettedContainer.unpack(strategy, container.pack(strategy)).getOrThrow();
    }

    // Also exercise SimpleBitStorage directly using the actual network storage.
    // This checks index order, unused high bits and the partially filled final long.
    static void verifyStorage(byte[] encoded) {
        FriendlyByteBuf input = new FriendlyByteBuf(Unpooled.wrappedBuffer(encoded));
        try {
            int bits = input.readUnsignedByte();
            int count = strategy.entryCount();
            if (bits == 0) {
                int value = input.readVarInt();
                for (int index = 0; index < count; index++) {
                    if (get(index) != value) throw new AssertionError("uniform storage");
                }
            } else {
                int[] palette = null;
                if (bits <= (axisBits == 4 ? 8 : 3)) {
                    palette = new int[input.readVarInt()];
                    for (int index = 0; index < palette.length; index++) palette[index] = input.readVarInt();
                }
                long[] raw = new long[(count + 64 / bits - 1) / (64 / bits)];
                for (int index = 0; index < raw.length; index++) raw[index] = input.readLong();
                SimpleBitStorage storage = new SimpleBitStorage(bits, count, raw);
                int[] indices = new int[count];
                for (int index = 0; index < count; index++) {
                    indices[index] = storage.get(index);
                    int actual = palette == null ? indices[index] : palette[indices[index]];
                    if (actual != get(index)) throw new AssertionError("storage index " + index);
                }
                SimpleBitStorage rebuilt = new SimpleBitStorage(bits, count, indices);
                if (!Arrays.equals(raw, rebuilt.getRaw())) throw new AssertionError("storage padding");
            }
            if (input.isReadable()) throw new AssertionError("unexpected trailing storage bytes");
        } finally {
            input.release();
        }
    }

    static byte[] encoded() {
        FriendlyByteBuf output = new FriendlyByteBuf(Unpooled.buffer());
        try {
            container.write(output);
            byte[] encoded = new byte[output.readableBytes()];
            output.readBytes(encoded);
            if (encoded.length != container.getSerializedSize()) throw new AssertionError("serialized size");
            return encoded;
        } finally {
            output.release();
        }
    }

    static void snapshot(String label) {
        byte[] encoded = encoded();
        verifyStorage(encoded);
        StringBuilder result = new StringBuilder(label).append('|')
            .append(container.bitsPerEntry()).append('|')
            .append(HexFormat.of().formatHex(encoded)).append('|');
        for (int index = 0; index < strategy.entryCount(); index++) {
            if (index != 0) result.append(',');
            result.append(get(index));
        }
        System.out.println(result);
    }

    public static void main(String[] args) throws Exception {
        int lineNumber = 0;
        for (String line : Files.readAllLines(Path.of(args[0]))) {
            lineNumber++;
            String[] fields = line.split(" ");
            switch (fields[0]) {
                case "new" -> create(fields[1], Integer.parseInt(fields[2]), Integer.parseInt(fields[3]));
                case "set" -> {
                    int old = set(Integer.parseInt(fields[1]), Integer.parseInt(fields[2]));
                    if (old != Integer.parseInt(fields[3])) throw new AssertionError("old value at line " + lineNumber);
                }
                case "fill" -> {
                    int value = Integer.parseInt(fields[1]);
                    for (int index = 0; index < strategy.entryCount(); index++) set(index, value);
                }
                case "repack" -> compact();
                case "dense" -> {
                    String[] cells = fields[3].split(",");
                    create(fields[1], Integer.parseInt(fields[2]), Integer.parseInt(cells[0]));
                    if (cells.length != strategy.entryCount()) throw new AssertionError("dense size");
                    for (int index = 0; index < cells.length; index++) set(index, Integer.parseInt(cells[index]));
                    compact();
                }
                case "read" -> {
                    create(fields[1], Integer.parseInt(fields[2]), 0);
                    FriendlyByteBuf input = new FriendlyByteBuf(Unpooled.wrappedBuffer(HexFormat.of().parseHex(fields[3])));
                    try {
                        container.read(input);
                        if (input.isReadable()) throw new AssertionError("reader consumed length at line " + lineNumber);
                        if (container.bitsPerEntry() != Integer.parseInt(fields[4])) throw new AssertionError("normalized bits at line " + lineNumber);
                    } finally {
                        input.release();
                    }
                    // Rust intentionally clears unused padding when reading. Compare
                    // both sides after public pack/unpack, which normalizes those bits.
                    compact();
                }
                case "remember" -> savedContainer = encoded();
                case "section" -> {
                    FriendlyByteBuf output = new FriendlyByteBuf(Unpooled.buffer());
                    try {
                        output.writeShort(Integer.parseInt(fields[2]));
                        output.writeShort(Integer.parseInt(fields[3]));
                        output.writeBytes(savedContainer);
                        container.write(output);
                        byte[] encoded = new byte[output.readableBytes()];
                        output.readBytes(encoded);
                        System.out.println(fields[1] + "|" + HexFormat.of().formatHex(encoded));
                    } finally {
                        output.release();
                    }
                }
                case "snapshot" -> snapshot(fields[1]);
                default -> throw new AssertionError("unknown operation at line " + lineNumber);
            }
        }
    }
}
"#;

fn name(kind: ContainerKind) -> &'static str {
    match kind {
        ContainerKind::Blocks => "blocks",
        ContainerKind::Biomes => "biomes",
    }
}

fn entry_count(kind: ContainerKind) -> usize {
    match kind {
        ContainerKind::Blocks => 4096,
        ContainerKind::Biomes => 64,
    }
}

fn hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(result, "{byte:02x}").unwrap();
    }
    result
}

#[derive(Default)]
struct Cases {
    script: String,
    expected: Vec<(String, String)>,
    checked_sets: usize,
}

impl Cases {
    fn new_container(
        &mut self,
        kind: ContainerKind,
        registry_size: u32,
        initial: u32,
    ) -> PalettedContainer {
        writeln!(self.script, "new {} {registry_size} {initial}", name(kind)).unwrap();
        PalettedContainer::single(kind, Registry::new(registry_size).unwrap(), initial).unwrap()
    }

    fn set(&mut self, container: &mut PalettedContainer, index: usize, value: u32) {
        let old = container.set(index, value, ALLOCATION_LIMIT).unwrap();
        writeln!(self.script, "set {index} {value} {old}").unwrap();
        self.checked_sets += 1;
    }

    fn fill(&mut self, container: &mut PalettedContainer, kind: ContainerKind, value: u32) {
        for index in 0..entry_count(kind) {
            container.set(index, value, ALLOCATION_LIMIT).unwrap();
        }
        writeln!(self.script, "fill {value}").unwrap();
    }

    fn repack(&mut self, container: &mut PalettedContainer) {
        container.repack(ALLOCATION_LIMIT).unwrap();
        writeln!(self.script, "repack").unwrap();
    }

    fn snapshot(&mut self, container: &PalettedContainer, kind: ContainerKind, label: String) {
        writeln!(self.script, "snapshot {label}").unwrap();
        let mut encoded = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES);
        container.write_network(&mut encoded).unwrap();
        let mut expected = format!("{label}|{}|", container.bits());
        for byte in encoded {
            write!(expected, "{byte:02x}").unwrap();
        }
        expected.push('|');
        for index in 0..entry_count(kind) {
            if index != 0 {
                expected.push(',');
            }
            write!(expected, "{}", container.get(index).unwrap()).unwrap();
        }
        self.expected.push((label, expected));
    }

    fn dense(&mut self, kind: ContainerKind, registry_size: u32, cells: &[u32], label: String) {
        let container = PalettedContainer::from_dense(
            kind,
            Registry::new(registry_size).unwrap(),
            cells,
            ALLOCATION_LIMIT,
        )
        .unwrap();
        write!(self.script, "dense {} {registry_size} ", name(kind)).unwrap();
        for (index, cell) in cells.iter().enumerate() {
            if index != 0 {
                self.script.push(',');
            }
            write!(self.script, "{cell}").unwrap();
        }
        self.script.push('\n');
        self.snapshot(&container, kind, label);
    }
}

fn growth_cases(cases: &mut Cases, kind: ContainerKind) {
    let mut container = cases.new_container(kind, 65_537, 65_536);
    cases.snapshot(&container, kind, format!("{}-initial", name(kind)));
    let states = match kind {
        ContainerKind::Blocks => 257,
        ContainerKind::Biomes => 33,
    };
    for added in 0..states - 1 {
        // The coprime stride makes first occurrence differ from insertion order.
        let index = (added * 37 + 13) % entry_count(kind);
        cases.set(&mut container, index, (added as u32 * 127) + 128);
        if [2, 3, 4, 5, 8, 9, 16, 17, 32, 33, 64, 65, 128, 129, 256, 257].contains(&(added + 2)) {
            cases.snapshot(
                &container,
                kind,
                format!("{}-growth-{}", name(kind), added + 2),
            );
        }
    }
    // Updating an already present entry must preserve the palette and old value.
    cases.set(&mut container, 13, 128);
    cases.set(&mut container, entry_count(kind) - 1, 128);
    cases.snapshot(&container, kind, format!("{}-existing", name(kind)));
    cases.repack(&mut container);
    cases.snapshot(&container, kind, format!("{}-compacted", name(kind)));
    cases.fill(&mut container, kind, 128);
    cases.snapshot(&container, kind, format!("{}-uniform-unpacked", name(kind)));
    cases.repack(&mut container);
    cases.snapshot(&container, kind, format!("{}-uniform-repacked", name(kind)));

    // Dead palette entries survive ordinary set, but a resize rebuilds live entries.
    let mut container = cases.new_container(kind, 65_537, 0);
    let capacity = match kind {
        ContainerKind::Blocks => 16,
        ContainerKind::Biomes => 2,
    };
    for value in 1..=capacity {
        cases.set(&mut container, 0, value);
        cases.snapshot(
            &container,
            kind,
            format!("{}-dead-entry-{value}", name(kind)),
        );
    }
    cases.repack(&mut container);
    cases.snapshot(
        &container,
        kind,
        format!("{}-dead-entry-repacked", name(kind)),
    );
}

fn dense_cases(cases: &mut Cases, kind: ContainerKind) {
    for registry_size in [1, 2, 8, 9, 16, 256, 257, 16_384, 16_385, 32_769, 65_537] {
        for states in [1, 2, 3, 8, 9, 16, 17, 32, 33, 64, 65, 128, 129, 256, 257] {
            if states > registry_size || states as usize > entry_count(kind) {
                continue;
            }
            let cells: Vec<_> = (0..entry_count(kind))
                .map(|index| {
                    // Descending high IDs exercise multi-byte VarInts and global widths.
                    registry_size - 1 - (index as u32 % states)
                })
                .collect();
            cases.dense(
                kind,
                registry_size,
                &cells,
                format!(
                    "{}-dense-registry-{registry_size}-states-{states}",
                    name(kind)
                ),
            );
        }
    }
}

fn decoder_cases(cases: &mut Cases, kind: ContainerKind) {
    let registry = Registry::new(65_537).unwrap();
    for states in [2, 17, 33, 65, 257] {
        if states > entry_count(kind) {
            continue;
        }
        let cells: Vec<_> = (0..entry_count(kind))
            .map(|index| 65_536 - (index % states) as u32)
            .collect();
        let container =
            PalettedContainer::from_dense(kind, registry, &cells, ALLOCATION_LIMIT).unwrap();
        let mut canonical = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES);
        container.write_network(&mut canonical).unwrap();
        let bits = usize::from(container.bits());
        let per_word = 64 / bits;
        let word_count = entry_count(kind).div_ceil(per_word);
        let storage_start = canonical.len() - word_count * 8;
        let headers: &[u8] = match (kind, bits) {
            (ContainerKind::Blocks, 4) => &[1, 2, 3, 4],
            (ContainerKind::Blocks, 17) => &[9, 17, 31, 127, 128, 255],
            (ContainerKind::Biomes, 17) => &[4, 17, 31, 127, 128, 255],
            _ => &canonical[..1],
        };
        for &header in headers {
            for padding in [false, true] {
                if padding
                    && 64_usize.is_multiple_of(bits)
                    && entry_count(kind).is_multiple_of(per_word)
                {
                    continue;
                }
                let mut encoded = canonical.clone();
                encoded[0] = header;
                if padding {
                    for (word_index, bytes) in
                        encoded[storage_start..].chunks_exact_mut(8).enumerate()
                    {
                        let entries = per_word.min(entry_count(kind) - word_index * per_word);
                        let used = entries * bits;
                        if used != 64 {
                            let raw = u64::from_be_bytes(bytes.try_into().unwrap());
                            bytes.copy_from_slice(&(raw | (u64::MAX << used)).to_be_bytes());
                        }
                    }
                }
                let mut input = encoded.as_slice();
                let mut decoded =
                    PalettedContainer::read_network(&mut input, kind, registry, ALLOCATION_LIMIT)
                        .unwrap();
                assert!(input.is_empty(), "decoder consumed its complete input");
                writeln!(
                    cases.script,
                    "read {} 65537 {} {}",
                    name(kind),
                    hex(&encoded),
                    decoded.bits()
                )
                .unwrap();
                decoded.repack(ALLOCATION_LIMIT).unwrap();
                cases.snapshot(
                    &decoded,
                    kind,
                    format!(
                        "{}-decode-states-{states}-header-{header}-padding-{padding}",
                        name(kind)
                    ),
                );
            }
        }
    }
}

fn prepared_section_cases(cases: &mut Cases) {
    for (case, states) in [1, 2, 16, 17, 32, 33, 64, 65, 128, 129, 256, 257]
        .into_iter()
        .enumerate()
    {
        let biome_states = [1, 2, 4, 8, 9, 17, 33, 64][case % 8];
        let blocks = std::array::from_fn(|index| 65_536 - (index % states) as u32);
        let biomes = std::array::from_fn(|index| 1024 - (index % biome_states) as u32);
        let label = format!("prepared-blocks-{states}-biomes-{biome_states}");
        cases.dense(
            ContainerKind::Blocks,
            65_537,
            &blocks,
            format!("{label}-blocks"),
        );
        writeln!(cases.script, "remember").unwrap();
        cases.dense(
            ContainerKind::Biomes,
            1025,
            &biomes,
            format!("{label}-biomes"),
        );
        let counts = SectionCounts {
            non_empty_blocks: 4096,
            fluid_blocks: states as u16,
        };
        writeln!(
            cases.script,
            "section {label} {} {}",
            counts.non_empty_blocks, counts.fluid_blocks
        )
        .unwrap();
        let mut encoded = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES);
        prepare_section(
            &blocks,
            &biomes,
            Registry::new(65_537).unwrap(),
            Registry::new(1025).unwrap(),
            counts,
            &mut encoded,
        )
        .unwrap();
        cases
            .expected
            .push((label.clone(), format!("{label}|{}", hex(&encoded))));
    }
}

#[test]
#[ignore = "requires Java and ARROW_MC_JAVA_REFERENCE_ROOT with locked Vanilla jars"]
fn matches_locked_java_section_palettes_and_bit_storage() {
    let reference_root = env::var_os("ARROW_MC_JAVA_REFERENCE_ROOT")
        .expect("set ARROW_MC_JAVA_REFERENCE_ROOT to the sibling Decompile directory");
    let artifacts = Path::new(&reference_root).join("artifacts/26.3-pre-2");
    let classpath = env::join_paths([
        artifacts.join("server-26.3-pre-2.jar"),
        artifacts.join("libraries/*"),
    ])
    .unwrap();

    let mut cases = Cases::default();
    for kind in [ContainerKind::Blocks, ContainerKind::Biomes] {
        growth_cases(&mut cases, kind);
        dense_cases(&mut cases, kind);
        decoder_cases(&mut cases, kind);
    }
    prepared_section_cases(&mut cases);

    let unique = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = env::temp_dir().join(format!(
        "arrow-mc-section-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir(&directory).unwrap();
    let source = directory.join("ChunkSectionOracle.java");
    let input = directory.join("input.txt");
    fs::write(&source, ORACLE).unwrap();
    fs::write(&input, &cases.script).unwrap();
    let execution = Command::new("java")
        .arg("--class-path")
        .arg(classpath)
        .arg(source)
        .arg(input)
        .output();
    fs::remove_dir_all(&directory).unwrap();
    let execution = execution.expect("Java must be installed and available on PATH");
    assert!(
        execution.status.success(),
        "Java oracle failed: {}",
        String::from_utf8_lossy(&execution.stderr)
    );
    let output = String::from_utf8(execution.stdout).unwrap();
    let results: Vec<_> = output.lines().collect();
    assert_eq!(results.len(), cases.expected.len(), "oracle response count");
    for ((label, expected), actual) in cases.expected.iter().zip(results) {
        let first_difference = actual
            .bytes()
            .zip(expected.bytes())
            .position(|(java, rust)| java != rust)
            .unwrap_or(actual.len().min(expected.len()));
        assert!(
            actual == expected,
            "Java disagreement for {label} at output byte {first_difference}; Java length {}, Rust length {}",
            actual.len(),
            expected.len()
        );
    }
    eprintln!(
        "Compared {} complete palette/section outputs and {} mutation return values with actual Vanilla 26.3-pre-2 PalettedContainer/SimpleBitStorage classes",
        cases.expected.len(),
        cases.checked_sets
    );
}
