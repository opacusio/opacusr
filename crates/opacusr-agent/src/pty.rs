//! Remote terminal support (host side) built on `portable-pty`
//! (ConPTY on Windows, openpty on Unix).
//!
//! One [`TermSession`] per `TermOpen`; PTY output is streamed to the
//! session loop through an mpsc channel. Desktop platforms only — mobile
//! builds gate this module off at the call sites.

use std::io::Read;
use std::sync::{Arc, Mutex};

use opacusr_proto::message::{TermData, TermOpened};

/// Platform default shell.
pub fn default_shell() -> String {
    #[cfg(windows)]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "powershell.exe".into())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())
    }
}

/// A live remote shell.
pub struct TermSession {
    pub id: u32,
    pub shell: String,
    writer: Arc<Mutex<Box<dyn std::io::Write + Send>>>,
    master: Arc<Mutex<Option<Box<dyn portable_pty::MasterPty + Send>>>>,
    child: Arc<Mutex<Option<Box<dyn portable_pty::Child + Send + Sync>>>>,
    done: Arc<std::sync::atomic::AtomicBool>,
}

impl TermSession {
    /// Spawn the shell sized `rows`×`cols`. Returns the session, the
    /// `TermOpened` reply, and the PTY output receiver (server → viewer).
    pub fn spawn(
        id: u32,
        rows: u16,
        cols: u16,
        shell_program: Option<String>,
    ) -> (
        std::io::Result<Self>,
        TermOpened,
        tokio::sync::mpsc::UnboundedReceiver<TermData>,
    ) {
        let shell = shell_program.unwrap_or_else(default_shell);
        let fail = |msg: String| {
            (
                std::io::Result::Err(std::io::Error::other(msg.clone())),
                TermOpened {
                    id,
                    ok: false,
                    error: Some(msg),
                    shell: shell.clone(),
                },
                // Dummy receiver; the caller discards it on error.
                tokio::sync::mpsc::unbounded_channel().1,
            )
        };

        let pty_system = portable_pty::native_pty_system();
        let pair = match pty_system.openpty(portable_pty::PtySize {
            rows: rows.max(1),
            cols: cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        }) {
            Ok(p) => p,
            Err(e) => return fail(format!("openpty: {e}")),
        };

        let mut cmd = portable_pty::CommandBuilder::new(&shell);
        cmd.env("TERM", "xterm-256color");
        let child = match pair.slave.spawn_command(cmd) {
            Ok(c) => c,
            Err(e) => return fail(format!("spawn {shell}: {e}")),
        };

        let reader = match pair.master.try_clone_reader() {
            Ok(r) => r,
            Err(e) => return fail(format!("pty reader: {e}")),
        };
        let writer = match pair.master.take_writer() {
            Ok(w) => w,
            Err(e) => return fail(format!("pty writer: {e}")),
        };

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));

        // Blocking PTY → session pump on a dedicated thread.
        let done2 = done.clone();
        std::thread::spawn(move || {
            let mut reader = reader;
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        if tx
                            .send(TermData {
                                id,
                                data: buf[..n].to_vec(),
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            done2.store(true, std::sync::atomic::Ordering::Relaxed);
        });

        let sess = Self {
            id,
            shell: shell.clone(),
            writer: Arc::new(Mutex::new(writer)),
            master: Arc::new(Mutex::new(Some(pair.master))),
            child: Arc::new(Mutex::new(Some(child))),
            done,
        };
        let reply = TermOpened {
            id,
            ok: true,
            error: None,
            shell,
        };
        (Ok(sess), reply, rx)
    }

    /// Write bytes from the remote user into the shell.
    pub fn write(&self, data: &[u8]) -> std::io::Result<()> {
        let mut w = self.writer.lock().unwrap();
        w.write_all(data)
    }

    /// Resize the pseudo terminal.
    pub fn resize(&self, rows: u16, cols: u16) -> std::io::Result<()> {
        let guard = self.master.lock().unwrap();
        let master = guard
            .as_ref()
            .ok_or_else(|| std::io::Error::other("pty master closed"))?;
        master
            .resize(portable_pty::PtySize {
                rows: rows.max(1),
                cols: cols.max(1),
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Kill the shell process.
    pub fn kill(&self) {
        if let Some(mut child) = self.child.lock().unwrap().take() {
            let _ = child.kill();
        }
        self.master.lock().unwrap().take();
    }

    /// Has the PTY output loop finished (shell exited)?
    pub fn is_done(&self) -> bool {
        self.done.load(std::sync::atomic::Ordering::Relaxed)
    }
}
