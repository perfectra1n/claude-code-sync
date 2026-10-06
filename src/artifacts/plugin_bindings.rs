//! Union merge for `plugin-directory-bindings.json`.
//!
//! Claude Code's plugin-directory v2 install path appends one entry to this
//! file's `bindings` map per plugin bound on that machine. Raw-overwriting it
//! would let the last machine to push erase bindings the others added — the
//! same lost-update class `history.jsonl` and memory indexes union away — so
//! both directions merge key-wise instead: entries are keyed by plugin name,
//! the incoming side wins per entry, and entries the destination lacks are
//! appended.
//!
//! The merge only ever adds or updates a binding, so unbinding a plugin on
//! one machine does not propagate (grow-only, like prompt history). An
//! unusable side degrades safely: an `incoming` that does not parse (empty,
//! truncated by a crashed write) contributes nothing, while a `dest` that
//! does not parse is raw-overwritten — never a hard failure mid-sync.
//!
//! Output keeps the incoming file's key layout (serde_json
//! `preserve_order`), so a byte-identical re-merge writes nothing. The
//! serialization style itself (compact, serde_json's) may differ from what
//! Claude Code last wrote; content converges, and a rewrite only happens
//! when a binding actually changed or the layout differs.

use std::path::Path;

/// The file name whose `bindings` map is union-merged rather than overwritten.
pub const PLUGIN_BINDINGS_FILE: &str = "plugin-directory-bindings.json";

/// True when this path's file name is a plugin-directory bindings file.
pub fn is_plugin_bindings(path: &Path) -> bool {
    path.file_name().map(|name| name == PLUGIN_BINDINGS_FILE) == Some(true)
}

/// Merge `incoming` into `dest`, returning the merged bytes and how many
/// bindings `incoming` contributed that `dest` did not have.
///
/// Neither side failing to parse is a hard error — the merge protects what it
/// can: an unusable `incoming` (empty, truncated by a crashed write, corrupt)
/// contributes nothing and leaves `dest` untouched, while an unusable `dest`
/// degrades to a raw overwrite by `incoming`, which has nothing to lose.
pub fn merge_plugin_bindings(dest: &[u8], incoming: &[u8]) -> (Vec<u8>, usize) {
    use serde_json::Value;

    if dest.is_empty() {
        let added = serde_json::from_slice::<Value>(incoming)
            .ok()
            .and_then(|v| {
                v.get("bindings")
                    .and_then(|b| b.as_object())
                    .map(|m| m.len())
            })
            .unwrap_or(0);
        return (incoming.to_vec(), added);
    }

    let (Ok(dest_v), Ok(incoming_v)) = (
        serde_json::from_slice::<Value>(dest),
        serde_json::from_slice::<Value>(incoming),
    ) else {
        // `incoming` did not parse: it may be a truncated or crashed write,
        // so it must not wipe a valid `dest`.
        return (dest.to_vec(), 0);
    };
    let Some(dest_map) = dest_v.as_object() else {
        // `dest` is not a JSON object: unusable as a merge base, incoming wins.
        return (incoming.to_vec(), 0);
    };
    let Value::Object(mut out) = incoming_v else {
        return (dest.to_vec(), 0);
    };

    // Top-level fields (schemaVersion, ...) follow the incoming side; only
    // the bindings map is a union, with incoming winning per shared key.
    let mut merged: serde_json::Map<String, Value> = dest_map
        .get("bindings")
        .and_then(|b| b.as_object())
        .cloned()
        .unwrap_or_default();
    let before = merged.len();
    if let Some(incoming_bindings) = out.get("bindings").and_then(|b| b.as_object()) {
        for (key, value) in incoming_bindings {
            merged.insert(key.clone(), value.clone());
        }
    }
    let added = merged.len().saturating_sub(before);

    out.insert("bindings".to_string(), Value::Object(merged));
    (
        serde_json::to_vec(&Value::Object(out)).unwrap_or_else(|_| incoming.to_vec()),
        added,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::path::Path;

    fn bindings(pairs: &[(&str, &str)]) -> Vec<u8> {
        let map: serde_json::Map<String, Value> = pairs
            .iter()
            .map(|(k, repo)| (k.to_string(), serde_json::json!({ "repositoryKey": repo })))
            .collect();
        serde_json::json!({ "schemaVersion": 1, "bindings": map })
            .to_string()
            .into_bytes()
    }

    fn binding_keys(bytes: &[u8]) -> Vec<String> {
        let v: Value = serde_json::from_slice(bytes).unwrap();
        v["bindings"].as_object().unwrap().keys().cloned().collect()
    }

    #[test]
    fn recognizes_the_bindings_file_by_name() {
        assert!(is_plugin_bindings(Path::new(
            "plugins/plugin-directory-bindings.json"
        )));
        assert!(!is_plugin_bindings(Path::new(
            "plugins/installed_plugins.json"
        )));
    }

    #[test]
    fn merging_the_same_bindings_again_changes_nothing() {
        let one = bindings(&[("a@dir", "github.com/o/r#plugins/a")]);
        let two = bindings(&[("a@dir", "github.com/o/r#plugins/a")]);

        let (merged, added) = merge_plugin_bindings(&one, &two);

        let v: Value = serde_json::from_slice(&merged).unwrap();
        assert_eq!(v, serde_json::from_slice::<Value>(&one).unwrap());
        assert_eq!(added, 0);
    }

    #[test]
    fn both_machines_end_up_with_every_binding() {
        let mut machine_a = bindings(&[("a@dir", "github.com/o/r#plugins/a")]);
        let mut repo = machine_a.clone();
        let machine_b = bindings(&[("b@dir", "github.com/o/r#plugins/b")]);

        // B pushes, then pulls; A pulls afterwards.
        repo = merge_plugin_bindings(&repo, &machine_b).0;
        let machine_b = merge_plugin_bindings(&machine_b, &repo).0;
        machine_a = merge_plugin_bindings(&machine_a, &repo).0;

        for side in [&machine_a, &machine_b, &repo] {
            let keys = binding_keys(side);
            assert!(keys.contains(&"a@dir".to_string()), "{keys:?}");
            assert!(keys.contains(&"b@dir".to_string()), "{keys:?}");
        }
        // And a second round settles: nothing further is written anywhere.
        assert_eq!(merge_plugin_bindings(&repo, &machine_a).0, repo);
        assert_eq!(merge_plugin_bindings(&machine_a, &repo).0, machine_a);
    }

    #[test]
    fn a_sparser_bindings_file_cannot_drop_entries() {
        let rich = bindings(&[
            ("a@dir", "github.com/o/r#plugins/a"),
            ("z@dir", "github.com/o/r#plugins/z"),
        ]);
        let sparse = bindings(&[("a@dir", "github.com/o/r#plugins/a")]);

        let (merged, added) = merge_plugin_bindings(&rich, &sparse);

        let keys = binding_keys(&merged);
        assert!(keys.contains(&"z@dir".to_string()), "{keys:?}");
        assert_eq!(added, 0);
    }

    #[test]
    fn incoming_wins_per_entry_and_new_entries_are_counted() {
        let dest = bindings(&[("a@dir", "github.com/o/OLD")]);
        let incoming = bindings(&[
            ("a@dir", "github.com/o/NEW"),
            ("b@dir", "github.com/o/r#plugins/b"),
        ]);

        let (merged, added) = merge_plugin_bindings(&dest, &incoming);

        let v: Value = serde_json::from_slice(&merged).unwrap();
        assert_eq!(v["bindings"]["a@dir"]["repositoryKey"], "github.com/o/NEW");
        assert_eq!(added, 1);
    }

    #[test]
    fn an_empty_file_takes_the_other_side_verbatim() {
        let incoming = bindings(&[("a@dir", "github.com/o/r#plugins/a")]);
        let (merged, added) = merge_plugin_bindings(b"", &incoming);
        assert_eq!(merged, incoming);
        assert_eq!(added, 1);
    }

    #[test]
    fn an_unparseable_incoming_side_cannot_wipe_the_destination() {
        // A truncated or crashed-write incoming file must contribute nothing,
        // not overwrite a valid dest with emptiness or garbage.
        let dest = bindings(&[("a@dir", "github.com/o/r#plugins/a")]);

        let (merged, added) = merge_plugin_bindings(&dest, b"");
        assert_eq!(merged, dest, "empty incoming leaves dest untouched");
        assert_eq!(added, 0);

        let corrupt = b"not json at all";
        let (merged, added) = merge_plugin_bindings(&dest, corrupt);
        assert_eq!(merged, dest, "corrupt incoming leaves dest untouched");
        assert_eq!(added, 0);
    }

    #[test]
    fn a_dest_that_is_not_an_object_degrades_to_raw_overwrite() {
        // A dest that parses but is not an object is unusable as a merge
        // base: a valid incoming side wins.
        let valid = bindings(&[("a@dir", "github.com/o/r#plugins/a")]);
        let (merged, _) = merge_plugin_bindings(b"[1,2]", &valid);
        assert_eq!(merged, valid);
    }

    #[test]
    fn top_level_fields_come_from_the_incoming_side() {
        let dest = b"{\"schemaVersion\":1,\"bindings\":{\"a@dir\":{\"repositoryKey\":\"old\"}},\"staleField\":true}";
        let incoming = b"{\"schemaVersion\":2,\"bindings\":{}}";

        let (merged, _) = merge_plugin_bindings(dest, incoming);

        let v: Value = serde_json::from_slice(&merged).unwrap();
        assert_eq!(v["schemaVersion"], 2, "schema version follows incoming");
        assert!(v.get("staleField").is_none(), "dest-only fields drop");
    }
}
