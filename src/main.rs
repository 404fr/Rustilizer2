#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod audio;

use audio::AudioAnalyzer;
use std::ffi::OsStr;
use std::iter::once;
use std::mem::{size_of, zeroed};
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
    MonitorFromWindow, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    SHQueryUserNotificationState, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
    NIM_DELETE, NOTIFYICONDATAW, QUNS_BUSY, QUNS_PRESENTATION_MODE, QUNS_RUNNING_D3D_FULL_SCREEN,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DispatchMessageW,
    FindWindowW, GetClassNameW, GetCursorPos, GetForegroundWindow, GetSystemMetrics, GetWindowRect,
    LoadCursorW, LoadIconW, PeekMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow,
    SetTimer, SetWindowPos, ShowWindow, TrackPopupMenu, TranslateMessage, UpdateLayeredWindow,
    HWND_TOPMOST, IDC_ARROW, IDI_APPLICATION, MF_CHECKED, MF_SEPARATOR, MF_STRING, MF_UNCHECKED,
    MSG, PM_REMOVE, SM_CXSCREEN, SM_CYSCREEN, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SW_HIDE,
    SW_SHOWNOACTIVATE, TPM_BOTTOMALIGN, TPM_LEFTALIGN, ULW_ALPHA, WM_COMMAND, WM_DESTROY,
    WM_RBUTTONUP, WM_TIMER, WM_USER, WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
};

const WM_TRAYICON: u32 = WM_USER + 1;
const ID_TRAY_FULLSCREEN: usize = 1001;
const ID_TRAY_PEAKS: usize = 1002;
const ID_TRAY_MODE: usize = 1003;
const ID_TRAY_SIZE: usize = 1004;
const ID_TRAY_POS: usize = 1005;
const ID_TRAY_EXIT: usize = 1006;
const TIMER_ID: usize = 1;

static DISABLE_ON_FULLSCREEN: AtomicBool = AtomicBool::new(true);
static SHOW_PEAKS: AtomicBool = AtomicBool::new(true);
static COLOR_MODE: AtomicUsize = AtomicUsize::new(0); // 0 = Flowing Rainbow, 1 = Static Rainbow, 2 = Mirror Rainbow
static SIZE_PRESET: AtomicUsize = AtomicUsize::new(0); // 0 = Small (28% x 16px), 1 = Mini (18% x 12px), 2 = Medium (45% x 20px), 3 = Full (100% x 16px)
static POS_PRESET: AtomicUsize = AtomicUsize::new(0); // 0 = Left (default), 1 = Center, 2 = Right

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(once(0)).collect()
}

fn main() {
    // Allow CLI flag `--no-fullscreen-hide`
    for arg in std::env::args() {
        if arg == "--no-fullscreen-hide" {
            DISABLE_ON_FULLSCREEN.store(false, Ordering::Relaxed);
        }
    }

    unsafe {
        let hinstance = GetModuleHandleW(null());
        let class_name = wide("Rustilizer2OverlayClass");

        let wc = WNDCLASSW {
            style: 0,
            lpfnWndProc: Some(wnd_proc),
            cbClsExtra: 0,
            cbWndExtra: 0,
            hInstance: hinstance,
            hIcon: LoadIconW(0 as _, IDI_APPLICATION),
            hCursor: LoadCursorW(0 as _, IDC_ARROW),
            hbrBackground: 0 as _,
            lpszMenuName: null(),
            lpszClassName: class_name.as_ptr(),
        };

        RegisterClassW(&wc);

        let (x, y, width, height) = get_taskbar_rect();

        let ex_style =
            WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE;

        let hwnd = CreateWindowExW(
            ex_style,
            class_name.as_ptr(),
            wide("Rustilizer2").as_ptr(),
            WS_POPUP,
            x,
            y,
            width,
            height,
            0 as _,
            0 as _,
            hinstance,
            null(),
        );

        if hwnd.is_null() {
            eprintln!("Failed to create overlay window");
            return;
        }

        add_tray_icon(hwnd);
        ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(hwnd, TIMER_ID, 16, None);

        let mut analyzer = AudioAnalyzer::new();
        let num_bars = 110usize;
        let mut raw_bars = vec![0.0_f32; num_bars];
        let mut smooth_bars = vec![0.0_f32; num_bars];
        let mut peak_bars = vec![0.0_f32; num_bars];
        let mut peak_vel = vec![0.0_f32; num_bars];

        let mut cur_x = x;
        let mut cur_y = y;
        let mut cur_w = width.max(64);
        let mut cur_h = height.max(8);
        let mut pixels = vec![0u32; (cur_w * cur_h) as usize];

        let start_time = Instant::now();
        let mut frame_count: u64 = 0;
        let mut hidden_for_fullscreen = false;

        let mut msg: MSG = zeroed();
        'running: loop {
            while PeekMessageW(&mut msg, 0 as _, 0, 0, PM_REMOVE) != 0 {
                if msg.message == windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT {
                    break 'running;
                }
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }

            // Periodically check fullscreen state & taskbar geometry (every ~10 frames)
            if frame_count.is_multiple_of(10) {
                let should_hide =
                    DISABLE_ON_FULLSCREEN.load(Ordering::Relaxed) && is_fullscreen_app_active(hwnd);

                if should_hide != hidden_for_fullscreen {
                    hidden_for_fullscreen = should_hide;
                    ShowWindow(
                        hwnd,
                        if hidden_for_fullscreen {
                            SW_HIDE
                        } else {
                            SW_SHOWNOACTIVATE
                        },
                    );
                }

                if !hidden_for_fullscreen {
                    let (tx, ty, tw, th) = get_taskbar_rect();
                    let tw = tw.max(64);
                    let th = th.max(8);
                    if tx != cur_x || ty != cur_y || tw != cur_w || th != cur_h {
                        cur_x = tx;
                        cur_y = ty;
                        cur_w = tw;
                        cur_h = th;
                        pixels.resize((cur_w * cur_h) as usize, 0);
                    }
                    // Keep topmost sitting right on the taskbar edge
                    SetWindowPos(
                        hwnd,
                        HWND_TOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
            }

            if !hidden_for_fullscreen {
                analyzer.compute_bars(&mut raw_bars);

                // Spatial smoothing across neighboring bars for a cohesive wave look
                for i in 0..num_bars {
                    let prev = raw_bars[i.saturating_sub(1)];
                    let curr = raw_bars[i];
                    let next = raw_bars[(i + 1).min(num_bars - 1)];
                    let target = prev * 0.22 + curr * 0.56 + next * 0.22;

                    if target > smooth_bars[i] {
                        // Fast attack
                        smooth_bars[i] = smooth_bars[i] * 0.35 + target * 0.65;
                    } else {
                        // Smooth decay
                        smooth_bars[i] = (smooth_bars[i] * 0.86 + target * 0.14).max(0.0);
                    }

                    // Peak hold dots with gravity
                    if smooth_bars[i] >= peak_bars[i] {
                        peak_bars[i] = smooth_bars[i];
                        peak_vel[i] = 0.0;
                    } else {
                        peak_vel[i] += 0.0012;
                        peak_bars[i] = (peak_bars[i] - peak_vel[i]).max(smooth_bars[i]);
                    }
                }

                let elapsed = start_time.elapsed().as_secs_f32();
                render_equalizer(
                    &mut pixels,
                    cur_w as usize,
                    cur_h as usize,
                    &smooth_bars,
                    &peak_bars,
                    elapsed,
                    SHOW_PEAKS.load(Ordering::Relaxed),
                    COLOR_MODE.load(Ordering::Relaxed),
                );

                present_layered_window(hwnd, cur_x, cur_y, cur_w, cur_h, &pixels);
            }

            frame_count = frame_count.wrapping_add(1);
            std::thread::sleep(std::time::Duration::from_millis(16));
        }

        remove_tray_icon(hwnd);
    }
}

/// Renders the rainbow equalizer bars into a premultiplied ARGB pixel buffer (top-down).
#[allow(clippy::too_many_arguments)]
fn render_equalizer(
    pixels: &mut [u32],
    width: usize,
    height: usize,
    bars: &[f32],
    peaks: &[f32],
    time: f32,
    show_peaks: bool,
    color_mode: usize,
) {
    pixels.fill(0);
    let num_bars = bars.len();
    if num_bars == 0 || width == 0 || height == 0 {
        return;
    }

    let max_bar_h = (height as f32 * 0.92).max(4.0);
    let total_w = width as f32;
    let slot_w = total_w / num_bars as f32;
    let gap = (slot_w * 0.25).clamp(1.0, 3.0);
    let bar_w = (slot_w - gap).max(1.0);

    for (i, (&val, &peak)) in bars.iter().zip(peaks.iter()).enumerate() {
        let x0 = (i as f32 * slot_w) as usize;
        let x1 = ((i as f32 * slot_w + bar_w).ceil() as usize).min(width);
        if x0 >= x1 {
            continue;
        }

        // Base baseline (1px subtle rainbow strip along the very bottom so you can see the bar sitting on the taskbar)
        let frac = i as f32 / num_bars as f32;
        let hue = match color_mode {
            0 => (frac * 360.0 - time * 75.0).rem_euclid(360.0),
            1 => (frac * 320.0).rem_euclid(360.0),
            _ => {
                let mirrored = (frac - 0.5).abs() * 2.0;
                (mirrored * 300.0 - time * 90.0).rem_euclid(360.0)
            }
        };

        let bar_h = (val * max_bar_h).round() as usize;
        let bar_h = bar_h.max(1).min(height); // Crisp 1px rainbow baseline sitting on the taskbar edge

        for py in 0..bar_h {
            let y = height - 1 - py;
            let v_frac = py as f32 / max_bar_h;
            // Shift hue slightly along the height and boost brightness near the top
            let pixel_hue = (hue + v_frac * 28.0).rem_euclid(360.0);
            let sat = (0.95 - v_frac * 0.15).clamp(0.7, 1.0);
            let val_b = if bar_h <= 1 && val < 0.01 {
                0.65 // Subtle idle rainbow line resting on top of the taskbar
            } else {
                (0.85 + v_frac * 0.15).clamp(0.8, 1.0)
            };

            let alpha = if bar_h <= 1 && val < 0.01 { 0.65 } else { 0.95 };

            let color = hsv_to_premultiplied_argb(pixel_hue, sat, val_b, alpha);
            let row_offset = y * width;
            for x in x0..x1 {
                pixels[row_offset + x] = color;
            }
        }

        // Draw crisp 1px peak hold cap
        if show_peaks && peak > 0.04 {
            let peak_py = ((peak * max_bar_h).round() as usize).clamp(1, height - 1);
            let cap_color =
                hsv_to_premultiplied_argb((hue + 35.0).rem_euclid(360.0), 0.50, 1.0, 0.98);
            let y = height.saturating_sub(1 + peak_py);
            if y < height {
                let row_offset = y * width;
                for x in x0..x1 {
                    pixels[row_offset + x] = cap_color;
                }
            }
        }
    }
}

fn hsv_to_premultiplied_argb(h: f32, s: f32, v: f32, a: f32) -> u32 {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0).rem_euclid(2.0) - 1.0).abs());
    let m = v - c;

    let (r1, g1, b1) = if h < 60.0 {
        (c, x, 0.0)
    } else if h < 120.0 {
        (x, c, 0.0)
    } else if h < 180.0 {
        (0.0, c, x)
    } else if h < 240.0 {
        (0.0, x, c)
    } else if h < 300.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };

    let alpha = a.clamp(0.0, 1.0);
    let r = ((r1 + m) * alpha * 255.0).round().clamp(0.0, 255.0) as u32;
    let g = ((g1 + m) * alpha * 255.0).round().clamp(0.0, 255.0) as u32;
    let b = ((b1 + m) * alpha * 255.0).round().clamp(0.0, 255.0) as u32;
    let al = (alpha * 255.0).round().clamp(0.0, 255.0) as u32;

    (al << 24) | (r << 16) | (g << 8) | b
}

unsafe fn present_layered_window(
    hwnd: HWND,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    pixels: &[u32],
) {
    let screen_dc = GetDC(0 as _);
    let mem_dc = CreateCompatibleDC(screen_dc);

    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height, // Top-down DIB
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: 0,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [zeroed()],
    };

    let mut bits: *mut core::ffi::c_void = null_mut();
    let hbitmap = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, 0 as _, 0);

    if !hbitmap.is_null() && !bits.is_null() {
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, (width * height) as usize);

        let old_obj = SelectObject(mem_dc, hbitmap);

        let pt_dst = POINT { x, y };
        let size = SIZE {
            cx: width,
            cy: height,
        };
        let pt_src = POINT { x: 0, y: 0 };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };

        UpdateLayeredWindow(
            hwnd, screen_dc, &pt_dst, &size, mem_dc, &pt_src, 0, &blend, ULW_ALPHA,
        );

        SelectObject(mem_dc, old_obj);
        DeleteObject(hbitmap);
    }

    DeleteDC(mem_dc);
    ReleaseDC(0 as _, screen_dc);
}

/// Positions the equalizer sitting ON TOP OF the primary Windows taskbar (`Shell_TrayWnd`),
/// aligned to the left of the screen by default.
unsafe fn get_taskbar_rect() -> (i32, i32, i32, i32) {
    let (width_ratio, eq_h): (f32, i32) = match SIZE_PRESET.load(Ordering::Relaxed) {
        0 => (0.28, 16), // Default: Small bar on the left (28% taskbar width, 16px tall)
        1 => (0.18, 12), // Mini bar (18% taskbar width, 12px tall)
        2 => (0.45, 20), // Medium bar (45% taskbar width, 20px tall)
        _ => (1.00, 16), // Full taskbar top edge (100% width, 16px tall)
    };
    let pos_mode = POS_PRESET.load(Ordering::Relaxed);

    let tray_class = wide("Shell_TrayWnd");
    let tray_hwnd = FindWindowW(tray_class.as_ptr(), null());
    if !tray_hwnd.is_null() {
        let mut rect: RECT = zeroed();
        if GetWindowRect(tray_hwnd, &mut rect) != 0 {
            let tb_w = rect.right - rect.left;
            let tb_h = rect.bottom - rect.top;
            if tb_w > 0 && tb_h > 0 {
                let w = ((tb_w as f32) * width_ratio).round() as i32;
                let x = match pos_mode {
                    0 => rect.left,                  // Left of the screen
                    1 => rect.left + (tb_w - w) / 2, // Center
                    _ => rect.right - w,             // Right of the screen
                };
                // Sit directly ON TOP of the taskbar's top border (not inside the taskbar)
                let y = if rect.top > eq_h {
                    rect.top - eq_h
                } else {
                    // If taskbar is docked at the top of the screen, sit right below it
                    rect.bottom
                };
                return (x, y, w, eq_h);
            }
        }
    }

    let screen_w = GetSystemMetrics(SM_CXSCREEN);
    let screen_h = GetSystemMetrics(SM_CYSCREEN);
    let taskbar_fallback_h = 48;
    let w = ((screen_w as f32) * width_ratio).round() as i32;
    let x = match pos_mode {
        0 => 0,
        1 => (screen_w - w) / 2,
        _ => screen_w - w,
    };
    let y = screen_h - taskbar_fallback_h - eq_h;
    (x, y, w, eq_h)
}

/// Detects whether a fullscreen game, video, or presentation is currently active.
unsafe fn is_fullscreen_app_active(our_hwnd: HWND) -> bool {
    // 1. Check Shell notification state (Direct3D fullscreen, presentation mode, fullscreen apps)
    let mut state: i32 = 0;
    if SHQueryUserNotificationState(&mut state) == 0
        && (state == QUNS_BUSY
            || state == QUNS_RUNNING_D3D_FULL_SCREEN
            || state == QUNS_PRESENTATION_MODE)
    {
        return true;
    }

    // 2. Check if the foreground window covers its entire monitor (excluding desktop/shell windows)
    let fg = GetForegroundWindow();
    if fg.is_null() || fg == our_hwnd {
        return false;
    }

    let mut class_buf = [0u16; 64];
    let len = GetClassNameW(fg, class_buf.as_mut_ptr(), class_buf.len() as i32);
    if len > 0 {
        let name = String::from_utf16_lossy(&class_buf[..len as usize]);
        if matches!(
            name.as_str(),
            "Progman" | "WorkerW" | "Shell_TrayWnd" | "Shell_SecondaryTrayWnd"
        ) {
            return false;
        }
    }

    let mut win_rect: RECT = zeroed();
    if GetWindowRect(fg, &mut win_rect) == 0 {
        return false;
    }

    let monitor = MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST);
    if monitor.is_null() {
        return false;
    }

    let mut mon_info: MONITORINFO = zeroed();
    mon_info.cbSize = size_of::<MONITORINFO>() as u32;
    if GetMonitorInfoW(monitor, &mut mon_info) == 0 {
        return false;
    }

    let mr = mon_info.rcMonitor;
    win_rect.left <= mr.left
        && win_rect.top <= mr.top
        && win_rect.right >= mr.right
        && win_rect.bottom >= mr.bottom
}

unsafe fn add_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
    nid.uCallbackMessage = WM_TRAYICON;
    nid.hIcon = LoadIconW(0 as _, IDI_APPLICATION);

    let tip = wide("Rustilizer2 - Rainbow Taskbar Equalizer");
    let copy_len = tip.len().min(nid.szTip.len());
    nid.szTip[..copy_len].copy_from_slice(&tip[..copy_len]);

    Shell_NotifyIconW(NIM_ADD, &nid);
}

unsafe fn remove_tray_icon(hwnd: HWND) {
    let mut nid: NOTIFYICONDATAW = zeroed();
    nid.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = 1;
    Shell_NotifyIconW(NIM_DELETE, &nid);
}

unsafe fn show_tray_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu.is_null() {
        return;
    }

    let fs_flags = if DISABLE_ON_FULLSCREEN.load(Ordering::Relaxed) {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING | MF_UNCHECKED
    };
    AppendMenuW(
        menu,
        fs_flags,
        ID_TRAY_FULLSCREEN,
        wide("Disable on Fullscreen").as_ptr(),
    );

    let peak_flags = if SHOW_PEAKS.load(Ordering::Relaxed) {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING | MF_UNCHECKED
    };
    AppendMenuW(
        menu,
        peak_flags,
        ID_TRAY_PEAKS,
        wide("Show Peak Caps").as_ptr(),
    );

    let mode = COLOR_MODE.load(Ordering::Relaxed);
    let mode_label = match mode {
        0 => "Rainbow Mode: Flowing Wave (Click to switch)",
        1 => "Rainbow Mode: Static Spectrum (Click to switch)",
        _ => "Rainbow Mode: Center Mirror (Click to switch)",
    };
    AppendMenuW(menu, MF_STRING, ID_TRAY_MODE, wide(mode_label).as_ptr());

    let size_preset = SIZE_PRESET.load(Ordering::Relaxed);
    let size_label = match size_preset {
        0 => "Bar Size: Small 28% x 16px (Click to switch)",
        1 => "Bar Size: Mini 18% x 12px (Click to switch)",
        2 => "Bar Size: Medium 45% x 20px (Click to switch)",
        _ => "Bar Size: Full-Width 100% x 16px (Click to switch)",
    };
    AppendMenuW(menu, MF_STRING, ID_TRAY_SIZE, wide(size_label).as_ptr());

    let pos_preset = POS_PRESET.load(Ordering::Relaxed);
    let pos_label = match pos_preset {
        0 => "Position: Left of Screen (Click to switch)",
        1 => "Position: Center of Screen (Click to switch)",
        _ => "Position: Right of Screen (Click to switch)",
    };
    AppendMenuW(menu, MF_STRING, ID_TRAY_POS, wide(pos_label).as_ptr());

    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(
        menu,
        MF_STRING,
        ID_TRAY_EXIT,
        wide("Exit Rustilizer2").as_ptr(),
    );

    let mut pt: POINT = zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    TrackPopupMenu(
        menu,
        TPM_LEFTALIGN | TPM_BOTTOMALIGN,
        pt.x,
        pt.y,
        0,
        hwnd,
        null(),
    );
    DestroyMenu(menu);
}

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAYICON => {
            if (lparam as u32) == WM_RBUTTONUP {
                show_tray_menu(hwnd);
            }
            0
        }
        WM_COMMAND => {
            let cmd = wparam & 0xFFFF;
            match cmd {
                ID_TRAY_FULLSCREEN => {
                    let prev = DISABLE_ON_FULLSCREEN.load(Ordering::Relaxed);
                    DISABLE_ON_FULLSCREEN.store(!prev, Ordering::Relaxed);
                }
                ID_TRAY_PEAKS => {
                    let prev = SHOW_PEAKS.load(Ordering::Relaxed);
                    SHOW_PEAKS.store(!prev, Ordering::Relaxed);
                }
                ID_TRAY_MODE => {
                    let next = (COLOR_MODE.load(Ordering::Relaxed) + 1) % 3;
                    COLOR_MODE.store(next, Ordering::Relaxed);
                }
                ID_TRAY_SIZE => {
                    let next = (SIZE_PRESET.load(Ordering::Relaxed) + 1) % 4;
                    SIZE_PRESET.store(next, Ordering::Relaxed);
                }
                ID_TRAY_POS => {
                    let next = (POS_PRESET.load(Ordering::Relaxed) + 1) % 3;
                    POS_PRESET.store(next, Ordering::Relaxed);
                }
                ID_TRAY_EXIT => {
                    PostQuitMessage(0);
                }
                _ => {}
            }
            0
        }
        WM_TIMER => 0,
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}
