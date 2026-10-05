use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::UI::Shell::{
    ExtractIconExW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_WARNING,
    NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::native_interop::Color;

const APP_TRAY_ICON_ID: u32 = 1;
const WM_APP_TRAY: u32 = crate::native_interop::WM_APP_TRAY;

/// Whether the shell already holds our icon: a second NIM_ADD for the same
/// (window, id) pair is rejected, so later refreshes must use NIM_MODIFY.
static REGISTERED: AtomicBool = AtomicBool::new(false);

pub enum TrayAction {
    None,
    ToggleWidget,
    ShowContextMenu,
}

pub struct TrayIconData {
    pub tooltip: String,
}

static CACHED_ICON: Mutex<Option<isize>> = Mutex::new(None);

/// The executable carries no icon resource in this build, so the tray glyph is
/// rendered into a 32bpp DIB at the shell's small-icon size on first use.
pub fn create_icon() -> HICON {
    let mut cached = CACHED_ICON.lock().unwrap_or_else(|error| error.into_inner());
    if let Some(raw) = *cached {
        return HICON(raw as *mut _);
    }
    let icon = build_icon();
    *cached = Some(icon.0 as isize);
    icon
}

fn build_icon() -> HICON {
    unsafe {
        let size = {
            let system_small = GetSystemMetrics(SM_CXSMICON);
            if system_small > 0 {
                system_small.max(16)
            } else {
                32
            }
        };
        let pixels = render_icon_pixels(size);

        let screen_dc = GetDC(None);
        let mem_dc = CreateCompatibleDC(screen_dc);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size,
                biHeight: -size,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let Ok(color_bmp) = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0) else {
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            return load_fallback_icon();
        };
        if bits.is_null() {
            let _ = DeleteObject(color_bmp);
            let _ = DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);
            return load_fallback_icon();
        }
        let count = (size * size) as usize;
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u32, count);

        let mask = CreateBitmap(size, size, 1, 1, None);
        let info = ICONINFO {
            fIcon: true.into(),
            xHotspot: 0,
            yHotspot: 0,
            hbmMask: mask,
            hbmColor: color_bmp,
        };
        let icon = CreateIconIndirect(&info).unwrap_or_else(|_| load_fallback_icon());

        SelectObject(mem_dc, HGDIOBJ::default());
        let _ = DeleteObject(mask);
        let _ = DeleteObject(color_bmp);
        let _ = DeleteDC(mem_dc);
        ReleaseDC(None, screen_dc);
        icon
    }
}

fn load_fallback_icon() -> HICON {
    unsafe {
        let mut exe_buf = [0u16; 260];
        let len = GetModuleFileNameW(None, &mut exe_buf) as usize;
        if len > 0 {
            let mut small = HICON::default();
            let mut large = HICON::default();
            if ExtractIconExW(
                PCWSTR::from_raw(exe_buf.as_ptr()),
                0,
                Some(&mut large),
                Some(&mut small),
                1,
            ) > 0
                && !small.is_invalid()
            {
                return small;
            }
        }
        LoadIconW(None, IDI_APPLICATION).unwrap_or_default()
    }
}

fn render_icon_pixels(size: i32) -> Vec<u32> {
    let side = size as f32;
    let centre = side / 2.0;
    let ring_radius = side * 0.30;
    let ring_width = side * 0.09;
    // The glyph opens towards the right, matching the Q mark.
    let gap_half_degrees = 32.0f32;

    let mut pixels = vec![0u32; (size * size) as usize];
    for y in 0..size {
        for x in 0..size {
            let px = x as f32 + 0.5;
            let py = y as f32 + 0.5;
            let mut layer = Layer::default();

            layer.over(
                Color::from_hex("#0B1220"),
                rounded_rect_coverage(px, py, side, side * 0.22),
            );

            let distance = (px - centre).hypot(py - centre);
            let ring_edge = (distance - ring_radius).abs() - ring_width / 2.0;
            let angle = {
                let degrees = (py - centre).atan2(px - centre).to_degrees();
                if degrees < 0.0 {
                    degrees + 360.0
                } else {
                    degrees
                }
            };
            let from_gap = angle.min(360.0 - angle);
            let gap_edge = from_gap - gap_half_degrees;
            let ring_coverage = (0.5 - ring_edge)
                .min(0.5 - gap_edge)
                .clamp(0.0, 1.0);
            if ring_coverage > 0.0 {
                let shade = if angle >= 360.0 - gap_half_degrees - 30.0 {
                    Color::from_hex("#A3E635")
                } else {
                    Color::from_hex("#38BDF8")
                };
                layer.over(shade, ring_coverage);
            }

            for (row, end) in [(0.375f32, 0.72f32), (0.5, 0.63), (0.625, 0.55)] {
                let half_thickness = side * 0.033;
                let coverage = bar_coverage(
                    px,
                    py,
                    side * 0.375,
                    side * end,
                    side * row - half_thickness,
                    side * row + half_thickness,
                );
                layer.over(Color::from_hex("#E2E8F0"), coverage);
            }

            pixels[(y * size + x) as usize] = layer.argb();
        }
    }
    pixels
}

fn bar_coverage(px: f32, py: f32, left: f32, right: f32, top: f32, bottom: f32) -> f32 {
    let edge = (left - px)
        .max(px - right)
        .max(top - py)
        .max(py - bottom);
    (0.5 - edge).clamp(0.0, 1.0)
}

#[derive(Clone, Copy, Default)]
struct Layer {
    r: f32,
    g: f32,
    b: f32,
    a: f32,
}

impl Layer {
    fn over(&mut self, color: Color, coverage: f32) {
        if coverage <= 0.0 {
            return;
        }
        let alpha = coverage.min(1.0);
        let keep = 1.0 - alpha;
        self.r = color.r as f32 * alpha + self.r * keep;
        self.g = color.g as f32 * alpha + self.g * keep;
        self.b = color.b as f32 * alpha + self.b * keep;
        self.a = alpha + self.a * keep;
    }

    fn argb(self) -> u32 {
        if self.a <= 0.0 {
            return 0;
        }
        let channel = |value: f32| (value / self.a).clamp(0.0, 255.0).round() as u32;
        ((self.a * 255.0).round() as u32) << 24
            | channel(self.r) << 16
            | channel(self.g) << 8
            | channel(self.b)
    }
}

fn rounded_rect_coverage(px: f32, py: f32, side: f32, radius: f32) -> f32 {
    let half = side / 2.0;
    let inset = half - radius;
    let dx = (px - half).abs() - inset;
    let dy = (py - half).abs() - inset;
    let outside = dx.max(0.0).hypot(dy.max(0.0)) + dx.min(dy).min(0.0) - radius;
    (0.5 - outside).clamp(0.0, 1.0)
}

pub fn notify_balloon(hwnd: HWND, title: &str, message: &str) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = APP_TRAY_ICON_ID;
        nid.uFlags = NIF_INFO;
        nid.dwInfoFlags = NIIF_WARNING;
        copy_wide(title, &mut nid.szInfoTitle);
        copy_wide(message, &mut nid.szInfo);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn copy_wide<const N: usize>(s: &str, buf: &mut [u16; N]) {
    let wide: Vec<u16> = s.encode_utf16().collect();
    let mut len = wide.len().min(N - 1);
    if len > 0 && (0xD800..=0xDBFF).contains(&wide[len - 1]) {
        len -= 1;
    }
    buf[..len].copy_from_slice(&wide[..len]);
    buf[len] = 0;
}

fn add(hwnd: HWND, tooltip: &str) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = APP_TRAY_ICON_ID;
        nid.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        nid.uCallbackMessage = WM_APP_TRAY;
        let icon = create_icon();
        nid.hIcon = icon;
        copy_wide(tooltip, &mut nid.szTip);
        if !Shell_NotifyIconW(NIM_ADD, &nid).as_bool() {
            crate::diagnose::log(format!(
                "tray icon registration failed: owner={} last_error={} icon_valid={}",
                hwnd.0 as isize,
                GetLastError().0,
                !icon.is_invalid()
            ));
        }
    }
}

fn update(hwnd: HWND, tooltip: &str) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = APP_TRAY_ICON_ID;
        nid.uFlags = NIF_ICON | NIF_TIP;
        nid.hIcon = create_icon();
        copy_wide(tooltip, &mut nid.szTip);
        let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
    }
}

fn remove(hwnd: HWND) {
    unsafe {
        let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
        nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        nid.hWnd = hwnd;
        nid.uID = APP_TRAY_ICON_ID;
        let _ = Shell_NotifyIconW(NIM_DELETE, &nid);
    }
}

pub fn sync(hwnd: HWND, icon: Option<&TrayIconData>) {
    match icon {
        Some(icon) => {
            // Only the first sync registers the icon; later ones refresh it.
            if REGISTERED.swap(true, Ordering::Relaxed) {
                update(hwnd, &icon.tooltip);
            } else {
                add(hwnd, &icon.tooltip);
            }
        }
        None => {
            remove(hwnd);
            REGISTERED.store(false, Ordering::Relaxed);
        }
    }
}

pub fn remove_all(hwnd: HWND) {
    remove(hwnd);
    REGISTERED.store(false, Ordering::Relaxed);
}

pub fn handle_message(lparam: LPARAM) -> TrayAction {
    match lparam.0 as u32 {
        WM_LBUTTONUP => TrayAction::ToggleWidget,
        WM_RBUTTONUP => TrayAction::ShowContextMenu,
        _ => TrayAction::None,
    }
}
