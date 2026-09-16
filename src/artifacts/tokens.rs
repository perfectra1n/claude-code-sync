//! Machine-neutral paths: this machine's absolute locations are stored in the
//! sync repository as tokens and rendered back on the way out, so a hook
//! command or a status line still resolves on a machine whose home directory
//! or `~/.claude` location differs. Any absolute path in the file is
//! neutralized, not specific keys.
//!
//! One type owns both directions: whatever `to_machine` renders, `to_repo`
//! must undo, or two machines rewrite each other's values on every sync.

use std::path::Path;

/// Stands for this machine's `~`.
pub const HOME_TOKEN: &str = "__HOME__";
/// Stands for this machine's `~/.claude`. Tokenized before the home directory,
/// since it is configurable and may sit outside it.
pub const CLAUDE_DIR_TOKEN: &str = "__CLAUDE_DIR__";

/// This machine's absolute locations and their neutral spellings.
#[derive(Debug, Clone, Default)]
pub struct PathTokens {
    home: String,
    claude_dir: String,
}

impl PathTokens {
    /// Tokens for the machine whose Claude directory is `claude_dir`.
    pub fn for_claude_dir(claude_dir: &Path) -> Self {
        PathTokens {
            home: dirs::home_dir()
                .map(|path| path.to_string_lossy().to_string())
                .unwrap_or_default(),
            claude_dir: claude_dir.to_string_lossy().to_string(),
        }
    }

    /// Machine bytes -> repo bytes. Non-UTF-8 content passes through unchanged.
    pub fn to_repo(&self, bytes: &[u8]) -> Vec<u8> {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };
        let text = replace_path(text, &self.claude_dir, CLAUDE_DIR_TOKEN);
        let text = replace_path(&text, &self.home, HOME_TOKEN);
        text.into_bytes()
    }

    /// Repo bytes -> machine bytes. Non-UTF-8 content passes through unchanged.
    pub fn to_machine(&self, bytes: &[u8]) -> Vec<u8> {
        let Ok(text) = std::str::from_utf8(bytes) else {
            return bytes.to_vec();
        };
        let text = text.replace(CLAUDE_DIR_TOKEN, &self.claude_dir);
        let text = text.replace(HOME_TOKEN, &self.home);
        text.into_bytes()
    }
}

/// Replace `needle` with `token` only where the match is a whole path prefix:
/// `/home/user` must not eat the `/home/username` of a machine that is not this one,
/// nor the tail of an unrelated `/opt/home/user`.
fn replace_path(text: &str, needle: &str, token: &str) -> String {
    if needle.is_empty() {
        return text.to_string();
    }

    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(needle) {
        let before = &rest[..start];
        let after = &rest[start + needle.len()..];
        out.push_str(before);
        if continues_a_segment(before.chars().next_back())
            || continues_a_segment(after.chars().next())
        {
            out.push_str(needle);
        } else {
            out.push_str(token);
        }
        rest = after;
    }
    out.push_str(rest);
    out
}

/// Whether a character next to a match makes it part of a longer path segment,
/// which is what tells `/home/username` and `/opt/home/user` from `/home/user` itself.
fn continues_a_segment(neighbour: Option<char>) -> bool {
    match neighbour {
        Some(character) => {
            character.is_alphanumeric() || matches!(character, '-' | '_' | '.' | '~')
        }
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tokens() -> PathTokens {
        PathTokens {
            home: "/home/user".to_string(),
            claude_dir: "/home/user/.claude".to_string(),
        }
    }

    #[test]
    fn tokenizes_the_claude_dir_before_the_home_directory() {
        let live = br#"{"hooks":{"Stop":[{"command":"/home/user/.claude/hooks/h.sh"}]}}"#;
        let stored = String::from_utf8(tokens().to_repo(live)).unwrap();
        assert!(stored.contains("__CLAUDE_DIR__/hooks/h.sh"), "{stored}");
        assert!(!stored.contains("/home/user"));
    }

    #[test]
    fn renders_tokens_back_to_this_machine() {
        let stored = b"node __CLAUDE_DIR__/plugins/x.js and __HOME__/bin/y";
        let live = String::from_utf8(tokens().to_machine(stored)).unwrap();
        assert_eq!(
            live,
            "node /home/user/.claude/plugins/x.js and /home/user/bin/y"
        );
    }

    #[test]
    fn a_round_trip_returns_the_original_bytes() {
        let live = br#"{"statusLine":{"command":"/home/user/bin/line --dir /home/user/.claude"}}"#;
        let stored = tokens().to_repo(live);
        assert_eq!(tokens().to_machine(&stored), live.to_vec());
    }

    #[test]
    fn another_users_home_is_left_alone() {
        let live = b"/home/username/work and /home/user/work";
        let stored = String::from_utf8(tokens().to_repo(live)).unwrap();
        assert_eq!(stored, "/home/username/work and __HOME__/work");
    }

    #[test]
    fn a_path_that_merely_contains_this_home_is_left_alone() {
        let live = b"/opt/home/user/x and /home/user/x";
        let stored = String::from_utf8(tokens().to_repo(live)).unwrap();
        assert_eq!(stored, "/opt/home/user/x and __HOME__/x");
    }

    #[test]
    fn non_utf8_content_passes_through() {
        let bytes = [0xff, 0xfe, 0x00, 0x01];
        assert_eq!(tokens().to_repo(&bytes), bytes.to_vec());
        assert_eq!(tokens().to_machine(&bytes), bytes.to_vec());
    }

    #[test]
    fn an_unknown_home_directory_changes_nothing() {
        let tokens = PathTokens {
            home: String::new(),
            claude_dir: "/srv/claude".to_string(),
        };
        let live = b"/home/user/x";
        assert_eq!(tokens.to_repo(live), live.to_vec());
    }
}
