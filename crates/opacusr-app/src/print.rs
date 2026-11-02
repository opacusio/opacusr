//! Viewer-side printing (privacy of scope: v1 prints the **current
//! frame** of the remote screen to the viewer's local printer — no
//! protocol channel, no host involvement).
//!
//! The frame is encoded to PNG (lossless), written to the platform temp
//! directory as `opacusr-print-<unix_secs>.png` and handed to the OS
//! print path:
//!
//! * **Linux / macOS** — `lp <file>` (CUPS). Missing `lp` / no default
//!   printer surfaces as an `Err` with the command's stderr.
//! * **Windows** — PowerShell `Start-Process -Verb Print` on the PNG,
//!   which opens the association's print flow (Photo Viewer / default
//!   app with a print dialog).
//!
//! The file is intentionally kept (not deleted) so the user has the
//! exact printed artifact; it lives in the temp dir and is cleaned by
//! the OS on the usual temp cleanup.

use std::path::PathBuf;

/// Encode `rgba` (w×h×4) as PNG and print it on the local printer.
///
/// Returns the path of the PNG file that was sent to the print system.
pub fn print_png(
    width: u32,
    height: u32,
    rgba: &[u8],
    now_unix_secs: u64,
) -> Result<PathBuf, String> {
    let path = temp_png_path(now_unix_secs);
    encode_png(width, height, rgba, &path)?;
    dispatch_to_printer(&path)?;
    Ok(path)
}

/// `<temp>/opacusr-print-<secs>.png`.
pub(crate) fn temp_png_path(now_unix_secs: u64) -> PathBuf {
    std::env::temp_dir().join(format!("opacusr-print-{now_unix_secs}.png"))
}

/// PNG-encode the RGBA buffer and write it to `path`.
pub(crate) fn encode_png(
    width: u32,
    height: u32,
    rgba: &[u8],
    path: &std::path::Path,
) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err(format!("empty frame ({width}×{height})"));
    }
    let expected = width as usize * height as usize * 4;
    if rgba.len() < expected {
        return Err(format!(
            "frame buffer too small: {} bytes for {width}×{height} (need {expected})",
            rgba.len()
        ));
    }
    let img = image::RgbaImage::from_raw(width, height, rgba[..expected].to_vec())
        .ok_or_else(|| "frame buffer size mismatch".to_string())?;
    img.save(path)
        .map_err(|e| format!("write {}: {e}", path.display()))
}

/// Hand `path` to the platform print system (CUPS `lp` on unix,
/// PowerShell `Start-Process -Verb Print` on Windows).
pub(crate) fn dispatch_to_printer(path: &std::path::Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        let (program, args) = print_command_unix();
        let out = std::process::Command::new(program)
            .args(&args)
            .arg(path)
            .output()
            .map_err(|e| format!("cannot run {program} (is CUPS installed?): {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            let err = if err.is_empty() { "lp failed" } else { err };
            return Err(format!("{program}: {err}"));
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        let (program, args) = print_command_windows(path);
        let out = std::process::Command::new(program)
            .args(&args)
            .output()
            .map_err(|e| format!("cannot run {program}: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            let err = err.trim();
            let err = if err.is_empty() {
                "print verb failed"
            } else {
                err
            };
            return Err(err.to_string());
        }
        Ok(())
    }
}

/// CUPS print command: `(program, fixed args)`; the file path is appended
/// by the caller.
#[cfg(unix)]
pub(crate) fn print_command_unix() -> (&'static str, Vec<&'static str>) {
    ("lp", Vec::new())
}

/// PowerShell print command for `path` (file path is embedded, so this is
/// Windows-only and not unit-tested on other platforms).
#[cfg(windows)]
pub(crate) fn print_command_windows(path: &std::path::Path) -> (&'static str, Vec<String>) {
    (
        "powershell",
        vec![format!(
            "-NoProfile -Command Start-Process -FilePath '{}' -Verb Print",
            path.display()
        )],
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn unix_print_command_is_lp() {
        let (program, args) = print_command_unix();
        assert_eq!(program, "lp");
        assert!(args.is_empty());
    }

    #[test]
    fn temp_path_has_the_expected_shape() {
        let p = temp_png_path(1_770_000_000);
        let s = p.to_string_lossy();
        assert!(s.ends_with("opacusr-print-1770000000.png"), "{s}");
    }

    #[test]
    fn png_encode_round_trips_and_rejects_short_buffers() {
        let dir = std::env::temp_dir();
        let path = dir.join("opacusr-test-encode.png");
        // 2×2 red frame.
        let rgba = vec![
            255u8, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255, 255, 0, 0, 255,
        ];
        encode_png(2, 2, &rgba, &path).expect("encode");
        let bytes = std::fs::read(&path).expect("read back");
        assert_eq!(&bytes[1..4], b"PNG", "PNG magic");
        let decoded = image::load_from_memory(&bytes).expect("decode");
        assert_eq!((decoded.width(), decoded.height()), (2, 2));
        let _ = std::fs::remove_file(&path);

        // A truncated buffer must fail with a clear message.
        let err = encode_png(4, 4, &rgba, &dir.join("opacusr-test-bad.png"))
            .expect_err("short buffer must fail");
        assert!(err.contains("too small"), "{err}");
    }

    #[test]
    fn print_png_rejects_empty_frames() {
        let err = print_png(0, 0, &[], 1).expect_err("empty frame");
        assert!(err.contains("empty frame"), "{err}");
    }
}
