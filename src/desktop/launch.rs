//! Detached application launching.
//!
//! Spawns the command directly (never via a shell — no `sh -c`, no expansion)
//! in a new session via `setsid`, so the launched app outlives the daemon. A
//! detached thread reaps the child to avoid zombies while the daemon runs;
//! once the daemon exits, the session-leader child is reparented to init.

use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};

/// Launch `command` with `args` detached. Returns the child PID.
pub fn launch_detached(command: &str, args: &[String]) -> std::io::Result<u32> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());

    // SAFETY: setsid() is async-signal-safe and the only thing we do in the
    // forked child before exec.
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    let child = cmd.spawn()?;
    let pid = child.id();

    // Reap on a detached thread so we don't accumulate zombies; if the daemon
    // exits first, the thread dies and the setsid child keeps running.
    std::thread::spawn(move || {
        let mut child = child;
        let _ = child.wait();
    });

    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn launches_real_process_returns_pid() {
        let pid = launch_detached("/bin/true", &[]).expect("spawn /bin/true");
        assert!(pid > 0);
    }

    #[test]
    fn passes_args_as_argv_not_shell() {
        // `/bin/echo` with two args runs fine; if this were routed through a
        // shell the args would be reinterpreted. We only assert it spawns.
        let pid = launch_detached("/bin/echo", &["hello".into(), "world".into()])
            .expect("spawn /bin/echo");
        assert!(pid > 0);
    }

    #[test]
    fn nonexistent_command_errors() {
        let err = launch_detached("/nonexistent/binary/xyzzy", &[]);
        assert!(err.is_err());
    }
}
