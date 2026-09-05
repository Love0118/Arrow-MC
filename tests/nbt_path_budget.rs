use arrow_mc::nbt::path::{self, ErrorKind, Limits, Path, Selection};
use arrow_mc::nbt::{Compound, NbtString, Tag};

fn parsed(input: &str) -> Path {
    Path::parse(input, Limits::default()).unwrap().0
}

#[test]
fn readonly_selection_borrows_a_large_payload_under_a_small_copy_budget() {
    let payload = Tag::String(NbtString::from_utf16(vec![65; 1024 * 1024]));
    let mut root = Compound::new();
    root.insert("payload".into(), payload).unwrap();
    let root = Tag::Compound(root);
    let selected = parsed("payload")
        .get(
            &root,
            Limits {
                allocation_bytes: 1024,
                ..Limits::default()
            },
        )
        .unwrap();
    let Tag::Compound(compound) = &root else {
        panic!()
    };
    let Selection::Borrowed(actual) = selected[0] else {
        panic!("read path unexpectedly copied value")
    };
    assert!(std::ptr::eq(
        actual,
        compound.get(&"payload".into()).unwrap()
    ));
}

#[test]
fn caller_factory_runs_before_returned_spare_capacity_is_admitted() {
    let mut root = Tag::Compound(Compound::new());
    let mut calls = 0;
    let mut factory = || {
        calls += 1;
        let mut payload = Vec::with_capacity(4096);
        payload.push(65);
        Tag::String(NbtString::from_utf16(payload))
    };
    let failure = parsed("payload")
        .get_or_create(
            &mut root,
            &mut factory,
            Limits {
                allocation_bytes: 1024,
                ..Limits::default()
            },
        )
        .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::AllocationBudget);
    assert_eq!(calls, 1);
    assert_eq!(root, Tag::Compound(Compound::new()));
}

#[test]
fn caller_factory_compound_keys_count_retained_capacity_too() {
    let mut root = Tag::Compound(Compound::new());
    let mut factory = || {
        let mut key = Vec::with_capacity(4096);
        key.push(65);
        let mut compound = Compound::new();
        compound
            .insert(NbtString::from_utf16(key), Tag::Int(1))
            .unwrap();
        Tag::Compound(compound)
    };
    assert_eq!(
        parsed("payload")
            .get_or_create(
                &mut root,
                &mut factory,
                Limits {
                    allocation_bytes: 1024,
                    ..Limits::default()
                }
            )
            .unwrap_err()
            .kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(root, Tag::Compound(Compound::new()));
}

#[test]
fn source_copy_admission_precedes_parent_creation() {
    let mut root = Tag::Compound(Compound::new());
    let source = Tag::ByteArray(vec![1; 8192]);
    let limits = Limits {
        allocation_bytes: 1024,
        ..Limits::default()
    };
    assert_eq!(
        parsed("created.value")
            .set(&mut root, &source, limits)
            .unwrap_err()
            .kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(root, Tag::Compound(Compound::new()));
    assert_eq!(
        parsed("created")
            .insert(&mut root, 0, &[source], limits)
            .unwrap_err()
            .kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(root, Tag::Compound(Compound::new()));
}

#[test]
fn wide_child_expansion_is_charged_before_its_scratch_allocation() {
    let source = Tag::List(vec![Tag::Int(1); 4096]);
    let failure = path::is_too_deep(
        &source,
        0,
        Limits {
            work_units: 1,
            allocation_bytes: 1024,
            ..Limits::default()
        },
    )
    .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::WorkLimit);
}

#[test]
fn predicate_scratch_and_path_selection_share_allocation_admission() {
    let root = Tag::List(vec![Tag::Compound(Compound::new()); 8]);
    let path = parsed("[{}]");
    assert_eq!(
        path.get(
            &root,
            Limits {
                allocation_bytes: 0,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::AllocationBudget
    );
    assert_eq!(
        path.get(
            &root,
            Limits {
                candidates: 2,
                ..Limits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::CandidateLimit
    );
    assert_eq!(root, Tag::List(vec![Tag::Compound(Compound::new()); 8]));
}

#[test]
fn depth_contract_uses_start_depth_and_ignores_array_elements() {
    let limits = Limits::default();
    assert!(!path::is_too_deep(&Tag::Int(1), 511, limits).unwrap());
    assert!(!path::is_too_deep(&Tag::List(Vec::new()), 511, limits).unwrap());
    assert!(!path::is_too_deep(&Tag::ByteArray(vec![1]), 511, limits).unwrap());
    assert!(path::is_too_deep(&Tag::List(vec![Tag::Int(1)]), 511, limits).unwrap());
    assert!(path::is_too_deep(&Tag::End, 512, limits).unwrap());
}

fn deep_list(depth: usize) -> Tag {
    let mut value = Tag::End;
    for _ in 0..depth {
        value = Tag::List(vec![value]);
    }
    value
}

fn deep_compound(depth: usize) -> Tag {
    let mut value = Tag::End;
    for _ in 0..depth {
        let mut compound = Compound::new();
        compound.insert("child".into(), value).unwrap();
        value = Tag::Compound(compound);
    }
    value
}

#[test]
fn rejected_deep_factory_values_are_released_without_recursive_drop() {
    for value in [deep_list(20_000), deep_compound(20_000)] {
        let mut supplied = Some(value);
        let mut root = Tag::Compound(Compound::new());
        let failure = parsed("a")
            .get_or_create(
                &mut root,
                &mut || supplied.take().unwrap(),
                Limits {
                    allocation_bytes: 4096,
                    ..Limits::default()
                },
            )
            .unwrap_err();
        assert_eq!(failure.kind, ErrorKind::AllocationBudget);
        assert_eq!(root, Tag::Compound(Compound::new()));
    }
}

#[test]
fn iterative_disposal_handles_branching_continuations_and_original_end_values() {
    let mut value = Tag::End;
    for _ in 0..20_000 {
        let mut compound = Compound::new();
        compound.insert("".into(), Tag::End).unwrap();
        compound
            .insert("array".into(), Tag::LongArray(vec![i64::MIN, i64::MAX]))
            .unwrap();
        compound.insert("child".into(), value).unwrap();
        value = Tag::List(vec![
            Tag::String("retained leaf".into()),
            Tag::Compound(compound),
            Tag::ByteArray(vec![1, 2, 3]),
            Tag::End,
        ]);
    }
    value.drop_iterative();
}

#[test]
fn replacements_and_removals_release_deep_owned_subtrees_without_recursion() {
    let path = parsed("a");
    for remove in [false, true] {
        let mut compound = Compound::new();
        compound.insert("a".into(), deep_list(20_000)).unwrap();
        let mut root = Tag::Compound(compound);
        if remove {
            assert_eq!(path.remove(&mut root, Limits::default()).unwrap(), 1);
        } else {
            assert_eq!(
                path.set(&mut root, &Tag::Int(1), Limits::default())
                    .unwrap(),
                1
            );
        }
        root.drop_iterative();
    }
}

#[test]
fn copied_sources_and_error_arguments_release_deep_owned_values_on_failure() {
    let path = parsed("a");
    let mut root = Tag::Compound(Compound::new());
    let source = deep_list(20_000);
    assert_eq!(
        path.insert(
            &mut root,
            0,
            std::slice::from_ref(&source),
            Limits::default()
        )
        .unwrap_err()
        .kind,
        ErrorKind::TooDeep
    );
    source.drop_iterative();
    assert_eq!(root, Tag::Compound(Compound::new()));

    let mut compound = Compound::new();
    compound.insert("a".into(), deep_compound(20_000)).unwrap();
    let mut root = Tag::Compound(compound);
    let failure = path
        .insert(&mut root, 0, &[], Limits::default())
        .unwrap_err();
    assert_eq!(failure.kind, ErrorKind::ExpectedList);
    drop(failure);
    root.drop_iterative();
}

#[test]
fn allocation_failure_after_a_completed_deep_child_releases_partial_copy() {
    let source = Tag::List(vec![
        deep_list(20_000),
        Tag::ByteArray(vec![1; 16 * 1024 * 1024]),
    ]);
    let mut root = Tag::Compound(Compound::new());
    let failure = parsed("a")
        .insert(
            &mut root,
            0,
            std::slice::from_ref(&source),
            Limits {
                allocation_bytes: 8 * 1024 * 1024,
                work_units: usize::MAX,
                ..Limits::default()
            },
        )
        .unwrap_err();
    source.drop_iterative();
    assert_eq!(failure.kind, ErrorKind::AllocationBudget);
    assert_eq!(root, Tag::Compound(Compound::new()));
}
