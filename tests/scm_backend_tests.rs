//! Backend-agnostic SCM tests.
//!
//! These tests are parameterized to run against all available SCM backends.
//! Currently supports Git only. When Mercurial is added, simply add
//! `#[case::mercurial(Backend::Mercurial)]` to each test.

use claude_code_sync::scm::{self, Backend};
use rstest::rstest;
use std::fs;
use tempfile::TempDir;

// =============================================================================
// Repository Lifecycle Tests
// =============================================================================

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_init_creates_marker(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let _repo = scm::init_with_backend(temp.path(), backend).unwrap();

    assert!(
        temp.path().join(backend.marker()).exists(),
        "Expected {} marker to exist",
        backend.marker()
    );
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_open_after_init(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let _repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Re-open should succeed
    let reopened = scm::open(temp.path());
    assert!(reopened.is_ok(), "Failed to reopen repository");
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_open_non_repo_fails(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let result = scm::open(temp.path());
    assert!(result.is_err(), "Opening non-repo should fail");
}

// =============================================================================
// Staging and Commit Tests
// =============================================================================

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_has_changes_empty_repo(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Empty repo should have no changes
    assert!(!repo.has_changes().unwrap());
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_has_changes_after_file_create(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Create a file
    fs::write(temp.path().join("test.txt"), "hello world").unwrap();

    // Should detect changes
    assert!(repo.has_changes().unwrap());
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_stage_and_commit(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Create and commit a file
    fs::write(temp.path().join("test.txt"), "hello world").unwrap();
    assert!(repo.has_changes().unwrap());

    repo.stage_all().unwrap();
    repo.commit("Initial commit").unwrap();

    // No more changes after commit
    assert!(!repo.has_changes().unwrap());
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_current_commit_hash(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Create initial commit
    fs::write(temp.path().join("test.txt"), "content").unwrap();
    repo.stage_all().unwrap();
    repo.commit("Initial commit").unwrap();

    let hash = repo.current_commit_hash().unwrap();

    // Hash should be non-empty hex string
    assert!(!hash.is_empty(), "Commit hash should not be empty");
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "Commit hash should be hex: {}",
        hash
    );
    // Git uses 40-char SHA-1, Hg uses 40-char or 12-char
    assert!(hash.len() >= 12, "Hash should be at least 12 chars");
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_current_branch(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Need a commit to have a branch
    fs::write(temp.path().join("test.txt"), "content").unwrap();
    repo.stage_all().unwrap();
    repo.commit("Initial commit").unwrap();

    let branch = repo.current_branch().unwrap();

    // Should have a branch name
    assert!(!branch.is_empty(), "Branch name should not be empty");
    // Git: "main" or "master", Hg: "default"
}

// =============================================================================
// Remote Operations Tests
// =============================================================================

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_add_remote(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    assert!(!repo.has_remote("origin"));

    repo.add_remote("origin", "https://example.com/repo.git")
        .unwrap();

    assert!(repo.has_remote("origin"));
    assert!(!repo.has_remote("upstream"));
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_get_remote_url(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    let url = "https://example.com/repo.git";
    repo.add_remote("origin", url).unwrap();

    let retrieved = repo.get_remote_url("origin").unwrap();
    assert_eq!(retrieved, url);
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_set_remote_url(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    repo.add_remote("origin", "https://example.com/old.git")
        .unwrap();

    let new_url = "https://example.com/new.git";
    repo.set_remote_url("origin", new_url).unwrap();

    let retrieved = repo.get_remote_url("origin").unwrap();
    assert_eq!(retrieved, new_url);
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_remove_remote(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    repo.add_remote("origin", "https://example.com/repo.git")
        .unwrap();
    assert!(repo.has_remote("origin"));

    repo.remove_remote("origin").unwrap();
    assert!(!repo.has_remote("origin"));
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_list_remotes(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Initially empty
    let remotes = repo.list_remotes().unwrap();
    assert!(remotes.is_empty());

    // Add remotes
    repo.add_remote("origin", "https://example.com/origin.git")
        .unwrap();
    repo.add_remote("upstream", "https://example.com/upstream.git")
        .unwrap();

    let remotes = repo.list_remotes().unwrap();
    assert_eq!(remotes.len(), 2);
    assert!(remotes.contains(&"origin".to_string()));
    assert!(remotes.contains(&"upstream".to_string()));
}

// =============================================================================
// Reset Operations Tests
// =============================================================================

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_reset_soft(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let repo = scm::init_with_backend(temp.path(), backend).unwrap();

    // Create first commit
    fs::write(temp.path().join("file1.txt"), "content1").unwrap();
    repo.stage_all().unwrap();
    repo.commit("First commit").unwrap();
    let first_hash = repo.current_commit_hash().unwrap();

    // Create second commit
    fs::write(temp.path().join("file2.txt"), "content2").unwrap();
    repo.stage_all().unwrap();
    repo.commit("Second commit").unwrap();
    let second_hash = repo.current_commit_hash().unwrap();

    assert_ne!(first_hash, second_hash);

    // Reset to first commit
    repo.reset_soft(&first_hash).unwrap();

    // Should be back at first commit
    let current_hash = repo.current_commit_hash().unwrap();
    assert_eq!(current_hash, first_hash);

    // File should still exist (soft reset keeps working directory)
    assert!(temp.path().join("file2.txt").exists());
}

// =============================================================================
// Backend Detection Tests
// =============================================================================

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_detect_backend(#[case] backend: Backend) {
    if !backend.is_available() {
        eprintln!("Skipping: {:?} not installed", backend);
        return;
    }

    let temp = TempDir::new().unwrap();
    let _repo = scm::init_with_backend(temp.path(), backend).unwrap();

    let detected = scm::detect_backend(temp.path());
    assert_eq!(detected, Some(backend));
}

#[test]
fn test_detect_backend_none() {
    let temp = TempDir::new().unwrap();
    let detected = scm::detect_backend(temp.path());
    assert_eq!(detected, None);
}

#[rstest]
#[case::git(Backend::Git)]
#[case::mercurial(Backend::Mercurial)]
fn test_is_available(#[case] backend: Backend) {
    // This test just verifies the method doesn't panic
    let _ = backend.is_available();
}

// =============================================================================
// FilterConfig Backend Selection Tests
// =============================================================================

use claude_code_sync::filter::FilterConfig;

#[test]
fn test_lfs_requires_git_backend() {
    // LFS + mercurial should error
    let config = FilterConfig {
        enable_lfs: true,
        scm_backend: "mercurial".to_string(),
        ..Default::default()
    };
    let result = config.validate();
    assert!(result.is_err(), "LFS with mercurial should fail validation");
    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("LFS") && err.contains("git"),
        "Error should mention LFS and git: {}",
        err
    );
}

#[test]
fn test_lfs_with_git_backend_ok() {
    // LFS + git should be fine
    let config = FilterConfig {
        enable_lfs: true,
        scm_backend: "git".to_string(),
        ..Default::default()
    };
    assert!(
        config.validate().is_ok(),
        "LFS with git should pass validation"
    );
}

#[test]
fn test_backend_selection_git() {
    let config = FilterConfig {
        scm_backend: "git".to_string(),
        ..Default::default()
    };
    assert_eq!(config.backend().unwrap(), Backend::Git);
}

#[test]
fn test_backend_selection_mercurial() {
    let config = FilterConfig {
        scm_backend: "mercurial".to_string(),
        ..Default::default()
    };
    assert_eq!(config.backend().unwrap(), Backend::Mercurial);
}

#[test]
fn test_backend_selection_hg_alias() {
    // "hg" should also work as an alias for mercurial
    let config = FilterConfig {
        scm_backend: "hg".to_string(),
        ..Default::default()
    };
    assert_eq!(config.backend().unwrap(), Backend::Mercurial);
}

#[test]
fn test_backend_selection_invalid() {
    let config = FilterConfig {
        scm_backend: "svn".to_string(),
        ..Default::default()
    };
    assert!(
        config.backend().is_err(),
        "Invalid backend should fail"
    );
}

#[test]
fn test_default_backend_is_git() {
    let config = FilterConfig::default();
    assert_eq!(config.scm_backend, "git");
    assert_eq!(config.backend().unwrap(), Backend::Git);
}
