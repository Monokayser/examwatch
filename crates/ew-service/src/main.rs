//! ExamWatch watchdog: a Windows service (LocalSystem) on each student PC.
//!
//! A service can't capture the desktop (Session 0 isolation), so this service
//! launches `ew-agent.exe` into the logged-in user's session and keeps it alive:
//!
//! * restarts it within ~1 s if it exits or crashes (with backoff in a crash loop);
//! * kills and restarts it if it stops sending heartbeats for 5 s (hung);
//! * follows the console session across logoff / logon / user switch;
//! * runs it inside a **Job Object** with a hard CPU cap and a memory cap, so
//!   even a bug in the agent can't slow the student's PC down.
//!
//! ```text
//! ew-service install | uninstall | console
//! ```

#[cfg(windows)]
mod win;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    win::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ew-service only runs on Windows");
}
