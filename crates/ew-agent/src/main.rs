//! ExamWatch student agent.
//!
//! Started in the student's session by `ew-service` (`--managed`). The V1
//! desktop shortcut runs `ew-agent.exe --v1`, which only tells the running
//! agent to show its Start window.
//!
//! Threads: UI (main) · network (Tokio, current-thread) · capture · heartbeat.

#![cfg_attr(not(test), windows_subsystem = "windows")]

mod config;
mod state;

#[cfg(windows)]
mod capture;
#[cfg(windows)]
mod net;
#[cfg(windows)]
mod ui;

#[cfg(windows)]
fn main() {
    if let Err(e) = win_main() {
        tracing::error!("agent stopped: {e:#}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("ew-agent only runs on Windows");
}

#[cfg(windows)]
fn win_main() -> anyhow::Result<()> {
    use std::sync::Arc;
    use std::time::Duration;

    let args: Vec<String> = std::env::args().collect();
    let v1 = args.iter().any(|a| a == "--v1");
    let managed = args.iter().any(|a| a == "--managed");

    // V1 shortcut while the agent is already running: just bring up its window.
    if v1 && ui::signal_running_instance() {
        return Ok(());
    }
    if !single_instance() {
        return Ok(());
    }
    init_logging();
    low_impact_process();

    let settings = match config::load() {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("{e:#}");
            if v1 {
                message_box(&format!("{e:#}"));
            }
            return Ok(());
        }
    };
    let probe = capture::probe();
    tracing::info!(encoder = ?probe.encoder, gpu = %probe.gpu, screen = ?probe.screen, "starting");

    let shared = Arc::new(state::Shared::default());
    let cfg = ew_proto::client::ClientConfig {
        server: settings.server.clone(),
        pin_sha256: settings.pin.clone(),
        enroll_token: settings.token.clone(),
        machine_id: settings.machine_id.clone(),
        pc_name: settings.pc_name.clone(),
        agent_version: env!("CARGO_PKG_VERSION").into(),
        encoder: probe.encoder,
        gpu: probe.gpu.clone(),
        screen: probe.screen,
        monitors: probe.monitors,
        rec_backlog_bytes: 24 * 1024 * 1024,
        backlog_kbps: 2000,
        // Spread a whole lab booting at once; no delay when a student starts us by hand.
        start_jitter: if managed { Duration::from_secs(3) } else { Duration::ZERO },
    };

    // Network thread.
    let (tx, rx) = std::sync::mpsc::channel();
    {
        let shared = shared.clone();
        std::thread::Builder::new().name("net".into()).spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("tokio runtime");
            rt.block_on(async move {
                let (handle, events) = ew_proto::client::spawn(cfg);
                let _ = tx.send(handle.clone());
                net::run(shared, handle, events, Box::new(ui::post_refresh)).await;
            });
        })?;
    }
    let net_handle = rx.recv()?;

    // Capture thread (idle until an exam starts and the student confirms their ID).
    {
        let shared = shared.clone();
        let h = net_handle.clone();
        std::thread::Builder::new().name("capture".into()).spawn(move || capture::run(shared, h))?;
    }

    // Heartbeat to the watchdog service.
    if managed {
        std::thread::Builder::new().name("heartbeat".into()).spawn(heartbeat)?;
    }

    ui::run(shared, net_handle, v1)
}

/// Writes one byte a second to the service's pipe; reconnects if the service restarts.
#[cfg(windows)]
fn heartbeat() {
    use std::io::Write;
    loop {
        if let Ok(mut pipe) = std::fs::OpenOptions::new().write(true).open(r"\\.\pipe\examwatch-agent-heartbeat") {
            while pipe.write_all(&[1]).is_ok() {
                std::thread::sleep(std::time::Duration::from_secs(1));
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(2));
    }
}

#[cfg(windows)]
fn single_instance() -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    unsafe {
        // Leaked on purpose: held for the life of the process.
        match CreateMutexW(None, true, w!("Local\\ExamWatchAgent")) {
            Ok(_) => GetLastError() != ERROR_ALREADY_EXISTS,
            Err(_) => false,
        }
    }
}

/// Design §2.8: below-normal priority, EcoQoS (efficiency cores on Windows 11),
/// background memory priority, and no crash dialogs ever shown to students.
#[cfg(windows)]
fn low_impact_process() {
    use windows::Win32::System::Diagnostics::Debug::{SetErrorMode, SEM_FAILCRITICALERRORS, SEM_NOGPFAULTERRORBOX, SEM_NOOPENFILEERRORBOX};
    use windows::Win32::System::Threading::*;
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOGPFAULTERRORBOX | SEM_NOOPENFILEERRORBOX);
        let me = GetCurrentProcess();
        let _ = SetPriorityClass(me, BELOW_NORMAL_PRIORITY_CLASS);
        let throttle = PROCESS_POWER_THROTTLING_STATE {
            Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
            ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
            StateMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        };
        let _ = SetProcessInformation(me, ProcessPowerThrottling, &throttle as *const _ as *const _, std::mem::size_of_val(&throttle) as u32);
        let mem = MEMORY_PRIORITY_INFORMATION { MemoryPriority: MEMORY_PRIORITY_LOW };
        let _ = SetProcessInformation(me, ProcessMemoryPriority, &mem as *const _ as *const _, std::mem::size_of_val(&mem) as u32);
    }
}

#[cfg(windows)]
fn message_box(text: &str) {
    use windows::core::{w, HSTRING};
    use windows::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONWARNING, MB_OK};
    unsafe {
        MessageBoxW(None, &HSTRING::from(text), w!("ExamWatch"), MB_OK | MB_ICONWARNING);
    }
}

#[cfg(windows)]
fn init_logging() {
    let dir = std::env::var_os("LOCALAPPDATA").map(std::path::PathBuf::from).unwrap_or_else(std::env::temp_dir).join("ExamWatch");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("agent.log");
    if std::fs::metadata(&path).map(|m| m.len() > 2_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("agent.log.1"));
    }
    if let Ok(file) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = tracing_subscriber::fmt()
            .with_writer(std::sync::Mutex::new(file))
            .with_ansi(false)
            .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,quinn=warn".into()))
            .try_init();
    }
}
