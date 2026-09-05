use arrow_mc::nbt::{
    Compound, NbtString, Tag,
    predicate::{CompareBudget, CompareError, CompareLimits},
};

fn compound(entries: &[(&str, Tag)]) -> Tag {
    let mut result = Compound::new();
    for (key, value) in entries {
        result.insert((*key).into(), value.clone()).unwrap();
    }
    Tag::Compound(result)
}

fn compare(expected: &Tag, actual: &Tag, partial_lists: bool) -> bool {
    CompareBudget::new(CompareLimits::default())
        .compare(Some(expected), Some(actual), partial_lists)
        .unwrap()
}

#[test]
fn null_types_and_partial_compound_contract() {
    let mut budget = CompareBudget::new(CompareLimits::default());
    assert!(budget.compare(None, None, true).unwrap());
    assert!(budget.compare(None, Some(&Tag::Int(1)), false).unwrap());
    assert!(!budget.compare(Some(&Tag::Int(1)), None, true).unwrap());
    assert!(!compare(&Tag::Int(1), &Tag::Byte(1), true));
    let expected = compound(&[("x", Tag::Int(1))]);
    let actual = compound(&[("x", Tag::Int(1)), ("y", Tag::Int(2))]);
    assert!(compare(&expected, &actual, true));
    assert!(compare(&expected, &actual, false));
    assert!(!compare(&actual, &expected, true));
    assert!(!budget.equal(&expected, &actual).unwrap());
    assert!(!compare(&expected, &compound(&[("z", Tag::Int(1))]), true));
}

#[test]
fn partial_lists_reuse_matches_but_preserve_length_guard() {
    let list = |values: &[i32]| Tag::List(values.iter().copied().map(Tag::Int).collect());
    assert!(compare(&list(&[1, 2]), &list(&[2, 1]), true));
    assert!(compare(&list(&[1, 1]), &list(&[1, 2]), true));
    assert!(!compare(&list(&[1, 1]), &list(&[1]), true));
    assert!(!compare(&list(&[]), &list(&[1]), true));
    assert!(compare(&list(&[]), &list(&[]), true));
    assert!(!compare(&list(&[1, 2]), &list(&[2, 1]), false));
    assert!(!compare(&list(&[1, 3]), &list(&[2, 1]), true));
    assert!(compare(
        &Tag::List(vec![compound(&[("x", Tag::Int(1))])]),
        &Tag::List(vec![
            Tag::Int(4),
            compound(&[("x", Tag::Int(1)), ("y", Tag::Int(2))])
        ]),
        true
    ));
}

#[test]
fn strict_list_requires_exact_descendant_compounds() {
    let expected = Tag::List(vec![compound(&[("x", Tag::Int(1))])]);
    let actual = Tag::List(vec![compound(&[("x", Tag::Int(1)), ("y", Tag::Int(2))])]);
    assert!(compare(&expected, &actual, true));
    assert!(!compare(&expected, &actual, false));
    let expected = compound(&[("a", expected)]);
    let actual = compound(&[("a", actual)]);
    assert!(compare(&expected, &actual, true));
    assert!(!compare(&expected, &actual, false));
}

#[test]
fn arrays_strings_nan_and_zero_use_exact_leaf_equality() {
    for partial in [false, true] {
        assert!(!compare(
            &Tag::ByteArray(vec![1]),
            &Tag::ByteArray(vec![1, 2]),
            partial
        ));
        assert!(!compare(
            &Tag::IntArray(vec![1]),
            &Tag::List(vec![Tag::Int(1)]),
            partial
        ));
        assert!(!compare(
            &Tag::LongArray(vec![1, 2]),
            &Tag::LongArray(vec![2, 1]),
            partial
        ));
        assert!(compare(
            &Tag::String(NbtString::from_utf16(vec![0xd800])),
            &Tag::String(NbtString::from_utf16(vec![0xd800])),
            partial
        ));
        assert!(compare(
            &Tag::Float(f32::NAN),
            &Tag::Float(f32::from_bits(0xff80_0001)),
            partial
        ));
        assert!(compare(
            &Tag::Double(f64::NAN),
            &Tag::Double(f64::from_bits(0xfff0_0000_0000_0001)),
            partial
        ));
        assert!(!compare(&Tag::Float(-0.0), &Tag::Float(0.0), partial));
        assert!(!compare(&Tag::Double(-0.0), &Tag::Double(0.0), partial));
    }
}

#[test]
fn work_limits_cover_arrays_strings_keys_and_all_attempts() {
    let mut budget = CompareBudget::new(CompareLimits {
        work_units: 3,
        ..CompareLimits::default()
    });
    budget.charge_work(1).unwrap();
    assert!(budget.equal(&Tag::Int(1), &Tag::Int(1)).unwrap());
    assert!(!budget.equal(&Tag::Int(1), &Tag::Int(2)).unwrap());
    assert_eq!(budget.work_remaining(), 0);
    assert_eq!(
        budget.equal(&Tag::Int(1), &Tag::Int(1)),
        Err(CompareError::WorkLimit)
    );
    for tag in [
        Tag::ByteArray(vec![1; 10]),
        Tag::IntArray(vec![1; 10]),
        Tag::LongArray(vec![1; 10]),
        Tag::String("abcdefghij".into()),
        compound(&[("abcdefghij", Tag::Int(1))]),
    ] {
        let copy = tag.clone();
        let mut budget = CompareBudget::new(CompareLimits {
            work_units: 5,
            ..CompareLimits::default()
        });
        assert_eq!(budget.equal(&tag, &copy), Err(CompareError::WorkLimit));
    }
    let expected = Tag::List(vec![Tag::Int(9), Tag::Int(9)]);
    let actual = Tag::List(vec![Tag::Int(1), Tag::Int(2), Tag::Int(9)]);
    let mut budget = CompareBudget::new(CompareLimits {
        work_units: 6,
        ..CompareLimits::default()
    });
    assert_eq!(
        budget.compare(Some(&expected), Some(&actual), true),
        Err(CompareError::WorkLimit)
    );
}

#[test]
fn scalar_work_does_not_allocate_a_dfs_stack() {
    let mut budget = CompareBudget::new(CompareLimits {
        stack_bytes: 0,
        max_depth: 0,
        ..CompareLimits::default()
    });
    assert!(budget.equal(&Tag::Int(1), &Tag::Int(1)).unwrap());
    assert!(
        budget
            .equal(&Tag::List(vec![]), &Tag::List(vec![]))
            .unwrap()
    );
    let a = Tag::List(vec![Tag::Int(1)]);
    let b = a.clone();
    assert_eq!(budget.equal(&a, &b), Err(CompareError::DepthLimit));
    let mut budget = CompareBudget::new(CompareLimits {
        stack_bytes: 0,
        ..CompareLimits::default()
    });
    assert_eq!(budget.equal(&a, &b), Err(CompareError::StackLimit));
}

#[test]
fn caller_allocation_budget_is_shared_and_checked_before_growth() {
    let left = Tag::List(vec![Tag::Int(1)]);
    let right = left.clone();
    let mut budget = CompareBudget::new(CompareLimits::default());
    let mut allocation_remaining = 0;
    assert!(
        budget
            .equal_accounted(&Tag::Int(1), &Tag::Int(1), &mut allocation_remaining)
            .unwrap()
    );
    assert_eq!(
        budget.equal_accounted(&left, &right, &mut allocation_remaining),
        Err(CompareError::AllocationLimit)
    );
    assert_eq!(allocation_remaining, 0);

    allocation_remaining = 10_000;
    assert!(
        budget
            .equal_accounted(&left, &right, &mut allocation_remaining)
            .unwrap()
    );
    let first_charge = 10_000 - allocation_remaining;
    assert!(first_charge > 0);
    assert!(
        budget
            .compare_accounted(Some(&left), Some(&right), true, &mut allocation_remaining)
            .unwrap()
    );
    assert_eq!(10_000 - allocation_remaining, first_charge * 2);

    let deep_left = Tag::List(vec![Tag::List(vec![Tag::List(vec![Tag::List(vec![
        left,
    ])])])]);
    let deep_right = deep_left.clone();
    allocation_remaining = first_charge;
    assert_eq!(
        budget.equal_accounted(&deep_left, &deep_right, &mut allocation_remaining),
        Err(CompareError::AllocationLimit)
    );
    assert_eq!(allocation_remaining, 0);
}

#[test]
fn deep_trees_compare_on_a_small_thread_stack() {
    std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            let wrap = || {
                let mut value = Tag::Int(1);
                for _ in 0..2000 {
                    value = Tag::List(vec![value]);
                }
                value
            };
            let mut expected = wrap();
            let mut actual = wrap();
            let mut budget = CompareBudget::new(CompareLimits::default());
            assert!(budget.equal(&expected, &actual).unwrap());
            let mut budget = CompareBudget::new(CompareLimits {
                max_depth: 1999,
                ..CompareLimits::default()
            });
            assert_eq!(
                budget.equal(&expected, &actual),
                Err(CompareError::DepthLimit)
            );
            // Tag's general Drop is recursive; this test isolates the comparator's
            // stack safety and dismantles the independently built trees iteratively.
            while let Tag::List(mut list) = expected {
                expected = list.pop().unwrap();
            }
            while let Tag::List(mut list) = actual {
                actual = list.pop().unwrap();
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
