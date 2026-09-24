//! Canonical project identity: an `id -> absolute path` table, configured per
//! machine, so one project syncs under one name when home directories, user
//! names or checkout paths differ.
//!
//! Resolution order in both directions: the map, then `use_project_name_only`,
//! then the encoded directory name. The map comes first because it survives a
//! rename and stays unambiguous when two checkouts share a folder name.

use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::filter::FilterConfig;
use crate::sync::discovery::{extract_project_name, find_local_project_by_name};

/// Canonical id -> absolute project path, as configured per machine.
pub type ProjectMap = BTreeMap<String, PathBuf>;

/// Encode a project path the way Claude Code names `~/.claude/projects/<dir>`.
///
/// Claude Code derives the directory with a JavaScript regex, which runs over
/// UTF-16 code units. A byte-wise version emits a different name for any
/// non-ASCII path, and files then land in a directory Claude Code never reads.
pub fn encode_project_path(path: &Path) -> String {
    path.to_string_lossy()
        .encode_utf16()
        .map(|unit| match u8::try_from(unit) {
            Ok(byte) if byte.is_ascii_alphanumeric() => byte as char,
            _ => '-',
        })
        .collect()
}

/// The spelling that encodes to the directory Claude Code created: no trailing
/// separator, no `.` components.
pub fn normalize_project_path(path: &Path) -> PathBuf {
    path.components().collect()
}

/// Reject a map that cannot be applied: an id becomes a directory name in the
/// sync repository, a path must be absolute, and a path spelled differently
/// from the project's own (a trailing slash) encodes to a different directory.
pub fn validate(map: &ProjectMap) -> Result<()> {
    for (id, path) in map {
        if id.is_empty() || id.contains('/') || id.contains('\\') || id == "." || id == ".." {
            bail!("project_map id {id:?} must be a plain directory name");
        }
        if !path.is_absolute() {
            bail!(
                "project_map entry {id:?} must be an absolute path, got {}",
                path.display()
            );
        }
        // Compared as text: `Path` equality ignores a trailing separator,
        // `encode_project_path` turns it into another dash.
        if normalize_project_path(path).to_string_lossy() != path.to_string_lossy() {
            bail!(
                "project_map entry {id:?} must be written exactly as the project's path, got {} \
                 (try {})",
                path.display(),
                normalize_project_path(path).display()
            );
        }
    }
    ensure_no_encoding_collisions(map)
}

/// Reject a map in which two ids encode to one directory: their files would
/// merge under both ids, and nothing could separate them afterwards.
pub fn ensure_no_encoding_collisions(map: &ProjectMap) -> Result<()> {
    let mut seen: BTreeMap<String, &String> = BTreeMap::new();
    for (id, path) in map {
        let encoded = encode_project_path(path);
        if let Some(previous) = seen.insert(encoded.clone(), id) {
            bail!("project_map entries {previous:?} and {id:?} both encode to {encoded:?}");
        }
    }
    Ok(())
}

/// The canonical id of a local encoded project directory, when one is mapped.
pub fn canonical_id(map: &ProjectMap, encoded_dir: &str) -> Option<String> {
    map.iter()
        .find(|(_, path)| encode_project_path(path) == encoded_dir)
        .map(|(id, _)| id.clone())
}

/// The directory name a local encoded project directory takes in the sync repo.
pub fn repo_dir_name(filter: &FilterConfig, encoded_dir: &str) -> String {
    if let Some(id) = canonical_id(&filter.project_map, encoded_dir) {
        return id;
    }
    if filter.use_project_name_only {
        return extract_project_name(encoded_dir).to_string();
    }
    encoded_dir.to_string()
}

/// [`repo_dir_name`] for a project directory under `projects_dir`. In
/// `use_project_name_only` mode the name comes from the working directory a
/// transcript in it records, the same name its sessions are pushed under:
/// the encoded directory alone cannot tell `shop-web` from `shop/web`, and
/// its last dash-separated piece would file this project's memory and
/// attachments apart from its sessions.
pub fn repo_dir_name_in(filter: &FilterConfig, projects_dir: &Path, encoded_dir: &str) -> String {
    if filter.use_project_name_only && canonical_id(&filter.project_map, encoded_dir).is_none() {
        if let Some(name) = project_name_from_transcripts(&projects_dir.join(encoded_dir)) {
            return name;
        }
    }
    repo_dir_name(filter, encoded_dir)
}

/// The folder name of the first `cwd` a transcript directly inside
/// `project_dir` records, if any does.
fn project_name_from_transcripts(project_dir: &Path) -> Option<String> {
    use std::io::BufRead;

    let mut transcripts: Vec<PathBuf> = std::fs::read_dir(project_dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_file() && p.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    transcripts.sort();

    for transcript in transcripts {
        let Ok(file) = std::fs::File::open(&transcript) else {
            continue;
        };
        for line in std::io::BufReader::new(file).lines().map_while(|l| l.ok()) {
            let Ok(entry) = serde_json::from_str::<serde_json::Value>(&line) else {
                continue;
            };
            if let Some(name) = entry
                .get("cwd")
                .and_then(|cwd| cwd.as_str())
                .and_then(|cwd| Path::new(cwd).file_name())
                .and_then(|name| name.to_str())
            {
                return Some(name.to_string());
            }
        }
    }
    None
}

/// The local project directory a sync-repo directory name belongs to, or
/// `None` when this machine has no destination for it.
pub fn local_project_dir(
    filter: &FilterConfig,
    projects_dir: &Path,
    repo_dir_name: &str,
) -> Option<PathBuf> {
    if let Some(path) = filter.project_map.get(repo_dir_name) {
        return Some(projects_dir.join(encode_project_path(path)));
    }
    if filter.use_project_name_only {
        return find_local_project_by_name(projects_dir, repo_dir_name);
    }
    // An existing directory, or an encoded path Claude Code would create.
    // Anything else is another machine's canonical id and has no destination
    // here until this machine maps it too.
    let candidate = projects_dir.join(repo_dir_name);
    if candidate.is_dir() || is_encoded_project_path(repo_dir_name) {
        return Some(candidate);
    }
    None
}

/// Files a pull has no local project for, grouped by the sync-repo directory
/// name they came from.
pub type SkippedByProject = BTreeMap<String, Vec<PathBuf>>;

/// Split a path relative to the sync repo's projects directory into the
/// project directory it belongs to and the path inside that project. A file
/// lying beside the project directories belongs to no project, and nothing on
/// this machine can take it.
pub fn split_project_path(relative: &Path) -> Option<(&str, &Path)> {
    let mut parts = relative.components();
    let project = parts.next()?.as_os_str().to_str()?;
    let inside_project = parts.as_path();
    if inside_project.as_os_str().is_empty() {
        return None;
    }
    Some((project, inside_project))
}

/// Fold one set of misses into another, so a project whose sessions and whose
/// attachments were both skipped is reported once rather than twice.
pub fn merge_skipped(into: &mut SkippedByProject, more: &SkippedByProject) {
    for (project, files) in more {
        into.entry(project.clone())
            .or_default()
            .extend(files.iter().cloned());
    }
}

/// What to do about a project this machine cannot place, in both warning modes.
const MAPPING_REMEDY: &str = "Map each one: claude-code-sync config --map-project <id>=<path>.";

/// The warning lines for files no local project claims: one line naming every
/// project by default, one line per file when `each_file` is set.
///
/// A project this machine has not mapped holds every file it ever had, and a
/// line each buries the one remedy under hundreds of repetitions of it.
pub fn skipped_project_warnings(skipped: &SkippedByProject, each_file: bool) -> Vec<String> {
    if skipped.is_empty() {
        return Vec::new();
    }

    if each_file {
        let mut lines: Vec<String> = skipped
            .iter()
            .flat_map(|(project, files)| {
                files.iter().map(move |file| {
                    format!(
                        "Skipping {} (no local project for '{project}')",
                        file.display()
                    )
                })
            })
            .collect();
        lines.push(MAPPING_REMEDY.to_string());
        return lines;
    }

    let total_files: usize = skipped.values().map(Vec::len).sum();
    let project_list: Vec<String> = skipped
        .iter()
        .map(|(project, files)| format!("{project} ({})", files.len()))
        .collect();

    vec![format!(
        "Skipped {total_files} file(s) with no local project: {}. {MAPPING_REMEDY} \
         A line per file: config --warn-each-skipped-file true.",
        project_list.join(", ")
    )]
}

/// Whether a directory name is an encoded project path. Encoding an absolute
/// path puts a dash where its root is: `/home/user/app` becomes
/// `-home-user-app`, `C:\src\app` becomes `C--src-app`.
fn is_encoded_project_path(name: &str) -> bool {
    name.starts_with('-') || name.get(1..3) == Some("--")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mapped(entries: &[(&str, &str)]) -> FilterConfig {
        FilterConfig {
            project_map: entries
                .iter()
                .map(|(id, path)| ((*id).to_string(), PathBuf::from(path)))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn encodes_every_non_alphanumeric_unit_as_a_dash() {
        assert_eq!(
            encode_project_path(Path::new("/home/user/.config-dir")),
            "-home-user--config-dir"
        );
        assert_eq!(
            encode_project_path(Path::new("/home/username/projects/site.example")),
            "-home-username-projects-site-example"
        );
    }

    #[test]
    fn encodes_by_utf16_unit_not_by_byte() {
        assert_eq!(encode_project_path(Path::new("a-\u{17e}_b")), "a---b");
    }

    #[test]
    fn mapped_project_uses_its_canonical_id_in_the_repo() {
        let filter = mapped(&[("shop", "/home/user/projects/shop-web")]);
        let encoded = "-home-user-projects-shop-web";
        assert_eq!(repo_dir_name(&filter, encoded), "shop");
    }

    #[test]
    fn mapped_id_resolves_to_this_machines_path_even_when_renamed() {
        let filter = mapped(&[("shop", "/home/username/work/shop")]);
        let projects = PathBuf::from("/home/username/.claude/projects");
        assert_eq!(
            local_project_dir(&filter, &projects, "shop"),
            Some(projects.join("-home-username-work-shop"))
        );
    }

    #[test]
    fn unmapped_project_keeps_the_encoded_directory() {
        let filter = FilterConfig::default();
        let encoded = "-home-user-projects-other";
        assert_eq!(repo_dir_name(&filter, encoded), encoded);
        let projects = PathBuf::from("/home/user/.claude/projects");
        assert_eq!(
            local_project_dir(&filter, &projects, encoded),
            Some(projects.join(encoded))
        );
    }

    #[test]
    fn unmapped_project_still_falls_back_to_name_only() {
        let filter = FilterConfig {
            use_project_name_only: true,
            ..Default::default()
        };
        assert_eq!(repo_dir_name(&filter, "-home-user-work-myapp"), "myapp");
    }

    #[test]
    fn name_only_files_attachments_under_the_same_name_as_sessions() {
        let projects = tempfile::TempDir::new().unwrap();
        let encoded = "-home-user-shop-web";
        std::fs::create_dir_all(projects.path().join(encoded).join("memory")).unwrap();
        std::fs::write(
            projects.path().join(encoded).join("s1.jsonl"),
            "{\"type\":\"summary\"}\n{\"type\":\"user\",\"cwd\":\"/home/user/shop-web\"}\n",
        )
        .unwrap();
        let filter = FilterConfig {
            use_project_name_only: true,
            ..Default::default()
        };

        assert_eq!(
            repo_dir_name_in(&filter, projects.path(), encoded),
            "shop-web",
            "the folder name, not its last dash-separated piece"
        );
        assert_eq!(
            repo_dir_name_in(&filter, projects.path(), "-home-user-no-transcripts"),
            "transcripts",
            "without a transcript it falls back to the old guess"
        );
    }

    #[test]
    fn the_map_wins_over_name_only_so_two_checkouts_stay_apart() {
        let mut filter = mapped(&[("work-myapp", "/home/user/work/myapp")]);
        filter.use_project_name_only = true;
        assert_eq!(
            repo_dir_name(&filter, "-home-user-work-myapp"),
            "work-myapp"
        );
        assert_eq!(repo_dir_name(&filter, "-home-user-personal-myapp"), "myapp");
    }

    #[test]
    fn an_id_that_is_not_a_plain_directory_name_is_refused() {
        for bad in ["../escape", "nested/id", "", "."] {
            let map: ProjectMap = [(bad.to_string(), PathBuf::from("/home/user/app"))]
                .into_iter()
                .collect();
            assert!(validate(&map).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn a_path_spelled_differently_from_the_encoded_one_is_refused() {
        let map: ProjectMap = [("app".to_string(), PathBuf::from("/home/user/work/app/"))]
            .into_iter()
            .collect();
        assert!(
            validate(&map).is_err(),
            "a trailing slash encodes differently"
        );
        assert_eq!(
            normalize_project_path(Path::new("/home/user/work/app/")),
            PathBuf::from("/home/user/work/app")
        );
    }

    #[test]
    fn another_machines_canonical_id_has_no_destination_here() {
        let filter = FilterConfig::default();
        let projects = PathBuf::from("/home/user/.claude/projects");

        for id in ["app", "a-b", "work-myapp"] {
            assert_eq!(local_project_dir(&filter, &projects, id), None, "{id}");
        }
        for encoded in ["-home-user-app", "-x", "C--src-app"] {
            assert_eq!(
                local_project_dir(&filter, &projects, encoded),
                Some(projects.join(encoded)),
                "{encoded}"
            );
        }
    }

    #[test]
    fn a_project_directory_this_machine_already_has_is_a_destination() {
        let projects = tempfile::tempdir().unwrap();
        std::fs::create_dir(projects.path().join("app")).unwrap();

        assert_eq!(
            local_project_dir(&FilterConfig::default(), projects.path(), "app"),
            Some(projects.path().join("app")),
            "an existing directory is one Claude Code reads, whatever its name"
        );
    }

    #[test]
    fn a_relative_project_path_is_refused() {
        let map: ProjectMap = [("app".to_string(), PathBuf::from("work/app"))]
            .into_iter()
            .collect();
        assert!(validate(&map).is_err());
    }

    fn skipped(entries: &[(&str, &[&str])]) -> SkippedByProject {
        entries
            .iter()
            .map(|(project, files)| {
                let paths = files.iter().map(PathBuf::from).collect();
                ((*project).to_string(), paths)
            })
            .collect()
    }

    #[test]
    fn every_miss_is_combined_into_one_warning() {
        let misses = skipped(&[
            (
                "shop",
                &["/repo/projects/shop/a.png", "/repo/projects/shop/b.png"],
            ),
            ("blog", &["/repo/projects/blog/c.png"]),
        ]);

        let lines = skipped_project_warnings(&misses, false);

        assert_eq!(
            lines.len(),
            1,
            "one warning, however many files were missed"
        );
        assert!(lines[0].contains("Skipped 3 file(s)"));
        assert!(lines[0].contains("blog (1)"));
        assert!(lines[0].contains("shop (2)"));
        assert!(
            lines[0].contains("--map-project"),
            "the remedy stays visible"
        );
    }

    #[test]
    fn each_file_gets_its_own_warning_when_asked() {
        let misses = skipped(&[
            (
                "shop",
                &["/repo/projects/shop/a.png", "/repo/projects/shop/b.png"],
            ),
            ("blog", &["/repo/projects/blog/c.png"]),
        ]);

        let lines = skipped_project_warnings(&misses, true);

        assert_eq!(lines.len(), 4, "one line per skipped file, plus the remedy");
        assert_eq!(
            lines[0],
            "Skipping /repo/projects/blog/c.png (no local project for 'blog')"
        );
        assert!(
            lines[3].contains("--map-project"),
            "the remedy stays visible in per-file mode too"
        );
    }

    #[test]
    fn a_projects_sessions_and_attachments_share_one_warning() {
        let mut skipped_sessions = skipped(&[("shop", &["/repo/projects/shop/sess-1.jsonl"])]);
        let skipped_attachments = skipped(&[("shop", &["/repo/projects/shop/diagram.png"])]);

        merge_skipped(&mut skipped_sessions, &skipped_attachments);
        let lines = skipped_project_warnings(&skipped_sessions, false);

        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("Skipped 2 file(s)"));
        assert!(lines[0].contains("shop (2)"), "named once, counted twice");
    }

    #[test]
    fn nothing_skipped_warns_about_nothing() {
        let nothing = SkippedByProject::new();

        assert!(skipped_project_warnings(&nothing, false).is_empty());
        assert!(skipped_project_warnings(&nothing, true).is_empty());
    }

    #[test]
    fn colliding_ids_are_refused() {
        let map: ProjectMap = [
            ("one".to_string(), PathBuf::from("/home/user/app")),
            ("two".to_string(), PathBuf::from("/home/user/app")),
        ]
        .into_iter()
        .collect();
        assert!(ensure_no_encoding_collisions(&map).is_err());
        assert!(ensure_no_encoding_collisions(&ProjectMap::new()).is_ok());
    }
}
