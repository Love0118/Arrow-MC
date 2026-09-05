use arrow_mc::world::section::{
    BIOMES_PER_SECTION, BLOCKS_PER_SECTION, ContainerKind, Error, MAX_SECTION_NETWORK_BYTES,
    PalettedContainer, Registry, Section, SectionCounts, prepare_section,
};

const BUDGET: usize = 128 * 1024;

fn registry() -> Registry {
    Registry::new(1 << 20).unwrap()
}
fn encoded(container: &PalettedContainer) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(container.network_len());
    container.write_network(&mut bytes).unwrap();
    bytes
}

#[test]
fn coordinate_order_and_registry_bounds() {
    for (kind, side) in [(ContainerKind::Blocks, 16), (ContainerKind::Biomes, 4)] {
        for y in 0..side {
            for z in 0..side {
                for x in 0..side {
                    assert_eq!(kind.index(x, y, z).unwrap(), x + side * z + side * side * y);
                }
            }
        }
        assert_eq!(kind.index(side, 0, 0), Err(Error::IndexOutOfBounds));
        assert_eq!(kind.index(0, side, 0), Err(Error::IndexOutOfBounds));
        assert_eq!(kind.index(0, 0, side), Err(Error::IndexOutOfBounds));
    }
    assert_eq!(Registry::new(0), Err(Error::InvalidRegistrySize(0)));
    assert!(Registry::new((1 << 31) + 1).is_err());
    assert_eq!(Registry::new(1 << 31).unwrap().bits(), 31);
    assert_eq!(Registry::new(1).unwrap().bits(), 0);
    assert!(
        PalettedContainer::single(ContainerKind::Blocks, Registry::new(2).unwrap(), 2).is_err()
    );
}

#[test]
fn uniform_wire_has_no_array_length_prefix() {
    let container = PalettedContainer::single(ContainerKind::Blocks, registry(), 300).unwrap();
    assert_eq!(encoded(&container), [0, 0xac, 0x02]);
    assert_eq!(container.heap_bytes(), 0);
    let mut input = &[0, 0xac, 0x02, 0x7f][..];
    let decoded =
        PalettedContainer::read_network(&mut input, ContainerKind::Blocks, registry(), 0).unwrap();
    assert_eq!(decoded.get(4095).unwrap(), 300);
    assert_eq!(input, &[0x7f]);
}

#[test]
fn mutable_palette_growth_preserves_order_until_repack() {
    let mut container = PalettedContainer::single(ContainerKind::Blocks, registry(), 9).unwrap();
    assert_eq!(container.set(1, 7, BUDGET).unwrap(), 9);
    assert_eq!(container.set(0, 8, BUDGET).unwrap(), 9);
    assert_eq!(&encoded(&container)[..5], &[4, 3, 9, 7, 8]);
    container.repack(BUDGET).unwrap();
    assert_eq!(&encoded(&container)[..5], &[4, 3, 8, 7, 9]);
    for index in 0..4096 {
        container.set(index, 8, BUDGET).unwrap();
    }
    assert_eq!(container.bits(), 4);
    container.repack(BUDGET).unwrap();
    assert_eq!(encoded(&container), [0, 8]);
    assert_eq!(container.heap_bytes(), 0);
}

#[test]
fn thresholds_and_padded_words_round_trip_all_entries() {
    for kind in [ContainerKind::Blocks, ContainerKind::Biomes] {
        for distinct in [1, 2, 8, 9, 16, 17, 32, 33, 64, 65, 128, 129, 256, 257] {
            let input: Vec<u32> = (0..kind.len())
                .map(|i| ((i % distinct) * 73) as u32)
                .collect();
            let container =
                PalettedContainer::from_dense(kind, registry(), &input, BUDGET).unwrap();
            let bytes = encoded(&container);
            let mut cursor = bytes.as_slice();
            let restored =
                PalettedContainer::read_network(&mut cursor, kind, registry(), BUDGET).unwrap();
            assert!(cursor.is_empty());
            for (i, &value) in input.iter().enumerate() {
                assert_eq!(restored.get(i).unwrap(), value);
            }
            assert_eq!(bytes, encoded(&restored));
        }
    }
}

#[test]
fn growth_requires_old_plus_new_budget_and_failure_is_transactional() {
    let mut container = PalettedContainer::single(ContainerKind::Blocks, registry(), 0).unwrap();
    for index in 1..16 {
        container.set(index, index as u32, BUDGET).unwrap();
    }
    assert_eq!(container.bits(), 4);
    let old_bytes = container.heap_bytes();
    let before = encoded(&container);
    // A five-bit payload requires ceil(4096/12)*8 bytes plus 32 palette entries.
    let replacement_bytes = 4096_usize.div_ceil(12) * 8 + 32 * 4;
    assert_eq!(
        container.set(16, 16, old_bytes + replacement_bytes - 1),
        Err(Error::AllocationBudgetExceeded)
    );
    assert_eq!(before, encoded(&container));
    container
        .set(16, 16, old_bytes + replacement_bytes)
        .unwrap();
    assert_eq!(container.bits(), 5);
    assert_eq!(container.heap_bytes(), replacement_bytes);
}

#[test]
fn decoder_normalizes_bits_and_ignored_padding() {
    let input: Vec<u32> = (0..4096).map(|i| (i % 17) as u32).collect();
    let container =
        PalettedContainer::from_dense(ContainerKind::Blocks, registry(), &input, BUDGET).unwrap();
    let canonical = encoded(&container);
    let mut dirty = canonical.clone();
    let words_offset = 2 + 17;
    // Five-bit entries use 60 of each complete word's 64 bits.
    for word in dirty[words_offset..].chunks_exact_mut(8) {
        word[0] |= 0xf0;
    }
    let mut cursor = dirty.as_slice();
    let decoded =
        PalettedContainer::read_network(&mut cursor, ContainerKind::Blocks, registry(), BUDGET)
            .unwrap();
    assert_eq!(encoded(&decoded), canonical);

    let input: Vec<u32> = (0..4096).map(|i| (i % 2) as u32).collect();
    let container =
        PalettedContainer::from_dense(ContainerKind::Blocks, registry(), &input, BUDGET).unwrap();
    let canonical = encoded(&container);
    for header in 1..=3 {
        let mut bytes = canonical.clone();
        bytes[0] = header;
        let decoded = PalettedContainer::read_network(
            &mut bytes.as_slice(),
            ContainerKind::Blocks,
            registry(),
            BUDGET,
        )
        .unwrap();
        assert_eq!(encoded(&decoded), canonical);
    }
    let input: Vec<u32> = (0..4096).map(|i| i as u32).collect();
    let container =
        PalettedContainer::from_dense(ContainerKind::Blocks, registry(), &input, BUDGET).unwrap();
    let canonical = encoded(&container);
    for header in [9, 21, 32, 128, 255] {
        let mut bytes = canonical.clone();
        bytes[0] = header;
        let decoded = PalettedContainer::read_network(
            &mut bytes.as_slice(),
            ContainerKind::Blocks,
            registry(),
            BUDGET,
        )
        .unwrap();
        assert_eq!(encoded(&decoded), canonical);
    }
}

#[test]
fn malformed_or_truncated_data_never_advances_the_input() {
    let input: Vec<u32> = (0..4096).map(|i| (i % 17) as u32).collect();
    let container =
        PalettedContainer::from_dense(ContainerKind::Blocks, registry(), &input, BUDGET).unwrap();
    let bytes = encoded(&container);
    for end in 0..bytes.len() {
        let original = &bytes[..end];
        let mut cursor = original;
        assert!(
            PalettedContainer::read_network(&mut cursor, ContainerKind::Blocks, registry(), BUDGET)
                .is_err()
        );
        assert_eq!(cursor, original);
    }
    for bytes in [
        vec![4, 0],
        vec![4, 17],
        vec![0, 0xff, 0xff, 0xff, 0xff, 0x07],
        vec![0, 0xff, 0xff, 0xff, 0xff, 0x0f],
    ] {
        let mut cursor = bytes.as_slice();
        assert!(
            PalettedContainer::read_network(
                &mut cursor,
                ContainerKind::Blocks,
                Registry::new(10).unwrap(),
                BUDGET
            )
            .is_err()
        );
        assert_eq!(cursor, bytes.as_slice());
    }
    let mut bad_index = vec![4, 1, 0];
    bad_index.resize(3 + 256 * 8, 0);
    bad_index[10] = 1;
    assert!(matches!(
        PalettedContainer::read_network(
            &mut bad_index.as_slice(),
            ContainerKind::Blocks,
            registry(),
            BUDGET
        ),
        Err(Error::InvalidPaletteIndex(1))
    ));
}

#[test]
fn maximum_registry_bits_use_full_bound_without_growing_output() {
    let registry = Registry::new(1 << 31).unwrap();
    let blocks = std::array::from_fn(|i| ((1_u32 << 31) - 1) - i as u32);
    let biomes = std::array::from_fn(|i| ((1_u32 << 31) - 1) - i as u32);
    let counts = SectionCounts {
        non_empty_blocks: 4096,
        fluid_blocks: 217,
    };
    let mut bytes = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES);
    let capacity = bytes.capacity();
    prepare_section(&blocks, &biomes, registry, registry, counts, &mut bytes).unwrap();
    assert_eq!(bytes.capacity(), capacity);
    assert_eq!(bytes.len(), MAX_SECTION_NETWORK_BYTES);
    assert_eq!(&bytes[..4], &[16, 0, 0, 217]);
    let decoded = Section::read_network(&mut bytes.as_slice(), registry, registry, BUDGET).unwrap();
    for (index, value) in blocks.iter().enumerate() {
        assert_eq!(decoded.blocks.get(index).unwrap(), *value);
    }
    for (index, value) in biomes.iter().enumerate() {
        assert_eq!(decoded.biomes.get(index).unwrap(), *value);
    }
}

#[test]
fn section_preparation_validates_everything_before_writing() {
    let mut blocks = [0; BLOCKS_PER_SECTION];
    let biomes = [0; BIOMES_PER_SECTION];
    let counts = SectionCounts {
        non_empty_blocks: 4096,
        fluid_blocks: 2,
    };
    let mut bytes = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES + 1);
    bytes.push(13);
    blocks[4095] = 1 << 20;
    assert_eq!(
        prepare_section(&blocks, &biomes, registry(), registry(), counts, &mut bytes),
        Err(Error::ValueOutOfRange(1 << 20))
    );
    assert_eq!(bytes, [13]);
    blocks[4095] = 0;
    let invalid_counts = SectionCounts {
        non_empty_blocks: 1,
        fluid_blocks: 2,
    };
    assert_eq!(
        prepare_section(
            &blocks,
            &biomes,
            registry(),
            registry(),
            invalid_counts,
            &mut bytes
        ),
        Err(Error::InvalidCounts)
    );
    assert_eq!(bytes, [13]);
    let mut short = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES - 1);
    assert_eq!(
        prepare_section(&blocks, &biomes, registry(), registry(), counts, &mut short),
        Err(Error::OutputCapacity)
    );
    assert!(short.is_empty());
}

#[test]
fn section_decoder_budget_and_trailing_data_are_transactional() {
    let blocks = std::array::from_fn(|i| (i % 17) as u32);
    let biomes = std::array::from_fn(|i| (i % 9) as u32);
    let mut bytes = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES);
    let counts = SectionCounts {
        non_empty_blocks: 0,
        fluid_blocks: 0,
    };
    prepare_section(&blocks, &biomes, registry(), registry(), counts, &mut bytes).unwrap();
    let mut cursor = bytes.as_slice();
    assert!(matches!(
        Section::read_network(&mut cursor, registry(), registry(), 1),
        Err(Error::AllocationBudgetExceeded)
    ));
    assert_eq!(cursor, bytes.as_slice());
    bytes.push(19);
    let mut cursor = bytes.as_slice();
    let section = Section::read_network(&mut cursor, registry(), registry(), BUDGET).unwrap();
    assert_eq!(cursor, &[19]);
    let mut output = Vec::with_capacity(MAX_SECTION_NETWORK_BYTES);
    section.write_network(&mut output).unwrap();
    assert_eq!(output, bytes[..bytes.len() - 1]);
}
