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

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// The file name whose contents are union-merged rather than overwritten.
pub const MEMORY_INDEX_FILE: &str = "MEMORY.md";

/// One line of an index: an entry carries the link target it is keyed by.
enum IndexLine {
    Entry { target: String, text: String },
    Other(String),
}

/// True when this path's file name is a memory index.
pub fn is_memory_index(path: &std::path::Path) -> bool {
    path.file_name().map(|name| name == MEMORY_INDEX_FILE) == Some(true)
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

/// Merge `incoming` into `dest`, returning the merged text and how many entries
/// `incoming` contributed that `dest` did not have.
pub fn merge_memory_index(dest: &[u8], incoming: &[u8]) -> (Vec<u8>, usize) {
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

    if dest_targets.is_empty() && dest.is_empty() {
        return (incoming.to_vec(), incoming_entries.len());
    }

    let mut merged = String::new();
    for line in &dest_lines {
        let text = match line {
            IndexLine::Entry { target, text } => incoming_entries
                .get(target.as_str())
                .copied()
                .unwrap_or(text.as_str()),
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
        merged.push_str(text);
        merged.push('\n');
        added += 1;
    }

    (merged.into_bytes(), added)
}

/// Every link target any index line references — bare file names,
/// anchor suffixes normalized to the base name, subdirectory paths kept
/// whole. Both `MEMORY.md` and aggregate indexes (`all-memories-index.md`)
/// feed the same set: any `.md` in a memory directory can hold references.
pub fn referenced_targets(content: &str) -> BTreeSet<String> {
    parse(content)
        .iter()
        .filter_map(|line| match line {
            IndexLine::Entry { target, .. } => {
                let unwrapped = target.trim_start_matches('<').trim_end_matches('>');
                let stripped_prefix = unwrapped.strip_prefix("./").unwrap_or(unwrapped);
                let base = stripped_prefix
                    .split_once('#')
                    .map(|(base, _)| base)
                    .unwrap_or(stripped_prefix);
                Some(base.to_string())
            }
            IndexLine::Other(_) => None,
        })
        .collect()
}

/// Whether a path's file name matches any referenced target. A target
/// with a directory part (`notes/sub.md`) also matches on its base name,
/// mirroring how the sync engine lists memory directories.
fn is_referenced(file_name: &str, targets: &BTreeSet<String>) -> bool {
    targets.iter().any(|t| {
        t == file_name
            || t.split('/')
                .next_back()
                .is_some_and(|base| base == file_name)
    })
}

/// `*.md` files under any `projects/*/memory/` directory that no `.md`
/// in the same directory links to. An orphaned memory file is invisible
/// by construction — indexes are all that sync summaries and other
/// machines ever read — so status surfaces them before they rot.
pub fn orphaned_memory_files(claude_dir: &Path) -> Vec<PathBuf> {
    use std::fs;

    let projects = claude_dir.join("projects");
    let Ok(entries) = fs::read_dir(&projects) else {
        return Vec::new();
    };
    let mut orphans = Vec::new();
    for project in entries.flatten() {
        let memory = project.path().join("memory");
        if !memory.is_dir() {
            continue;
        }
        let Ok(files) = fs::read_dir(&memory) else {
            continue;
        };
        let md_files: Vec<_> = files
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("md"))
            .collect();
        let mut targets = BTreeSet::new();
        // Holders: MEMORY.md always, and any aggregate index — a file
        // that itself references others is a holder, never an orphan.
        let mut holders = BTreeSet::new();
        for file in &md_files {
            if is_memory_index(file) {
                holders.insert(file.clone());
            }
            if let Ok(text) = fs::read_to_string(file) {
                let refs = referenced_targets(&text);
                if !refs.is_empty() {
                    holders.insert(file.clone());
                }
                targets.extend(refs);
            }
        }
        for file in md_files {
            if holders.contains(&file) {
                continue;
            }
            let Some(name) = file.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if !is_referenced(name, &targets) {
                orphans.push(file);
            }
        }
    }
    orphans
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
mod orphan_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn write(path: &std::path::Path, text: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }

    #[test]
    fn referenced_targets_extracts_bare_and_anchored_names() {
        let text = "# Index\n\n- [a](a.md) A\n- [b](b.md#sec) B\n- [p](notes/sub.md) P\n";
        let targets = referenced_targets(text);
        assert!(targets.contains("a.md") && targets.contains("b.md"));
        assert!(targets.contains("sub.md") || targets.contains("notes/sub.md"));
    }

    #[test]
    fn a_memory_file_no_index_references_is_orphaned() {
        let dir = TempDir::new().unwrap();
        let memory = dir.path().join("projects/-home-x/memory");
        write(&memory.join("MEMORY.md"), "# Index\n\n- [a](a.md) A\n");
        write(&memory.join("a.md"), "a\n");
        write(&memory.join("ghost.md"), "nobody points here\n");
        let orphans = orphaned_memory_files(dir.path());
        assert_eq!(orphans.len(), 1);
        assert!(orphans[0].ends_with("ghost.md"));
    }

    #[test]
    fn an_aggregate_index_counts_as_a_reference_holder() {
        let dir = TempDir::new().unwrap();
        let memory = dir.path().join("projects/-home-x/memory");
        write(&memory.join("MEMORY.md"), "# Index\n\n- [a](a.md) A\n");
        write(&memory.join("a.md"), "a\n");
        write(
            &memory.join("all-memories-index.md"),
            "# All\n\n- [g](ghost.md) G\n",
        );
        write(&memory.join("ghost.md"), "g\n");
        assert!(
            orphaned_memory_files(dir.path()).is_empty(),
            "any .md in the dir can hold references, not just MEMORY.md"
        );
    }

    #[test]
    fn a_dir_without_memory_dir_reports_nothing() {
        let dir = TempDir::new().unwrap();
        assert!(orphaned_memory_files(dir.path()).is_empty());
    }
}
