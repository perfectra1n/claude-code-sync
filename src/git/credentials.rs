use git2::{Cred, CredentialType};

/// Credential callback for git operations
///
/// This function is used by git2 to request credentials during remote operations.
/// It tries multiple authentication methods in order:
/// 1. Git credential helper (reads from git config)
/// 2. SSH agent (for SSH URLs)
pub(super) fn credential_callback(
    _url: &str,
    username_from_url: Option<&str>,
    _allowed_types: CredentialType,
) -> Result<Cred, git2::Error> {
    // Try credential helper first
    git2::Cred::credential_helper(&git2::Config::open_default()?, _url, username_from_url)
        // Fall back to SSH agent if credential helper fails
        .or_else(|_| git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git")))
}
