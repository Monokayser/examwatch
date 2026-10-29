//! Student-facing UI (plain Win32, Segoe UI, per-monitor DPI aware):
//!
//! * **V1 window**: opened by the V1 shortcut. Shows the exam name and two
//!   Student ID fields (type it twice to catch typos), or a plain message when
//!   no exam is open / the server is unreachable.
//! * **Notice**: a small, always-on-top, click-through pill above the taskbar,
//!   "● Screen monitored · <Student ID>", visible the whole time capture runs.
//!
//! The UI never blocks: everything slow happens on other threads, which post
//! `WM_APP_REFRESH` when state changes.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{Arc, OnceLock};

use anyhow::Result;
use ew_proto::client::ClientHandle;
use ew_proto::AgentMsg;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Controls::EM_SETLIMITTEXT;
use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForWindow};
use windows::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::state::{self, Shared};

pub const MAIN_CLASS: PCWSTR = w!("ExamWatchAgentWnd");
const V1_CLASS: PCWSTR = w!("ExamWatchV1Wnd");
const NOTICE_CLASS: PCWSTR = w!("ExamWatchNoticeWnd");
pub const WM_APP_V1: u32 = WM_APP + 1;
pub const WM_APP_REFRESH: u32 = WM_APP + 2;

const ID_EDIT1: i32 = 101;
const ID_EDIT2: i32 = 102;
const ID_START: i32 = 103;
const ID_CLOSE: i32 = 104;

const ACCENT: COLORREF = COLORREF(0x00D6_5725); // #2557D6 as BGR
const BAD: COLORREF = COLORREF(0x0020_2DD9); // #D92D20
const TEXT: COLORREF = COLORREF(0x001D_1916);
const MUTED: COLORREF = COLORREF(0x0070_655D);

struct Globals {
    shared: Arc<Shared>,
    net: ClientHandle,
}
static G: OnceLock<Globals> = OnceLock::new();

// HWNDs, stored as isize so they can live in statics.
static MAIN: AtomicIsize = AtomicIsize::new(0);
static V1: AtomicIsize = AtomicIsize::new(0);
static NOTICE: AtomicIsize = AtomicIsize::new(0);
static C_TITLE: AtomicIsize = AtomicIsize::new(0);
static C_BODY: AtomicIsize = AtomicIsize::new(0);
static C_LBL1: AtomicIsize = AtomicIsize::new(0);
static C_LBL2: AtomicIsize = AtomicIsize::new(0);
static C_EDIT1: AtomicIsize = AtomicIsize::new(0);
static C_EDIT2: AtomicIsize = AtomicIsize::new(0);
static C_ERR: AtomicIsize = AtomicIsize::new(0);
static C_START: AtomicIsize = AtomicIsize::new(0);
static C_CLOSE: AtomicIsize = AtomicIsize::new(0);
static F_TITLE: AtomicIsize = AtomicIsize::new(0);
static F_BODY: AtomicIsize = AtomicIsize::new(0);
static LOCAL_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn h(a: &AtomicIsize) -> HWND {
    HWND(a.load(Ordering::Relaxed) as *mut _)
}
fn put(a: &AtomicIsize, hwnd: HWND) {
    a.store(hwnd.0 as isize, Ordering::Relaxed);
}

/// Called from any thread: wake the UI to re-read state.
pub fn post_refresh() {
    let m = h(&MAIN);
    if !m.0.is_null() {
        unsafe {
            let _ = PostMessageW(m, WM_APP_REFRESH, WPARAM(0), LPARAM(0));
        }
    }
}

/// Another instance asked us to show the V1 window (the V1 shortcut was clicked).
pub fn signal_running_instance() -> bool {
    unsafe {
        match FindWindowW(MAIN_CLASS, PCWSTR::null()) {
            Ok(hwnd) if !hwnd.0.is_null() => PostMessageW(hwnd, WM_APP_V1, WPARAM(0), LPARAM(0)).is_ok(),
            _ => false,
        }
    }
}

pub fn run(shared: Arc<Shared>, net: ClientHandle, show_v1: bool) -> Result<()> {
    let _ = G.set(Globals { shared, net });
    unsafe {
        let inst: HINSTANCE = GetModuleHandleW(None)?.into();
        let cursor = LoadCursorW(None, IDC_ARROW)?;
        let white = HBRUSH(GetStockObject(WHITE_BRUSH).0);
        for (class, proc_) in [
            (MAIN_CLASS, Some(main_proc) as WNDPROC),
            (V1_CLASS, Some(v1_proc) as WNDPROC),
            (NOTICE_CLASS, Some(notice_proc) as WNDPROC),
        ] {
            RegisterClassExW(&WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: proc_,
                hInstance: inst,
                hCursor: cursor,
                hbrBackground: white,
                lpszClassName: class,
                ..Default::default()
            });
        }
        // Hidden message window: target for WM_APP_* from other threads/instances.
        let main = CreateWindowExW(WINDOW_EX_STYLE(0), MAIN_CLASS, w!("ExamWatch"), WINDOW_STYLE(0), 0, 0, 0, 0, None, None, inst, None)?;
        put(&MAIN, main);
        create_v1(inst)?;
        create_notice(inst)?;
        refresh();
        if show_v1 {
            show_v1_window();
        }

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let v1 = h(&V1);
            if IsDialogMessageW(v1, &msg).as_bool() {
                continue; // Tab / Enter / Esc handling inside the V1 window
            }
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    Ok(())
}

fn scale(v: i32, dpi: u32) -> i32 {
    (v as i64 * dpi as i64 / 96) as i32
}

unsafe fn make_font(dpi: u32, px: i32, weight: i32) -> HFONT {
    CreateFontW(
        -scale(px, dpi), 0, 0, 0, weight, 0, 0, 0,
        DEFAULT_CHARSET.0 as u32, OUT_DEFAULT_PRECIS.0 as u32, CLIP_DEFAULT_PRECIS.0 as u32,
        CLEARTYPE_QUALITY.0 as u32, (DEFAULT_PITCH.0 | FF_SWISS.0) as u32, w!("Segoe UI"),
    )
}

unsafe fn child(inst: HINSTANCE, parent: HWND, class: PCWSTR, style: u32, id: i32) -> Result<HWND> {
    Ok(CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR::null(),
        WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
        0, 0, 10, 10,
        parent,
        HMENU(id as isize as *mut _),
        inst,
        None,
    )?)
}

unsafe fn create_v1(inst: HINSTANCE) -> Result<()> {
    let wnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_CONTROLPARENT | WS_EX_DLGMODALFRAME,
        V1_CLASS,
        w!("ExamWatch: start your exam"),
        WS_POPUP | WS_CAPTION | WS_SYSMENU,
        CW_USEDEFAULT, CW_USEDEFAULT, 10, 10,
        None, None, inst, None,
    )?;
    put(&V1, wnd);
    put(&C_TITLE, child(inst, wnd, w!("STATIC"), 0, 0)?);
    put(&C_BODY, child(inst, wnd, w!("STATIC"), 0, 0)?);
    put(&C_LBL1, child(inst, wnd, w!("STATIC"), 0, 0)?);
    put(&C_EDIT1, child(inst, wnd, w!("EDIT"), (WS_TABSTOP.0 | WS_BORDER.0) | ES_AUTOHSCROLL as u32 | ES_UPPERCASE as u32, ID_EDIT1)?);
    put(&C_LBL2, child(inst, wnd, w!("STATIC"), 0, 0)?);
    put(&C_EDIT2, child(inst, wnd, w!("EDIT"), (WS_TABSTOP.0 | WS_BORDER.0) | ES_AUTOHSCROLL as u32 | ES_UPPERCASE as u32, ID_EDIT2)?);
    put(&C_ERR, child(inst, wnd, w!("STATIC"), 0, 0)?);
    put(&C_START, child(inst, wnd, w!("BUTTON"), WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32, ID_START)?);
    put(&C_CLOSE, child(inst, wnd, w!("BUTTON"), WS_TABSTOP.0 | BS_PUSHBUTTON as u32, ID_CLOSE)?);
    SetWindowTextW(h(&C_LBL1), w!("Student ID"))?;
    SetWindowTextW(h(&C_LBL2), w!("Type your Student ID again"))?;
    SetWindowTextW(h(&C_START), w!("Start exam"))?;
    SetWindowTextW(h(&C_CLOSE), w!("Close"))?;
    for e in [&C_EDIT1, &C_EDIT2] {
        SendMessageW(h(e), EM_SETLIMITTEXT, WPARAM(32), LPARAM(0));
    }
    layout_v1();
    Ok(())
}

/// Positions controls for the window's current DPI.
unsafe fn layout_v1() {
    let wnd = h(&V1);
    let dpi = GetDpiForWindow(wnd).max(96);
    let old = [F_TITLE.swap(make_font(dpi, 20, 600).0 as isize, Ordering::Relaxed), F_BODY.swap(make_font(dpi, 14, 400).0 as isize, Ordering::Relaxed)];
    for f in old {
        if f != 0 {
            let _ = DeleteObject(HGDIOBJ(f as *mut _));
        }
    }
    let ft = WPARAM(F_TITLE.load(Ordering::Relaxed) as usize);
    let fb = WPARAM(F_BODY.load(Ordering::Relaxed) as usize);
    let s = |v| scale(v, dpi);
    let pad = s(24);
    let width = s(440);
    let inner = width - 2 * pad;
    let mut y = s(20);
    let mut place = |a: &AtomicIsize, x: i32, yy: i32, w: i32, hh: i32, font: WPARAM| {
        let c = h(a);
        let _ = SetWindowPos(c, None, x, yy, w, hh, SWP_NOZORDER | SWP_NOACTIVATE);
        SendMessageW(c, WM_SETFONT, font, LPARAM(1));
    };
    place(&C_TITLE, pad, y, inner, s(30), ft);
    y += s(36);
    place(&C_BODY, pad, y, inner, s(40), fb);
    y += s(48);
    place(&C_LBL1, pad, y, inner, s(20), fb);
    y += s(22);
    place(&C_EDIT1, pad, y, inner, s(32), ft);
    y += s(42);
    place(&C_LBL2, pad, y, inner, s(20), fb);
    y += s(22);
    place(&C_EDIT2, pad, y, inner, s(32), ft);
    y += s(40);
    place(&C_ERR, pad, y, inner, s(40), fb);
    y += s(46);
    let bw = s(120);
    place(&C_CLOSE, width - pad - bw * 2 - s(8), y, bw, s(34), fb);
    place(&C_START, width - pad - bw, y, bw, s(34), fb);
    y += s(34) + pad;
    // Size the client area exactly, then centre on the screen.
    let mut r = RECT { left: 0, top: 0, right: width, bottom: y };
    let _ = AdjustWindowRectExForDpi(&mut r, WS_POPUP | WS_CAPTION | WS_SYSMENU, false, WS_EX_TOPMOST | WS_EX_DLGMODALFRAME, dpi);
    let (ww, wh) = (r.right - r.left, r.bottom - r.top);
    let sw = GetSystemMetrics(SM_CXSCREEN);
    let sh = GetSystemMetrics(SM_CYSCREEN);
    let _ = SetWindowPos(wnd, None, (sw - ww) / 2, (sh - wh) / 3, ww, wh, SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn create_notice(inst: HINSTANCE) -> Result<()> {
    // Click-through, never takes focus, not in the taskbar or Alt+Tab.
    let wnd = CreateWindowExW(
        WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
        NOTICE_CLASS,
        w!("ExamWatch notice"),
        WS_POPUP,
        0, 0, 10, 10,
        None, None, inst, None,
    )?;
    SetLayeredWindowAttributes(wnd, COLORREF(0), 235, LWA_ALPHA)?;
    put(&NOTICE, wnd);
    Ok(())
}

unsafe fn place_notice() {
    let wnd = h(&NOTICE);
    let dpi = GetDpiForWindow(wnd).max(96);
    let mut work = RECT::default();
    let _ = SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut work as *mut _ as *mut _), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0));
    let (w, hh) = (scale(300, dpi), scale(32, dpi));
    let m = scale(12, dpi);
    let _ = SetWindowPos(wnd, HWND_TOPMOST, work.right - w - m, work.bottom - hh - m, w, hh, SWP_NOACTIVATE);
}

fn set_text(a: &AtomicIsize, s: &str) {
    unsafe {
        let _ = SetWindowTextW(h(a), &HSTRING::from(s));
    }
}

fn show(a: &AtomicIsize, on: bool) {
    unsafe {
        let _ = ShowWindow(h(a), if on { SW_SHOW } else { SW_HIDE });
    }
}

fn read(a: &AtomicIsize) -> String {
    let mut buf = [0u16; 64];
    let n = unsafe { GetWindowTextW(h(a), &mut buf) } as usize;
    String::from_utf16_lossy(&buf[..n])
}

/// Re-renders both windows from shared state.
fn refresh() {
    let Some(g) = G.get() else { return };
    let ui = g.shared.ui.lock().unwrap().clone();
    let capturing = g.shared.capturing.load(Ordering::Relaxed);

    let (title, body, form) = match (&ui.exam, &ui.student_id, ui.connected) {
        (_, Some(id), _) if capturing => (
            "Your exam has started".to_string(),
            format!("You are registered as {id}. Your screen is being monitored and recorded until the exam ends."),
            false,
        ),
        (_, _, false) => ("Connecting…".into(), "Connecting to the exam server. If this takes more than a minute, tell your invigilator.".into(), false),
        (None, _, true) => ("No exam is open".into(), "Please wait. Your invigilator will open the exam, then press V1 again.".into(), false),
        (Some(e), _, true) => (e.name.clone(), format!("{}Enter your Student ID twice, then press Start exam.", if ui.lab_name.is_empty() { String::new() } else { format!("{} · ", ui.lab_name) }), true),
    };
    set_text(&C_TITLE, &title);
    set_text(&C_BODY, &body);
    for c in [&C_LBL1, &C_EDIT1, &C_LBL2, &C_EDIT2, &C_START] {
        show(c, form);
    }
    let err = LOCAL_ERROR.lock().unwrap().clone().or(ui.message.clone()).filter(|_| form || ui.message.is_some());
    set_text(&C_ERR, err.as_deref().unwrap_or(""));
    unsafe {
        let _ = EnableWindow(h(&C_START), !ui.claim_pending);
        let _ = EnableWindow(h(&C_EDIT1), !ui.claim_pending);
        let _ = EnableWindow(h(&C_EDIT2), !ui.claim_pending);
    }
    set_text(&C_START, if ui.claim_pending { "Starting…" } else { "Start exam" });

    // Notice: visible exactly while capture runs.
    unsafe {
        let n = h(&NOTICE);
        if capturing {
            place_notice();
            let _ = ShowWindow(n, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(n, None, true);
        } else {
            let _ = ShowWindow(n, SW_HIDE);
        }
    }
}

fn show_v1_window() {
    *LOCAL_ERROR.lock().unwrap() = None;
    refresh();
    unsafe {
        let v1 = h(&V1);
        layout_v1();
        let _ = ShowWindow(v1, SW_SHOW);
        let _ = SetForegroundWindow(v1);
        let _ = SetFocus(h(&C_EDIT1));
    }
}

fn start_clicked() {
    let Some(g) = G.get() else { return };
    match state::validate_entry(&read(&C_EDIT1), &read(&C_EDIT2)) {
        Err(e) => *LOCAL_ERROR.lock().unwrap() = Some(e.to_string()),
        Ok(id) => {
            *LOCAL_ERROR.lock().unwrap() = None;
            let mut ui = g.shared.ui.lock().unwrap();
            ui.claim_pending = true;
            ui.message = None;
            drop(ui);
            g.net.send(AgentMsg::Claim { student_id: id });
        }
    }
    refresh();
}

unsafe extern "system" fn main_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_APP_V1 => {
            show_v1_window();
            LRESULT(0)
        }
        WM_APP_REFRESH => {
            refresh();
            // Successful start: close the V1 window by itself.
            if let Some(g) = G.get() {
                let ui = g.shared.ui.lock().unwrap();
                if ui.student_id.is_some() && g.shared.capturing.load(Ordering::Relaxed) && !ui.claim_pending && IsWindowVisible(h(&V1)).as_bool() {
                    drop(ui);
                    let _ = SetWindowTextW(h(&C_EDIT1), w!(""));
                    let _ = SetWindowTextW(h(&C_EDIT2), w!(""));
                    let _ = ShowWindow(h(&V1), SW_HIDE);
                }
            }
            LRESULT(0)
        }
        WM_DISPLAYCHANGE | WM_SETTINGCHANGE => {
            refresh();
            DefWindowProcW(hwnd, msg, wp, lp)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn v1_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wp.0 & 0xffff) as i32;
            match id {
                ID_START => start_clicked(),
                ID_CLOSE | 2 /* IDCANCEL (Esc) */ => {
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_CLOSE => {
            let _ = ShowWindow(hwnd, SW_HIDE); // hide, never quit: the agent keeps running
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC => {
            let hdc = HDC(wp.0 as *mut _);
            let ctl = HWND(lp.0 as *mut _);
            SetBkMode(hdc, TRANSPARENT);
            let color = if ctl == h(&C_ERR) { BAD } else if ctl == h(&C_BODY) || ctl == h(&C_LBL1) || ctl == h(&C_LBL2) { MUTED } else { TEXT };
            SetTextColor(hdc, color);
            LRESULT(GetStockObject(WHITE_BRUSH).0 as isize)
        }
        WM_DPICHANGED => {
            layout_v1();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

unsafe extern "system" fn notice_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let mut rc = RECT::default();
            let _ = GetClientRect(hwnd, &mut rc);
            let dpi = GetDpiForWindow(hwnd).max(96);
            let bg = CreateSolidBrush(COLORREF(0x0012_100F));
            FillRect(hdc, &rc, bg);
            let _ = DeleteObject(bg);
            // red "recording" dot
            let dot = CreateSolidBrush(COLORREF(0x004D_5AFF));
            let old_b = SelectObject(hdc, dot);
            let old_p = SelectObject(hdc, GetStockObject(NULL_PEN));
            let d = scale(10, dpi);
            let cy = (rc.bottom - rc.top) / 2;
            let _ = Ellipse(hdc, scale(12, dpi), cy - d / 2, scale(12, dpi) + d, cy + d / 2);
            SelectObject(hdc, old_b);
            SelectObject(hdc, old_p);
            let _ = DeleteObject(dot);

            let font = make_font(dpi, 13, 600);
            let old_f = SelectObject(hdc, font);
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00FF_FFFF));
            let id = G.get().and_then(|g| g.shared.ui.lock().unwrap().student_id.clone()).unwrap_or_default();
            let mut text: Vec<u16> = format!("Screen monitored · {id}").encode_utf16().collect();
            let mut tr = RECT { left: scale(30, dpi), ..rc };
            DrawTextW(hdc, &mut text, &mut tr, DT_SINGLELINE | DT_VCENTER | DT_LEFT | DT_END_ELLIPSIS | DT_NOPREFIX);
            SelectObject(hdc, old_f);
            let _ = DeleteObject(font);
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_NCHITTEST => LRESULT(-1), // HTTRANSPARENT: clicks pass through
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

#[allow(dead_code)]
fn accent() -> COLORREF {
    ACCENT
}
