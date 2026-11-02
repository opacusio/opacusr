//! Update check: compare the running version against the latest published
//! GitHub release. By default strictly informational: OpacusR never
//! downloads or installs anything on its own.
//!
//! Opt-in self-update lives in [`self_update`] (CLI `opacusr-agent update`
//! and the GUI banner button): it downloads the release asset for this
//! platform, verifies the SHA-256 against the release's `SHA256SUMS`,
//! extracts the binary and swaps the running executable aside
//! (`<name>.opacusr-old` keeps the previous version for rollback). The
//! new version activates on the next start. The checksum comes over
//! HTTPS from the same release channel, which protects the download
//! against corruption but is not a code signature.
//!
//! Set the environment variable `OPACUSR_NO_UPDATE_CHECK=1` to disable
//! the *check* entirely (used by the helpers below); an explicit
//! self-update command still works.

use serde_json::Value;
use std::time::Duration;

const LATEST_URL: &str = "https://api.github.com/repos/opacusio/opacusr/releases/latest";

/// The latest published GitHub release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatestRelease {
    /// Release tag, for example `v0.3.2`.
    pub tag: String,
    /// Tag without the leading `v`, for example `0.3.2`.
    pub version: String,
    /// Human-facing release page URL.
    pub url: String,
}

/// Fetch the latest published release from GitHub. Blocking call that times
/// out after 10 seconds; `/releases/latest` never returns drafts or
/// prereleases. All failures are reported as `Err(message)`, callers decide
/// how loudly to complain.
pub fn fetch_latest_release() -> Result<LatestRelease, String> {
    let resp = match ureq::get(LATEST_URL)
        .set("User-Agent", concat!("opacusr/", env!("CARGO_PKG_VERSION")))
        .set("Accept", "application/vnd.github+json")
        .timeout(Duration::from_secs(10))
        .call()
    {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
        Err(e) => return Err(format!("request failed: {e}")),
    };
    let body = resp
        .into_string()
        .map_err(|e| format!("failed to read response body: {e}"))?;
    let json: Value = serde_json::from_str(&body).map_err(|e| format!("unexpected JSON: {e}"))?;
    let tag = json
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "response has no tag_name".to_string())?
        .trim()
        .to_string();
    let url = json
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("https://github.com/opacusio/opacusr/releases")
        .to_string();
    Ok(LatestRelease {
        version: tag.trim_start_matches('v').to_string(),
        tag,
        url,
    })
}

/// Compare dotted numeric versions. Returns true if `latest` is strictly
/// newer than `current`. Missing components count as zero, so `0.3` equals
/// `0.3.0`. Non-numeric suffixes like `-beta` are ignored.
pub fn is_newer(current: &str, latest: &str) -> bool {
    let cur = version_parts(current);
    let lat = version_parts(latest);
    let len = cur.len().max(lat.len());
    for i in 0..len {
        let c = cur.get(i).copied().unwrap_or(0);
        let l = lat.get(i).copied().unwrap_or(0);
        if l != c {
            return l > c;
        }
    }
    false
}

fn version_parts(version: &str) -> Vec<u64> {
    version
        .trim()
        .trim_start_matches('v')
        .split('.')
        .map(|part| {
            let digits: String = part.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse::<u64>().unwrap_or(0)
        })
        .collect()
}

/// Check GitHub for a release newer than this build. Returns `None` when the
/// check fails (offline, rate limited) or when the running version is the
/// latest one. Blocking for up to 10 seconds: call from a background thread.
pub fn check_for_update() -> Option<LatestRelease> {
    let latest = fetch_latest_release().ok()?;
    if is_newer(env!("CARGO_PKG_VERSION"), &latest.version) {
        Some(latest)
    } else {
        None
    }
}

/// Fire-and-forget update notice for CLI binaries: runs
/// [`check_for_update`] on a background thread and prints one line to
/// stderr when a newer release exists. No download, no auto-install.
pub fn spawn_update_notice() {
    if std::env::var_os("OPACUSR_NO_UPDATE_CHECK").is_some() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("opacusr-update-check".into())
        .spawn(|| {
            if let Some(latest) = check_for_update() {
                eprintln!(
                    "opacusr: update available: {} (you are running {}) - {}",
                    latest.tag,
                    env!("CARGO_PKG_VERSION"),
                    latest.url
                );
            }
        });
}

// ---------------------------------------------------------------------
// Opt-in self-update: download, verify, swap. Blocking — call from a
// background thread (CLI command or the GUI banner button).
// ---------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssetKind {
    Zip,
    TarGz,
}

/// Release artifact that carries `binary` for the running platform.
fn asset_for(_binary: &str, version: &str) -> Result<(String, AssetKind), String> {
    let (os, arch) = (std::env::consts::OS, std::env::consts::ARCH);
    match (os, arch) {
        ("windows", "x86_64") => Ok((
            format!("opacusr-{version}-windows-x86_64.zip"),
            AssetKind::Zip,
        )),
        ("macos", "aarch64") => Ok((
            format!("opacusr-{version}-macos-aarch64.tar.gz"),
            AssetKind::TarGz,
        )),
        ("linux", "x86_64") => Ok((
            format!("opacusr-{version}-linux-x86_64.tar.gz"),
            AssetKind::TarGz,
        )),
        _ => Err(format!(
            "self-update has no package for {os}/{arch}; download one from \
             https://github.com/opacusio/opacusr/releases/latest"
        )),
    }
}

fn github_download_url(tag: &str, asset: &str) -> String {
    format!("https://github.com/opacusio/opacusr/releases/download/{tag}/{asset}")
}

fn http_get(url: &str) -> Result<ureq::Response, String> {
    ureq::get(url)
        .set("User-Agent", concat!("opacusr/", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(300)) // binaries are tens of MB
        .call()
        .map_err(|e| match e {
            ureq::Error::Status(code, _) => format!("HTTP {code} for {url}"),
            e => format!("{url}: {e}"),
        })
}

/// Parse `SHA256SUMS` (`<hex>  <file>` per line) for the given asset.
fn parse_sha256sums(text: &str, asset: &str) -> Option<String> {
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let (Some(hash), Some(name)) = (parts.next(), parts.next()) else {
            continue;
        };
        if name.rsplit('/').next() == Some(asset) {
            return Some(hash.to_ascii_lowercase());
        }
    }
    None
}

/// Stream the download into `dest`, returning the SHA-256 digest.
fn download_and_hash(url: &str, dest: &std::path::Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let resp = http_get(url)?;
    let mut reader = resp.into_reader();
    let mut file =
        std::fs::File::create(dest).map_err(|e| format!("create {}: {e}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("{url}: {e}"))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])
            .map_err(|e| format!("write {}: {e}", dest.display()))?;
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Extract the file named `want` (basename match) from the archive into
/// `dest_dir` and return its path.
fn extract_binary(
    archive: &std::path::Path,
    kind: AssetKind,
    want: &str,
    dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    match kind {
        AssetKind::Zip => extract_from_zip(archive, want, dest_dir),
        AssetKind::TarGz => extract_from_targz(archive, want, dest_dir),
    }
}

#[cfg(windows)]
fn extract_from_zip(
    archive: &std::path::Path,
    want: &str,
    dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    let file =
        std::fs::File::open(archive).map_err(|e| format!("open {}: {e}", archive.display()))?;
    let mut zip =
        zip::ZipArchive::new(file).map_err(|e| format!("read {}: {e}", archive.display()))?;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(|e| format!("zip entry {i}: {e}"))?;
        if entry.is_dir() {
            continue;
        }
        // zip 9: name() returns ZipResult<Cow<str>>.
        let name = entry
            .name()
            .map_err(|e| format!("zip entry {i} name: {e}"))?;
        let base = name.rsplit('/').next().unwrap_or_default().to_string();
        if base.eq_ignore_ascii_case(want) {
            let out = dest_dir.join(&base);
            let mut out_f = std::fs::File::create(&out)
                .map_err(|e| format!("create {}: {e}", out.display()))?;
            std::io::copy(&mut entry, &mut out_f).map_err(|e| format!("extract {base}: {e}"))?;
            return Ok(out);
        }
    }
    Err(format!("{want} not found in {}", archive.display()))
}

#[cfg(unix)]
fn extract_from_targz(
    archive: &std::path::Path,
    want: &str,
    dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    use std::io::Read;
    let file =
        std::fs::File::open(archive).map_err(|e| format!("open {}: {e}", archive.display()))?;
    let gz = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(gz);
    for entry in tar
        .entries()
        .map_err(|e| format!("read {}: {e}", archive.display()))?
    {
        let mut entry = entry.map_err(|e| format!("tar entry: {e}"))?;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let base = entry
            .path()
            .map_err(|e| format!("tar path: {e}"))?
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if base == want {
            let out = dest_dir.join(&base);
            let mut out_f = std::fs::File::create(&out)
                .map_err(|e| format!("create {}: {e}", out.display()))?;
            let mut data = Vec::new();
            entry
                .read_to_end(&mut data)
                .map_err(|e| format!("extract {base}: {e}"))?;
            std::io::Write::write_all(&mut out_f, &data)
                .map_err(|e| format!("write {base}: {e}"))?;
            // Preserve the executable bit from the archive header.
            if let Ok(mode) = entry.header().mode() {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&out, std::fs::Permissions::from_mode(mode | 0o755));
            }
            return Ok(out);
        }
    }
    Err(format!("{want} not found in {}", archive.display()))
}

#[cfg(not(unix))]
fn extract_from_targz(
    _archive: &std::path::Path,
    _want: &str,
    _dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    Err("tar.gz archives are only used on unix targets".into())
}

#[cfg(not(windows))]
fn extract_from_zip(
    _archive: &std::path::Path,
    _want: &str,
    _dest_dir: &std::path::Path,
) -> Result<std::path::PathBuf, String> {
    Err("zip archives are only used on Windows".into())
}

/// Download the latest release for this platform, verify its SHA-256
/// against the release's `SHA256SUMS`, extract `binary`
/// (`"opacusr"` or `"opacusr-agent"`) and swap the running executable
/// aside as `<exe>.opacusr-old`. The new version activates on restart.
///
/// Blocking (tens of MB over HTTPS); call from a background thread.
/// Returns a human-readable result message.
pub fn self_update(binary: &str) -> Result<String, String> {
    let release = fetch_latest_release()?;
    if !is_newer(env!("CARGO_PKG_VERSION"), &release.version) {
        return Ok(format!(
            "already up to date ({})",
            env!("CARGO_PKG_VERSION")
        ));
    }
    let (asset, kind) = asset_for(binary, &release.version)?;

    let sums = http_get(&github_download_url(&release.tag, "SHA256SUMS"))?
        .into_string()
        .map_err(|e| format!("read SHA256SUMS: {e}"))?;
    let expected = parse_sha256sums(&sums, &asset)
        .ok_or_else(|| format!("SHA256SUMS has no entry for {asset}"))?;

    let exe =
        std::env::current_exe().map_err(|e| format!("cannot locate the running binary: {e}"))?;
    let want = format!("{binary}{}", if cfg!(windows) { ".exe" } else { "" });
    let current = exe
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_string();
    if current != want {
        return Err(format!(
            "this command updates the '{binary}' binary, but the running file is '{current}'"
        ));
    }
    // Package-manager installs are owned by dpkg/apt; replacing files
    // under /usr by hand breaks the database (and usually lacks perms).
    #[cfg(unix)]
    if exe.starts_with("/usr") || exe.starts_with("/opt") {
        return Err(
            "this binary is installed under /usr or /opt — update via the \
             system package manager (apt/dpkg with the release .deb); \
             self-update targets portable installs"
                .into(),
        );
    }

    let tmp = tempfile::tempdir().map_err(|e| format!("tempdir: {e}"))?;
    let archive_path = tmp.path().join(&asset);
    let got = download_and_hash(&github_download_url(&release.tag, &asset), &archive_path)?;
    if got != expected.to_lowercase() {
        return Err(format!(
            "checksum mismatch for {asset}: expected {expected}, got {got}"
        ));
    }

    let extracted = extract_binary(&archive_path, kind, &want, tmp.path())?;
    let old = exe.with_extension("opacusr-old");
    let _ = std::fs::remove_file(&old); // drop a leftover from a previous update
    std::fs::rename(&exe, &old)
        .map_err(|e| format!("cannot move the running binary aside: {e}"))?;
    if let Err(e) = std::fs::copy(&extracted, &exe) {
        // Roll back so the tool keeps working.
        let _ = std::fs::rename(&old, &exe);
        return Err(format!("cannot install the new binary: {e}"));
    }
    Ok(format!(
        "updated {} to {} — restart to apply (previous binary kept as {})",
        binary,
        release.tag,
        old.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newer_versions() {
        assert!(is_newer("0.3.1", "0.3.2"));
        assert!(is_newer("0.3.1", "0.4.0"));
        assert!(is_newer("0.9", "1.0.0"));
        assert!(!is_newer("0.3.2", "0.3.1"));
        assert!(!is_newer("0.3.1", "0.3.1"));
        assert!(!is_newer("0.3.1", "0.3"));
        assert!(!is_newer("0.3.1", "v0.3.1"));
    }

    #[test]
    fn parses_odd_versions() {
        assert_eq!(version_parts("v1.2.3-beta"), vec![1, 2, 3]);
        assert_eq!(version_parts("2"), vec![2]);
        assert_eq!(version_parts("x"), vec![0]);
    }

    #[test]
    fn sha256sums_parsing_tolerates_blank_lines_and_paths() {
        let sums = "\
abc123  opacusr-0.9.1-linux-x86_64.tar.gz\n\
\n\
def456  dir/opacusr-0.9.1-windows-x86_64.zip\n";
        assert_eq!(
            parse_sha256sums(sums, "opacusr-0.9.1-linux-x86_64.tar.gz").as_deref(),
            Some("abc123")
        );
        assert_eq!(
            parse_sha256sums(sums, "opacusr-0.9.1-windows-x86_64.zip").as_deref(),
            Some("def456")
        );
        assert_eq!(parse_sha256sums(sums, "missing.zip"), None);
        assert_eq!(parse_sha256sums("", "x"), None);
    }

    #[test]
    fn asset_names_match_the_release_pipeline() {
        let (a, k) = asset_for("opacusr-agent", "1.2.3").unwrap();
        match std::env::consts::OS {
            "linux" => {
                assert_eq!(a, "opacusr-1.2.3-linux-x86_64.tar.gz");
                assert_eq!(k, AssetKind::TarGz);
            }
            "macos" => {
                assert_eq!(a, "opacusr-1.2.3-macos-aarch64.tar.gz");
                assert_eq!(k, AssetKind::TarGz);
            }
            "windows" => {
                assert_eq!(a, "opacusr-1.2.3-windows-x86_64.zip");
                assert_eq!(k, AssetKind::Zip);
            }
            other => assert!(asset_for("x", "1.0.0").is_err(), "{other} should error"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn targz_extraction_finds_binary_by_basename() {
        let tmp = tempfile::tempdir().unwrap();
        let arch = tmp.path().join("a.tar.gz");
        let f = std::fs::File::create(&arch).unwrap();
        let gz = flate2::write::GzEncoder::new(f, flate2::Compression::fast());
        {
            let mut b = tar::Builder::new(gz);
            let mut h = tar::Header::new_gnu();
            let content: &[u8] = b"FAKE-BIN";
            h.set_size(content.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            b.append_data(&mut h, "opacusr-0.9.1-linux-x86_64/opacusr-agent", content)
                .unwrap();
            // A decoy with the same suffix must not match.
            let mut h2 = tar::Header::new_gnu();
            h2.set_size(4);
            h2.set_mode(0o644);
            h2.set_cksum();
            b.append_data(&mut h2, "doc/README.md", b"read".as_slice())
                .unwrap();
            b.into_inner().unwrap();
        }
        let out = tmp.path().join("out");
        std::fs::create_dir(&out).unwrap();
        let got = extract_from_targz(&arch, "opacusr-agent", &out).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), b"FAKE-BIN");
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&got).unwrap().permissions().mode() & 0o111,
            0o111
        );
        assert!(extract_from_targz(&arch, "nope", &out).is_err());
    }
}
