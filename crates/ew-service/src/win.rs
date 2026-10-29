use std::ffi::OsString;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use windows::core::{w, PCWSTR, PWSTR};
use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree, WAIT_OBJECT_0};
use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
use windows::Win32::Security::{
    DuplicateTokenEx, SecurityImpersonation, TokenPrimary, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_ALL_ACCESS,
};
use windows::Win32::Storage::FileSystem::{ReadFile, PIPE_ACCESS_INBOUND};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::JobObjects::*;
use windows::Win32::System::Pipes::{ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE, PIPE_WAIT};
use windows::Win32::System::RemoteDesktop::{WTSGetActiveConsoleSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{
    CreateProcessAsUserW, GetExitCodeProcess, ResumeThread, TerminateProcess, WaitForSingleObject, BELOW_NORMAL_PRIORITY_CLASS,
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows_service::service::{
    ServiceAccess, ServiceAction, ServiceActionType, ServiceControl, ServiceControlAccept, ServiceErrorControl, ServiceExitCode,
    ServiceFailureActions, ServiceFailureResetPeriod, ServiceInfo, ServiceStartType, ServiceState, ServiceStatus, ServiceType,
};
use windows_service::service_control_handler::{self, ServiceControlHandlerResult};
use windows_service::service_manager::{ServiceManager, ServiceManagerAccess};
use windows_service::{define_windows_service, service_dispatcher};

pub const SERVICE_NAME: &str = "ExamWatchService";
const DISPLAY_NAME: &str = "ExamWatch Student Service";
pub const PIPE_NAME: PCWSTR = w!(r"\\.\pipe\examwatch-agent-heartbeat");
/// Agent is considered hung after this long without a heartbeat.
const HANG_AFTER: Duration = Duration::from_secs(5);
/// Grace period after launch before hang detection applies.
const STARTUP_GRACE: Duration = Duration::from_secs(15);
/// Hard caps for the agent (design §2.8): 5 % of total CPU, 250 MB.
const CPU_CAP_PERCENT: u32 = 5;
const MEMORY_CAP: usize = 250 * 1024 * 1024;

pub fn main() -> Result<()> {
    let cmd = std::env::args().nth(1).unwrap_or_default();
    match cmd.as_str() {
        "install" => install(),
        "uninstall" => uninstall(),
        // Run by the student MSI: copies examwatch-student.cfg into HKLM.
        "import-config" => import_config(std::path::Path::new(&std::env::args().nth(2).context("usage: import-config <file>")?)),
        // Foreground mode for testing from an elevated console.
        "console" => {
            let (_tx, rx) = mpsc::channel();
            supervise(rx);
            Ok(())
        }
        _ => {
            service_dispatcher::start(SERVICE_NAME, ffi_service_main).context("not started by the service manager (use install / console)")?;
            Ok(())
        }
    }
}

// ------------------------------------------------------------------ logging

fn data_dir() -> PathBuf {
    let base = std::env::var_os("ProgramData").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"));
    base.join("ExamWatch")
}

fn log(msg: &str) {
    let dir = data_dir().join("logs");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("service.log");
    if std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("service.log.1"));
    }
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let _ = writeln!(f, "{t} {msg}");
    }
}

// ------------------------------------------------------------- installation

/// Parses `examwatch-student.cfg` (JSON: server, pin, token) and stores it in
/// `HKLM\SOFTWARE\ExamWatch`, where the agent reads it.
pub fn parse_cfg(text: &str) -> Result<(String, String, String)> {
    let v: serde_json::Value = serde_json::from_str(text.trim_start_matches('\u{feff}')).context("examwatch-student.cfg is not valid")?;
    let get = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::trim).filter(|s| !s.is_empty()).map(String::from);
    let server = get("server").context("server missing")?;
    let pin = get("pin").context("pin missing")?;
    let token = get("token").context("token missing")?;
    anyhow::ensure!(server.contains(':'), "server must be host:port");
    anyhow::ensure!(pin.len() == 64 && pin.chars().all(|c| c.is_ascii_hexdigit()), "pin must be a SHA-256 fingerprint");
    anyhow::ensure!(token.len() >= 16, "token too short");
    Ok((server, pin, token))
}

fn import_config(path: &std::path::Path) -> Result<()> {
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_LOCAL_MACHINE, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ,
    };
    let text = std::fs::read_to_string(path).with_context(|| format!("cannot read {}", path.display()))?;
    let (server, pin, token) = parse_cfg(&text)?;
    unsafe {
        let mut key = HKEY::default();
        RegCreateKeyExW(HKEY_LOCAL_MACHINE, w!("SOFTWARE\\ExamWatch"), 0, None, REG_OPTION_NON_VOLATILE, KEY_WRITE, None, &mut key, None)
            .ok()
            .context("cannot create HKLM\\SOFTWARE\\ExamWatch (run as administrator)")?;
        for (name, value) in [("Server", &server), ("ServerPin", &pin), ("EnrollToken", &token)] {
            let wide: Vec<u16> = value.encode_utf16().chain(Some(0)).collect();
            let bytes = std::slice::from_raw_parts(wide.as_ptr() as *const u8, wide.len() * 2);
            let wname: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
            RegSetValueExW(key, PCWSTR(wname.as_ptr()), 0, REG_SZ, Some(bytes)).ok().with_context(|| format!("cannot write {name}"))?;
        }
        let _ = RegCloseKey(key);
    }
    log(&format!("settings imported from {} (server {server})", path.display()));
    Ok(())
}

fn install() -> Result<()> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT | ServiceManagerAccess::CREATE_SERVICE)?;
    let info = ServiceInfo {
        name: OsString::from(SERVICE_NAME),
        display_name: OsString::from(DISPLAY_NAME),
        service_type: ServiceType::OWN_PROCESS,
        start_type: ServiceStartType::AutoStart,
        error_control: ServiceErrorControl::Normal,
        executable_path: std::env::current_exe()?,
        launch_arguments: vec![],
        dependencies: vec![],
        account_name: None, // LocalSystem
        account_password: None,
    };
    let svc = mgr.create_service(&info, ServiceAccess::CHANGE_CONFIG | ServiceAccess::START)?;
    svc.set_description("Keeps the ExamWatch exam-monitoring agent running during exams. Captures nothing outside an exam.")?;
    // The service manager restarts us too: the watchdog has its own watchdog.
    svc.update_failure_actions(ServiceFailureActions {
        reset_period: ServiceFailureResetPeriod::After(Duration::from_secs(24 * 3600)),
        reboot_msg: None,
        command: None,
        actions: Some(vec![
            ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(1) },
            ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(2) },
            ServiceAction { action_type: ServiceActionType::Restart, delay: Duration::from_secs(5) },
        ]),
    })?;
    svc.set_failure_actions_on_non_crash_failures(true)?;
    svc.start::<&str>(&[])?;
    println!("{DISPLAY_NAME} installed and started");
    Ok(())
}

fn uninstall() -> Result<()> {
    let mgr = ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)?;
    let svc = mgr.open_service(SERVICE_NAME, ServiceAccess::STOP | ServiceAccess::DELETE | ServiceAccess::QUERY_STATUS)?;
    let _ = svc.stop();
    for _ in 0..20 {
        if svc.query_status()?.current_state == ServiceState::Stopped {
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    svc.delete()?;
    println!("{DISPLAY_NAME} removed");
    Ok(())
}

// ------------------------------------------------------------------ service

define_windows_service!(ffi_service_main, service_main);

fn service_main(_args: Vec<OsString>) {
    if let Err(e) = run_service() {
        log(&format!("service failed: {e:#}"));
    }
}

fn run_service() -> Result<()> {
    let (stop_tx, stop_rx) = mpsc::channel();
    let handler = move |ev| match ev {
        ServiceControl::Stop | ServiceControl::Shutdown => {
            let _ = stop_tx.send(());
            ServiceControlHandlerResult::NoError
        }
        ServiceControl::Interrogate => ServiceControlHandlerResult::NoError,
        _ => ServiceControlHandlerResult::NotImplemented,
    };
    let status = service_control_handler::register(SERVICE_NAME, handler)?;
    let set = |state, accept| {
        status.set_service_status(ServiceStatus {
            service_type: ServiceType::OWN_PROCESS,
            current_state: state,
            controls_accepted: accept,
            exit_code: ServiceExitCode::Win32(0),
            checkpoint: 0,
            wait_hint: Duration::from_secs(5),
            process_id: None,
        })
    };
    set(ServiceState::Running, ServiceControlAccept::STOP | ServiceControlAccept::SHUTDOWN)?;
    log("service started");
    supervise(stop_rx);
    log("service stopping");
    set(ServiceState::Stopped, ServiceControlAccept::empty())?;
    Ok(())
}

// --------------------------------------------------------------- supervisor

struct Agent {
    process: HANDLE,
    session: u32,
    started: Instant,
    _job: HANDLE,
}

impl Drop for Agent {
    fn drop(&mut self) {
        unsafe {
            let _ = TerminateProcess(self.process, 1);
            let _ = CloseHandle(self.process);
            // Closing the job kills anything left in it (KILL_ON_JOB_CLOSE).
            let _ = CloseHandle(self._job);
        }
    }
}

fn supervise(stop: mpsc::Receiver<()>) {
    let last_beat = Arc::new(AtomicU64::new(0));
    spawn_heartbeat_listener(last_beat.clone());

    let agent_path = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("ew-agent.exe")));
    let Some(agent_path) = agent_path.filter(|p| p.exists()) else {
        log("ew-agent.exe not found next to the service; nothing to supervise");
        let _ = stop.recv();
        return;
    };

    let mut agent: Option<Agent> = None;
    let mut next_start = Instant::now();
    let mut recent_restarts: Vec<Instant> = Vec::new();

    loop {
        match stop.recv_timeout(Duration::from_millis(500)) {
            Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
            Err(RecvTimeoutError::Timeout) => {}
        }
        let session = unsafe { WTSGetActiveConsoleSessionId() };

        if let Some(a) = &agent {
            let exited = unsafe { WaitForSingleObject(a.process, 0) } == WAIT_OBJECT_0;
            let wrong_session = a.session != session;
            let beat_age = Duration::from_millis(now_ms().saturating_sub(last_beat.load(Ordering::Relaxed)));
            let hung = a.started.elapsed() > STARTUP_GRACE && beat_age > HANG_AFTER;
            if exited || wrong_session || hung {
                let mut code = 0u32;
                unsafe {
                    let _ = GetExitCodeProcess(a.process, &mut code);
                }
                log(&format!(
                    "agent {} (session {} → {session}, exit code {code})",
                    if exited { "exited" } else if hung { "hung; restarting" } else { "left the active session" },
                    a.session
                ));
                agent = None; // Drop terminates it if still running
                recent_restarts.retain(|t| t.elapsed() < Duration::from_secs(60));
                recent_restarts.push(Instant::now());
                // Crash loop: back off so we never burn CPU restarting.
                let delay = if recent_restarts.len() > 5 { Duration::from_secs(10) } else { Duration::from_millis(500) };
                next_start = Instant::now() + delay;
            }
        }

        if agent.is_none() && session != u32::MAX && Instant::now() >= next_start {
            match launch(&agent_path, session) {
                Ok(a) => {
                    log(&format!("agent started in session {session}"));
                    last_beat.store(now_ms(), Ordering::Relaxed);
                    agent = Some(a);
                }
                Err(e) => {
                    // Normal at the logon screen (no user token yet).
                    next_start = Instant::now() + Duration::from_secs(2);
                    if !format!("{e:#}").contains("1008") {
                        log(&format!("agent launch failed: {e:#}"));
                    }
                }
            }
        }
    }
    drop(agent);
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn to_wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn launch(path: &std::path::Path, session: u32) -> Result<Agent> {
    unsafe {
        let mut user = HANDLE::default();
        WTSQueryUserToken(session, &mut user).context("no user logged on")?;
        let mut primary = HANDLE::default();
        let dup = DuplicateTokenEx(user, TOKEN_ALL_ACCESS, None, SecurityImpersonation, TokenPrimary, &mut primary);
        let _ = CloseHandle(user);
        dup?;

        let mut env = std::ptr::null_mut();
        let have_env = CreateEnvironmentBlock(&mut env, primary, false).is_ok();

        let job = CreateJobObjectW(None, PCWSTR::null())?;
        let cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
            ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
            Anonymous: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0 { CpuRate: CPU_CAP_PERCENT * 100 },
        };
        SetInformationJobObject(
            job,
            JobObjectCpuRateControlInformation,
            &cpu as *const _ as *const _,
            std::mem::size_of_val(&cpu) as u32,
        )?;
        let mut ext = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        ext.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_PRIORITY_CLASS;
        ext.BasicLimitInformation.PriorityClass = BELOW_NORMAL_PRIORITY_CLASS.0;
        ext.ProcessMemoryLimit = MEMORY_CAP;
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &ext as *const _ as *const _,
            std::mem::size_of_val(&ext) as u32,
        )?;

        let mut desktop = to_wide("winsta0\\default");
        let si = STARTUPINFOW {
            cb: std::mem::size_of::<STARTUPINFOW>() as u32,
            lpDesktop: PWSTR(desktop.as_mut_ptr()),
            ..Default::default()
        };
        let mut pi = PROCESS_INFORMATION::default();
        let app = to_wide(&path.to_string_lossy());
        let mut cmdline = to_wide(&format!("\"{}\" --managed", path.display()));
        let created = CreateProcessAsUserW(
            primary,
            PCWSTR(app.as_ptr()),
            PWSTR(cmdline.as_mut_ptr()),
            None,
            None,
            false,
            CREATE_SUSPENDED | CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS,
            if have_env { Some(env as *const _) } else { None },
            PCWSTR::null(),
            &si,
            &mut pi,
        );
        if have_env {
            let _ = DestroyEnvironmentBlock(env);
        }
        let _ = CloseHandle(primary);
        if let Err(e) = created {
            let _ = CloseHandle(job);
            bail!("CreateProcessAsUser failed: {e}");
        }
        // Into the job before it runs a single instruction.
        if let Err(e) = AssignProcessToJobObject(job, pi.hProcess) {
            let _ = TerminateProcess(pi.hProcess, 1);
            let _ = CloseHandle(pi.hThread);
            let _ = CloseHandle(pi.hProcess);
            let _ = CloseHandle(job);
            bail!("AssignProcessToJobObject failed: {e}");
        }
        ResumeThread(pi.hThread);
        let _ = CloseHandle(pi.hThread);
        Ok(Agent { process: pi.hProcess, session, started: Instant::now(), _job: job })
    }
}

/// Named pipe the agent writes one byte to every second. Only SYSTEM and
/// interactive users may open it.
fn spawn_heartbeat_listener(last_beat: Arc<AtomicU64>) {
    std::thread::Builder::new()
        .name("heartbeat".into())
        .spawn(move || loop {
            if let Err(e) = unsafe { serve_pipe_once(&last_beat) } {
                log(&format!("heartbeat pipe: {e:#}"));
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .expect("spawn heartbeat thread");
}

unsafe fn serve_pipe_once(last_beat: &AtomicU64) -> Result<()> {
    let mut psd = PSECURITY_DESCRIPTOR::default();
    ConvertStringSecurityDescriptorToSecurityDescriptorW(w!("D:(A;;GA;;;SY)(A;;GRGW;;;IU)"), SDDL_REVISION_1, &mut psd, None)?;
    let sa = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: psd.0,
        bInheritHandle: false.into(),
    };
    let pipe = CreateNamedPipeW(PIPE_NAME, PIPE_ACCESS_INBOUND, PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT, 1, 0, 64, 0, Some(&sa));
    let _ = LocalFree(HLOCAL(psd.0));
    if pipe.is_invalid() {
        bail!("CreateNamedPipe failed: {}", windows::core::Error::from_win32());
    }
    // Blocks until the agent connects (ERROR_PIPE_CONNECTED also means connected).
    let _ = ConnectNamedPipe(pipe, None);
    let mut buf = [0u8; 16];
    loop {
        let mut n = 0u32;
        if ReadFile(pipe, Some(&mut buf), Some(&mut n), None).is_err() || n == 0 {
            break;
        }
        last_beat.store(now_ms(), Ordering::Relaxed);
    }
    let _ = DisconnectNamedPipe(pipe);
    let _ = CloseHandle(pipe);
    Ok(())
}
