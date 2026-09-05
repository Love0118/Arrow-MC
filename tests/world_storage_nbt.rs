use arrow_mc::nbt::{self, CompoundEntry, Limits, Tag};

#[test]
fn accounted_root_reports_existing_backing_requests_and_same_cursor() {
    let limits = Limits::default();
    let input = [10, 3, 0, 1, b'a', 0, 0, 0, 7, 0, 99];
    let mut regular = input.as_slice();
    let old = nbt::read_network(&mut regular, limits).unwrap();
    let mut accounted = input.as_slice();
    let (new, bytes) = nbt::read_network_accounted(&mut accounted, limits).unwrap();
    assert_eq!(old, new);
    assert_eq!(regular, accounted);
    assert_eq!(accounted, &[99]);
    // Existing compound reader requests 8 entries for its first capacity and
    // one UTF-16 name unit. The integer payload has no heap allocation.
    assert_eq!(bytes, 8 * size_of::<CompoundEntry>() + 2);
    let mut list = &[9, 3, 0, 0, 0, 2, 0, 0, 0, 1, 0, 0, 0, 2][..];
    let (tag, bytes) = nbt::read_network_accounted(&mut list, limits).unwrap();
    assert!(matches!(tag, Tag::List(_)));
    assert!(list.is_empty());
    assert_eq!(bytes, 2 * size_of::<Tag>());
}

#[test]
fn accounted_errors_preserve_the_original_cursor_and_error_kind() {
    for input in [
        &[10, 3, 0, 1, b'a', 0, 0][..],
        &[9, 3, 255, 255, 255, 255][..],
        &[99][..],
    ] {
        let mut one = input;
        let mut two = input;
        assert_eq!(
            nbt::read_network(&mut one, Limits::default()).unwrap_err(),
            nbt::read_network_accounted(&mut two, Limits::default()).unwrap_err()
        );
        assert_eq!(one, input);
        assert_eq!(two, input);
    }
}
