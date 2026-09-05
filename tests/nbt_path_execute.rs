use arrow_mc::nbt::path::{ErrorKind, Limits, Path, Selection, SelectionMut};
use arrow_mc::nbt::{Compound, CompoundEntry, NbtString, Tag};
use arrow_mc::snbt;

fn read(input: &str) -> Tag {
    snbt::parse(input, snbt::Limits::default()).unwrap()
}

fn path(input: &str) -> Path {
    let (path, consumed) = Path::parse(input, Limits::default()).unwrap();
    assert_eq!(consumed, input.encode_utf16().count());
    path
}

fn selected(path: &Path, root: &Tag) -> Vec<Tag> {
    path.get(root, Limits::default())
        .unwrap()
        .iter()
        .map(|selection| selection.as_tag().clone())
        .collect()
}

fn deep_cleanup_tree(depth: usize, compound_root: bool) -> (Tag, usize) {
    let mut value = Tag::Int(1);
    let mut retained_bytes = 0;
    for level in (0..depth).rev() {
        if (level % 2 == 0) == compound_root {
            let name = vec![u16::from(b'x')];
            retained_bytes += name.capacity() * size_of::<u16>();
            let entries = vec![CompoundEntry::new(NbtString::from_utf16(name), value)];
            retained_bytes += entries.capacity() * size_of::<CompoundEntry>();
            value = Tag::Compound(Compound::from_entries(entries).unwrap());
        } else {
            let values = vec![value];
            retained_bytes += values.capacity() * size_of::<Tag>();
            value = Tag::List(values);
        }
    }
    (value, retained_bytes)
}

// Caller-owned inputs are dismantled independently of executor cleanup. This
// helper prevents their ordinary recursive drop from masking a library failure.
fn discard_cleanup_tree(value: Tag) {
    let mut pending = vec![value];
    while let Some(value) = pending.pop() {
        match value {
            Tag::List(values) => pending.extend(values),
            Tag::Compound(mut compound) => {
                while let Some(entry) = compound.entries().first() {
                    let name = entry.name.clone();
                    pending.push(compound.remove(&name).unwrap());
                }
            }
            _ => {}
        }
    }
}

fn deep_cleanup_root(query: &str) -> Tag {
    if query.starts_with('a') {
        Tag::Compound(
            Compound::from_entries(vec![CompoundEntry::new(
                NbtString::from("a"),
                deep_cleanup_tree(20_000, true).0,
            )])
            .unwrap(),
        )
    } else {
        let count = if query == "[]" { 2 } else { 1 };
        Tag::List(
            (0..count)
                .map(|_| deep_cleanup_tree(20_000, true).0)
                .collect(),
        )
    }
}

#[test]
fn get_flattens_each_stage_in_collection_order_and_skips_nonmatches() {
    let root = read("{a:[{b:[3,1]}, {}, 9, {b:[2]}, {b:[]}, {b:[4,5]}]}");
    let query = path("a[].b[]");
    assert_eq!(
        selected(&query, &root),
        vec![
            Tag::Int(3),
            Tag::Int(1),
            Tag::Int(2),
            Tag::Int(4),
            Tag::Int(5)
        ]
    );
    assert_eq!(query.count_matching(&root, Limits::default()).unwrap(), 5);
}

#[test]
fn only_an_empty_whole_stage_is_a_get_error() {
    let root = read("{a:[{}, {b:2}, 1]}");
    assert_eq!(selected(&path("a[].b"), &root), vec![Tag::Int(2)]);
    for query in ["absent", "a[].c", "a[99]", "a[-4]", "a[].b.c"] {
        let query = path(query);
        assert_eq!(
            query.get(&root, Limits::default()).unwrap_err().kind,
            ErrorKind::NothingFound
        );
        assert_eq!(query.count_matching(&root, Limits::default()).unwrap(), 0);
    }
}

#[test]
fn root_pattern_filters_without_replacing_or_removing_the_root() {
    let mut root = read("{a:1,b:2}");
    let expected = root.clone();
    let query = path("{a:1}");
    assert_eq!(selected(&query, &root), vec![expected.clone()]);
    assert_eq!(
        query
            .set(&mut root, &Tag::Int(9), Limits::default())
            .unwrap(),
        0
    );
    assert_eq!(query.remove(&mut root, Limits::default()).unwrap(), 0);
    assert_eq!(root, expected);
    assert_eq!(
        path("{a:2}")
            .count_matching(&root, Limits::default())
            .unwrap(),
        0
    );
}

#[test]
fn negative_indices_use_the_current_collection_size() {
    for root in [read("[10,20,30]"), read("[I;10,20,30]")] {
        assert_eq!(selected(&path("[-1]"), &root), vec![Tag::Int(30)]);
        assert_eq!(selected(&path("[-3]"), &root), vec![Tag::Int(10)]);
        assert_eq!(
            path("[-4]")
                .count_matching(&root, Limits::default())
                .unwrap(),
            0
        );
    }
}

#[test]
fn ordinary_get_returns_borrowed_tags_and_array_get_returns_detached_numbers() {
    let root = read("{a:[{x:1}],b:[B;2,3]}");
    let query = path("a[0]");
    let result = query.get(&root, Limits::default()).unwrap();
    assert!(matches!(&result[0], Selection::Borrowed(_)));
    let query = path("b[]");
    let result = query.get(&root, Limits::default()).unwrap();
    assert!(matches!(&result[0], Selection::Detached(Tag::Byte(2))));
    assert!(matches!(&result[1], Selection::Detached(Tag::Byte(3))));
}

#[test]
fn matching_element_uses_partial_compounds_and_partial_list_semantics() {
    let root =
        read("{a:[{x:[1,2],id:0},{x:[2,1],id:1},{x:[1],id:2},{x:[1,1],id:3},{x:[I;1,1],id:4}]}");
    assert_eq!(
        selected(&path("a[{x:[1,1]}].id"), &root),
        vec![Tag::Int(0), Tag::Int(1), Tag::Int(3)]
    );
    assert_eq!(
        path("a[{x:[]}]")
            .count_matching(&root, Limits::default())
            .unwrap(),
        0
    );
}

#[test]
fn create_selects_existing_values_without_calling_the_supplier() {
    let mut root = read("{a:[1,2,3]}");
    let mut calls = 0;
    let query = path("a[]");
    let result = query
        .get_or_create(
            &mut root,
            &mut || {
                calls += 1;
                Tag::Int(9)
            },
            Limits::default(),
        )
        .unwrap();
    assert_eq!(
        result
            .iter()
            .map(|value| value.as_tag().clone())
            .collect::<Vec<_>>(),
        vec![Tag::Int(1), Tag::Int(2), Tag::Int(3)]
    );
    assert_eq!(calls, 0);
}

#[test]
fn create_builds_parents_from_the_next_node_and_supplies_each_missing_leaf() {
    for (query, expected) in [
        ("a.b", "{a:{b:3}}"),
        ("a[].b", "{a:[{b:3}]}"),
        ("a[{x:1}].b", "{a:[{x:1,b:3}]}"),
        ("a{x:1}.b", "{a:{x:1,b:3}}"),
    ] {
        let mut root = read("{}");
        let mut calls = 0;
        let query = path(query);
        let result = query
            .get_or_create(
                &mut root,
                &mut || {
                    calls += 1;
                    Tag::Int(3)
                },
                Limits::default(),
            )
            .unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(calls, 1);
        assert_eq!(root, read(expected));
    }
    let mut root = read("{a:[{}, {}, {b:7}]}");
    let mut calls = 0;
    path("a[].b")
        .get_or_create(
            &mut root,
            &mut || {
                calls += 1;
                Tag::Int(calls)
            },
            Limits::default(),
        )
        .unwrap();
    assert_eq!(calls, 2);
    assert_eq!(root, read("{a:[{b:1},{b:2},{b:7}]}"));
}

#[test]
fn create_matching_nodes_copy_the_pattern_without_using_the_supplier() {
    for (query, expected) in [("a{x:1}", "{a:{x:1}}"), ("a[{x:1}]", "{a:[{x:1}]}")] {
        let mut root = read("{}");
        let mut calls = 0;
        path(query)
            .get_or_create(
                &mut root,
                &mut || {
                    calls += 1;
                    Tag::Int(9)
                },
                Limits::default(),
            )
            .unwrap();
        assert_eq!(calls, 0);
        assert_eq!(root, read(expected));
    }
}

#[test]
fn created_pattern_values_are_independent_of_later_path_executions() {
    let query = path("a{x:[1]}");
    let mut first = read("{}");
    let mut selections = query
        .get_or_create(
            &mut first,
            &mut || panic!("pattern supplies its own value"),
            Limits::default(),
        )
        .unwrap();
    *selections[0].as_tag_mut() = read("{x:[9]}");
    drop(selections);

    let mut second = read("{}");
    query
        .get_or_create(
            &mut second,
            &mut || panic!("pattern supplies its own value"),
            Limits::default(),
        )
        .unwrap();
    assert_eq!(first, read("{a:{x:[9]}}"));
    assert_eq!(second, read("{a:{x:[1]}}"));
}

#[test]
fn create_never_grows_an_index_and_keeps_previously_created_parents_on_error() {
    let mut root = read("{}");
    let mut calls = 0;
    assert!(
        path("a[0].b")
            .get_or_create(
                &mut root,
                &mut || {
                    calls += 1;
                    Tag::Int(9)
                },
                Limits::default()
            )
            .is_err()
    );
    assert_eq!(root, read("{a:[]}"));
    assert_eq!(calls, 0);
    let query = path("a[0]");
    let result = query
        .get_or_create(
            &mut root,
            &mut || {
                calls += 1;
                Tag::Int(9)
            },
            Limits::default(),
        )
        .unwrap();
    assert!(result.is_empty());
    assert_eq!(calls, 0);
}

#[test]
fn create_wrong_parent_type_is_not_replaced_and_final_misses_may_be_empty() {
    let mut root = read("{a:1}");
    let query = path("a.b");
    assert!(
        query
            .get_or_create(
                &mut root,
                &mut || panic!("wrong parent must not request a value"),
                Limits::default()
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        path("a.b.c")
            .get_or_create(&mut root, &mut || Tag::Int(3), Limits::default())
            .is_err()
    );
    assert_eq!(root, read("{a:1}"));
}

#[test]
fn create_returns_live_mutable_references_for_stored_values() {
    let mut root = read("{a:[{},{}]}");
    let query = path("a[].b");
    let mut selected = query
        .get_or_create(&mut root, &mut || read("{x:1}"), Limits::default())
        .unwrap();
    assert_eq!(selected.len(), 2);
    assert!(matches!(&selected[0], SelectionMut::Borrowed(_)));
    *selected[0].as_tag_mut() = read("{x:8}");
    *selected[1].as_tag_mut() = read("{x:9}");
    drop(selected);
    assert_eq!(root, read("{a:[{b:{x:8}},{b:{x:9}}]}"));
}

#[test]
fn empty_array_create_returns_the_supplied_tag_before_numeric_coercion() {
    let mut root = read("{a:[B;]}");
    let query = path("a[]");
    let mut selected = query
        .get_or_create(&mut root, &mut || Tag::Double(2.7), Limits::default())
        .unwrap();
    assert_eq!(selected.len(), 1);
    assert!(matches!(&selected[0], SelectionMut::Detached(Tag::Double(value)) if *value == 2.7));
    *selected[0].as_tag_mut() = Tag::Int(90);
    drop(selected);
    assert_eq!(root, read("{a:[B;2]}"));
}

#[test]
fn existing_array_create_results_are_detached_from_array_slots() {
    let mut root = read("{a:[I;4,5]}");
    let query = path("a[-1]");
    let mut result = query
        .get_or_create(
            &mut root,
            &mut || panic!("existing array slot"),
            Limits::default(),
        )
        .unwrap();
    assert!(matches!(&result[0], SelectionMut::Detached(Tag::Int(5))));
    *result[0].as_tag_mut() = Tag::Int(99);
    drop(result);
    assert_eq!(root, read("{a:[I;4,5]}"));
}

#[test]
fn an_empty_array_rejecting_create_still_calls_the_supplier_once() {
    let mut root = read("{a:[B;]}");
    let mut calls = 0;
    let query = path("a[]");
    let result = query
        .get_or_create(
            &mut root,
            &mut || {
                calls += 1;
                read("\"x\"")
            },
            Limits::default(),
        )
        .unwrap();
    assert!(result.is_empty());
    assert_eq!(calls, 1);
    assert_eq!(root, read("{a:[B;]}"));
}

#[test]
fn set_matching_named_missing_is_a_noop_but_empty_matching_list_gets_replacement() {
    let mut named = read("{}");
    assert_eq!(
        path("a{x:1}")
            .set(&mut named, &Tag::Int(9), Limits::default())
            .unwrap(),
        0
    );
    assert_eq!(named, read("{}"));
    let mut list = read("{}");
    assert_eq!(
        path("a[{x:1}]")
            .set(&mut list, &Tag::Int(9), Limits::default())
            .unwrap(),
        1
    );
    assert_eq!(list, read("{a:[9]}"));
}

#[test]
fn set_array_count_is_not_a_count_of_final_changed_bytes() {
    for (initial, query, replacement, count, expected) in [
        ("{a:[B;1]}", "a[0]", "257", 1, "{a:[B;1]}"),
        ("{a:[B;1,1]}", "a[]", "1", 2, "{a:[B;1,1]}"),
        ("{a:[I;1,1]}", "a[]", "1", 0, "{a:[I;1,1]}"),
        ("{a:[B;1,2]}", "a[]", "\"x\"", 0, "{a:[B;]}"),
        ("{a:[B;]}", "a[]", "\"x\"", 1, "{a:[B;]}"),
    ] {
        let mut root = read(initial);
        assert_eq!(
            path(query)
                .set(&mut root, &read(replacement), Limits::default())
                .unwrap(),
            count,
            "{initial} {query} {replacement}"
        );
        assert_eq!(root, read(expected));
    }
}

#[test]
fn set_array_numeric_conversion_preserves_float_and_double_differences() {
    for (initial, replacement, expected) in [
        ("{a:[B;0]}", "-1.5f", "{a:[B;-2]}"),
        ("{a:[I;0]}", "-1.5d", "{a:[I;-2]}"),
        ("{a:[L;0]}", "-1.5f", "{a:[L;-1]}"),
        ("{a:[L;0]}", "-1.5d", "{a:[L;-2]}"),
        ("{a:[B;0]}", "258", "{a:[B;2]}"),
    ] {
        let mut root = read(initial);
        assert_eq!(
            path("a[0]")
                .set(&mut root, &read(replacement), Limits::default())
                .unwrap(),
            1
        );
        assert_eq!(root, read(expected), "{initial} {replacement}");
    }
}

#[test]
fn set_deep_copies_each_replacement_and_preserves_the_source() {
    let source = read("{x:[1]}");
    let mut root = read("{a:[{},{}]}");
    assert_eq!(
        path("a[].b")
            .set(&mut root, &source, Limits::default())
            .unwrap(),
        2
    );
    let query = path("a[0].b.x");
    let mut result = query
        .get_or_create(
            &mut root,
            &mut || panic!("existing field"),
            Limits::default(),
        )
        .unwrap();
    *result[0].as_tag_mut() = read("[7,8]");
    drop(result);
    assert_eq!(root, read("{a:[{b:{x:[7,8]}},{b:{x:[1]}}]}"));
    assert_eq!(source, read("{x:[1]}"));
}

#[test]
fn insert_preserves_source_order_and_counts_target_collections() {
    let mut root = read("{a:[[1,2],[3]]}");
    assert_eq!(
        path("a[]")
            .insert(&mut root, 0, &[Tag::Int(7), Tag::Int(8)], Limits::default())
            .unwrap(),
        2
    );
    assert_eq!(root, read("{a:[[7,8,1,2],[7,8,3]]}"));
}

#[test]
fn negative_insert_indices_are_one_past_get_indices() {
    for (index, expected) in [
        (-3, "{a:[7,1,2]}"),
        (-2, "{a:[1,7,2]}"),
        (-1, "{a:[1,2,7]}"),
    ] {
        let mut root = read("{a:[1,2]}");
        assert_eq!(
            path("a")
                .insert(&mut root, index, &[Tag::Int(7)], Limits::default())
                .unwrap(),
            1
        );
        assert_eq!(root, read(expected));
    }
    for index in [-4, 3] {
        let mut root = read("{a:[1,2]}");
        assert!(
            path("a")
                .insert(&mut root, index, &[Tag::Int(7)], Limits::default())
                .is_err()
        );
        assert_eq!(root, read("{a:[1,2]}"));
    }
}

#[test]
fn array_insert_skips_rejected_values_without_advancing_the_index() {
    let mut root = read("{a:[B;1]}");
    assert_eq!(
        path("a")
            .insert(
                &mut root,
                1,
                &[read("\"x\""), Tag::Int(258), Tag::Double(-1.5)],
                Limits::default()
            )
            .unwrap(),
        1
    );
    assert_eq!(root, read("{a:[B;1,2,-2]}"));
}

#[test]
fn insert_defers_index_validation_until_an_element_is_accepted() {
    let mut root = read("{a:[B;1]}");
    let query = path("a");
    assert_eq!(
        query
            .insert(&mut root, 900, &[read("\"x\"")], Limits::default())
            .unwrap(),
        0
    );
    assert_eq!(
        query
            .insert(&mut root, 900, &[], Limits::default())
            .unwrap(),
        0
    );
    assert!(
        query
            .insert(&mut root, 900, &[Tag::Int(2)], Limits::default())
            .is_err()
    );
    assert_eq!(root, read("{a:[B;1]}"));
}

#[test]
fn empty_insert_creates_targets_and_still_checks_collection_types() {
    let mut root = read("{}");
    assert_eq!(
        path("a")
            .insert(&mut root, 900, &[], Limits::default())
            .unwrap(),
        0
    );
    assert_eq!(root, read("{a:[]}"));
    let mut scalar = read("{a:1}");
    assert!(
        path("a")
            .insert(&mut scalar, 900, &[], Limits::default())
            .is_err()
    );
    assert_eq!(scalar, read("{a:1}"));
}

#[test]
fn insert_keeps_earlier_target_changes_when_a_later_target_fails() {
    for (initial, index, expected) in [
        ("{a:[[1],2,[3]]}", 0, "{a:[[7,1],2,[3]]}"),
        ("{a:[[1,2],[],[3]]}", 2, "{a:[[1,2,7],[],[3]]}"),
    ] {
        let mut root = read(initial);
        assert!(
            path("a[]")
                .insert(&mut root, index, &[Tag::Int(7)], Limits::default())
                .is_err()
        );
        assert_eq!(root, read(expected));
    }
}

#[test]
fn failed_insert_keeps_a_pattern_parent_created_before_type_validation() {
    let mut root = read("{}");
    assert!(
        path("a[{x:1}]")
            .insert(&mut root, 0, &[Tag::Int(7)], Limits::default())
            .is_err()
    );
    assert_eq!(root, read("{a:[{x:1}]}"));
}

#[test]
fn insert_copies_sources_for_every_target_collection() {
    let source = read("{x:[1]}");
    let mut root = read("{a:[[],[]]}");
    assert_eq!(
        path("a[]")
            .insert(
                &mut root,
                0,
                std::slice::from_ref(&source),
                Limits::default()
            )
            .unwrap(),
        2
    );
    assert_eq!(
        path("a[0][0].x")
            .set(&mut root, &read("[9]"), Limits::default())
            .unwrap(),
        1
    );
    assert_eq!(root, read("{a:[[{x:[9]}],[{x:[1]}]]}"));
    assert_eq!(source, read("{x:[1]}"));
}

#[test]
fn remove_does_not_create_missing_parents_or_replace_wrong_types() {
    for initial in ["{}", "{a:1}", "{a:[]}"] {
        let mut root = read(initial);
        assert_eq!(
            path("a[].b").remove(&mut root, Limits::default()).unwrap(),
            0
        );
        assert_eq!(root, read(initial));
    }
}

#[test]
fn remove_matching_elements_does_not_skip_adjacent_matches() {
    let mut root = read("{a:[{x:1,id:0},{x:1,id:1},{x:2,id:2},{x:1,id:3}]}");
    assert_eq!(
        path("a[{x:1}]")
            .remove(&mut root, Limits::default())
            .unwrap(),
        3
    );
    assert_eq!(root, read("{a:[{x:2,id:2}]}"));
}

#[test]
fn remove_counts_removed_elements_and_preserves_collection_type() {
    for (initial, query, count, expected) in [
        ("{a:[B;1,2,3]}", "a[-1]", 1, "{a:[B;1,2]}"),
        ("{a:[I;1,2,3]}", "a[]", 3, "{a:[I;]}"),
        ("{a:[1,2,3]}", "a[]", 3, "{a:[]}"),
        ("{a:{x:1},b:2}", "a{x:1}", 1, "{b:2}"),
        ("{a:{x:2},b:2}", "a{x:1}", 0, "{a:{x:2},b:2}"),
    ] {
        let mut root = read(initial);
        assert_eq!(
            path(query).remove(&mut root, Limits::default()).unwrap(),
            count
        );
        assert_eq!(root, read(expected));
    }
}

#[test]
fn set_and_insert_reject_source_depth_before_creating_any_parents() {
    let text = std::iter::repeat_n("a", 512).collect::<Vec<_>>().join(".");
    let query = path(&text);
    let mut root = Tag::Compound(Compound::new());
    assert_eq!(
        query
            .set(&mut root, &Tag::Int(1), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TooDeep
    );
    assert_eq!(root, read("{}"));
    assert_eq!(
        query
            .insert(&mut root, 0, &[Tag::Int(1)], Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TooDeep
    );
    assert_eq!(root, read("{}"));

    let text = std::iter::repeat_n("a", 511).collect::<Vec<_>>().join(".");
    let query = path(&text);
    assert_eq!(
        query
            .set(&mut root, &read("[1]"), Limits::default())
            .unwrap_err()
            .kind,
        ErrorKind::TooDeep
    );
    assert_eq!(root, read("{}"));
    assert_eq!(
        query
            .set(&mut root, &Tag::Int(1), Limits::default())
            .unwrap(),
        1
    );
    assert_eq!(selected(&query, &root), vec![Tag::Int(1)]);
}

#[test]
fn create_has_no_vanilla_set_depth_gate_and_read_remove_can_reach_its_result() {
    let text = std::iter::repeat_n("a", 513).collect::<Vec<_>>().join(".");
    let query = path(&text);
    let mut root = Tag::Compound(Compound::new());
    assert_eq!(
        query
            .get_or_create(&mut root, &mut || Tag::Int(4), Limits::default())
            .unwrap()
            .len(),
        1
    );
    assert_eq!(selected(&query, &root), vec![Tag::Int(4)]);
    assert_eq!(query.count_matching(&root, Limits::default()).unwrap(), 1);
    assert_eq!(query.remove(&mut root, Limits::default()).unwrap(), 1);
    assert_eq!(query.count_matching(&root, Limits::default()).unwrap(), 0);
}

#[test]
fn default_node_limit_deep_create_mutate_read_remove_and_drop_fit_the_test_stack() {
    let limits = Limits::default();
    assert_eq!(limits.node_count, 4096);
    let text = std::iter::repeat_n("a", limits.node_count)
        .collect::<Vec<_>>()
        .join(".");
    let query = path(&text);
    let mut root = Tag::Compound(Compound::new());
    let mut selections = query
        .get_or_create(&mut root, &mut || Tag::Int(4), limits)
        .unwrap();
    assert_eq!(selections.len(), 1);
    *selections[0].as_tag_mut() = Tag::Int(7);
    drop(selections);
    assert_eq!(selected(&query, &root), vec![Tag::Int(7)]);
    assert_eq!(query.count_matching(&root, limits).unwrap(), 1);
    assert_eq!(query.remove(&mut root, limits).unwrap(), 1);
    assert_eq!(query.count_matching(&root, limits).unwrap(), 0);
    // Normal value destruction must not require a special cleanup helper.
    drop(root);
}

#[test]
fn insert_source_depth_gate_does_not_count_the_destination_list_as_an_extra_node() {
    let text = std::iter::repeat_n("a", 511).collect::<Vec<_>>().join(".");
    let query = path(&text);
    let mut root = Tag::Compound(Compound::new());
    assert_eq!(
        query
            .insert(&mut root, 0, &[Tag::Int(1)], Limits::default())
            .unwrap(),
        1
    );
    assert_eq!(selected(&query, &root), vec![read("[1]")]);
    assert_eq!(selected(&path(&(text + "[0]")), &root), vec![Tag::Int(1)]);
}

#[test]
fn nothing_found_uses_the_last_wildcard_end_but_excludes_a_trailing_dot() {
    let root = read("{a:[]}");
    for (text, prefix) in [("a[][]", "a[][]"), ("a[].b[]", "a[].b[]"), ("b.", "b")] {
        let query = path(text);
        let error = query.get(&root, Limits::default()).unwrap_err();
        assert_eq!(error.kind, ErrorKind::NothingFound);
        assert_eq!(
            error.translation_key(),
            Some("arguments.nbtpath.nothing_found")
        );
        assert_eq!(error.cursor, None);
        let mut argument = Vec::new();
        assert!(
            error
                .write_argument(query.as_string().as_utf16(), &mut argument, 1024)
                .unwrap()
        );
        assert_eq!(String::from_utf16(&argument).unwrap(), prefix);
    }
}

#[test]
fn selection_work_and_allocation_limits_are_real_admission_boundaries() {
    let root = read("{a:[1,2,3,4]}");
    let query = path("a[]");
    for (limits, expected) in [
        (
            Limits {
                candidates: 1,
                ..Limits::default()
            },
            ErrorKind::CandidateLimit,
        ),
        (
            Limits {
                work_units: 0,
                ..Limits::default()
            },
            ErrorKind::WorkLimit,
        ),
        (
            Limits {
                allocation_bytes: 0,
                ..Limits::default()
            },
            ErrorKind::AllocationBudget,
        ),
    ] {
        assert_eq!(query.get(&root, limits).unwrap_err().kind, expected);
    }
    assert_eq!(
        selected(&query, &root),
        vec![Tag::Int(1), Tag::Int(2), Tag::Int(3), Tag::Int(4)]
    );
}

#[test]
fn compound_lookup_charges_repeated_long_common_prefix_comparisons() {
    let prefix = "p".repeat(2048);
    let entries = (0..256)
        .map(|index| {
            CompoundEntry::new(
                NbtString::from(format!("{prefix}{index:03}").as_str()),
                Tag::Int(index),
            )
        })
        .collect();
    let root = Tag::Compound(Compound::from_entries(entries).unwrap());
    let query = path(&format!("{prefix}009"));
    let limits = Limits {
        // Enough for one full comparison, but not a binary search over all keys.
        work_units: 4096 + 32,
        ..Limits::default()
    };
    assert_eq!(
        query.get(&root, limits).unwrap_err().kind,
        ErrorKind::WorkLimit
    );
    assert_eq!(selected(&query, &root), vec![Tag::Int(9)]);
}

#[test]
fn runtime_end_is_a_list_value_but_not_a_numeric_array_element() {
    let query = path("[]");
    let mut list = Tag::List(Vec::new());
    assert_eq!(
        query.set(&mut list, &Tag::End, Limits::default()).unwrap(),
        1
    );
    assert_eq!(list, Tag::List(vec![Tag::End]));
    assert_eq!(selected(&query, &list), vec![Tag::End]);
    assert_eq!(path("").get(&list, Limits::default()).unwrap().len(), 1);

    let mut root = read("{a:[]}");
    assert_eq!(
        path("a")
            .insert(&mut root, 0, &[Tag::End], Limits::default())
            .unwrap(),
        1
    );
    assert_eq!(selected(&path("a[]"), &root), vec![Tag::End]);

    let mut array = Tag::ByteArray(Vec::new());
    assert_eq!(
        query.set(&mut array, &Tag::End, Limits::default()).unwrap(),
        1
    );
    assert_eq!(array, Tag::ByteArray(Vec::new()));
    assert!(
        query
            .get_or_create(&mut array, &mut || Tag::End, Limits::default())
            .unwrap()
            .is_empty()
    );
    let mut root = read("{a:[B;]}");
    assert_eq!(
        path("a")
            .insert(&mut root, 900, &[Tag::End], Limits::default())
            .unwrap(),
        0
    );
    assert_eq!(root, read("{a:[B;]}"));
}

#[test]
fn exhausted_mutation_budget_does_not_admit_unbudgeted_parent_creation() {
    for limits in [
        Limits {
            work_units: 0,
            ..Limits::default()
        },
        Limits {
            allocation_bytes: 0,
            ..Limits::default()
        },
    ] {
        let mut root = read("{}");
        assert!(path("a.b").set(&mut root, &Tag::Int(1), limits).is_err());
        assert_eq!(root, read("{}"));
    }
}

#[test]
fn deep_factory_rejection_cleans_unattached_values_without_recursive_drop() {
    let limits = Limits {
        allocation_bytes: 4096,
        ..Limits::default()
    };
    for compound_root in [false, true] {
        for (text, initial) in [("a", "{}"), ("[]", "[]"), ("[]", "[B;]")] {
            let query = path(text);
            let mut root = read(initial);
            let mut calls = 0;
            let result = query.get_or_create(
                &mut root,
                &mut || {
                    calls += 1;
                    deep_cleanup_tree(20_000, compound_root).0
                },
                limits,
            );
            let kind = result.err().map(|error| error.kind);
            let unchanged = root == read(initial);
            discard_cleanup_tree(root);
            assert_eq!(calls, 1, "{text} {initial}");
            assert_eq!(kind, Some(ErrorKind::AllocationBudget), "{text} {initial}");
            assert!(unchanged, "{text} {initial}");
        }
    }
}

#[test]
fn deep_admitted_factory_value_is_cleaned_when_attachment_allocation_fails() {
    let (value, retained_bytes) = deep_cleanup_tree(20_000, true);
    let limits = Limits {
        // One root candidate vector and one admission traversal vector. The
        // array case proves this exact budget admits the entire factory value.
        allocation_bytes: 4 * size_of::<SelectionMut<'_>>()
            + 4 * size_of::<&Tag>()
            + retained_bytes,
        ..Limits::default()
    };
    let mut supplied = Some(value);
    let mut array = Tag::ByteArray(Vec::new());
    let query = path("[]");
    let result = query.get_or_create(&mut array, &mut || supplied.take().unwrap(), limits);
    let admitted_and_rejected_by_array = result.is_ok_and(|values| values.is_empty());
    discard_cleanup_tree(array);
    assert!(admitted_and_rejected_by_array);
    assert!(supplied.is_none());

    let mut root = read("{}");
    let query = path("a");
    let result = query.get_or_create(&mut root, &mut || deep_cleanup_tree(20_000, true).0, limits);
    let kind = result.err().map(|error| error.kind);
    let unchanged = root == read("{}");
    discard_cleanup_tree(root);
    assert_eq!(kind, Some(ErrorKind::AllocationBudget));
    assert!(unchanged);
}

#[test]
fn deep_replaced_subtrees_are_cleaned_for_every_mutating_selector() {
    for text in ["a", "a{}", "[0]", "[]", "[{}]"] {
        let mut root = deep_cleanup_root(text);
        let result = path(text).set(&mut root, &Tag::Int(9), Limits::default());
        let (count, expected) = match text {
            "a" | "a{}" => (1, "{a:9}"),
            "[]" => (2, "[9,9]"),
            _ => (1, "[9]"),
        };
        let correct_root = root == read(expected);
        discard_cleanup_tree(root);
        assert_eq!(result.unwrap(), count, "{text}");
        assert!(correct_root, "{text}");
    }
}

#[test]
fn deep_removed_subtrees_are_cleaned_for_every_mutating_selector() {
    for text in ["a", "a{}", "[0]", "[]", "[{}]"] {
        let mut root = deep_cleanup_root(text);
        let result = path(text).remove(&mut root, Limits::default());
        let expected = if text.starts_with('a') { "{}" } else { "[]" };
        let count = if text == "[]" { 2 } else { 1 };
        let correct_root = root == read(expected);
        discard_cleanup_tree(root);
        assert_eq!(result.unwrap(), count, "{text}");
        assert!(correct_root, "{text}");
    }
}

#[test]
fn deep_insert_source_copy_is_cleaned_before_the_too_deep_error_returns() {
    let source = deep_cleanup_tree(20_000, true).0;
    let mut root = read("{}");
    let error = path("a")
        .insert(
            &mut root,
            0,
            std::slice::from_ref(&source),
            Limits::default(),
        )
        .unwrap_err();
    let mut depth = 0;
    let mut leaf = &source;
    loop {
        leaf = match leaf {
            Tag::List(values) if values.len() == 1 => &values[0],
            Tag::Compound(compound) if compound.entries().len() == 1 => {
                &compound.entries()[0].value
            }
            _ => break,
        };
        depth += 1;
    }
    let source_unchanged = depth == 20_000 && matches!(leaf, Tag::Int(1));
    discard_cleanup_tree(source);
    assert_eq!(error.kind, ErrorKind::TooDeep);
    assert_eq!(root, read("{}"));
    assert!(source_unchanged);
}

#[test]
fn deep_insert_partial_source_copy_is_cleaned_when_allocation_is_exhausted() {
    let source = deep_cleanup_tree(20_000, true).0;
    let mut root = read("{}");
    let result = path("a").insert(
        &mut root,
        0,
        std::slice::from_ref(&source),
        Limits {
            allocation_bytes: 2 * 1024 * 1024,
            ..Limits::default()
        },
    );
    let kind = result.err().map(|error| error.kind);
    let unchanged = root == read("{}");
    discard_cleanup_tree(source);
    discard_cleanup_tree(root);
    assert_eq!(kind, Some(ErrorKind::AllocationBudget));
    assert!(unchanged);
}

#[test]
fn empty_path_reads_the_root_and_reports_invalid_mutating_operations() {
    let query = path("");
    let mut root = read("{a:1}");
    assert_eq!(selected(&query, &root), vec![root.clone()]);
    assert_eq!(query.count_matching(&root, Limits::default()).unwrap(), 1);
    assert!(
        query
            .get_or_create(&mut root, &mut || Tag::Int(1), Limits::default())
            .is_err()
    );
    assert!(
        query
            .set(&mut root, &Tag::Int(1), Limits::default())
            .is_err()
    );
    assert!(query.insert(&mut root, 0, &[], Limits::default()).is_err());
    assert!(query.remove(&mut root, Limits::default()).is_err());
    assert_eq!(root, read("{a:1}"));
}
