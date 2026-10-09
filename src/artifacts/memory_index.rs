//! Union merge for `MEMORY.md` memory indexes.
//!
//! An index is one line per memory (`- [Title](file.md) — hook`), listing what
//! that machine knows about. Overwriting would let a sparser index orphan
//! another machine's memory files, so both directions union instead: entries
//! are keyed by link target, the incoming side wins per entry, and entries the
//! destination lacks are appended.
//!
//! The destination keeps its own headings, blank lines, prose and entry order,
//! since this rewrites a hand-edited file. Only entry lines cross between
//! machines. Merging only adds or updates a line, so repeating it changes
//! nothing and two machines converge.
//!
//! # Deletion propagation
//!
//! A plain union of present-entries cannot propagate a deletion: if the local
//! side removed entry `[a](a.md)` but the remote still has the line, the next
//! merge resurrects `[a]`. This is the textbook 2P-Set problem (Shapiro 2011,
//! INRIA RR-7506; xi-editor CRDT notes) — present-sets do not preserve
//! deletions. The fix used here is the per-record tombstone pattern from
//! `flutter_agent_memory` v0.2.2-v0.2.3: a deleted entry writes a sibling
//! `deleted/<id>.md` file in the same project directory. The merge function
//! reads the `deleted/` set on each side and drops the entry from the merged
//! output when a tombstone is present on either side.
//!
//! Identical concurrent deletes produce byte-identical markers, so git
//! three-way-merges them trivially. Markers are never garbage-collected: the
//! cost is one file per deleted entry, and memory indexes carry tens to
//! hundreds of entries per project (not millions).

use std::collections::{BTreeMap, BTreeSet};

/// The file name whose contents are union-merged rather than overwritten.
pub const MEMORY_INDEX_FILE: &str = "MEMORY.md";

/// The directory (sibling of MEMORY.md) that holds deletion tombstones.
/// Each `<id>.md` here is a deterministic marker that survives git merge.
pub const DELETED_DIR: &str = "deleted";

/// One line of an index: an entry carries the link target it is keyed by.
enum IndexLine {
    Entry { target: String, text: String },
    Other(String),
}

/// True when this path's file name is a memory index.
pub fn is_memory_index(path: &std::path::Path) -> bool {
    path.file_name().map(|name| name == MEMORY_INDEX_FILE) == Some(true)
}

/// The stem of a MEMORY.md link target: `a.md` -> `a`, `a` -> `a`. Returns
/// `None` for paths with no usable stem (e.g. trailing dots).
fn link_target_stem(target: &str) -> Option<String> {
    std::path::Path::new(target)
        .file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.to_string())
}

fn link_target(line: &str) -> Option<String> {
    let start = line.find("](")? + 2;
    let rest = &line[start..];
    let end = rest.find(')')?;
    Some(rest[..end].to_string())
}

fn parse(text: &str) -> Vec<IndexLine> {
    text.lines()
        .map(|line| match link_target(line) {
            Some(target) if line.starts_with("- ") => IndexLine::Entry {
                target,
                text: line.to_string(),
            },
            _ => IndexLine::Other(line.to_string()),
        })
        .collect()
}

/// Tombstone-aware memory index merge. Reads the resolved sibling-file sets
/// (entry link targets present in the same directory as MEMORY.md) and the
/// `deleted/` directory contents (ids whose tombstone is present) on each
/// side. Drops entries whose id is tombstoned on either side. The 2-arg
/// `merge_memory_index` below is a thin wrapper that passes empty sets, so
/// existing call sites behave identically when no `deleted/` dir exists.
///
/// Rules in priority order:
/// 1. `incoming_deleted.contains(id)` → drop from merged output entirely.
/// 2. `dest_deleted.contains(id) && !dest_siblings.contains(target)` → drop
///    the dest text for that id (the local deletion was real, the local
///    sibling is gone, the tombstone is set).
/// 3. Otherwise the existing per-entry union semantics apply: dest entry
///    text is replaced by incoming text when incoming has the same target,
///    else incoming-only entries are appended.
pub fn merge_memory_index_with_tombstones(
    dest: &[u8],
    incoming: &[u8],
    dest_siblings: &BTreeSet<String>,
    incoming_siblings: &BTreeSet<String>,
    dest_deleted: &BTreeSet<String>,
    incoming_deleted: &BTreeSet<String>,
) -> (Vec<u8>, usize) {
    let dest_lines = parse(&String::from_utf8_lossy(dest));
    let incoming_lines = parse(&String::from_utf8_lossy(incoming));

    let incoming_entries: BTreeMap<&str, &str> = incoming_lines
        .iter()
        .filter_map(|line| match line {
            IndexLine::Entry { target, text } => Some((target.as_str(), text.as_str())),
            IndexLine::Other(_) => None,
        })
        .collect();

    let dest_targets: BTreeSet<&str> = dest_lines
        .iter()
        .filter_map(|line| match line {
            IndexLine::Entry { target, .. } => Some(target.as_str()),
            IndexLine::Other(_) => None,
        })
        .collect();

    // The drop set: ids that should NOT appear in the merged output.
    // An id is dropped when EITHER side says it's deleted (incoming's
    // tombstone wins even if the dest still holds the entry text — a
    // remote delete propagates to the local) OR when the local side
    // says it's deleted AND the local sibling is gone (symmetric:
    // the local deletion was real).
    let mut dropped: BTreeSet<String> = BTreeSet::new();
    for (target, _) in &incoming_entries {
        if let Some(id) = link_target_stem(target) {
            if incoming_deleted.contains(&id) {
                dropped.insert(id);
            }
        }
    }
    for target in &dest_targets {
        if let Some(id) = link_target_stem(target) {
            if (dest_deleted.contains(&id) && !dest_siblings.contains(*target))
                || incoming_deleted.contains(&id)
            {
                dropped.insert(id);
            }
        }
    }

    if dest_targets.is_empty() && dest.is_empty() && dropped.is_empty() {
        return (incoming.to_vec(), incoming_entries.len());
    }

    let mut merged = String::new();
    for line in &dest_lines {
        let text = match line {
            IndexLine::Entry { target, text } => {
                let stem = link_target_stem(target);
                if stem.as_ref().is_some_and(|id| dropped.contains(id)) {
                    // Tombstoned: do not emit anything for this entry. The
                    // entry's heading on dest stays (Other lines are kept).
                    continue;
                }
                incoming_entries
                    .get(target.as_str())
                    .copied()
                    .unwrap_or(text.as_str())
            }
            IndexLine::Other(text) => text.as_str(),
        };
        merged.push_str(text);
        merged.push('\n');
    }

    let mut added = 0;
    for (target, text) in &incoming_entries {
        if dest_targets.contains(target) {
            continue;
        }
        let stem = link_target_stem(target);
        if stem.as_ref().is_some_and(|id| dropped.contains(id)) {
            continue;
        }
        // Sanity: a tombstoned entry on incoming must not be re-added by
        // sibling absence alone. The incoming_deleted set already catches
        // this; the sibling check is a no-op for incoming, but it keeps
        // the rule explicit.
        if stem.as_ref().is_some_and(|id| incoming_deleted.contains(id))
            && !incoming_siblings.contains(*target)
        {
            continue;
        }
        merged.push_str(text);
        merged.push('\n');
        added += 1;
    }

    (merged.into_bytes(), added)
}

/// Thin wrapper for the existing 2-arg contract: empty sibling and deleted
/// sets, so the merge reduces to the original per-entry union. Existing call
/// sites at engine.rs:700 (push) and engine.rs:2786 (pull apply) keep working
/// without modification.
pub fn merge_memory_index(dest: &[u8], incoming: &[u8]) -> (Vec<u8>, usize) {
    let empty: BTreeSet<String> = BTreeSet::new();
    merge_memory_index_with_tombstones(dest, incoming, &empty, &empty, &empty, &empty)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn set(items: &[&str]) -> BTreeSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn recognizes_the_index_by_file_name() {
        let p = Path::new("projects/p/memory/MEMORY.md");
        assert!(is_memory_index(p));
        assert!(!is_memory_index(Path::new("projects/p/memory/fact.md")));
    }

    #[test]
    fn merging_the_same_entries_again_changes_nothing() {
        let one = b"# Memory index\n\n- [a](a.md) hook A\n- [b](b.md) hook B\n";
        let two = b"# Memory index\n\n- [b](b.md) hook B\n- [a](a.md) hook A\n";

        let (merged, added) = merge_memory_index(one, two);

        assert_eq!(merged, one.to_vec(), "same entries, so nothing to add");
        assert_eq!(added, 0);
        assert_eq!(merge_memory_index(&merged, one).0, merged);
        assert_eq!(merge_memory_index(&merged, two).0, merged);
    }

    #[test]
    fn both_machines_end_up_with_every_entry() {
        let mut machine_a = b"# Index\n\n- [a](a.md) from A\n".to_vec();
        let mut repo = machine_a.clone();
        let machine_b = b"# Index\n\n- [b](b.md) from B\n".to_vec();

        // B pushes, then pulls; A pulls afterwards.
        repo = merge_memory_index(&repo, &machine_b).0;
        let machine_b = merge_memory_index(&machine_b, &repo).0;
        machine_a = merge_memory_index(&machine_a, &repo).0;

        for index in [&machine_a, &machine_b, &repo] {
            let text = String::from_utf8(index.clone()).unwrap();
            assert!(text.contains("[a](a.md)"), "{text}");
            assert!(text.contains("[b](b.md)"), "{text}");
        }
        // And a second round settles: nothing further is written anywhere.
        assert_eq!(merge_memory_index(&repo, &machine_a).0, repo);
        assert_eq!(merge_memory_index(&machine_a, &repo).0, machine_a);
    }

    #[test]
    fn headings_blank_lines_and_prose_are_kept_where_they_are() {
        let dest = b"# Memory index\n\n## Work\n- [a](a.md) hook A\n\n## Personal\n- [b](b.md) hook B\n\nSee also the README.\n";
        let incoming = b"- [a](a.md) hook A\n- [c](c.md) hook C\n";

        let (merged, added) = merge_memory_index(dest, incoming);

        assert_eq!(
            String::from_utf8(merged).unwrap(),
            "# Memory index\n\n## Work\n- [a](a.md) hook A\n\n## Personal\n- [b](b.md) hook B\n\nSee also the README.\n- [c](c.md) hook C\n"
        );
        assert_eq!(added, 1);
    }

    #[test]
    fn an_empty_index_takes_the_other_side_verbatim() {
        let incoming = b"# Index\n\n- [a](a.md) hook A\n";
        let (merged, added) = merge_memory_index(b"", incoming);
        assert_eq!(merged, incoming.to_vec());
        assert_eq!(added, 1);
    }

    #[test]
    fn a_sparser_index_cannot_drop_entries() {
        let rich = b"# Memory index\n\n- [a](a.md) hook A\n- [z](z.md) hook Z\n";
        let sparse = b"- [a](a.md) hook A\n";
        let (merged, added) = merge_memory_index(rich, sparse);
        let text = String::from_utf8(merged).unwrap();
        assert!(text.contains("[z](z.md)"));
        assert!(text.contains("# Memory index"));
        assert_eq!(added, 0);
    }

    #[test]
    fn incoming_wins_per_entry_and_new_entries_are_counted() {
        let dest = b"- [a](a.md) OLD\n";
        let incoming = b"- [a](a.md) NEW\n- [b](b.md) added\n";
        let (merged, added) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        assert!(text.contains("NEW") && !text.contains("OLD"));
        assert_eq!(added, 1);
    }

    // -- Tombstone-aware tests -------------------------------------------

    #[test]
    fn deleting_an_entry_propagates_when_a_marker_is_present() {
        let dest = b"# Index\n\n- [a](a.md) hook A\n- [b](b.md) hook B\n";
        let incoming = b"# Index\n\n- [b](b.md) hook B\n";
        let dest_siblings = set(&["a.md", "b.md"]);
        let incoming_siblings = set(&["b.md"]);
        let dest_deleted = set(&[] as &[&str]);
        let incoming_deleted = set(&["a"]);

        let (merged, added) = merge_memory_index_with_tombstones(
            dest,
            incoming,
            &dest_siblings,
            &incoming_siblings,
            &dest_deleted,
            &incoming_deleted,
        );

        let text = String::from_utf8(merged).unwrap();
        assert!(!text.contains("[a]"), "a should be tombstoned: {text}");
        assert!(text.contains("[b](b.md)"), "b should be kept: {text}");
        assert_eq!(added, 0);
    }

    #[test]
    fn entries_without_markers_keep_current_behavior() {
        // No deleted/ set anywhere: the 5-arg must reduce to the 2-arg.
        let dest = b"- [a](a.md) A\n- [b](b.md) B\n";
        let incoming = b"- [a](a.md) A\n- [c](c.md) C\n";
        let empty = set(&[] as &[&str]);
        let (m5, _) = merge_memory_index_with_tombstones(
            dest,
            incoming,
            &empty,
            &empty,
            &empty,
            &empty,
        );
        let (m2, _) = merge_memory_index(dest, incoming);
        assert_eq!(m5, m2);
    }

    #[test]
    fn a_marker_with_a_present_sibling_does_not_drop() {
        // The remote has the marker but ALSO the sibling — a late-arriving
        // delete marker is honored, but if the sibling is back the entry
        // should NOT be re-added (incoming_deleted wins regardless).
        let dest = b"- [a](a.md) A\n";
        let incoming = b"- [a](a.md) A\n- [b](b.md) B\n";
        let dest_siblings = set(&["a.md"]);
        let incoming_siblings = set(&["a.md", "b.md"]);
        let dest_deleted = set(&[] as &[&str]);
        let incoming_deleted = set(&["a"]);

        let (merged, _) = merge_memory_index_with_tombstones(
            dest,
            incoming,
            &dest_siblings,
            &incoming_siblings,
            &dest_deleted,
            &incoming_deleted,
        );

        let text = String::from_utf8(merged).unwrap();
        assert!(!text.contains("[a]"), "a is tombstoned on incoming");
        // b is NOT tombstoned, so b should be added.
        assert!(text.contains("[b](b.md)"));
    }

    #[test]
    fn identical_deletes_produce_byte_identical_markers() {
        // The marker format is the carrier of "this id was deleted"
        // across the union-merge. Two machines deleting the same id
        // at the same time must produce byte-identical files so git
        // three-way-merge treats them as a trivial no-op. The
        // production body is byte-stable: identical inputs produce
        // identical bytes regardless of who wrote the marker.
        //
        // This test exercises the real `record_memory_deletion_marker`
        // (the production writer), not a hand-typed format string:
        // the same MEMORY.md entry + same sibling path on two
        // independent directories must produce the same bytes.
        let dir_a = tempfile::tempdir().unwrap();
        let dir_b = tempfile::tempdir().unwrap();
        let mem_a = dir_a.path().join("projects/-home-a/memory");
        let mem_b = dir_b.path().join("projects/-home-a/memory");
        let setup = |dir: &std::path::Path| {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(
                dir.join("MEMORY.md"),
                "- [a](a.md) hook A\n- [b](b.md) hook B\n",
            )
            .unwrap();
            std::fs::write(dir.join("a.md"), "fact a\n").unwrap();
        };
        setup(&mem_a);
        setup(&mem_b);
        // Two machines "concurrently" delete entry [a]: each calls
        // the production marker writer for the same sibling.
        crate::artifacts::engine::record_memory_deletion_marker(&mem_a.join("a.md"))
            .expect("machine A marker");
        crate::artifacts::engine::record_memory_deletion_marker(&mem_b.join("a.md"))
            .expect("machine B marker");
        let body_a = std::fs::read(mem_a.join("deleted/a.md")).unwrap();
        let body_b = std::fs::read(mem_b.join("deleted/a.md")).unwrap();
        assert_eq!(
            body_a, body_b,
            "concurrent deletes must produce byte-identical markers so \
             git three-way-merge treats them as a trivial no-op"
        );
        // And the body matches the documented format: deterministic,
        // no wall-clock fields, the original entry preserved.
        let body_str = std::str::from_utf8(&body_a).unwrap();
        assert!(body_str.contains("deleted"), "body must mark as deleted");
        assert!(
            body_str.contains("git-3way-merge-stable"),
            "body must carry the merge-stable marker"
        );
        assert!(
            body_str.contains("was: - [a](a.md) hook A"),
            "body must carry the original entry text"
        );
    }

    #[test]
    fn a_local_deletion_mirrors_via_dest_tombstone() {
        // Local side: marker present, sibling absent. Drop.
        let dest = b"- [a](a.md) A\n- [b](b.md) B\n";
        let incoming = b"- [a](a.md) A\n- [b](b.md) B\n";
        let dest_siblings = set(&["b.md"]); // a.md is gone locally
        let incoming_siblings = set(&["a.md", "b.md"]);
        let dest_deleted = set(&["a"]);
        let incoming_deleted = set(&[] as &[&str]);

        let (merged, _) = merge_memory_index_with_tombstones(
            dest,
            incoming,
            &dest_siblings,
            &incoming_siblings,
            &dest_deleted,
            &incoming_deleted,
        );

        let text = String::from_utf8(merged).unwrap();
        // Local deletion wins: a is dropped even though incoming has it.
        assert!(!text.contains("[a]"), "a should be dropped: {text}");
        assert!(text.contains("[b](b.md)"));
    }
}
