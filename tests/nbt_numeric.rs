use arrow_mc::nbt::{Compound, NbtString, Tag};

#[test]
fn integer_narrowing_retains_low_bits() {
    let tags = [Tag::Byte(-1), Tag::Short(-1), Tag::Int(-1), Tag::Long(-1)];
    for tag in tags {
        assert_eq!(tag.as_byte(), Some(-1));
        assert_eq!(tag.as_short(), Some(-1));
        assert_eq!(tag.as_int(), Some(-1));
        assert_eq!(tag.as_long(), Some(-1));
        assert_eq!(tag.as_float(), Some(-1.0));
        assert_eq!(tag.as_double(), Some(-1.0));
    }
    let tag = Tag::Long(0x1234_5678_89ab_cdef);
    assert_eq!(tag.as_byte(), Some(-17));
    assert_eq!(tag.as_short(), Some(-12_817));
    assert_eq!(tag.as_int(), Some(-1_985_229_329));
    assert_eq!(Tag::Short(256).as_byte(), Some(0));
    assert_eq!(Tag::Int(65_536).as_short(), Some(0));
}

#[test]
fn floating_integer_conversions_floor_before_saturating() {
    for tag in [Tag::Float(-1.5), Tag::Double(-1.5)] {
        assert_eq!(tag.as_byte(), Some(-2));
        assert_eq!(tag.as_short(), Some(-2));
        assert_eq!(tag.as_int(), Some(-2));
    }
    assert_eq!(Tag::Float(-1.5).as_long(), Some(-1));
    assert_eq!(Tag::Double(-1.5).as_long(), Some(-2));
    for tag in [
        Tag::Float(f32::NEG_INFINITY),
        Tag::Double(f64::NEG_INFINITY),
    ] {
        assert_eq!(tag.as_byte(), Some(0));
        assert_eq!(tag.as_short(), Some(0));
        assert_eq!(tag.as_int(), Some(i32::MIN));
        assert_eq!(tag.as_long(), Some(i64::MIN));
    }
    for tag in [Tag::Float(f32::INFINITY), Tag::Double(f64::INFINITY)] {
        assert_eq!(tag.as_byte(), Some(-1));
        assert_eq!(tag.as_short(), Some(-1));
        assert_eq!(tag.as_int(), Some(i32::MAX));
        assert_eq!(tag.as_long(), Some(i64::MAX));
    }
    assert_eq!(
        Tag::Float(f32::from_bits(0xcf00_0001)).as_int(),
        Some(i32::MIN)
    );
    assert_eq!(
        Tag::Double(f64::from_bits(0xc1e0_0000_0000_0001)).as_int(),
        Some(i32::MIN)
    );
    assert_eq!(
        Tag::Double(f64::from_bits(0xc1e0_0000_0000_0001)).as_long(),
        Some(-2_147_483_649)
    );
}

#[test]
fn nan_zero_and_subnormal_conversions_follow_java() {
    for tag in [
        Tag::Float(f32::NAN),
        Tag::Double(f64::from_bits(0xfff0_0000_0000_0001)),
    ] {
        assert_eq!(tag.as_byte(), Some(0));
        assert_eq!(tag.as_short(), Some(0));
        assert_eq!(tag.as_int(), Some(0));
        assert_eq!(tag.as_long(), Some(0));
        assert!(tag.as_float().unwrap().is_nan());
        assert!(tag.as_double().unwrap().is_nan());
    }
    assert_eq!(Tag::Float(f32::from_bits(0x8000_0001)).as_int(), Some(-1));
    assert_eq!(Tag::Float(f32::from_bits(0x8000_0001)).as_long(), Some(0));
    assert_eq!(
        Tag::Double(f64::from_bits(0x8000_0000_0000_0001)).as_long(),
        Some(-1)
    );
    assert_eq!(Tag::Float(-0.0).as_float().unwrap().to_bits(), 0x8000_0000);
    assert_eq!(
        Tag::Float(-0.0).as_double().unwrap().to_bits(),
        0x8000_0000_0000_0000
    );
    assert_eq!(Tag::Double(-0.0).as_float().unwrap().to_bits(), 0x8000_0000);
    let nan = f32::from_bits(0xff80_0001);
    assert_eq!(Tag::Float(nan).as_float().unwrap().to_bits(), nan.to_bits());
}

#[test]
fn floating_conversion_avoids_intermediate_double_rounding() {
    assert_eq!(
        Tag::Long(4_611_686_293_305_294_849)
            .as_float()
            .unwrap()
            .to_bits(),
        0x5e80_0001
    );
    assert_eq!(
        Tag::Long(-4_611_686_293_305_294_849)
            .as_float()
            .unwrap()
            .to_bits(),
        0xde80_0001
    );
    assert_eq!(
        Tag::Double(f64::from_bits(0x3690_0000_0000_0000))
            .as_float()
            .unwrap()
            .to_bits(),
        0
    );
    assert_eq!(
        Tag::Double(f64::from_bits(0x3690_0000_0000_0001))
            .as_float()
            .unwrap()
            .to_bits(),
        1
    );
    assert_eq!(
        Tag::Double(f64::from_bits(0xb690_0000_0000_0000))
            .as_float()
            .unwrap()
            .to_bits(),
        0x8000_0000
    );
    assert_eq!(
        Tag::Double(f64::from_bits(0x47ef_ffff_f000_0000)).as_float(),
        Some(f32::INFINITY)
    );
    assert_eq!(
        Tag::Double(f64::from_bits(0x47ef_ffff_efff_ffff)).as_float(),
        Some(f32::MAX)
    );
}

#[test]
fn nonnumeric_tags_have_no_numeric_conversion() {
    for tag in [
        Tag::End,
        Tag::String(NbtString::from("1")),
        Tag::List(vec![]),
        Tag::Compound(Compound::new()),
        Tag::ByteArray(vec![1]),
        Tag::IntArray(vec![1]),
        Tag::LongArray(vec![1]),
    ] {
        assert_eq!(tag.as_byte(), None);
        assert_eq!(tag.as_short(), None);
        assert_eq!(tag.as_int(), None);
        assert_eq!(tag.as_long(), None);
        assert_eq!(tag.as_float(), None);
        assert_eq!(tag.as_double(), None);
    }
}
