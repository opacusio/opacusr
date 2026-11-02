//! macOS launchd support for the agent: `opacusr-agent service
//! install|uninstall|status`.
//!
//! Unlike Windows (session-0 supervisor + per-session child, see
//! [`crate::win_service`]), macOS needs no two-process topology: a
//! per-user **LaunchAgent** already lives in the interactive GUI session,
//! so capture and input work directly. launchd starts the agent at login
//! (`RunAtLoad`) and restarts it if it crashes (`KeepAlive`); output is
//! appended to `~/Library/Logs/opacusr-agent.log`.
//!
//! The catch on macOS is TCC: the first start prompts for Screen
//! Recording (capture) and Accessibility (input) permissions, which the
//! user grants once for the binary — `install` prints that reminder.
//!
//! On non-macOS targets the install/uninstall/status helpers return an
//! error pointing at the platform's mechanism (Windows SCM service,
//! Linux systemd user unit). The plist/escaping helpers are pure and
//! unit-tested on every platform.

pub const SERVICE_LABEL: &str = "io.opacusr.agent";

// ---------------------------------------------------------------------
// Pure helpers (compile + unit-test on every platform).
// ---------------------------------------------------------------------

/// Escape a string for embedding into an XML text node.
#[allow(dead_code)] // macOS-only callers; kept + tested everywhere
pub(crate) fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

/// Render the LaunchAgent plist for `exe_path`.
///
/// `opacusr-agent run` with no overrides: config comes from the user's
/// `~/Library/Application Support/opacusr/agent.toml`, so the agent and
/// the GUI share one configuration, same as a plain login start.
#[allow(dead_code)] // macOS-only callers; kept + tested everywhere
pub(crate) fn plist_xml(exe_path: &str, log_path: &str) -> String {
    let exe = xml_escape(exe_path);
    let log = xml_escape(log_path);
    let label = xml_escape(SERVICE_LABEL);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{exe}</string>
        <string>run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>ThrottleInterval</key>
    <integer>10</integer>
    <key>StandardOutPath</key>
    <string>{log}</string>
    <key>StandardErrorPath</key>
    <string>{log}</string>
</dict>
</plist>
"#
    )
}

#[allow(dead_code)] // macOS-only callers; kept + tested everywhere
fn home_dir() -> anyhow::Result<std::path::PathBuf> {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("HOME is not set; cannot locate the LaunchAgents directory"))
}

/// `~/Library/LaunchAgents/io.opacusr.agent.plist`.
#[allow(dead_code)] // macOS-only callers; kept + tested everywhere
pub(crate) fn plist_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(home_dir()?
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{SERVICE_LABEL}.plist")))
}

/// `~/Library/Logs/opacusr-agent.log`.
#[allow(dead_code)] // macOS-only callers; kept + tested everywhere
fn log_path() -> anyhow::Result<std::path::PathBuf> {
    Ok(home_dir()?
        .join("Library")
        .join("Logs")
        .join("opacusr-agent.log"))
}

// ---------------------------------------------------------------------
// Public API (CLI surface; mirrors win_service).
// ---------------------------------------------------------------------

/// Register the per-user LaunchAgent (starts at login, kept alive).
pub fn install() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        imp::install()
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!(
            "launchd management is macOS-only; on Windows use `service install` (SCM), \
             on Linux enable the systemd user unit (see README)"
        )
    }
}

/// Unload and remove the LaunchAgent.
pub fn uninstall() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        imp::uninstall()
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!(
            "launchd management is macOS-only; on Windows use `service uninstall` (SCM), \
             on Linux disable the systemd user unit (see README)"
        )
    }
}

/// Print the LaunchAgent status (`opacusr-agent service status`).
pub fn status() -> anyhow::Result<()> {
    #[cfg(target_os = "macos")]
    {
        imp::status()
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!(
            "launchd management is macOS-only; on Windows use `service status` (SCM), \
             on Linux use `systemctl --user status opacusr-agent`"
        )
    }
}

// ---------------------------------------------------------------------
// macOS implementation (launchctl).
// ---------------------------------------------------------------------

#[cfg(target_os = "macos")]
mod imp {
    use super::{log_path, plist_path, plist_xml, SERVICE_LABEL};
    use anyhow::Context;

    fn launchctl(args: &[&str]) -> anyhow::Result<std::process::Output> {
        let out = std::process::Command::new("launchctl")
            .args(args)
            .output()
            .context("run launchctl")?;
        Ok(out)
    }

    fn launchctl_ok(args: &[&str], what: &str) -> anyhow::Result<()> {
        let out = launchctl(args)?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            anyhow::bail!("{what} failed: {}", err.trim());
        }
        Ok(())
    }

    pub fn install() -> anyhow::Result<()> {
        let exe = std::env::current_exe().context("resolve the running agent binary")?;
        let log = log_path()?;
        let path = plist_path()?;

        std::fs::create_dir_all(
            path.parent()
                .ok_or_else(|| anyhow::anyhow!("plist path has no parent"))?,
        )?;
        std::fs::create_dir_all(
            log.parent()
                .ok_or_else(|| anyhow::anyhow!("log path has no parent"))?,
        )?;
        let xml = plist_xml(&exe.display().to_string(), &log.display().to_string());
        std::fs::write(&path, xml).with_context(|| format!("write {}", path.display()))?;

        // Replace a previously loaded instance; a not-loaded failure of
        // `unload` is fine (first install).
        let path_str = path.display().to_string();
        let _ = launchctl(&["unload", &path_str]);

        launchctl_ok(&["load", &path_str], "launchctl load")
            .with_context(|| format!("load {SERVICE_LABEL} (plist: {})", path.display()))?;

        println!("LaunchAgent '{SERVICE_LABEL}' installed and loaded.");
        println!("  plist: {}", path.display());
        println!("  log:   {}", log.display());
        println!(
            "The agent starts at login and restarts if it crashes. On first use macOS asks \
             for Screen Recording and Accessibility permissions (System Settings → Privacy \
             & Security) — grant them once for this binary; run `service status` to check \
             that it is running."
        );
        Ok(())
    }

    pub fn uninstall() -> anyhow::Result<()> {
        let path = plist_path()?;
        if !path.exists() {
            anyhow::bail!(
                "{SERVICE_LABEL} is not installed ({} not found)",
                path.display()
            );
        }
        let path_str = path.display().to_string();
        let _ = launchctl(&["unload", &path_str]); // loaded-ness may vary
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
        println!("LaunchAgent '{SERVICE_LABEL}' uninstalled.");
        Ok(())
    }

    pub fn status() -> anyhow::Result<()> {
        let path = plist_path()?;
        let out = launchctl(&["list", SERVICE_LABEL])?;
        if out.status.success() {
            let stdout = String::from_utf8_lossy(&out.stdout);
            println!("{SERVICE_LABEL}: loaded (launchctl list)");
            for line in stdout.lines() {
                println!("  {line}");
            }
        } else {
            println!("{SERVICE_LABEL}: not loaded");
        }
        println!(
            "  plist: {} ({})",
            path.display(),
            if path.exists() { "present" } else { "missing" }
        );
        Ok(())
    }
}

// ---------------------------------------------------------------------
// Tests (pure helpers; run on every platform).
// ---------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xml_escape_escapes_the_five_entities() {
        assert_eq!(
            xml_escape("a&b<c>d\"e'f"),
            "a&amp;b&lt;c&gt;d&quot;e&apos;f"
        );
        assert_eq!(xml_escape("plain path"), "plain path");
    }

    #[test]
    fn plist_contains_label_flags_and_run() {
        let xml = plist_xml(
            "/Applications/OpacusR/opacusr-agent",
            "/Users/u/Library/Logs/opacusr-agent.log",
        );
        assert!(xml.contains("<string>io.opacusr.agent</string>"), "{xml}");
        assert!(xml.contains("<key>RunAtLoad</key>\n    <true/>"), "{xml}");
        assert!(xml.contains("<key>KeepAlive</key>\n    <true/>"), "{xml}");
        // ProgramArguments = [exe, "run"]
        assert!(
            xml.contains("<string>/Applications/OpacusR/opacusr-agent</string>"),
            "{xml}"
        );
        assert!(xml.contains("<string>run</string>"), "{xml}");
        // Log paths are wired both ways.
        assert_eq!(
            xml.matches("<string>/Users/u/Library/Logs/opacusr-agent.log</string>")
                .count(),
            2,
            "{xml}"
        );
    }

    #[test]
    fn plist_escapes_xml_in_paths() {
        let xml = plist_xml("/Users/a&b/Apps/opacusr-agent", "/tmp/log");
        assert!(xml.contains("/Users/a&amp;b/Apps/opacusr-agent"), "{xml}");
        assert!(!xml.contains("/Users/a&b/"), "{xml}");
    }

    #[test]
    fn plist_paths_live_under_home() {
        // Uses $HOME of the test process; only sanity-checks the shape.
        let p = plist_path().expect("plist path");
        assert!(p
            .to_string_lossy()
            .ends_with("Library/LaunchAgents/io.opacusr.agent.plist"));
    }
}
