use anyhow::{anyhow, bail, Context, Result};
use colored::Colorize;
use semver::Version;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::Path;

const REPO_URL: &str = "https://github.com/perfectra1n/claude-code-sync";
const BIN_NAME: &str = if cfg!(windows) {
    "claude-code-sync.exe"
} else {
    "claude-code-sync"
};

/// Largest download accepted. The release archives are a few MB; this only
/// guards against an unbounded read if a URL ever serves something else.
const MAX_DOWNLOAD_BYTES: u64 = 200 * 1024 * 1024;

/// The release asset built for the platform this binary was compiled for,
/// mirroring the matrix in `.github/workflows/build.yml`.
///
/// Keyed on the compile target rather than the running OS, so a musl build
/// updates to musl and an x86_64 build running under emulation stays x86_64.
fn release_asset() -> Option<&'static str> {
    let asset = match (
        std::env::consts::OS,
        std::env::consts::ARCH,
        cfg!(target_env = "musl"),
    ) {
        ("linux", "x86_64", false) => "claude-code-sync-linux-x86_64.tar.gz",
        ("linux", "x86_64", true) => "claude-code-sync-linux-x86_64-musl.tar.gz",
        ("linux", "aarch64", false) => "claude-code-sync-linux-aarch64.tar.gz",
        ("linux", "aarch64", true) => "claude-code-sync-linux-aarch64-musl.tar.gz",
        ("macos", "x86_64", _) => "claude-code-sync-macos-x86_64.tar.gz",
        ("macos", "aarch64", _) => "claude-code-sync-macos-aarch64.tar.gz",
        ("windows", "x86_64", _) => "claude-code-sync-windows-x86_64.exe.zip",
        ("windows", "aarch64", _) => "claude-code-sync-windows-aarch64.exe.zip",
        _ => return None,
    };
    Some(asset)
}

/// A package manager that owns the installed binary. Replacing its file in
/// place would desync the manager's records (or fail on a read-only store),
/// so `self-update` defers to it.
#[derive(Debug, PartialEq, Eq)]
enum Manager {
    Nix,
    Homebrew,
    Scoop,
    Cargo,
}

impl Manager {
    fn detect(exe: &Path) -> Option<Self> {
        // Normalise separators so one set of patterns covers Windows paths.
        let path = exe.to_string_lossy().replace('\\', "/").to_lowercase();
        if path.starts_with("/nix/store/") {
            Some(Manager::Nix)
        } else if path.contains("/cellar/") || path.contains("/homebrew/") {
            Some(Manager::Homebrew)
        } else if path.contains("/scoop/apps/") {
            Some(Manager::Scoop)
        } else if path.contains("/.cargo/bin/") {
            Some(Manager::Cargo)
        } else {
            None
        }
    }

    fn name(&self) -> &'static str {
        match self {
            Manager::Nix => "Nix",
            Manager::Homebrew => "Homebrew",
            Manager::Scoop => "Scoop",
            Manager::Cargo => "cargo",
        }
    }

    fn upgrade_hint(&self) -> &'static str {
        match self {
            Manager::Nix => "nix profile upgrade claude-code-sync  (or bump your flake input)",
            Manager::Homebrew => "brew upgrade claude-code-sync",
            Manager::Scoop => "scoop update claude-code-sync",
            Manager::Cargo => {
                "cargo binstall claude-code-sync  (or: cargo install claude-code-sync)"
            }
        }
    }
}

/// Normalise a user-supplied version to a release tag: `0.3.3` -> `v0.3.3`.
fn normalize_tag(version: &str) -> String {
    let version = version.trim();
    if version.starts_with('v') {
        version.to_string()
    } else {
        format!("v{version}")
    }
}

/// Parse the version out of a release tag, accepting a leading `v`.
fn parse_tag(tag: &str) -> Result<Version> {
    Version::parse(tag.trim_start_matches('v'))
        .with_context(|| format!("'{tag}' is not a valid release version"))
}

/// Extract the tag from the `Location` of a `/releases/latest` redirect,
/// e.g. `.../releases/tag/v0.3.3` -> `v0.3.3`.
fn tag_from_location(location: &str) -> Option<&str> {
    let (_, tag) = location.rsplit_once("/releases/tag/")?;
    let tag = tag.trim_end_matches('/');
    (!tag.is_empty() && !tag.contains('/')).then_some(tag)
}

/// The expected hash from a `<asset>.sha256` file (`<hex>  <filename>`).
fn parse_checksum(contents: &str) -> Option<String> {
    let hash = contents.split_whitespace().next()?.to_ascii_lowercase();
    (hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())).then_some(hash)
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn agent(follow_redirects: bool) -> ureq::Agent {
    let config = ureq::Agent::config_builder()
        .user_agent(concat!("claude-code-sync/", env!("CARGO_PKG_VERSION")))
        .timeout_global(Some(std::time::Duration::from_secs(120)))
        .max_redirects(if follow_redirects { 10 } else { 0 });
    #[cfg(windows)]
    let config = config.tls_config(
        ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::NativeTls)
            .build(),
    );
    config.build().new_agent()
}

/// Resolve the newest release tag.
///
/// Reads the redirect GitHub serves for `/releases/latest` instead of calling
/// the REST API, whose unauthenticated limit (60/hour, shared per IP) is easy
/// to exhaust behind a NAT or on CI.
fn latest_tag() -> Result<String> {
    let url = format!("{REPO_URL}/releases/latest");
    let response = agent(false)
        .get(&url)
        .call()
        .with_context(|| format!("failed to query {url}"))?;
    let location = response
        .headers()
        .get("location")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| {
            anyhow!(
                "{url} did not redirect to a release (HTTP {})",
                response.status()
            )
        })?;
    tag_from_location(location)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("unexpected redirect target for latest release: {location}"))
}

fn download(url: &str) -> Result<Vec<u8>> {
    agent(true)
        .get(url)
        .call()
        .with_context(|| format!("failed to download {url}"))?
        .body_mut()
        .with_config()
        .limit(MAX_DOWNLOAD_BYTES)
        .read_to_vec()
        .with_context(|| format!("failed to read {url}"))
}

/// Pull the binary out of a release archive.
#[cfg(not(windows))]
fn extract_binary(archive: &[u8]) -> Result<Vec<u8>> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    for entry in tar
        .entries()
        .context("release archive is not a valid .tar.gz")?
    {
        let mut entry = entry?;
        if entry.path()?.file_name() == Some(std::ffi::OsStr::new(BIN_NAME)) {
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes)?;
            return Ok(bytes);
        }
    }
    bail!("release archive does not contain {BIN_NAME}")
}

/// Pull the binary out of a release archive.
#[cfg(windows)]
fn extract_binary(archive: &[u8]) -> Result<Vec<u8>> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(archive))
        .context("release archive is not a valid .zip")?;
    let mut entry = zip
        .by_name(BIN_NAME)
        .with_context(|| format!("release archive does not contain {BIN_NAME}"))?;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// Swap the running executable for `binary`.
fn install_binary(binary: &[u8]) -> Result<()> {
    let mut staged = tempfile::NamedTempFile::new().context("failed to create a temp file")?;
    std::io::Write::write_all(&mut staged, binary)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged.path(), std::fs::Permissions::from_mode(0o755))?;
    }
    // self_replace handles the platform quirks: an atomic rename on Unix, and
    // on Windows moving the locked, running .exe aside before writing the new one.
    self_replace::self_replace(staged.path()).context(
        "failed to replace the running binary (is its directory writable? a system \
         path may need sudo)",
    )
}

/// Update this binary to the latest release, or to `target` if given.
///
/// With `check_only`, report whether an update is available and change
/// nothing. `force` reinstalls even when already current, and overrides the
/// refusal to touch a binary owned by a package manager.
pub fn self_update(check_only: bool, target: Option<&str>, force: bool) -> Result<()> {
    let current = parse_tag(env!("CARGO_PKG_VERSION"))?;
    let tag = match target {
        Some(v) => normalize_tag(v),
        None => latest_tag()?,
    };
    let wanted = parse_tag(&tag)?;

    let up_to_date = if target.is_some() {
        wanted == current
    } else {
        wanted <= current
    };

    if check_only {
        if up_to_date {
            println!("{} claude-code-sync {} is up to date", "✓".green(), current);
        } else {
            println!(
                "{} {} → {} (run {})",
                "Update available:".yellow().bold(),
                current,
                wanted.to_string().green(),
                "claude-code-sync self-update".cyan()
            );
        }
        return Ok(());
    }

    if up_to_date && !force {
        println!(
            "{} claude-code-sync {} is already installed",
            "✓".green(),
            current
        );
        return Ok(());
    }

    let exe = std::env::current_exe().context("cannot locate the running executable")?;
    let exe = exe.canonicalize().unwrap_or(exe);
    if let Some(manager) = Manager::detect(&exe) {
        if !force {
            bail!(
                "{} is managed by {}; update it with:\n  {}\n(pass --force to replace it anyway)",
                exe.display(),
                manager.name(),
                manager.upgrade_hint()
            );
        }
    }

    let asset = release_asset().ok_or_else(|| {
        anyhow!(
            "no prebuilt release for {}-{}; update with: cargo install claude-code-sync",
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    })?;
    let url = format!("{REPO_URL}/releases/download/{tag}/{asset}");

    println!("{} {} ({})...", "Downloading".cyan(), asset, tag);
    let archive = download(&url)?;
    let checksum_file = String::from_utf8(download(&format!("{url}.sha256"))?)
        .context("checksum file is not valid UTF-8")?;
    let expected = parse_checksum(&checksum_file)
        .ok_or_else(|| anyhow!("malformed checksum file for {asset}"))?;
    let actual = sha256_hex(&archive);
    if expected != actual {
        bail!("checksum mismatch for {asset}: expected {expected}, got {actual}");
    }

    let binary = extract_binary(&archive)?;
    install_binary(&binary)?;

    println!(
        "{} claude-code-sync {} → {} ({})",
        "Updated".green().bold(),
        current,
        wanted,
        exe.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_tag_adds_missing_v() {
        assert_eq!(normalize_tag("0.3.3"), "v0.3.3");
        assert_eq!(normalize_tag("v0.3.3"), "v0.3.3");
        assert_eq!(normalize_tag(" 1.0.0 "), "v1.0.0");
    }

    #[test]
    fn parse_tag_accepts_both_forms() {
        assert_eq!(parse_tag("v0.3.3").unwrap(), Version::new(0, 3, 3));
        assert_eq!(parse_tag("0.3.3").unwrap(), Version::new(0, 3, 3));
        assert!(parse_tag("latest").is_err());
    }

    #[test]
    fn tag_from_location_reads_release_redirect() {
        assert_eq!(
            tag_from_location("https://github.com/o/r/releases/tag/v0.3.3"),
            Some("v0.3.3")
        );
        assert_eq!(
            tag_from_location("https://github.com/o/r/releases/tag/v1.2.0/"),
            Some("v1.2.0")
        );
        // No releases yet: GitHub redirects to the releases index instead.
        assert_eq!(tag_from_location("https://github.com/o/r/releases"), None);
        assert_eq!(
            tag_from_location("https://github.com/o/r/releases/tag/"),
            None
        );
    }

    #[test]
    fn parse_checksum_reads_sha256sum_format() {
        let hash = "ac67e4bafff4ccffdd3e80fa148f1afa29e2b4ec2c4d5bbb52a16d1f81f32ff5";
        assert_eq!(
            parse_checksum(&format!("{hash}  claude-code-sync-linux-x86_64.tar.gz\n")),
            Some(hash.to_string())
        );
        assert_eq!(parse_checksum(&hash.to_uppercase()), Some(hash.to_string()));
        assert_eq!(parse_checksum("not-a-hash  file"), None);
        assert_eq!(parse_checksum(""), None);
    }

    #[test]
    fn sha256_hex_matches_known_digest() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn manager_detection_from_install_path() {
        let detect = |p: &str| Manager::detect(Path::new(p));
        assert_eq!(
            detect("/nix/store/abc-claude-code-sync-0.3.3/bin/claude-code-sync"),
            Some(Manager::Nix)
        );
        assert_eq!(
            detect("/opt/homebrew/Cellar/claude-code-sync/0.3.3/bin/claude-code-sync"),
            Some(Manager::Homebrew)
        );
        assert_eq!(
            detect("/home/linuxbrew/.linuxbrew/Cellar/claude-code-sync/0.3.3/bin/claude-code-sync"),
            Some(Manager::Homebrew)
        );
        assert_eq!(
            detect(r"C:\Users\me\scoop\apps\claude-code-sync\current\claude-code-sync.exe"),
            Some(Manager::Scoop)
        );
        assert_eq!(
            detect("/home/me/.cargo/bin/claude-code-sync"),
            Some(Manager::Cargo)
        );
        assert_eq!(detect("/home/me/.local/bin/claude-code-sync"), None);
        assert_eq!(detect("/usr/local/bin/claude-code-sync"), None);
    }

    #[test]
    fn this_platform_has_a_release_asset() {
        // Every platform CI builds on ships a release asset; a new CI runner
        // OS without one should fail here rather than at update time.
        assert!(release_asset().is_some());
    }

    #[cfg(not(windows))]
    #[test]
    fn extract_binary_finds_the_executable() {
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            Vec::new(),
            flate2::Compression::default(),
        ));
        for (name, body) in [("README.md", &b"docs"[..]), (BIN_NAME, &b"\x7fELF"[..])] {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, body).unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();

        assert_eq!(extract_binary(&archive).unwrap(), b"\x7fELF");
        assert!(extract_binary(b"not an archive").is_err());
    }
}
