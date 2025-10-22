use git2::Repository;

fn main() {
    let repo_path = "/root/.config/claude-code-sync/repo";

    println!("Testing git2 status detection...");
    println!("Repository: {}", repo_path);
    println!();

    // Open repository
    let repo = Repository::open(repo_path).expect("Failed to open repo");

    // Test 1: Default statuses (what current code does)
    println!("=== Test 1: Default statuses (current implementation) ===");
    let statuses = repo.statuses(None).expect("Failed to get statuses");
    println!("Total status entries: {}", statuses.len());

    if statuses.is_empty() {
        println!("No changes detected");
    } else {
        println!("Changes detected:");
        for (i, entry) in statuses.iter().take(10).enumerate() {
            let path = entry.path().unwrap_or("<unknown>");
            let status = entry.status();
            println!("  {}: {} - {:?}", i+1, path, status);
        }
        if statuses.len() > 10 {
            println!("  ... and {} more", statuses.len() - 10);
        }
    }
    println!();

    // Test 2: Exclude untracked and ignored (what we should do)
    println!("=== Test 2: Exclude untracked and ignored files ===");
    let mut opts = git2::StatusOptions::new();
    opts.include_untracked(false);
    opts.include_ignored(false);

    let statuses2 = repo.statuses(Some(&mut opts)).expect("Failed to get statuses");
    println!("Total status entries: {}", statuses2.len());

    if statuses2.is_empty() {
        println!("No changes detected");
    } else {
        println!("Changes detected:");
        for (i, entry) in statuses2.iter().take(10).enumerate() {
            let path = entry.path().unwrap_or("<unknown>");
            let status = entry.status();
            println!("  {}: {} - {:?}", i+1, path, status);
        }
        if statuses2.len() > 10 {
            println!("  ... and {} more", statuses2.len() - 10);
        }
    }
    println!();

    // Test 3: Only show index and workdir modifications
    println!("=== Test 3: Only tracked file modifications ===");
    let mut opts3 = git2::StatusOptions::new();
    opts3.include_untracked(false);
    opts3.include_ignored(false);
    opts3.include_unmodified(false);

    let statuses3 = repo.statuses(Some(&mut opts3)).expect("Failed to get statuses");
    println!("Total status entries: {}", statuses3.len());

    if statuses3.is_empty() {
        println!("No changes detected");
    } else {
        println!("Changes detected:");
        for (i, entry) in statuses3.iter().enumerate() {
            let path = entry.path().unwrap_or("<unknown>");
            let status = entry.status();
            println!("  {}: {} - {:?}", i+1, path, status);
        }
    }
    println!();

    println!("=== Comparison with git command ===");
    println!("Now run: cd {} && git status --porcelain", repo_path);
}
