//! Union merge for `MEMORY.md` memory indexes.
//!
//! An index lists what that machine knows about; a line may carry one
//! pointer (`- [Title](file.md) — hook`) or bundle several of them.
//! Overwriting would let a sparser index orphan another machine's memory
//! files, so both directions union instead: a line's identity is its full
//! set of link targets. An incoming line supersedes a destination line
//! only when it covers ALL of the destination line's pointers (and, for
//! a single-link destination, is itself single-link — a bundle merely
//! containing the pointer appends instead of swallowing the curated
//! hook text). Every other incoming entry line is appended unless its
//! exact text is already present.
//!
//! The destination keeps its own headings, blank lines, prose and entry
//! order, since this rewrites a hand-edited file. Only entry lines cross
//! between machines. Merging only adds or supersedes, so repeating it
//! changes nothing and two machines converge.

use std::collections::BTreeSet;

/// The file name whose contents are union-merged rather than overwritten.
pub const MEMORY_INDEX_FILE: &str = "MEMORY.md";

/// One line of an index: an entry carries every link target the line
/// references (one, or a whole bundle on a compacted line).
enum IndexLine {
    Entry { targets: Vec<String>, text: String },
    Other(String),
}

/// True when this path's file name is a memory index.
pub fn is_memory_index(path: &std::path::Path) -> bool {
    path.file_name().map(|name| name == MEMORY_INDEX_FILE) == Some(true)
}

/// Every markdown link target on the line, left to right. Compacted
/// indexes group several pointers on one line — a line is not one
/// reference, and taking only the first `](` would key the whole bundle
/// on a single pointer.
fn link_targets(line: &str) -> Vec<String> {
    let mut targets = Vec::new();
    let mut rest = line;
    while let Some(start) = rest.find("](") {
        let after = &rest[start + 2..];
        match after.find(')') {
            Some(end) => {
                targets.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    targets
}

fn parse(text: &str) -> Vec<IndexLine> {
    text.lines()
        .map(|line| {
            let targets = link_targets(line);
            if line.starts_with("- ") && !targets.is_empty() {
                IndexLine::Entry {
                    targets,
                    text: line.to_string(),
                }
            } else {
                IndexLine::Other(line.to_string())
            }
        })
        .collect()
}

/// Merge `incoming` into `dest`, returning the merged text and how many
/// entry lines `incoming` contributed beyond what `dest` already held
/// (verbatim or superseded). Every link pointer present on either side
/// ends up present in the merge.
pub fn merge_memory_index(dest: &[u8], incoming: &[u8]) -> (Vec<u8>, usize) {
    let dest_lines = parse(&String::from_utf8_lossy(dest));
    let incoming_lines = parse(&String::from_utf8_lossy(incoming));

    let incoming_entry_count = incoming_lines
        .iter()
        .filter(|line| matches!(line, IndexLine::Entry { .. }))
        .count();

    let dest_targets: BTreeSet<&str> = dest_lines
        .iter()
        .flat_map(|line| match line {
            IndexLine::Entry { targets, .. } => {
                targets.iter().map(|t| t.as_str()).collect::<Vec<_>>()
            }
            IndexLine::Other(_) => Vec::new(),
        })
        .collect();

    if dest_targets.is_empty() && dest.is_empty() {
        return (incoming.to_vec(), incoming_entry_count);
    }

    // Pointer-set semantics: a compact line bundles related pointers, so
    // a line is superseded only by an incoming line covering ALL its
    // pointers (lossless, no duplication). Anything else keeps the dest
    // line — the old first-link-keyed replacement silently dropped the
    // other pointers of a grouped line.
    let mut used_for_replacement = vec![false; incoming_lines.len()];
    let mut merged = String::new();
    for line in &dest_lines {
        let text = match line {
            IndexLine::Entry { targets, text } => {
                let covering = incoming_lines.iter().enumerate().find(|(i, inc)| {
                    !used_for_replacement[*i]
                        && match inc {
                            IndexLine::Entry {
                                targets: inc_targets,
                                ..
                            } => {
                                // incoming COVERS dest: every dest pointer
                                // is present in the incoming line. A
                                // single-link dest line is a hand-written
                                // entry with curated text — only an
                                // exact single-link match supersedes it;
                                // a bundle merely CONTAINING the pointer
                                // must append instead of swallowing the
                                // note.
                                targets.iter().all(|t| inc_targets.contains(t))
                                    && (targets.len() > 1 || inc_targets.len() == 1)
                            }
                            IndexLine::Other(_) => false,
                        }
                });
                match covering {
                    Some((i, inc)) => {
                        used_for_replacement[i] = true;
                        match inc {
                            IndexLine::Entry { text, .. } => text.as_str(),
                            IndexLine::Other(text) => text.as_str(),
                        }
                    }
                    None => text.as_str(),
                }
            }
            IndexLine::Other(text) => text.as_str(),
        };
        merged.push_str(text);
        merged.push('\n');
    }

    // Incoming entries not used as replacements are appended unless
    // byte-identical to a line already there. A single-link line whose
    // pointer is covered by a bundle but whose text differs still
    // arrives — the edit must be visible — which is the appended-line
    // form of the incoming-wins contract.
    let mut added = 0;
    for (i, line) in incoming_lines.iter().enumerate() {
        if used_for_replacement[i] {
            continue;
        }
        let IndexLine::Entry { text, .. } = line else {
            continue;
        };
        // Arrive unless the exact line is already there. A line whose
        // pointers are all covered but whose text differs is an edit —
        // it must be visible (as its own line; the bundled line keeps
        // the other pointers).
        let merged_has_text = merged.lines().any(|l| l == text);
        if merged_has_text {
            continue;
        }
        merged.push_str(text);
        merged.push('\n');
        added += 1;
    }

    (merged.into_bytes(), added)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn recognizes_the_index_by_file_name() {
        assert!(is_memory_index(Path::new("projects/p/memory/MEMORY.md")));
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
}

#[cfg(test)]
mod grouped_lines_tests {
    use super::*;

    #[test]
    fn a_grouped_line_survives_an_incoming_edit_at_one_of_its_pointers() {
        // The compact format bundles related pointers on one line. An
        // incoming single-link line editing ONE of those pointers must
        // not erase the other pointers of the bundle: the grouped line
        // survives, the incoming line is appended, every pointer from
        // both sides is present.
        let dest = b"- [bundle](a.md) - also [b](b.md); plus [c](c.md)\n";
        let incoming = b"- [a](a.md) edited on the other machine\n";
        let (merged, added) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        for needle in ["a.md", "b.md", "c.md"] {
            assert!(
                text.contains(needle),
                "pointer survives: {needle} in {text}"
            );
        }
        assert!(
            text.contains("edited on the other machine"),
            "the edit arrives: {text}"
        );
        assert_eq!(added, 1, "the incoming line is appended, not dropped");
    }

    #[test]
    fn a_covering_line_supersedes_a_grouped_line_without_duplicates() {
        // The incoming line carries every pointer of the dest line plus
        // new ones: replacing the dest line is lossless (all its pointers
        // are covered) and avoids duplicating them.
        let dest = b"- [bundle](a.md) - also [b](b.md)\n";
        let incoming = b"- [bundle+](a.md) - also [b](b.md); plus [c](c.md)\n";
        let (merged, _) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        for needle in ["a.md", "b.md", "c.md"] {
            assert!(
                text.contains(needle),
                "pointer survives: {needle} in {text}"
            );
        }
        assert!(
            text.matches("a.md").count() == 1,
            "no duplicated pointer: {text}"
        );
    }

    #[test]
    fn grouped_vs_grouped_partial_overlap_loses_no_pointer() {
        // Two machines grouped DIFFERENT subsets under the same first
        // pointer: neither line supersedes the other, both are kept, and
        // the union of pointers is complete — the old first-link-keyed
        // merge silently dropped the losing line's pointers here.
        let dest = b"- [shared](a.md) - mine [b](b.md)\n";
        let incoming = b"- [shared](a.md) - theirs [d](d.md)\n";
        let (merged, _) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        for needle in ["a.md", "b.md", "d.md"] {
            assert!(
                text.contains(needle),
                "pointer survives: {needle} in {text}"
            );
        }
    }

    #[test]
    fn single_link_lines_keep_the_incoming_wins_contract() {
        // The pre-existing contract is untouched: one link per line, the
        // incoming side's text wins at a shared key.
        let dest = b"- [a](a.md) dest text\n";
        let incoming = b"- [a](a.md) incoming text\n- [n](n.md) new\n";
        let (merged, added) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        assert!(text.contains("incoming text"), "incoming wins: {text}");
        assert!(
            !text.contains("dest text"),
            "replaced, not duplicated: {text}"
        );
        assert!(text.contains("n.md"));
        assert_eq!(added, 1);
    }

    #[test]
    fn prose_only_lines_are_never_entries_and_never_merge() {
        // A prose line carrying links is not an index entry: it passes
        // through verbatim and never keys a merge decision.
        let dest = b"See [detail](detail.md) for context.\n";
        let incoming = b"- [detail](detail.md) replaced text\n";
        let (merged, _) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        assert!(text.contains("See [detail](detail.md) for context."));
    }
}

#[cfg(test)]
mod review_fixes_tests {
    use super::*;

    #[test]
    fn a_bundle_line_never_replaces_a_single_link_entry() {
        // A single-link line is a hand-written entry with curated hook
        // text. An incoming bundle merely CONTAINING that pointer must
        // not swallow the note: the entry survives, the bundle is
        // appended, no pointer is lost.
        let dest = b"- [b](b.md) IMPORTANT NOTE\n";
        let incoming = b"- [a](a.md) - also [b](b.md)\n";
        let (merged, _) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        assert!(
            text.contains("IMPORTANT NOTE"),
            "the hook text survives: {text}"
        );
        assert!(
            text.contains("a.md") && text.contains("b.md"),
            "all pointers present: {text}"
        );
    }

    #[test]
    fn a_covering_bundle_still_replaces_a_grouped_line() {
        // The lossless replacement stays: a grouped dest line whose every
        // pointer the incoming line carries is superseded (same bundle,
        // evolved on the other machine) — without duplicating pointers.
        let dest = b"- [bundle](a.md) - also [b](b.md)\n";
        let incoming = b"- [bundle+](a.md) - also [b](b.md); plus [c](c.md)\n";
        let (merged, _) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        for needle in ["a.md", "b.md", "c.md"] {
            assert!(
                text.contains(needle),
                "pointer survives: {needle} in {text}"
            );
        }
        assert!(
            text.matches("(a.md)").count() == 1,
            "no duplicated pointer: {text}"
        );
    }

    #[test]
    fn merging_the_same_merged_output_again_changes_nothing() {
        // The merge is idempotent on its own output — two machines
        // converge to one fixed point and a second round is byte-stable.
        let dest = b"# Index\n\n- [a](a.md) A\n- [b](b.md) B\n";
        let incoming = b"# Index\n\n- [b](b.md) B\n- [c](c.md) C\n";
        let (once, _) = merge_memory_index(dest, incoming);
        let (twice, added) = merge_memory_index(&once, incoming);
        assert_eq!(once, twice, "byte-stable second round");
        assert_eq!(added, 0);
    }

    #[test]
    fn a_replacement_never_dups_a_line_already_emitted() {
        // The replaced line and a kept dest line can be byte-identical
        // (an edit line + a bundle line the incoming bundle covers): the
        // output carries the bundle line once.
        let dest = b"- [edit](e.md) edit\n- [bundle](a.md) - also [b](b.md)\n";
        let incoming = b"- [bundle](a.md) - also [b](b.md)\n";
        let (merged, _) = merge_memory_index(dest, incoming);
        let text = String::from_utf8(merged).unwrap();
        assert_eq!(
            text.matches("- [bundle](a.md) - also [b](b.md)").count(),
            1,
            "one bundle line, not two: {text}"
        );
        assert!(text.contains("edit"), "the edit line survives: {text}");
    }
}
