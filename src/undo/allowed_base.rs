//! The one allowed-base computation every snapshot-driven write is
//! sandboxed to (files AND artifact records): the given directory when
//! present, the home directory otherwise — canonicalized, so symlinked
//! spellings cannot slip a path past the boundary.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Resolve and canonicalize the base directory every restored path must
/// live under.
pub(crate) fn allowed_base(allowed: Option<&Path>) -> Result<PathBuf> {
    match allowed {
        Some(base) => base
            .canonicalize()
            .with_context(|| format!("Failed to canonicalize base directory: {}", base.display())),
        None => dirs::home_dir()
            .context("Failed to get home directory")?
            .canonicalize()
            .context("Failed to canonicalize home directory"),
    }
}
