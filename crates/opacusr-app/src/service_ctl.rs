//! Windows host-agent service from the GUI side: query the SCM state of
//! `OpacusRAgent` and install it with a UAC elevation (the MSI path for
//! zip users — no admin console needed).
//!
//! The service itself (supervisor + per-session child) lives in
//! `opacusr-agent`; this module only talks to the service control
//! manager and launches the sibling `opacusr-agent.exe` elevated with
//! `service install`.

use std::path::PathBuf;

/// Service states the UI distinguishes (SCM states collapsed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SvcState {
    Running,
    Stopped,
    StartPending,
    StopPending,
    /// Installed but in an unusual state (paused, pause/continue pending).
    Other,
    /// The service is not registered (or the SCM refuses to talk —
    /// treated the same way, the GUI then offers the install button).
    NotInstalled,
}

/// Query the current state of the agent service. Does not require
/// admin: CONNECT + QUERY_STATUS are unprivileged operations.
#[cfg(windows)]
pub fn query_state() -> SvcState {
    use windows_service::service::{ServiceAccess, ServiceState};
    use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};

    let q = || -> Option<ServiceState> {
        let manager = ServiceManager::local_computer(
            None::<&std::ffi::OsString>,
            ServiceManagerAccess::CONNECT,
        )
        .ok()?;
        let svc = manager
            .open_service(
                opacusr_agent::win_service::SERVICE_NAME,
                ServiceAccess::QUERY_STATUS,
            )
            .ok()?;
        svc.query_status().ok().map(|s| s.current_state)
    };
    match q() {
        Some(ServiceState::Running) => SvcState::Running,
        Some(ServiceState::Stopped) => SvcState::Stopped,
        Some(ServiceState::StartPending) => SvcState::StartPending,
        Some(ServiceState::StopPending) => SvcState::StopPending,
        Some(_) => SvcState::Other,
        None => SvcState::NotInstalled,
    }
}

/// Non-Windows builds have no service to query (the module is only
/// compiled on Windows — kept for the lib-level cfg fallback).
#[cfg(not(windows))]
pub fn query_state() -> SvcState {
    SvcState::NotInstalled
}

/// Path of the sibling `opacusr-agent.exe` (MSI and zip layouts put the
/// three binaries side by side).
pub fn agent_exe_path() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let agent = dir.join("opacusr-agent.exe");
    agent.is_file().then_some(agent)
}

/// Install the service with a UAC prompt: launches the elevated
/// `opacusr-agent.exe service install`, which registers the auto-start
/// service and starts it immediately. Blocks until the elevated process
/// exits (it waits for SCM bookkeeping, normally a few seconds).
#[cfg(windows)]
pub fn install_elevated() -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};

    // SEE_MASK_NOCLOSEPROCESS: return hProcess so we can wait and
    // read the exit code. SEE_MASK_NOASYNC: the call must behave
    // synchronously when we wait on it.
    const SEE_MASK_NOCLOSEPROCESS: u32 = 0x0000_0040;
    const SEE_MASK_NOASYNC: u32 = 0x0000_1000;
    const SW_SHOW: i32 = 5;

    let agent = agent_exe_path().ok_or_else(|| {
        anyhow::anyhow!("рядом с opacusr.exe нет opacusr-agent.exe — установите пакет целиком")
    })?;

    let verb: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
    let file: Vec<u16> = agent
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let params: Vec<u16> = "service install"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let mut sei: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    sei.fMask = SEE_MASK_NOCLOSEPROCESS | SEE_MASK_NOASYNC;
    sei.lpVerb = verb.as_ptr();
    sei.lpFile = file.as_ptr();
    sei.lpParameters = params.as_ptr();
    sei.nShow = SW_SHOW;

    let ok = unsafe { ShellExecuteExW(&mut sei) };
    if ok == 0 {
        let err = unsafe { windows_sys::Win32::Foundation::GetLastError() };
        // SE_ERR_ACCESSDENIED / ERROR_CANCELLED: the user dismissed
        // the UAC prompt.
        if err == 5 || err == 1223 {
            anyhow::bail!("установка отменена (UAC отклонён)");
        }
        anyhow::bail!("запуск с правами администратора не удался (код {err})");
    }

    // Wait for the elevated installer to finish.
    let code = unsafe {
        if WaitForSingleObject(sei.hProcess, 120_000) != 0 {
            anyhow::bail!("установщик службы не завершился за 2 минуты");
        }
        let mut exit: u32 = 0;
        if GetExitCodeProcess(sei.hProcess, &mut exit) == 0 {
            anyhow::bail!("не удалось получить код выхода установщика службы");
        }
        exit
    };
    unsafe {
        let _ = windows_sys::Win32::Foundation::CloseHandle(sei.hProcess);
    }
    if code != 0 {
        anyhow::bail!(
            "«service install» завершился с кодом {code} — проверьте вывод в консоли установщика"
        );
    }
    Ok(())
}
