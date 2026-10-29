//! Network thread: one current-thread Tokio runtime driving the connection
//! manager and translating server messages into agent state.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use ew_proto::client::{ClientEvent, ClientHandle};
use ew_proto::{AgentMsg, ServerMsg};
use tokio::sync::mpsc;

use crate::state::{self, Saved, Shared};

/// Asks the UI thread to redraw from `Shared::ui`.
pub type Notify = Box<dyn Fn() + Send + Sync>;

pub async fn run(shared: Arc<Shared>, net: ClientHandle, mut events: mpsc::Receiver<ClientEvent>, notify: Notify) {
    let mut saved = state::load_saved();
    let mut hb = tokio::time::interval(Duration::from_secs(1));
    let mut usage = ProcUsage::new();
    {
        loop {
            tokio::select! {
                ev = events.recv() => {
                    let Some(ev) = ev else { return };
                    handle(&shared, &net, ev, &mut saved);
                    notify();
                }
                _ = hb.tick() => {
                    if shared.quit.load(Ordering::Relaxed) { return; }
                    if shared.ui.lock().unwrap().connected {
                        let (cpu, mb) = usage.sample();
                        net.send(AgentMsg::Heartbeat {
                            sent_us: ew_proto::now_us(),
                            cpu_pct: cpu,
                            rss_mb: mb,
                            throttle: shared.throttle.load(Ordering::Relaxed),
                        });
                    }
                }
            }
        }
    }
}

fn handle(shared: &Shared, net: &ClientHandle, ev: ClientEvent, saved: &mut Option<Saved>) {
    let mut ui = shared.ui.lock().unwrap();
    match ev {
        ClientEvent::Connected => ui.connected = true,
        ClientEvent::Disconnected => {
            ui.connected = false;
            ui.claim_pending = false;
            // Keep capturing into the backlog; restart REC on a keyframe so it stays decodable.
            shared.force_key.store(true, Ordering::SeqCst);
            shared.live.store(false, Ordering::SeqCst);
        }
        ClientEvent::Server(m) => match m {
            ServerMsg::Welcome { lab_name, exam, server_us, .. } => {
                shared.clock_offset_us.store(server_us as i64 - ew_proto::now_us() as i64, Ordering::Relaxed);
                ui.lab_name = lab_name;
                set_exam(shared, &mut ui, net, exam, saved);
            }
            ServerMsg::ExamOpened { exam } => set_exam(shared, &mut ui, net, Some(exam), saved),
            ServerMsg::ExamClosed { .. } => set_exam(shared, &mut ui, net, None, saved),
            ServerMsg::ClaimResult { ok, student_id, reason } => {
                ui.claim_pending = false;
                if ok {
                    ui.student_id = Some(student_id.clone());
                    ui.message = None;
                    if let Some(e) = &ui.exam {
                        *saved = Some(Saved { exam_id: e.exam_id, student_id });
                        state::store_saved(saved.as_ref());
                    }
                    shared.set_capturing(true);
                } else {
                    // Rejected, or the teacher reset this PC: stop and ask again.
                    if ui.student_id.is_some() {
                        shared.set_capturing(false);
                        ui.student_id = None;
                        *saved = None;
                        state::store_saved(None);
                    }
                    ui.message = reason;
                }
            }
            ServerMsg::LiveOn { .. } => {
                shared.live.store(true, Ordering::SeqCst);
                shared.force_key.store(true, Ordering::SeqCst);
            }
            ServerMsg::LiveOff => shared.live.store(false, Ordering::SeqCst),
            ServerMsg::Pressure { .. } => {}
            ServerMsg::Reject { reason } => ui.message = Some(format!("The exam server refused this PC: {reason}")),
        },
    }
}

fn set_exam(shared: &Shared, ui: &mut crate::state::UiState, net: &ClientHandle, exam: Option<ew_proto::ExamInfo>, saved: &mut Option<Saved>) {
    match &exam {
        None => {
            shared.set_capturing(false);
            ui.student_id = None;
            ui.message = None;
            *saved = None;
            state::store_saved(None);
        }
        Some(e) => {
            // Same exam as before a crash/reconnect: re-register silently.
            let again = saved.as_ref().filter(|s| s.exam_id == e.exam_id).map(|s| s.student_id.clone())
                .or_else(|| ui.student_id.clone().filter(|_| ui.exam.as_ref().map(|x| x.exam_id) == Some(e.exam_id)));
            match again {
                Some(id) => {
                    ui.student_id = Some(id.clone());
                    shared.set_capturing(true);
                    net.send(AgentMsg::Claim { student_id: id });
                }
                None => {
                    if saved.is_some() {
                        *saved = None;
                        state::store_saved(None);
                    }
                    shared.set_capturing(false);
                    ui.student_id = None;
                }
            }
        }
    }
    ui.exam = exam;
}

/// This process's CPU % (of the whole machine) and working set, for the dashboard.
struct ProcUsage {
    last_cpu: u64,
    last_wall: std::time::Instant,
    cores: u32,
}

impl ProcUsage {
    fn new() -> Self {
        Self { last_cpu: proc_cpu_100ns(), last_wall: std::time::Instant::now(), cores: std::thread::available_parallelism().map(|n| n.get() as u32).unwrap_or(1) }
    }

    fn sample(&mut self) -> (f32, f32) {
        let cpu = proc_cpu_100ns();
        let wall = self.last_wall.elapsed().as_nanos() as f64 / 100.0;
        let pct = if wall > 0.0 { (cpu.saturating_sub(self.last_cpu)) as f64 / wall / self.cores as f64 * 100.0 } else { 0.0 };
        self.last_cpu = cpu;
        self.last_wall = std::time::Instant::now();
        (pct as f32, working_set_mb())
    }
}

#[cfg(windows)]
fn proc_cpu_100ns() -> u64 {
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessTimes};
    let mut f = [FILETIME::default(); 4];
    unsafe {
        let [a, b, c, d] = &mut f;
        let _ = GetProcessTimes(GetCurrentProcess(), a, b, c, d);
    }
    let v = |t: FILETIME| ((t.dwHighDateTime as u64) << 32) | t.dwLowDateTime as u64;
    v(f[2]) + v(f[3])
}

#[cfg(windows)]
fn working_set_mb() -> f32 {
    use windows::Win32::System::ProcessStatus::{K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let mut c = PROCESS_MEMORY_COUNTERS { cb: std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32, ..Default::default() };
    unsafe {
        if K32GetProcessMemoryInfo(GetCurrentProcess(), &mut c, c.cb).as_bool() {
            return c.WorkingSetSize as f32 / 1_048_576.0;
        }
    }
    0.0
}
