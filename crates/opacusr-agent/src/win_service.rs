//! Windows service mode for the agent: `opacusr-agent service
//! install|uninstall|status|run`.
//!
//! Two-process design (TeamViewer-style), which sidesteps session 0
//! isolation entirely:
//!
//! * **Supervisor** — the SCM service itself (`service run`), runs in
//!   session 0 as LocalSystem. It never captures or talks to signaling:
//!   it only watches the active console session and (re)spawns a
//!   per-session agent child there, killing it on logoff, session
//!   switch, unexpected exit or service stop. A kill-on-close job object
//!   guarantees the child cannot outlive a crashed supervisor.
//! * **Child** — `service run --in-session`, created inside the user's
//!   console session with the user's token, environment and
//!   `winsta0\default` desktop. It is a regular agent: DXGI/GDI capture
//!   and input injection work because the process lives in the
//!   interactive session; config, recordings and the OS-keychain
//!   password belong to the logged-on user.
//!
//! Only the *console* session is supervised (RDP multi-session support
//! is a roadmap item). Without a logged-on user the supervisor keeps
//! polling — logon starts the child automatically. The child needs no
//! extra privileges; the LocalSystem service holds the ones required to
//! query the session token (`WTSQueryUserToken`).
//!
//! On non-Windows targets the Windows-specific operations return an
//! error; `install|uninstall|status` are routed by platform: macOS goes
//! to the per-user launchd LaunchAgent (see [`crate::launchd_service`]),
//! Linux ships a systemd **user** unit instead (see README).

pub const SERVICE_NAME: &str = "OpacusRAgent";

/// Sentinel returned by `WTSGetActiveConsoleSessionId` when no console
/// session is active (no user logged on).
// Used by the Windows supervisor; kept cfg-independent so the decision
// table is unit-testable everywhere.
#[allow(dead_code)]
pub(crate) const NO_SESSION: u32 = u32::MAX;

// ---------------------------------------------------------------------
// Public API (called from the CLI; compiles on every platform).
// ---------------------------------------------------------------------

/// Register the agent as an auto-start Windows service (needs admin).
pub fn install() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        imp::install()
    }
    #[cfg(target_os = "macos")]
    {
        crate::launchd_service::install()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        anyhow::bail!(
            "service management is Windows-only; on Linux use the systemd user unit (see README)"
        )
    }
}

/// Remove the Windows service registration (needs admin).
pub fn uninstall() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        imp::uninstall()
    }
    #[cfg(target_os = "macos")]
    {
        crate::launchd_service::uninstall()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        anyhow::bail!(
            "service management is Windows-only; on Linux use the systemd user unit (see README)"
        )
    }
}

/// Print the service status (`opacusr-agent service status`).
pub fn status() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        imp::status()
    }
    #[cfg(target_os = "macos")]
    {
        crate::launchd_service::status()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        anyhow::bail!(
            "service management is Windows-only; on Linux use the systemd user unit (see README)"
        )
    }
}

/// Enter the SCM dispatcher (blocks). This is the `service run` branch:
/// without `--in-session` it must be launched by the service control
/// manager, with `--in-session` it is the per-session child started by
/// the supervisor (handled in `main.rs`).
pub fn dispatch() -> anyhow::Result<()> {
    #[cfg(windows)]
    {
        imp::dispatch()
    }
    #[cfg(not(windows))]
    {
        anyhow::bail!("service mode is Windows-only; use `opacusr-agent run` on this platform")
    }
}

// ---------------------------------------------------------------------
// Supervisor decision table (pure, platform-independent, unit-tested).
// ---------------------------------------------------------------------

/// What the supervisor loop should do on this poll.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TickAction {
    /// Child is alive and belongs to the current console session.
    Keep,
    /// No console session and nothing to clean up.
    Wait,
    /// Kill the running child (console session ended or changed);
    /// the next tick spawns a replacement when a session exists.
    StopChild,
    /// Ensure a child exists for `session_id` (kill a stale one first).
    Spawn(u32),
}

/// Decide the next supervisor action.
///
/// `child` is `Some((session_id, still_alive))` for the supervised
/// child; `console` is the active console session id
/// ([`NO_SESSION`] when nobody is logged on).
#[allow(dead_code)]
pub(crate) fn tick(child: Option<(u32, bool)>, console: u32) -> TickAction {
    let alive = matches!(child, Some((_, true)));
    match (console, alive) {
        (NO_SESSION, true) => TickAction::StopChild,
        (NO_SESSION, false) => TickAction::Wait,
        (sid, true) if child.is_some_and(|(s, _)| s == sid) => TickAction::Keep,
        (sid, _) => TickAction::Spawn(sid),
    }
}

#[cfg(test)]
mod tests {
    use super::{tick, TickAction, NO_SESSION};

    #[test]
    fn spawns_child_for_active_session() {
        assert_eq!(tick(None, 1), TickAction::Spawn(1));
    }

    #[test]
    fn keeps_matching_alive_child() {
        assert_eq!(tick(Some((1, true)), 1), TickAction::Keep);
    }

    #[test]
    fn respawns_dead_child() {
        assert_eq!(tick(Some((1, false)), 1), TickAction::Spawn(1));
    }

    #[test]
    fn respawns_on_session_switch() {
        assert_eq!(tick(Some((1, true)), 2), TickAction::Spawn(2));
    }

    #[test]
    fn stops_child_on_logoff() {
        assert_eq!(tick(Some((1, true)), NO_SESSION), TickAction::StopChild);
    }

    #[test]
    fn waits_without_user_and_child() {
        assert_eq!(tick(None, NO_SESSION), TickAction::Wait);
        assert_eq!(tick(Some((1, false)), NO_SESSION), TickAction::Wait);
    }
}

// ---------------------------------------------------------------------
// Windows implementation.
// ---------------------------------------------------------------------

#[cfg(windows)]
mod imp {
    use super::{tick, TickAction, NO_SESSION, SERVICE_NAME};
    use anyhow::{bail, Context};
    use std::ffi::OsString;
    use std::time::Duration;
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_NO_TOKEN, HANDLE, WAIT_TIMEOUT, WIN32_ERROR,
    };
    use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
    use windows::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
    use windows::Win32::System::Threading::{
        CreateProcessAsUserW, TerminateProcess, WaitForSingleObject, CREATE_NO_WINDOW,
        CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTUPINFOW,
    };
    use windows_service::define_windows_service;
    use windows_service::service::{
        ServiceAccess, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
        ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
    };
    use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
    use windows_service::service_dispatcher;
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    /// How often the supervisor polls the console session / child state.
    const POLL_INTERVAL: Duration = Duration::from_secs(5);

    // -- SCM plumbing ---------------------------------------------------

    define_windows_service!(ffi_service_main, service_main);

    /// Entry point invoked by the SCM in a fresh thread.
    fn service_main(_args: Vec<OsString>) {
        if let Err(e) = run_supervisor() {
            tracing::error!(target: "opacusr", "service supervisor failed: {e:#}");
        }
    }

    fn report(
        handle: &windows_service::service_control_handler::ServiceStatusHandle,
        state: ServiceState,
        wait_hint: Duration,
    ) -> windows_service::Result<()> {
        handle.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: match state {
                ServiceState::Running => {
                    ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN
                }
                _ => ServiceControlAccept::empty(),
            },
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint,
            process_id: None,
        })
    }

    /// Supervisor loop: report Running, keep exactly one child alive for
    /// the active console session, report Stopped on stop/shutdown.
    fn run_supervisor() -> anyhow::Result<()> {
        let (tx, rx) = std::sync::mpsc::channel::<ServiceControl>();
        let handler = move |control: ServiceControl| -> ServiceControlHandlerResult {
            match control {
                ServiceControl::Stop | ServiceControl::Shutdown => {
                    let _ = tx.send(control);
                    ServiceControlHandlerResult::NoError
                }
                ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
                _ => ServiceControlHandlerResult::NotImplemented,
            }
        };
        let status_handle = service_control_handler::register(SERVICE_NAME, handler)
            .context("register with the service control manager")?;
        report(
            &status_handle,
            ServiceState::StartPending,
            Duration::from_secs(10),
        )?;
        tracing::info!(target: "opacusr", "service supervisor started");

        let mut child: Option<ChildProc> = None;
        let mut child_session: u32 = NO_SESSION;
        // Suppress repetitive spawn-failure logs (no user logged on yet).
        let mut last_spawn_error: Option<String> = None;

        loop {
            match rx.recv_timeout(POLL_INTERVAL) {
                Ok(ServiceControl::Stop) | Ok(ServiceControl::Shutdown) => break,
                Ok(_) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }

            let console = unsafe { WTSGetActiveConsoleSessionId() };
            let decision = tick(child.as_ref().map(|c| (child_session, c.alive())), console);
            match decision {
                TickAction::Keep => {}
                TickAction::Wait => {}
                TickAction::StopChild => {
                    tracing::info!(target: "opacusr", "console session ended; stopping session agent");
                    child = None;
                    child_session = NO_SESSION;
                }
                TickAction::Spawn(sid) => {
                    child = None;
                    child_session = NO_SESSION;
                    match spawn_child(sid) {
                        Ok(c) => {
                            tracing::info!(target: "opacusr", "session agent started in console session {sid}");
                            last_spawn_error = None;
                            child = Some(c);
                            child_session = sid;
                        }
                        Err(e) => {
                            let msg = format!("{e:#}");
                            if last_spawn_error.as_deref() != Some(msg.as_str()) {
                                tracing::warn!(target: "opacusr", "spawning session agent in session {sid} failed: {msg}");
                                last_spawn_error = Some(msg);
                            }
                        }
                    }
                }
            }
        }

        tracing::info!(target: "opacusr", "service stop requested; terminating session agent");
        drop(child);
        report(&status_handle, ServiceState::Stopped, Duration::ZERO)
            .context("report Stopped to the service control manager")?;
        Ok(())
    }

    // -- Child process --------------------------------------------------

    /// One supervised session-agent child plus its kill-on-close job.
    struct ChildProc {
        process: HANDLE,
        _job: HANDLE,
    }

    impl ChildProc {
        fn alive(&self) -> bool {
            // WAIT_TIMEOUT = still running; WAIT_OBJECT_0 = exited.
            let ev = unsafe { WaitForSingleObject(self.process, 0) };
            ev == WAIT_TIMEOUT
        }
    }

    impl Drop for ChildProc {
        fn drop(&mut self) {
            unsafe {
                let _ = TerminateProcess(self.process, 1);
                let _ = CloseHandle(self.process);
                // Closing the last job handle kills the assigned child
                // (JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE) even if
                // TerminateProcess above failed.
                let _ = CloseHandle(self._job);
            }
        }
    }

    /// Spawn `opacusr-agent service run --in-session` inside the console
    /// session `sid` with the logged-on user's token and environment.
    fn spawn_child(sid: u32) -> anyhow::Result<ChildProc> {
        let exe = std::env::current_exe().context("resolve the agent executable path")?;
        let exe_str = exe.display().to_string();

        let mut token = HANDLE::default();
        if let Err(e) = unsafe { WTSQueryUserToken(sid, &mut token) } {
            if WIN32_ERROR(e.code().0 as u32) == ERROR_NO_TOKEN {
                bail!("no user is logged on to console session {sid} yet");
            }
            bail!("WTSQueryUserToken({sid}) failed: {e}");
        }

        // Build the child environment so PATH and friends match the user's
        // session (without it the child starts with a near-empty env).
        let mut env: *mut core::ffi::c_void = std::ptr::null_mut();
        let has_env = unsafe { CreateEnvironmentBlock(&mut env, Some(token), false) }.is_ok();
        if !has_env {
            tracing::warn!(target: "opacusr", "CreateEnvironmentBlock failed; the child inherits a minimal environment");
        }

        let mut cmdline: Vec<u16> = format!("\"{exe_str}\" service run --in-session\0")
            .encode_utf16()
            .collect();
        let mut desktop: Vec<u16> = "winsta0\\default\0".encode_utf16().collect();
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();

        let created = unsafe {
            CreateProcessAsUserW(
                Some(token),
                PCWSTR::null(),
                Some(PWSTR(cmdline.as_mut_ptr())),
                None,
                None,
                false,
                CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT,
                if has_env { Some(env as *const _) } else { None },
                PCWSTR::null(),
                &si,
                &mut pi,
            )
        };
        if has_env {
            unsafe {
                let _ = DestroyEnvironmentBlock(env as *const _);
            }
        }
        unsafe {
            let _ = CloseHandle(token);
        }
        created.context("CreateProcessAsUserW (spawn the per-session agent)")?;

        // Kill-on-close job: if the supervisor dies (crash, force-stop),
        // the kernel closes the job handle and reaps the child with it.
        let job = unsafe { CreateJobObjectW(None, PCWSTR::null()) }.context("CreateJobObjectW")?;
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const core::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        }
        .context("SetInformationJobObject")?;
        unsafe { AssignProcessToJobObject(job, pi.hProcess) }
            .context("AssignProcessToJobObject")?;

        Ok(ChildProc {
            process: pi.hProcess,
            _job: job,
        })
    }

    // -- Management subcommands ------------------------------------------

    fn manager(access: ServiceManagerAccess) -> windows_service::Result<ServiceManager> {
        ServiceManager::local_computer(None::<&OsString>, access)
    }

    pub fn install() -> anyhow::Result<()> {
        let exe = std::env::current_exe()?;
        let manager = manager(ServiceManagerAccess::CREATE_SERVICE | ServiceManagerAccess::CONNECT)
            .context("connect to the service control manager (run as Administrator)")?;
        let info = ServiceInfo {
            name: SERVICE_NAME.into(),
            display_name: "OpacusR Agent".into(),
            service_type: ServiceType::OWN_PROCESS,
            start_type: ServiceStartType::AutoStart,
            error_control: ServiceErrorControl::Normal,
            executable_path: exe,
            launch_arguments: vec![OsString::from("service"), OsString::from("run")],
            dependencies: vec![],
            account_name: None, // LocalSystem: needed for WTSQueryUserToken
            account_password: None,
        };
        let svc = manager
            .create_service(
                &info,
                ServiceAccess::QUERY_STATUS | ServiceAccess::CHANGE_CONFIG
                    | ServiceAccess::START,
            )
            .context(format!(
                "create service '{SERVICE_NAME}' (is it already installed? use `service uninstall` first)"
            ))?;
        svc.set_description(
            "OpacusR remote-desktop host agent: supervises a per-session agent instance in the active console session.",
        )
        .ok(); // cosmetic; the service works without a description

        // Start immediately: `service install` must leave the machine
        // reachable, not «after the next reboot» (TeamViewer behavior).
        match svc.start(&[] as &[std::ffi::OsString]) {
            Ok(()) => {
                println!("Service '{SERVICE_NAME}' installed and started (auto-start on boot).")
            }
            Err(e) => {
                println!("Service '{SERVICE_NAME}' installed (auto-start on boot).");
                eprintln!(
                    "note: the service was not started now ({e}); it will start on the next \
                     boot or `sc start {SERVICE_NAME}`"
                );
            }
        }

        let cfg = crate::config::AgentConfig::load();
        if !cfg.is_unattended() {
            eprintln!(
                "warning: no unattended password is set. The service child has no console for the \
                 attended consent prompt, so every session would be rejected. \
                 Run `opacusr-agent passwd` first."
            );
        }
        println!("Service '{SERVICE_NAME}' installed (auto-start).");
        println!("Or reboot; the agent starts for every console logon.");
        Ok(())
    }

    #[allow(clippy::too_many_lines)]
    pub fn uninstall() -> anyhow::Result<()> {
        let manager = manager(ServiceManagerAccess::CONNECT)
            .context("connect to the service control manager (run as Administrator)")?;
        let svc = manager
            .open_service(
                SERVICE_NAME,
                ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS,
            )
            .with_context(|| format!("open service '{SERVICE_NAME}' (installed?)"))?;
        if svc
            .query_status()
            .is_ok_and(|s| s.current_state == ServiceState::Running)
        {
            eprintln!(
                "note: the service is running; it is now marked for deletion and will disappear \
                 once stopped (sc stop {SERVICE_NAME}) or at reboot."
            );
        }
        svc.delete()
            .context(format!("delete service '{SERVICE_NAME}'"))?;
        println!("Service '{SERVICE_NAME}' uninstalled.");
        Ok(())
    }

    pub fn status() -> anyhow::Result<()> {
        let manager = manager(ServiceManagerAccess::CONNECT)?;
        let svc = manager
            .open_service(SERVICE_NAME, ServiceAccess::QUERY_STATUS)
            .with_context(|| format!("service '{SERVICE_NAME}' is not installed"))?;
        let st = svc.query_status()?;
        let state = match st.current_state {
            ServiceState::Stopped => "stopped",
            ServiceState::StartPending => "starting",
            ServiceState::StopPending => "stopping",
            ServiceState::Running => "running",
            ServiceState::ContinuePending => "resuming",
            ServiceState::PausePending => "pausing",
            ServiceState::Paused => "paused",
        };
        println!("{SERVICE_NAME}: {state}");
        Ok(())
    }

    /// `service run` (without `--in-session`): hand control to the SCM
    /// dispatcher. Blocks until the service stops.
    pub fn dispatch() -> anyhow::Result<()> {
        service_dispatcher::start(SERVICE_NAME, ffi_service_main).context(format!(
            "start the '{SERVICE_NAME}' service dispatcher (this subcommand is meant to be \
             launched by the service control manager, not interactively)"
        ))
    }
}
