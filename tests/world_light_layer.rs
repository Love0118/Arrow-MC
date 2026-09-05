use arrow_mc::world::lighting::layer::{DataLayer, Error, LAYER_BYTES};

#[test]
fn uniform_and_allocated_zero_have_distinct_observations() {
    let mut uniform = DataLayer::uniform(0);
    assert!(uniform.is_empty());
    assert!(uniform.is_definitely_homogeneous());
    assert!(uniform.is_filled_with(0));
    assert!(!uniform.is_filled_with(16));
    assert_eq!(uniform.heap_bytes(), 0);
    assert_eq!(uniform.bytes(), None);

    let allocated = DataLayer::from_bytes(&[0; LAYER_BYTES], LAYER_BYTES).unwrap();
    assert!(!allocated.is_empty());
    assert!(!allocated.is_definitely_homogeneous());
    assert!(!allocated.is_filled_with(0));
    assert_eq!(allocated.heap_bytes(), LAYER_BYTES);
    uniform.set(0, 0, 0, 0, LAYER_BYTES).unwrap();
    assert!(!uniform.is_empty());
    assert!(!uniform.is_definitely_homogeneous());
    assert_eq!(uniform.bytes(), allocated.bytes());
    uniform.fill(0);
    assert!(uniform.is_empty());
    assert_eq!(uniform.heap_bytes(), 0);
    assert_eq!(uniform.bytes(), None);
}

#[test]
fn arbitrary_uniform_values_change_only_when_materialized() {
    // Expected bytes are independent examples of Java byte narrowing and OR,
    // including cases where the existing high nibble must remain visible.
    for (value, byte) in [
        (i32::MIN, 0x00),
        (-257, 0xff),
        (-256, 0x00),
        (-16, 0xf0),
        (-1, 0xff),
        (0, 0x00),
        (1, 0x11),
        (15, 0xff),
        (16, 0x10),
        (18, 0x32),
        (32, 0x20),
        (128, 0x80),
        (256, 0x00),
        (i32::MAX, 0xff),
    ] {
        let mut layer = DataLayer::uniform(value);
        assert!(layer.is_filled_with(value));
        assert_eq!(layer.get(0, 0, 0).unwrap(), value);
        assert_eq!(layer.get(15, 15, 15).unwrap(), value);
        assert!(
            layer
                .materialize(LAYER_BYTES)
                .unwrap()
                .iter()
                .all(|&v| v == byte)
        );
        assert_eq!(layer.get(0, 0, 0).unwrap(), i32::from(byte & 15));
        assert_eq!(layer.get(15, 15, 15).unwrap(), i32::from(byte >> 4));
        assert!(!layer.is_filled_with(value));
        layer.fill(value);
        assert_eq!(layer.get(0, 0, 0).unwrap(), value);
        assert_eq!(layer.heap_bytes(), 0);
    }
}

#[test]
fn all_coordinates_follow_y_z_x_nibble_order() {
    let mut bytes = [0; LAYER_BYTES];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = (index.wrapping_mul(71) ^ (index / 19)) as u8;
    }
    let layer = DataLayer::from_bytes(&bytes, LAYER_BYTES).unwrap();
    let mut observed = Vec::new();
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                observed.push(layer.get(x, y, z).unwrap());
            }
        }
    }
    let expected: Vec<_> = bytes
        .iter()
        .flat_map(|&byte| [i32::from(byte & 15), i32::from(byte >> 4)])
        .collect();
    assert_eq!(observed, expected);
}

#[test]
fn every_set_masks_the_value_and_preserves_other_nibbles() {
    let mut layer = DataLayer::uniform(7);
    let mut expected = vec![7; 4096];
    let values = [i32::MIN, -257, -16, -1, 0, 15, 16, 23, 255, i32::MAX];
    for (index, expected) in expected.iter_mut().enumerate() {
        let value = values[index % values.len()];
        let x = (index % 16) as u8;
        let z = (index / 16 % 16) as u8;
        let y = (index / 256) as u8;
        layer.set(x, y, z, value, LAYER_BYTES).unwrap();
        *expected = value.rem_euclid(16);
        assert_eq!(layer.get(x, y, z).unwrap(), *expected);
        if index + 1 < 4096 {
            let next = index + 1;
            assert_eq!(
                layer
                    .get(
                        (next % 16) as u8,
                        (next / 256) as u8,
                        (next / 16 % 16) as u8
                    )
                    .unwrap(),
                7
            );
        }
    }
    let actual: Vec<_> = layer
        .bytes()
        .unwrap()
        .iter()
        .flat_map(|&byte| [i32::from(byte & 15), i32::from(byte >> 4)])
        .collect();
    assert_eq!(actual, expected);
}

#[test]
fn invalid_coordinates_are_rejected_before_materialization() {
    let mut layer = DataLayer::uniform(-18);
    for (x, y, z) in [(16, 0, 0), (0, 16, 0), (0, 0, 16), (255, 255, 255)] {
        let expected = Error::CoordinateOutOfBounds { x, y, z };
        assert_eq!(layer.get(x, y, z), Err(expected));
        assert_eq!(layer.set(x, y, z, 8, LAYER_BYTES), Err(expected));
        assert!(layer.is_filled_with(-18));
        assert_eq!(layer.heap_bytes(), 0);
    }
    layer.materialize(LAYER_BYTES).unwrap();
    let before = layer.bytes().unwrap().to_vec();
    assert!(matches!(
        layer.set(16, 0, 0, 8, 0),
        Err(Error::CoordinateOutOfBounds { .. })
    ));
    assert_eq!(layer.bytes().unwrap(), before);
}

#[test]
fn failed_allocations_preserve_source_and_representation() {
    for limit in [0, LAYER_BYTES - 1] {
        let mut layer = DataLayer::uniform(18);
        assert_eq!(
            layer.materialize(limit),
            Err(Error::AllocationBudgetExceeded)
        );
        assert_eq!(
            layer.set(15, 15, 15, 6, limit),
            Err(Error::AllocationBudgetExceeded)
        );
        assert!(layer.is_filled_with(18));
        assert_eq!(layer.heap_bytes(), 0);
        assert!(layer.try_copy(limit).unwrap().is_filled_with(18));
        assert!(layer.repeat_first_layer(limit).unwrap().is_filled_with(18));
        let allocated = DataLayer::from_bytes(&[0xab; LAYER_BYTES], LAYER_BYTES).unwrap();
        assert!(matches!(
            allocated.try_copy(limit),
            Err(Error::AllocationBudgetExceeded)
        ));
        assert!(matches!(
            allocated.repeat_first_layer(limit),
            Err(Error::AllocationBudgetExceeded)
        ));
        assert_eq!(allocated.bytes().unwrap(), &[0xab; LAYER_BYTES]);
        assert!(matches!(
            DataLayer::from_bytes(&[0; LAYER_BYTES], limit),
            Err(Error::AllocationBudgetExceeded)
        ));
    }
    let mut allocated = DataLayer::from_bytes(&[0xab; LAYER_BYTES], LAYER_BYTES).unwrap();
    assert_eq!(allocated.materialize(0).unwrap(), &[0xab; LAYER_BYTES]);
    allocated.set(0, 0, 0, 1, 0).unwrap();
    assert_eq!(allocated.bytes().unwrap()[0], 0xa1);
}

#[test]
fn byte_input_length_is_exact_and_copied() {
    for len in [0, LAYER_BYTES - 1, LAYER_BYTES + 1] {
        assert!(matches!(DataLayer::from_bytes(&vec![0; len], 0),
            Err(Error::InvalidLength { expected: LAYER_BYTES, actual }) if actual == len));
    }
    let mut bytes = [0x21; LAYER_BYTES];
    let layer = DataLayer::from_bytes(&bytes, LAYER_BYTES).unwrap();
    bytes.fill(0xff);
    assert_eq!(layer.bytes().unwrap(), &[0x21; LAYER_BYTES]);
}

#[test]
fn copies_and_repeated_planes_are_independent_and_preserve_allocation() {
    let mut source = DataLayer::uniform(0);
    source.materialize(LAYER_BYTES).unwrap();
    let copy = source.try_copy(LAYER_BYTES).unwrap();
    let zero_repeat = source.repeat_first_layer(LAYER_BYTES).unwrap();
    assert!(!copy.is_empty());
    assert!(!zero_repeat.is_empty());
    source.set(0, 0, 0, 8, 0).unwrap();
    assert_eq!(copy.get(0, 0, 0).unwrap(), 0);
    assert_eq!(zero_repeat.get(0, 0, 0).unwrap(), 0);

    for z in 0..16 {
        for x in 0..16 {
            source
                .set(x, 0, z, i32::from(x) + i32::from(z) * 3, 0)
                .unwrap();
        }
    }
    source.set(0, 15, 0, 9, 0).unwrap();
    let mut repeated = source.repeat_first_layer(LAYER_BYTES).unwrap();
    for y in 0..16 {
        for z in 0..16 {
            for x in 0..16 {
                assert_eq!(repeated.get(x, y, z).unwrap(), source.get(x, 0, z).unwrap());
            }
        }
    }
    repeated.set(0, 0, 0, 15, 0).unwrap();
    assert_eq!(source.get(0, 0, 0).unwrap(), 0);
    assert_eq!(repeated.get(0, 1, 0).unwrap(), 0);
    assert_eq!(source.get(0, 15, 0).unwrap(), 9);
}
