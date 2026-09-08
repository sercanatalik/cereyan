//! Process liveness and signals for engine children, including adopted ones
//! that this server did not spawn.

#[cfg(unix)]
pub fn is_alive(pid: u32) -> bool {
    // Signal 0 checks existence and permission without delivering anything.
    // A zombie also answers, which is why owned children use try_wait first.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
pub fn is_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

#[cfg(unix)]
pub fn terminate(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
}

#[cfg(windows)]
pub fn terminate(pid: u32) {
    // Windows has no graceful signal; TerminateProcess after the grace period.
    kill(pid);
}

#[cfg(unix)]
pub fn kill(pid: u32) {
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
}

#[cfg(windows)]
pub fn kill(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
}
