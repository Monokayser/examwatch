//! Runs the server under the Windows service manager (`ew-server service`).
//! The teacher installer registers it as an auto-start service with
//! restart-on-failure; logs go to `<data_dir>/logs/server.log`.

use std::ffi::OsString;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use windows_service::service::{ServiceControl, ServiceControlAccept, ServiceExitCode, ServiceState, ServiceStatus, ServiceType};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::{define_windows_service, service_dispatcher};

use crate::config::Config;

pub const SERVICE_NAME: &str = "ExamWatchServer";
static CONFIG_PATH: OnceLock<PathBuf> = OnceLock::new();

define_windows_service!(ffi_service_main, service_main);

pub fn start(cfg_path: PathBuf) -> Result<()> {
    let _ = CONFIG_PATH.set(cfg_path);
    service_dispatcher::start(SERVICE_NAME, ffi_service_main)?;
    Ok(())
}

fn service_main(_args: Vec<OsString>) {
    let Some(path) = CONFIG_PATH.get() else { return };
    let cfg = match Config::load(path) {
        Ok(c) => c,
        Err(e) => {
            // No logger yet: leave a note next to the config.
            let _ = std::fs::write(path.with_extension("error.txt"), format!("{e:#}"));
            return;
        }
    };
    init_file_logging(&cfg);

    let rt = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!("runtime: {e}");
            return;
        }
    };
    let (stop_tx, mut stop_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let handler = move |ev| match ev {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let Ok(status) = service_control_handler::register(SERVICE_NAME, handler) else { return };
    let set = |state, accept, code| {
        let _ = status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(code),
            checkpoint: 0,
            wait_hint: Duration::from_secs(10),
            process_id: None,
        });
    };
    set(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN, 0);
    let code = rt.block_on(async {
        tokio::select! {
            r = crate::run(cfg) => match r {
                Ok(()) => 0,
                Err(e) => { tracing::error!("server stopped: {e:#}"); 1 }
            },
            _ = stop_rx.recv() => { tracing::info!("service stop requested"); 0 }
        }
    });
    // Let the recorder thread flush open segments before the process exits.
    rt.shutdown_timeout(Duration::from_secs(5));
    std::thread::sleep(Duration::from_millis(500));
    // A non-zero exit code makes the service manager apply its restart policy.
    set(ServiceState::Stopped, ServiceControlAccept::empty(), code);
}

fn init_file_logging(cfg: &Config) {
    let dir = cfg.data_dir.join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("server.log");
    if std::fs::metadata(&path).map(|m| m.len() > 10_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("server.log.1"));
    }
    if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let _ = tracing_subscriber::fmt().with_writer(std::sync::Mutex::new(f)).with_ansi(false).with_env_filter(crate::filter()).try_init();
    }
}
