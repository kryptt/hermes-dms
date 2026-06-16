//! Detached application launching.
//!
//! Spawns the command directly (never via a shell — no `sh -c`, no expansion)
//! in its own process group via [`process_group`], with all std streams pointed
//! at `/dev/null`, so the launched app is insulated from signals delivered to
//! the daemon's process group and outlives the daemon. The child is reaped on a
//! background task to avoid zombies while the daemon runs; once the daemon
//! exits the child is reparented to the init/user-manager subreaper and keeps
//! running.
//!
//! ## Why `process_group(0)` and not `setsid()`
//!
//! The previous implementation called `libc::setsid()` from a `pre_exec` hook,
//! which required an `unsafe` block. `setsid()` makes the child a *session*
//! leader (new session + new process group) and detaches it from the
//! controlling terminal. This daemon runs as a systemd **user service**, which
//! has no controlling terminal, so the only effect of `setsid()` that ever
//! mattered here is the new process group — and that is exactly what the safe,
//! stable `Command::process_group` provides without any `unsafe`.
//!
//! Behavioral difference: the child is no longer a session leader. For a GUI
//! app launched from a ttyless service this is invisible. (Surviving a *service
//! stop* is governed by the unit's `KillMode`, not by session leadership —
//! neither `setsid` nor `process_group` escapes the service cgroup, so that
//! concern is unchanged and lives in the systemd unit, not here.)
//!
//! [`process_group`]: tokio::process::Command::process_group

use std::process::Stdio;

use tokio::process::Command;

/// Launch `command` with `args` detached. Returns the child PID.
///
/// Must be called from within a Tokio runtime: the child is reaped on a
/// spawned task using async `wait` (no blocking thread is parked for the life
/// of the launched app).
pub fn launch_detached(command: &str, args: &[String]) -> std::io::Result<u32> {
    let mut child = Command::new(command)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // New process group (pgid = child pid): a signal sent to the daemon's
        // process group won't reach the launched app. Safe and `unsafe`-free.
        .process_group(0)
        .spawn()?;

    // `Child::id()` is `None` only once the child has been reaped; we have not
    // awaited `wait()` yet, so the PID is always present here.
    let pid = child
        .id()
        .ok_or_else(|| std::io::Error::other("child exited before its PID could be read"))?;

    // Reap on a background task so we don't accumulate zombies. If the daemon
    // exits first the task is dropped; the child is reparented and keeps
    // running (Tokio does not kill it — `kill_on_drop` defaults to false).
    tokio::spawn(async move {
        if let Err(e) = child.wait().await {
            tracing::debug!(pid, error = %e, "reaping launched child failed");
        }
    });

    Ok(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn launches_real_process_returns_pid() {
        let pid = launch_detached("/bin/true", &[]).expect("spawn /bin/true");
        assert!(pid > 0);
    }

    #[tokio::test]
    async fn passes_args_as_argv_not_shell() {
        // `/bin/echo` with two args runs fine; if this were routed through a
        // shell the args would be reinterpreted. We only assert it spawns.
        let pid = launch_detached("/bin/echo", &["hello".into(), "world".into()])
            .expect("spawn /bin/echo");
        assert!(pid > 0);
    }

    #[tokio::test]
    async fn nonexistent_command_errors() {
        let err = launch_detached("/nonexistent/binary/xyzzy", &[]);
        assert!(err.is_err());
    }
}
