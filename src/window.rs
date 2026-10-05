use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};
use windows::core::PCWSTR;
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
use windows::Win32::System::Registry::*;
use windows::Win32::System::Threading::{CreateMutexW, WaitForSingleObject};
use windows::Win32::UI::Accessibility::HWINEVENTHOOK;
use windows::Win32::UI::HiDpi::*;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::cli;
use crate::diagnose;
use crate::localization::{self, LanguageId, Strings};
use crate::models::{Quota, QuotaKind, Snapshot};
use crate::native_interop::{
    self, Color, TIMER_COUNTDOWN, TIMER_CREDENTIAL_WATCH, TIMER_POLL, WM_APP_TRAY,
    WM_APP_USAGE_UPDATED,
};
use crate::poller::{self, PollError};
use crate::theme;
use crate::tray_icon;

#[derive(Clone, Copy)]
struct SendHwnd(isize);

unsafe impl Send for SendHwnd {}

impl SendHwnd {
    fn from_hwnd(hwnd: HWND) -> Self {
        Self(hwnd.0 as isize)
    }

    fn to_hwnd(self) -> HWND {
        HWND(self.0 as *mut _)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum AppearanceTheme {
    System,
    Light,
    Dark,
}

impl Default for AppearanceTheme {
    fn default() -> Self {
        Self::System
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum BarStyle {
    Continuous,
    Segmented,
}

impl Default for BarStyle {
    fn default() -> Self {
        Self::Segmented
    }
}

/// Whether the widget speaks in consumed credits or in what is still left.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TextMode {
    Used,
    Remaining,
}

impl Default for TextMode {
    fn default() -> Self {
        Self::Remaining
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Loading,
    Ready,
    Failed(PollError),
}

// Window handles are only ever touched from the UI thread or posted back to it.
unsafe impl Sync for AppState {}
unsafe impl Send for AppState {}

struct AppState {
    hwnd: SendHwnd,
    taskbar_hwnd: Option<HWND>,
    tray_notify_hwnd: Option<HWND>,
    win_event_hook: Option<HWINEVENTHOOK>,
    embedded: bool,
    preview_mode: bool,
    widget_visible: bool,

    is_dark: bool,
    appearance_theme: AppearanceTheme,
    bar_style: BarStyle,
    text_mode: TextMode,
    transparent_background: bool,
    language: LanguageId,
    language_override: Option<LanguageId>,

    snapshot: Option<Snapshot>,
    status: Status,

    show_user: bool,
    show_addon: bool,
    show_org: bool,
    show_packages: bool,
    show_cycle: bool,

    alert_threshold_percent: u8,
    notified_alerts: BTreeSet<String>,

    poll_interval_ms: u32,
    retry_count: u32,
    credential_watch: Option<String>,

    taskbar_index: usize,
    /// `None` keeps the widget docked next to the notification area, so it never
    /// ends up underneath the Start button; a drag pins it to an explicit offset.
    anchor_offset: Option<i32>,
    anchor_taskbar_right: bool,
    dragging: bool,
    drag_start_mouse_x: i32,
    drag_start_offset: i32,
}

impl AppState {
    fn strings(&self) -> Strings {
        self.language.strings()
    }

    fn ready(&self) -> bool {
        matches!(self.status, Status::Ready) && self.snapshot.is_some()
    }
}

static STATE: Mutex<Option<AppState>> = Mutex::new(None);

fn lock_state() -> MutexGuard<'static, Option<AppState>> {
    STATE.lock().unwrap_or_else(|error| error.into_inner())
}

const WIDGET_HEIGHT: i32 = 46;
const LEFT_DIVIDER_W: i32 = 3;
const DIVIDER_RIGHT_MARGIN: i32 = 6;
const LABEL_RIGHT_MARGIN: i32 = 7;
const SEGMENT_W: i32 = 10;
const SEGMENT_H: i32 = 13;
const SEGMENT_GAP: i32 = 1;
const SEGMENT_COUNT: i32 = 10;
const BAR_RIGHT_MARGIN: i32 = 5;
const TIME_PROGRESS_H: i32 = 2;
const TIME_PROGRESS_GAP: i32 = 2;
const ROW_GAP: i32 = 7;
const ROW_COLUMN_GAP: i32 = 9;
const RIGHT_MARGIN: i32 = 6;
const TRAY_ANCHOR_GAP: i32 = 8;
const MAX_BARS: usize = 3;
/// Nominal credits period used only to scale the countdown progress line.
const CYCLE_NOMINAL_SECS: f64 = 30.0 * 86_400.0;

const RETRY_BASE_MS: u32 = 30_000;
const POLL_1_MIN: u32 = 60_000;
const POLL_5_MIN: u32 = 300_000;
const POLL_15_MIN: u32 = 900_000;
const POLL_1_HOUR: u32 = 3_600_000;

const IDM_REFRESH: usize = 1;
const IDM_EXIT: usize = 2;
const IDM_FREQ_1MIN: usize = 10;
const IDM_FREQ_5MIN: usize = 11;
const IDM_FREQ_15MIN: usize = 12;
const IDM_FREQ_1HOUR: usize = 13;
const IDM_START_WITH_WINDOWS: usize = 20;
const IDM_RESET_POSITION: usize = 30;
const IDM_LANG_SYSTEM: usize = 40;
const IDM_LANG_ENGLISH: usize = 41;
const IDM_LANG_CHINESE: usize = 51;
const IDM_SHOW_USER: usize = 60;
const IDM_SHOW_ADDON: usize = 61;
const IDM_SHOW_ORG: usize = 62;
const IDM_SHOW_PACKAGES: usize = 63;
const IDM_SHOW_CYCLE: usize = 64;
const IDM_ALERT_OFF: usize = 80;
const IDM_ALERT_10: usize = 81;
const IDM_ALERT_25: usize = 82;
const IDM_ALERT_50: usize = 83;
const IDM_THEME_SYSTEM: usize = 90;
const IDM_THEME_LIGHT: usize = 91;
const IDM_THEME_DARK: usize = 92;
const IDM_BAR_CONTINUOUS: usize = 93;
const IDM_BAR_SEGMENTED: usize = 94;
const IDM_MODE_USED: usize = 95;
const IDM_MODE_REMAINING: usize = 96;
const IDM_TRANSPARENT_BACKGROUND: usize = 97;
const IDM_ANCHOR_TASKBAR_RIGHT: usize = 98;
const IDM_TOGGLE_WIDGET: usize = 70;
const IDM_OPEN_QODER: usize = 100;

const WM_DPICHANGED_MSG: u32 = 0x02E0;
const FIRST_POLL_DELAY_MS: u32 = 1_500;
const ENV_RELAUNCH: &str = "QODER_USAGE_RELAUNCH";

static CURRENT_DPI: AtomicU32 = AtomicU32::new(96);
static SUPPRESS_REPOSITION_UNTIL: Mutex<Option<Instant>> = Mutex::new(None);

fn sc(px: i32) -> i32 {
    let dpi = CURRENT_DPI.load(Ordering::Relaxed);
    (px as f64 * dpi as f64 / 96.0).round() as i32
}

fn refresh_dpi() {
    let hwnd = lock_state().as_ref().map(|state| state.hwnd.to_hwnd());
    let Some(hwnd) = hwnd else { return };
    let dpi = unsafe { GetDpiForWindow(hwnd) };
    if dpi > 0 {
        CURRENT_DPI.store(dpi, Ordering::Relaxed);
    }
}

fn appearance_is_dark(appearance: AppearanceTheme) -> bool {
    match appearance {
        AppearanceTheme::System => theme::is_dark_mode(),
        AppearanceTheme::Light => false,
        AppearanceTheme::Dark => true,
    }
}

// ---------------------------------------------------------------- settings --

const SETTINGS_DIR: &str = "QoderUsage";

fn settings_path() -> PathBuf {
    let appdata = std::env::var("APPDATA").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(appdata).join(SETTINGS_DIR).join("settings.json")
}

#[derive(Debug, Serialize, Deserialize)]
struct SettingsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    anchor_offset: Option<i32>,
    #[serde(default)]
    anchor_taskbar_right: bool,
    #[serde(default)]
    taskbar_index: usize,
    #[serde(default = "default_poll_interval")]
    poll_interval_ms: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    language: Option<String>,
    #[serde(default = "default_true")]
    widget_visible: bool,
    #[serde(default = "default_true")]
    show_user: bool,
    #[serde(default = "default_true")]
    show_addon: bool,
    #[serde(default)]
    show_org: bool,
    #[serde(default)]
    show_packages: bool,
    #[serde(default = "default_true")]
    show_cycle: bool,
    #[serde(default)]
    alert_threshold_percent: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    notified_alerts: Vec<String>,
    #[serde(default)]
    appearance_theme: AppearanceTheme,
    #[serde(default)]
    bar_style: BarStyle,
    #[serde(default)]
    text_mode: TextMode,
    #[serde(default = "default_true")]
    transparent_background: bool,
}

fn default_true() -> bool {
    true
}

fn default_poll_interval() -> u32 {
    POLL_5_MIN
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            anchor_offset: None,
            anchor_taskbar_right: false,
            taskbar_index: 0,
            poll_interval_ms: default_poll_interval(),
            language: None,
            widget_visible: true,
            show_user: true,
            show_addon: true,
            show_org: false,
            show_packages: false,
            show_cycle: true,
            alert_threshold_percent: 0,
            notified_alerts: Vec::new(),
            appearance_theme: AppearanceTheme::default(),
            bar_style: BarStyle::default(),
            text_mode: TextMode::default(),
            transparent_background: true,
        }
    }
}

fn normalize_settings(mut settings: SettingsFile) -> SettingsFile {
    if !settings.show_user
        && !settings.show_addon
        && !settings.show_org
        && !settings.show_packages
        && !settings.show_cycle
    {
        settings.show_user = true;
    }
    if !matches!(settings.alert_threshold_percent, 0 | 10 | 25 | 50) {
        settings.alert_threshold_percent = 0;
    }
    settings.notified_alerts.sort();
    settings.notified_alerts.dedup();
    settings
}

fn load_settings() -> SettingsFile {
    let path = settings_path();
    let loaded = std::fs::read_to_string(path)
        .ok()
        .and_then(|content| serde_json::from_str::<SettingsFile>(&content).ok())
        .unwrap_or_default();
    normalize_settings(loaded)
}

fn save_settings(settings: &SettingsFile) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(json) = serde_json::to_string_pretty(settings) {
        let _ = std::fs::write(path, json);
    }
}

fn save_state_settings() {
    let state = lock_state();
    let Some(state) = state.as_ref() else {
        return;
    };
    save_settings(&SettingsFile {
        anchor_offset: state.anchor_offset,
        anchor_taskbar_right: state.anchor_taskbar_right,
        taskbar_index: state.taskbar_index,
        poll_interval_ms: state.poll_interval_ms,
        language: state.language_override.map(|language| language.code().to_string()),
        widget_visible: state.widget_visible,
        show_user: state.show_user,
        show_addon: state.show_addon,
        show_org: state.show_org,
        show_packages: state.show_packages,
        show_cycle: state.show_cycle,
        alert_threshold_percent: state.alert_threshold_percent,
        notified_alerts: state.notified_alerts.iter().cloned().collect(),
        appearance_theme: state.appearance_theme,
        bar_style: state.bar_style,
        text_mode: state.text_mode,
        transparent_background: state.transparent_background,
    });
}

const STARTUP_REGISTRY_PATH: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const STARTUP_REGISTRY_KEY: &str = "QoderUsage";

fn is_startup_enabled() -> bool {
    let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
    let key = native_interop::wide_str(STARTUP_REGISTRY_KEY);
    unsafe {
        let mut hkey = HKEY::default();
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            KEY_READ,
            &mut hkey,
        )
        .is_err()
        {
            return false;
        }
        let mut size = 0u32;
        let result = RegQueryValueExW(
            hkey,
            PCWSTR::from_raw(key.as_ptr()),
            None,
            None,
            None,
            Some(&mut size),
        );
        let _ = RegCloseKey(hkey);
        result.is_ok()
    }
}

fn set_startup_enabled(enable: bool) {
    let path = native_interop::wide_str(STARTUP_REGISTRY_PATH);
    let key = native_interop::wide_str(STARTUP_REGISTRY_KEY);
    unsafe {
        let mut hkey = HKEY::default();
        if RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(path.as_ptr()),
            0,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE | KEY_QUERY_VALUE,
            None,
            &mut hkey,
            None,
        )
        .is_err()
        {
            return;
        }
        if enable {
            let Some(value) = current_exe_path_string() else {
                let _ = RegCloseKey(hkey);
                return;
            };
            let encoded = native_interop::wide_str(&value);
            let _ = RegSetValueExW(
                hkey,
                PCWSTR::from_raw(key.as_ptr()),
                0,
                REG_SZ,
                Some(bytemes(encoded.as_slice())),
            );
        } else {
            let _ = RegDeleteValueW(hkey, PCWSTR::from_raw(key.as_ptr()));
        }
        let _ = RegCloseKey(hkey);
    }
}

fn bytemes(value: &[u16]) -> &[u8] {
    unsafe {
        std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * std::mem::size_of::<u16>())
    }
}

fn current_exe_path_string() -> Option<String> {
    let mut buffer = [0u16; 32_768];
    let length = unsafe { GetModuleFileNameW(None, &mut buffer) } as usize;
    if length == 0 {
        return None;
    }
    Some(String::from_utf16_lossy(&buffer[..length]))
}

// ------------------------------------------------------------ widget data --

struct QuotaBar {
    label: String,
    accent: Color,
    percent: f64,
    text: String,
    time_remaining: Option<f64>,
    detail: String,
}

struct CycleBar {
    percent: f64,
    text: String,
    detail: String,
}

fn user_accent() -> Color {
    Color::from_hex("#38BDF8")
}

fn addon_accent() -> Color {
    Color::from_hex("#A3E635")
}

fn org_accent() -> Color {
    Color::from_hex("#818CF8")
}

fn package_accent() -> Color {
    Color::from_hex("#FBBF24")
}

fn widget_colors(is_dark: bool) -> (Color, Color, Color) {
    if is_dark {
        (
            Color::from_hex("#20242B"),
            Color::from_hex("#3C424B"),
            Color::from_hex("#E6EAF0"),
        )
    } else {
        (
            Color::from_hex("#F5F6F8"),
            Color::from_hex("#D8DDE4"),
            Color::from_hex("#1F2328"),
        )
    }
}

/// Recolour a bar towards amber/red when the remaining credits run low.
fn health_color(accent: Color, remaining_percent: f64) -> Color {
    if remaining_percent <= 10.0 {
        Color::from_hex("#EF4444")
    } else if remaining_percent <= 25.0 {
        Color::from_hex("#F59E0B")
    } else {
        accent
    }
}

fn row_visible(state: &AppState, kind: QuotaKind) -> bool {
    match kind {
        QuotaKind::User => state.show_user,
        QuotaKind::AddOn => state.show_addon,
        QuotaKind::Org => state.show_org,
        QuotaKind::Package => state.show_packages,
    }
}

fn row_label(state: &AppState, kind: QuotaKind, name: &str) -> String {
    let strings = state.strings();
    let base = match kind {
        QuotaKind::User => strings.user_quota,
        QuotaKind::AddOn => strings.add_on,
        QuotaKind::Org => strings.org_package,
        QuotaKind::Package => strings.package,
    };
    if kind == QuotaKind::Package && !name.is_empty() {
        name.to_string()
    } else {
        base.to_string()
    }
}

fn fmt_credits(value: f64) -> String {
    if value == value.trunc() && value.abs() < 1e9 {
        format!("{}", value as i64)
    } else {
        format!("{:.1}", value)
    }
}

fn quota_text(state: &AppState, quota: &Quota) -> String {
    let strings = state.strings();
    if state.text_mode == TextMode::Used {
        format!("{}/{}", fmt_credits(quota.used), fmt_credits(quota.total))
    } else {
        match state.language {
            LanguageId::SimplifiedChinese => {
                format!("{} {}", strings.remaining, fmt_credits(quota.remaining))
            }
            LanguageId::English => {
                format!("{} {}", fmt_credits(quota.remaining), strings.remaining)
            }
        }
    }
}

fn quota_detail(state: &AppState, quota: &Quota) -> String {
    let percent = match state.text_mode {
        TextMode::Used => quota.used_percentage,
        TextMode::Remaining => quota.remaining_percentage(),
    };
    format!(
        "{}/{} {} · {:.0}%",
        fmt_credits(quota.used),
        fmt_credits(quota.total),
        quota.unit,
        percent
    )
}

fn time_remaining_ratio(expires_at: Option<SystemTime>, nominal_secs: f64) -> Option<f64> {
    let expires = expires_at?;
    let remaining = expires
        .duration_since(SystemTime::now())
        .map(|value| value.as_secs_f64())
        .unwrap_or(0.0);
    Some((remaining / nominal_secs.max(1.0)).clamp(0.0, 1.0))
}

fn countdown_text(expires_at: SystemTime, strings: Strings) -> String {
    let remaining = match expires_at.duration_since(SystemTime::now()) {
        Ok(value) => value.as_secs(),
        Err(_) => return strings.expired.to_string(),
    };
    let days = remaining / 86_400;
    let hours = (remaining % 86_400) / 3_600;
    let minutes = (remaining % 3_600) / 60;
    if days > 0 {
        format!("{days}{}{hours}{}", strings.days_short, strings.hours_short)
    } else if hours > 0 {
        format!("{hours}{}{minutes}{}", strings.hours_short, strings.minutes_short)
    } else {
        format!("{minutes}{}", strings.minutes_short)
    }
}

fn status_text(state: &AppState) -> String {
    let strings = state.strings();
    match state.status {
        Status::Loading => strings.loading.to_string(),
        Status::Ready => String::new(),
        Status::Failed(error) => match error {
            PollError::NetworkUnavailable => strings.network_error.to_string(),
            PollError::NoCredentials | PollError::AuthRequired => {
                strings.error_marker.to_string()
            }
            PollError::RateLimited
            | PollError::ServerError
            | PollError::RequestFailed => strings.error_marker.to_string(),
        },
    }
}

/// Bars for the credits row, in display order and limited to MAX_BARS.
fn quota_bars(state: &AppState) -> Vec<QuotaBar> {
    let Some(snapshot) = state.snapshot.as_ref() else {
        return Vec::new();
    };
    let strings = state.strings();
    let mut bars = Vec::new();
    for row in &snapshot.usage.rows {
        if !row_visible(state, row.kind) {
            continue;
        }
        if bars.len() >= MAX_BARS {
            break;
        }
        let accent = match row.kind {
            QuotaKind::User => user_accent(),
            QuotaKind::AddOn => addon_accent(),
            QuotaKind::Org => org_accent(),
            QuotaKind::Package => package_accent(),
        };
        let remaining = row.quota.remaining_percentage();
        let percent = match state.text_mode {
            TextMode::Used => row.quota.used_percentage,
            TextMode::Remaining => remaining,
        };
        let text = if state.ready() {
            quota_text(state, &row.quota)
        } else {
            status_text(state)
        };
        let label = row_label(state, row.kind, &row.label);
        let detail = if row.quota.available {
            quota_detail(state, &row.quota)
        } else {
            format!("{} · {}", quota_detail(state, &row.quota), strings.unavailable)
        };
        bars.push(QuotaBar {
            label,
            accent: health_color(accent, remaining),
            percent: percent.clamp(0.0, 100.0),
            text,
            time_remaining: if state.text_mode == TextMode::Used {
                time_remaining_ratio(row.quota.expires_at, CYCLE_NOMINAL_SECS)
            } else {
                None
            },
            detail,
        });
    }
    bars
}

fn cycle_bar(state: &AppState) -> Option<CycleBar> {
    if !state.show_cycle {
        return None;
    }
    let strings = state.strings();
    let snapshot = state.snapshot.as_ref()?;
    let Some(expires_at) = snapshot.usage.cycle_expires_at else {
        return None;
    };
    let remaining = time_remaining_ratio(Some(expires_at), CYCLE_NOMINAL_SECS).unwrap_or(0.0);
    let percent = match state.text_mode {
        TextMode::Used => (1.0 - remaining) * 100.0,
        TextMode::Remaining => remaining * 100.0,
    };
    let text = if state.ready() {
        countdown_text(expires_at, strings)
    } else {
        status_text(state)
    };
    Some(CycleBar {
        percent: percent.clamp(0.0, 100.0),
        text,
        detail: format!(
            "{} ({})",
            native_interop::format_local(expires_at),
            countdown_text(expires_at, strings)
        ),
    })
}

// ----------------------------------------------------------------- layout --

/// True while at least one credits bucket may be shown.
fn wants_quota_row(state: &AppState) -> bool {
    state.show_user || state.show_addon || state.show_org || state.show_packages
}

/// Keeps the credits row meaningful when the account reports no numeric quota
/// (enterprise mode) or when the last poll failed.
fn placeholder_bar(state: &AppState) -> QuotaBar {
    let strings = state.strings();
    let usage = state.snapshot.as_ref().map(|snapshot| &snapshot.usage);
    let enterprise = usage.map(|usage| usage.enterprise).unwrap_or(false);
    let exceeded = usage.map(|usage| usage.quota_exceeded).unwrap_or(false);
    let total = usage.and_then(|usage| usage.total_usage_percentage).unwrap_or(0.0);
    let text = match state.status {
        Status::Ready if exceeded => strings.exhausted.to_string(),
        Status::Ready if enterprise => strings.enterprise_hint.to_string(),
        Status::Ready => String::new(),
        _ => status_text(state),
    };
    QuotaBar {
        label: String::new(),
        accent: health_color(user_accent(), (100.0 - total).clamp(0.0, 100.0)),
        percent: total.clamp(0.0, 100.0),
        text,
        time_remaining: None,
        detail: String::new(),
    }
}

struct BarLayout {
    x: i32,
    segments: i32,
    percent: f64,
    accent: Color,
    text: String,
    text_width: i32,
    time_remaining: Option<f64>,
}

struct WidgetRowLayout {
    y: i32,
    label: String,
    label_width: i32,
    bars: Vec<BarLayout>,
}

struct Layout {
    width: i32,
    height: i32,
    rows: Vec<WidgetRowLayout>,
    background: Color,
    text_color: Color,
    track: Color,
    time_progress: Color,
    style: BarStyle,
}

fn segment_count_for(bars: usize) -> i32 {
    match bars.max(1) {
        1 => SEGMENT_COUNT,
        2 => 6,
        _ => 4,
    }
}

fn bar_pixels(segments: i32) -> i32 {
    let segment = sc(SEGMENT_W);
    let gap = sc(SEGMENT_GAP);
    segments * (segment + gap) - gap
}

/// Measure and position everything up front so the window width always matches
/// what is painted, including variable-length credit figures.
fn compute_layout(state: &AppState) -> Layout {
    let strings = state.strings();
    let (background, track, text_color) = widget_colors(state.is_dark);
    let time_progress = Color::from_hex(if state.is_dark { "#8A9099" } else { "#6B7280" });

    let quota_row = wants_quota_row(state);
    let cycle = cycle_bar(state);
    let mut bars = quota_bars(state);
    if quota_row && bars.is_empty() {
        bars.push(placeholder_bar(state));
    }
    let (height, row_one_y, row_two_y) = row_positions(quota_row && cycle.is_some());

    unsafe {
        let screen = GetDC(HWND::default());
        let dc = CreateCompatibleDC(screen);
        let font = widget_font();
        let old = SelectObject(dc, font);

        let measure = |text: &str| -> i32 {
            let wide = native_interop::wide_str(text);
            let mut size = SIZE::default();
            if GetTextExtentPoint32W(dc, &wide[..wide.len() - 1], &mut size).as_bool() {
                size.cx
            } else {
                text.len() as i32 * sc(7)
            }
        };

        let quota_label = strings.quota_row_label.to_string();
        let cycle_label = strings.cycle_row_label.to_string();
        let mut label_width = if quota_row {
            measure(&quota_label)
        } else {
            0
        };
        if cycle.is_some() {
            label_width = label_width.max(measure(&cycle_label));
        }

        let left_column = sc(LEFT_DIVIDER_W) + sc(DIVIDER_RIGHT_MARGIN);
        let bars_x = left_column + label_width + sc(LABEL_RIGHT_MARGIN);
        let mut rows: Vec<WidgetRowLayout> = Vec::new();
        let mut content_right = left_column;

        if quota_row {
            let segments = segment_count_for(bars.len());
            let mut x = bars_x;
            let mut laid_out = Vec::new();
            for bar in &bars {
                let text_width = measure(&bar.text);
                laid_out.push(BarLayout {
                    x,
                    segments,
                    percent: bar.percent,
                    accent: bar.accent,
                    text: bar.text.clone(),
                    text_width,
                    time_remaining: bar.time_remaining,
                });
                x += bar_pixels(segments) + sc(BAR_RIGHT_MARGIN) + text_width + sc(ROW_COLUMN_GAP);
            }
            content_right = content_right.max(x - sc(ROW_COLUMN_GAP));
            rows.push(WidgetRowLayout {
                y: row_one_y,
                label: quota_label,
                label_width,
                bars: laid_out,
            });
        }

        if let Some(cycle) = cycle {
            let segments = SEGMENT_COUNT;
            let text_width = measure(&cycle.text);
            content_right =
                content_right.max(bars_x + bar_pixels(segments) + sc(BAR_RIGHT_MARGIN) + text_width);
            rows.push(WidgetRowLayout {
                y: row_two_y,
                label: cycle_label,
                label_width,
                bars: vec![BarLayout {
                    x: bars_x,
                    segments,
                    percent: cycle.percent,
                    accent: time_progress,
                    text: cycle.text,
                    text_width,
                    time_remaining: None,
                }],
            });
        }

        SelectObject(dc, old);
        let _ = DeleteObject(font);
        let _ = DeleteDC(dc);
        ReleaseDC(HWND::default(), screen);

        Layout {
            width: (content_right + sc(RIGHT_MARGIN)).max(sc(96)),
            height,
            rows,
            background,
            text_color,
            track,
            time_progress,
            style: state.bar_style,
        }
    }
}

/// (height, first row top, second row top); a single row is vertically centred.
fn row_positions(two_rows: bool) -> (i32, i32, i32) {
    let height = sc(WIDGET_HEIGHT);
    let row_height = sc(SEGMENT_H) + sc(TIME_PROGRESS_GAP) + sc(TIME_PROGRESS_H);
    if !two_rows {
        return (height, (height - row_height) / 2, 0);
    }
    let gap = sc(ROW_GAP);
    let top = (height - (row_height * 2 + gap)) / 2;
    (height, top, top + row_height + gap)
}

fn widget_font() -> HFONT {
    let name = native_interop::wide_str("Segoe UI");
    unsafe {
        CreateFontW(
            sc(-12),
            0,
            0,
            0,
            FW_MEDIUM.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET.0 as u32,
            OUT_TT_PRECIS.0 as u32,
            CLIP_DEFAULT_PRECIS.0 as u32,
            CLEARTYPE_QUALITY.0 as u32,
            (DEFAULT_PITCH.0 | FF_DONTCARE.0) as u32,
            PCWSTR::from_raw(name.as_ptr()),
        )
    }
}

// ------------------------------------------------------------------ paint --

fn render_layered() {
    refresh_dpi();
    let (hwnd, embedded, preview_mode, layout, transparent) = {
        let state = lock_state();
        let Some(state) = state.as_ref() else {
            return;
        };
        (
            state.hwnd.to_hwnd(),
            state.embedded,
            state.preview_mode,
            compute_layout(state),
            state.transparent_background,
        )
    };

    if !embedded && !preview_mode {
        unsafe {
            let _ = InvalidateRect(hwnd, None, false);
        }
        return;
    }

    unsafe {
        let screen_dc = GetDC(hwnd);
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: layout.width,
                biHeight: -layout.height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let mem_dc = CreateCompatibleDC(screen_dc);
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let dib = CreateDIBSection(mem_dc, &bmi, DIB_RGB_COLORS, &mut bits, None, 0)
            .unwrap_or_default();
        if dib.is_invalid() || bits.is_null() {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(hwnd, screen_dc);
            return;
        }

        let old_bmp = SelectObject(mem_dc, dib);
        paint_content(mem_dc, &layout);

        let pixels =
            std::slice::from_raw_parts_mut(bits as *mut u32, (layout.width * layout.height) as usize);
        apply_background_alpha(pixels, &layout.background, transparent);

        let source = POINT { x: 0, y: 0 };
        let size = SIZE {
            cx: layout.width,
            cy: layout.height,
        };
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        let _ = UpdateLayeredWindow(
            hwnd,
            screen_dc,
            None,
            Some(&size),
            mem_dc,
            Some(&source),
            COLORREF(0),
            Some(&blend),
            ULW_ALPHA,
        );

        let _ = SelectObject(mem_dc, old_bmp);
        let _ = DeleteObject(dib);
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(hwnd, screen_dc);
    }
}

fn apply_background_alpha(pixels: &mut [u32], background: &Color, transparent: bool) {
    if !transparent {
        for pixel in pixels {
            *pixel |= 0xFF00_0000;
        }
        return;
    }
    let background_rgb =
        ((background.r as u32) << 16) | ((background.g as u32) << 8) | background.b as u32;
    for pixel in pixels {
        let rgb = *pixel & 0x00FF_FFFF;
        if rgb == background_rgb {
            // Alpha 1 keeps the rectangle hit-testable for drag and right-click
            // while making the background nearly invisible against the taskbar.
            *pixel = 0x0100_0000;
        } else {
            *pixel |= 0xFF00_0000;
        }
    }
}

fn paint_content(hdc: HDC, layout: &Layout) {
    unsafe {
        let rect = RECT {
            left: 0,
            top: 0,
            right: layout.width,
            bottom: layout.height,
        };
        let background_brush = CreateSolidBrush(COLORREF(layout.background.to_colorref()));
        FillRect(hdc, &rect, background_brush);
        let _ = DeleteObject(background_brush);

        draw_left_divider(hdc, layout);

        let font = widget_font();
        let old = SelectObject(hdc, font);
        let _ = SetBkMode(hdc, TRANSPARENT);

        for row in &layout.rows {
            let mut label: Vec<u16> = row.label.encode_utf16().collect();
            let mut label_rect = RECT {
                left: sc(LEFT_DIVIDER_W) + sc(DIVIDER_RIGHT_MARGIN),
                top: row.y,
                right: sc(LEFT_DIVIDER_W) + sc(DIVIDER_RIGHT_MARGIN) + row.label_width + sc(2),
                bottom: row.y + sc(SEGMENT_H),
            };
            let _ = SetTextColor(hdc, COLORREF(layout.text_color.to_colorref()));
            let _ = DrawTextW(
                hdc,
                &mut label,
                &mut label_rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );

            for bar in &row.bars {
                draw_bar(hdc, bar, row.y, layout);
            }
        }

        let _ = SelectObject(hdc, old);
        let _ = DeleteObject(font);
    }
}

fn draw_left_divider(hdc: HDC, layout: &Layout) {
    let divider_height = sc(25);
    let top = (layout.height - divider_height) / 2;
    let (near, far) = if layout.background.r < 128 {
        (Color::new(80, 80, 80), Color::new(40, 40, 40))
    } else {
        (Color::new(160, 160, 160), Color::new(230, 230, 230))
    };
    unsafe {
        let near_brush = CreateSolidBrush(COLORREF(near.to_colorref()));
        FillRect(
            hdc,
            &RECT {
                left: 0,
                top,
                right: sc(2),
                bottom: top + divider_height,
            },
            near_brush,
        );
        let _ = DeleteObject(near_brush);
        let far_brush = CreateSolidBrush(COLORREF(far.to_colorref()));
        FillRect(
            hdc,
            &RECT {
                left: sc(2),
                top,
                right: sc(LEFT_DIVIDER_W),
                bottom: top + divider_height,
            },
            far_brush,
        );
        let _ = DeleteObject(far_brush);
    }
}

fn draw_bar(hdc: HDC, bar: &BarLayout, y: i32, layout: &Layout) {
    let segment_height = sc(SEGMENT_H);
    let width = bar_pixels(bar.segments);
    let corner = segment_height / 2;
    let percent = bar.percent.clamp(0.0, 100.0);
    let track = COLORREF(layout.track.to_colorref());
    let accent = COLORREF(bar.accent.to_colorref());

    unsafe {
        let bar_rect = RECT {
            left: bar.x,
            top: y,
            right: bar.x + width,
            bottom: y + segment_height,
        };

        if bar.percent > 0.0 && bar.accent.to_colorref() != track.0 {
            match layout.style {
                BarStyle::Continuous => {
                    paint_round_rect(hdc, &bar_rect, track, corner);
                    let filled = (width as f64 * percent / 100.0).round() as i32;
                    if filled > 0 {
                        let clip = CreateRoundRectRgn(
                            bar_rect.left,
                            bar_rect.top,
                            bar_rect.right + 1,
                            bar_rect.bottom + 1,
                            corner * 2,
                            corner * 2,
                        );
                        let _ = SelectClipRgn(hdc, clip);
                        paint_rect(hdc, &rect_at(bar.x, y, filled, segment_height), accent);
                        let _ = SelectClipRgn(hdc, HRGN::default());
                        let _ = DeleteObject(clip);
                    }
                }
                BarStyle::Segmented => {
                    let covered = percent * bar.segments as f64 / 100.0;
                    let segment = sc(SEGMENT_W);
                    let gap = sc(SEGMENT_GAP);
                    for index in 0..bar.segments {
                        let left = bar.x + index * (segment + gap);
                        let rect = RECT {
                            left,
                            top: y,
                            right: left + segment,
                            bottom: y + segment_height,
                        };
                        paint_rect(hdc, &rect, track);
                        let height = (segment_height as f64 * (covered - index as f64).clamp(0.0, 1.0))
                            .round() as i32;
                        if height > 0 {
                            paint_rect(
                                hdc,
                                &RECT {
                                    top: rect.bottom - height,
                                    ..rect
                                },
                                accent,
                            );
                        }
                    }
                }
            }
        } else {
            paint_rect(hdc, &bar_rect, track);
        }

        if let Some(progress) = bar.time_remaining {
            let progress_height = sc(TIME_PROGRESS_H);
            let top = y + segment_height + sc(TIME_PROGRESS_GAP);
            let filled = (width as f64 * progress.clamp(0.0, 1.0)).round() as i32;
            let base = COLORREF(layout.track.to_colorref());
            let value = COLORREF(layout.time_progress.to_colorref());
            paint_rect(hdc, &rect_at(bar.x, top, width, progress_height), base);
            if filled > 0 {
                paint_rect(hdc, &rect_at(bar.x, top, filled, progress_height), value);
            }
        }

        if !bar.text.is_empty() {
            let left = bar.x + width + sc(BAR_RIGHT_MARGIN);
            let mut text: Vec<u16> = bar.text.encode_utf16().collect();
            let mut rect = RECT {
                left,
                top: y,
                right: left + bar.text_width + sc(2),
                bottom: y + segment_height,
            };
            let _ = SetTextColor(hdc, COLORREF(layout.text_color.to_colorref()));
            let _ = DrawTextW(
                hdc,
                &mut text,
                &mut rect,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX,
            );
        }
    }
}

fn rect_at(x: i32, y: i32, width: i32, height: i32) -> RECT {
    RECT {
        left: x,
        top: y,
        right: x + width,
        bottom: y + height,
    }
}

unsafe fn paint_rect(hdc: HDC, rect: &RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    FillRect(hdc, rect, brush);
    let _ = DeleteObject(brush);
}

unsafe fn paint_round_rect(hdc: HDC, rect: &RECT, color: COLORREF, radius: i32) {
    let region = CreateRoundRectRgn(
        rect.left,
        rect.top,
        rect.right + 1,
        rect.bottom + 1,
        radius * 2,
        radius * 2,
    );
    let brush = CreateSolidBrush(color);
    let _ = FillRgn(hdc, region, brush);
    let _ = DeleteObject(brush);
    let _ = DeleteObject(region);
}

/// Fallback path when the window could not be reparented into the taskbar.
fn paint_hwnd(hdc: HDC, layout: &Layout) {
    unsafe {
        let background_brush = CreateSolidBrush(COLORREF(layout.background.to_colorref()));
        FillRect(
            hdc,
            &RECT {
                left: 0,
                top: 0,
                right: layout.width,
                bottom: layout.height,
            },
            background_brush,
        );
        let _ = DeleteObject(background_brush);
        paint_content(hdc, layout);
    }
}

// ------------------------------------------------------------- window sizing --

fn widget_size() -> (i32, i32) {
    let state = lock_state();
    let Some(state) = state.as_ref() else {
        return (sc(160), sc(WIDGET_HEIGHT));
    };
    let layout = compute_layout(state);
    (layout.width, layout.height)
}

// ------------------------------------------------------------------- polling --

static POLL_IN_FLIGHT: AtomicBool = AtomicBool::new(false);

fn begin_poll(hwnd: HWND) {
    if POLL_IN_FLIGHT.swap(true, Ordering::SeqCst) {
        return;
    }
    let send_hwnd = SendHwnd::from_hwnd(hwnd);
    if std::thread::Builder::new()
        .spawn(move || do_poll(send_hwnd))
        .is_err()
    {
        POLL_IN_FLIGHT.store(false, Ordering::SeqCst);
    }
}

fn do_poll(send_hwnd: SendHwnd) {
    let hwnd = send_hwnd.to_hwnd();
    let outcome = poller::poll();
    let mut alerts: Vec<(String, String)> = Vec::new();
    let mut notify_credentials = false;
    {
        let mut guard = lock_state();
        let Some(state) = guard.as_mut() else {
            POLL_IN_FLIGHT.store(false, Ordering::SeqCst);
            return;
        };
        match outcome {
            Ok(snapshot) => {
                alerts = collect_low_quota_alerts(state, &snapshot);
                diagnose::log(format!(
                    "usage refreshed from {} at {} (region {})",
                    snapshot.source,
                    native_interop::format_local(snapshot.fetched_at),
                    snapshot.region
                ));
                state.snapshot = Some(snapshot);
                state.status = Status::Ready;
                state.retry_count = 0;
                state.credential_watch = None;
            }
            Err(error) => {
                diagnose::log_error("poll failed", error.category());
                notify_credentials = error.is_credential_failure() && !was_credential_failure(state);
                state.status = Status::Failed(error);
                state.retry_count = state.retry_count.saturating_add(1);
                if error.is_credential_failure() {
                    state.credential_watch = poller::credential_signature();
                }
            }
        }
    }
    POLL_IN_FLIGHT.store(false, Ordering::SeqCst);
    if !alerts.is_empty() {
        save_state_settings();
    }

    unsafe {
        let _ = SetTimer(hwnd, TIMER_POLL, next_poll_interval(), None);
    }

    if notify_credentials {
        let (title, body) = {
            let state = lock_state();
            state
                .as_ref()
                .map(|state| {
                    let strings = state.strings();
                    (
                        strings.credential_title.to_string(),
                        strings.credential_body.to_string(),
                    )
                })
                .unwrap_or_else(|| ("Qoder".to_string(), String::new()))
        };
        tray_icon::notify_balloon(hwnd, &title, &body);
    }
    for (title, body) in alerts {
        tray_icon::notify_balloon(hwnd, &title, &body);
    }
    unsafe {
        let _ = PostMessageW(hwnd, WM_APP_USAGE_UPDATED, WPARAM(0), LPARAM(0));
    }
}

fn was_credential_failure(state: &AppState) -> bool {
    matches!(state.status, Status::Failed(error) if error.is_credential_failure())
}

fn next_poll_interval() -> u32 {
    let state = lock_state();
    let Some(state) = state.as_ref() else {
        return POLL_5_MIN;
    };
    let base = state.poll_interval_ms;
    match state.status {
        Status::Failed(error) if !error.is_credential_failure() => {
            let backoff = RETRY_BASE_MS.saturating_mul(1u32 << state.retry_count.min(4));
            backoff.max(5_000).min(base)
        }
        _ => base,
    }
}

/// One-shot balloons for buckets that fell to or below the chosen threshold.
/// A bucket is re-armed once it climbs back above the threshold.
fn collect_low_quota_alerts(state: &mut AppState, snapshot: &Snapshot) -> Vec<(String, String)> {
    let mut alerts = Vec::new();
    if state.alert_threshold_percent == 0 {
        return alerts;
    }
    let strings = state.strings();
    let threshold = state.alert_threshold_percent as f64;
    for row in &snapshot.usage.rows {
        if !row_visible(state, row.kind) {
            continue;
        }
        let key = format!("{}:{}", row.key, state.alert_threshold_percent);
        let remaining = row.quota.remaining_percentage();
        if remaining > threshold {
            state.notified_alerts.remove(&key);
            continue;
        }
        if state.notified_alerts.contains(&key) {
            continue;
        }
        let label = row_label(state, row.kind, &row.label);
        state.notified_alerts.insert(key);
        alerts.push((
            strings.quota_alerts.to_string(),
            strings
                .quota_alert_body
                .replace("{label}", &label)
                .replace("{remaining}", &format!("{:.0}", remaining)),
        ));
    }
    alerts
}

// ------------------------------------------------------------- taskbar anchor --

fn attach_to_taskbar(hwnd: HWND, requested_index: usize) -> bool {
    let taskbars = native_interop::find_taskbars();
    let Some(taskbar) = taskbars
        .get(requested_index.min(taskbars.len().saturating_sub(1)))
        .copied()
    else {
        diagnose::log("no taskbar found; using fallback popup window");
        return false;
    };

    let old_hook = {
        let mut guard = lock_state();
        guard.as_mut().and_then(|state| state.win_event_hook.take())
    };
    if let Some(hook) = old_hook {
        native_interop::unhook_win_event(hook);
    }

    native_interop::embed_in_taskbar(hwnd, taskbar.hwnd);
    let tray_notify = native_interop::find_child_window(taskbar.hwnd, "TrayNotifyWnd");
    let hook = tray_notify.and_then(|tray_hwnd| {
        let thread_id = native_interop::get_window_thread_id(tray_hwnd);
        native_interop::set_tray_event_hook(thread_id, on_tray_location_changed)
    });

    let mut guard = lock_state();
    let index = requested_index.min(taskbars.len().saturating_sub(1));
    let (hwnd_for_log, rect) = (taskbar.hwnd, taskbar.rect);
    if let Some(state) = guard.as_mut() {
        state.taskbar_hwnd = Some(taskbar.hwnd);
        state.tray_notify_hwnd = tray_notify;
        state.win_event_hook = hook;
        state.taskbar_index = index;
        state.embedded = true;
    }
    drop(guard);
    diagnose::log(format!(
        "embedding requested index={requested_index} used index={index} count={} hwnd={hwnd_for_log:?} rect=({}, {}, {}, {})",
        taskbars.len(),
        rect.left,
        rect.top,
        rect.right,
        rect.bottom
    ));
    true
}

fn taskbar_at_point(pt: POINT) -> Option<(usize, native_interop::TaskbarWindow)> {
    native_interop::find_taskbars()
        .into_iter()
        .enumerate()
        .find(|(_, taskbar)| {
            pt.x >= taskbar.rect.left
                && pt.x < taskbar.rect.right
                && pt.y >= taskbar.rect.top
                && pt.y < taskbar.rect.bottom
        })
}

/// Reposition the widget from the stored anchor offset.
fn apply_anchor() {
    refresh_dpi();
    let (hwnd, embedded, preview_mode, taskbar_hwnd, offset, right_anchor) = {
        let guard = lock_state();
        let Some(state) = guard.as_ref() else {
            return;
        };
        if state.dragging {
            return;
        }
        (
            state.hwnd.to_hwnd(),
            state.embedded,
            state.preview_mode,
            state.taskbar_hwnd,
            state.anchor_offset,
            state.anchor_taskbar_right,
        )
    };

    if preview_mode {
        let (width, height) = widget_size();
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                HWND_TOPMOST,
                160,
                160,
                width,
                height,
                SWP_NOACTIVATE,
            );
        }
        return;
    }

    let Some(taskbar_hwnd) = taskbar_hwnd else {
        return;
    };
    let Some(rect) = native_interop::get_taskbar_rect(taskbar_hwnd) else {
        return;
    };

    let (width, height) = widget_size();
    let taskbar_width = rect.right - rect.left;
    let left = match offset {
        None => auto_anchor_left(taskbar_hwnd, &rect, width),
        Some(offset) => {
            let clamped = offset.clamp(0, (taskbar_width - width).max(0));
            if clamped != offset {
                {
                    let mut guard = lock_state();
                    if let Some(state) = guard.as_mut() {
                        state.anchor_offset = Some(clamped);
                    }
                }
                save_state_settings();
            }
            if right_anchor {
                taskbar_width - width - clamped
            } else {
                clamped
            }
        }
    };
    let y = rect.bottom - height;
    if embedded {
        native_interop::move_window(hwnd, left, y - rect.top, width, height);
    } else {
        native_interop::move_window(hwnd, rect.left + left, y, width, height);
    }
}

/// The notification area is the one stretch of a Windows 11 taskbar that stays
/// free of shell buttons, so an unpinned widget docks just to its left.
fn auto_anchor_left(taskbar: HWND, rect: &RECT, width: i32) -> i32 {
    let tray_left = native_interop::find_child_window(taskbar, "TrayNotifyWnd")
        .and_then(|tray| native_interop::get_window_rect_safe(tray))
        .map(|tray_rect| tray_rect.left - rect.left)
        .unwrap_or(rect.right - rect.left);
    (tray_left - width - sc(TRAY_ANCHOR_GAP)).clamp(0, ((rect.right - rect.left) - width).max(0))
}

/// Replaces the auto dock position with an explicit offset, optionally for the
/// other anchor edge so the widget keeps its on-screen position.
fn pin_anchor(for_right_mode: Option<bool>) {
    let (width, right_now, left, taskbar_width) = {
        let guard = lock_state();
        let Some(state) = guard.as_ref() else {
            return;
        };
        let Some(taskbar) = state.taskbar_hwnd else {
            return;
        };
        let Some(rect) = native_interop::get_taskbar_rect(taskbar) else {
            return;
        };
        let width = compute_layout(state).width;
        let taskbar_width = rect.right - rect.left;
        let max = (taskbar_width - width).max(0);
        let left = match state.anchor_offset {
            None => auto_anchor_left(taskbar, &rect, width),
            Some(offset) if state.anchor_taskbar_right => (taskbar_width - width - offset).clamp(0, max),
            Some(offset) => offset.clamp(0, max),
        };
        (width, state.anchor_taskbar_right, left, taskbar_width)
    };
    let target_right = for_right_mode.unwrap_or(right_now);
    let offset = if target_right {
        (taskbar_width - width - left).max(0)
    } else {
        left
    };
    if let Some(state) = lock_state().as_mut() {
        state.anchor_offset = Some(offset);
    }
}

fn position_at_taskbar() {
    apply_anchor();
}

unsafe extern "system" fn on_tray_location_changed(
    _hook: HWINEVENTHOOK,
    _event: u32,
    hwnd: HWND,
    _id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    let is_tray = {
        let guard = lock_state();
        guard
            .as_ref()
            .and_then(|state| state.tray_notify_hwnd)
            .map(|tray| tray == hwnd)
            .unwrap_or(false)
    };
    if !is_tray || tray_reposition_suppressed() {
        return;
    }
    suppress_reposition(Duration::from_millis(400));
    apply_anchor();
}

fn tray_reposition_suppressed() -> bool {
    let until = SUPPRESS_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    until
        .as_ref()
        .map(|instant| Instant::now() < *instant)
        .unwrap_or(false)
}

fn suppress_reposition(how_long: Duration) {
    let mut until = SUPPRESS_REPOSITION_UNTIL
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    *until = Some(Instant::now() + how_long);
}

// ------------------------------------------------------------------ refresh --

fn update_display() {
    apply_anchor();
    render_layered();
    sync_tray_icon();
    apply_widget_visibility();
}

fn apply_widget_visibility() {
    let (hwnd, visible) = {
        let guard = lock_state();
        let Some(state) = guard.as_ref() else {
            return;
        };
        (state.hwnd.to_hwnd(), state.widget_visible || state.preview_mode)
    };
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

fn sync_tray_icon() {
    let (hwnd, tooltip, preview_mode) = {
        let guard = lock_state();
        let Some(state) = guard.as_ref() else {
            return;
        };
        (
            state.hwnd.to_hwnd(),
            tray_tooltip(state),
            state.preview_mode,
        )
    };
    if preview_mode {
        return;
    }
    tray_icon::sync(hwnd, Some(&tray_icon::TrayIconData { tooltip }));
}

fn tray_tooltip(state: &AppState) -> String {
    let strings = state.strings();
    let mut lines = vec![match state.snapshot.as_ref() {
        Some(snapshot) if !snapshot.usage.user_type.is_empty() => {
            format!("{} · {}", strings.window_title, snapshot.usage.user_type)
        }
        Some(snapshot) => format!("{} · {}", strings.window_title, snapshot.region),
        None => strings.window_title.to_string(),
    }];
    for bar in quota_bars(state) {
        lines.push(format!("{} {}", bar.label, bar.detail));
    }
    if let Some(snapshot) = state.snapshot.as_ref() {
        if snapshot.usage.rows.is_empty() {
            if let Some(percent) = snapshot.usage.total_usage_percentage {
                lines.push(format!("{} {:.0}%", strings.total_usage, percent));
            }
        }
    }
    if let Some(cycle) = cycle_bar(state) {
        lines.push(format!("{} {}", strings.cycle_row_label, cycle.detail));
    }
    let mut tooltip = String::new();
    for line in lines {
        let candidate = if tooltip.is_empty() {
            line
        } else {
            format!("{tooltip}\n{line}")
        };
        if candidate.chars().count() > 240 {
            break;
        }
        tooltip = candidate;
    }
    tooltip
}

fn check_theme_change() {
    let (appearance, current) = {
        let guard = lock_state();
        let Some(state) = guard.as_ref() else {
            return;
        };
        (state.appearance_theme, state.is_dark)
    };
    let next = appearance_is_dark(appearance);
    if next != current {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            state.is_dark = next;
        }
        render_layered();
        sync_tray_icon();
    }
}

fn schedule_countdown_timer(hwnd: HWND) {
    unsafe {
        let _ = SetTimer(hwnd, TIMER_COUNTDOWN, COUNTDOWN_TICK_MS, None);
    }
}

const COUNTDOWN_TICK_MS: u32 = 60_000;
const CREDENTIAL_WATCH_MS: u32 = 2_000;

/// Fast tick: resume polling as soon as the desktop app refreshes its token, and
/// relaunch if explorer restarted (our taskbar child window dies with it).
fn on_watch_tick(hwnd: HWND) {
    let taskbar_alive = {
        let guard = lock_state();
        guard
            .as_ref()
            .and_then(|state| state.taskbar_hwnd)
            .and_then(native_interop::get_window_rect_safe)
            .is_some()
    };
    let embedded = lock_state()
        .as_ref()
        .map(|state| state.embedded)
        .unwrap_or(false);
    if embedded && !taskbar_alive {
        diagnose::log("taskbar window vanished (explorer restart); relaunching");
        relaunch_self();
        return;
    }

    let needs_credentials = {
        let guard = lock_state();
        guard
            .as_ref()
            .map(|state| was_credential_failure(state))
            .unwrap_or(false)
    };
    if !needs_credentials {
        return;
    }
    let changed = {
        let guard = lock_state();
        match (
            guard.as_ref().and_then(|state| state.credential_watch.clone()),
            poller::credential_signature(),
        ) {
            (Some(previous), Some(current)) => previous != current,
            (None, Some(_)) => true,
            _ => false,
        }
    };
    if changed {
        diagnose::log("credential file changed; resuming polling");
        begin_poll(hwnd);
    }
}

fn relaunch_self() {
    let Some(path) = current_exe_path_string() else {
        return;
    };
    let mut command = std::process::Command::new(&path);
    command.env(ENV_RELAUNCH, "1");
    for argument in std::env::args().skip(1) {
        command.arg(argument);
    }
    let _ = command.spawn();
    unsafe {
        PostQuitMessage(0);
    }
}

// -------------------------------------------------------------- user actions --

fn begin_drag(hwnd: HWND) {
    let preview_mode = {
        let guard = lock_state();
        guard.as_ref().map(|state| state.preview_mode).unwrap_or(true)
    };
    if preview_mode {
        return;
    }

    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
        let _ = SetCapture(hwnd);
    }
    pin_anchor(None);
    let mut guard = lock_state();
    if let Some(state) = guard.as_mut() {
        state.dragging = true;
        state.drag_start_mouse_x = pt.x;
        state.drag_start_offset = state.anchor_offset.unwrap_or_default();
    }
}

fn update_drag() {
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    let (start_x, start_offset, right_anchor, taskbar_hwnd) = {
        let guard = lock_state();
        let Some(state) = guard.as_ref() else {
            return;
        };
        if !state.dragging {
            return;
        }
        (
            state.drag_start_mouse_x,
            state.drag_start_offset,
            state.anchor_taskbar_right,
            state.taskbar_hwnd,
        )
    };
    let Some(taskbar_hwnd) = taskbar_hwnd else {
        return;
    };
    let Some(rect) = native_interop::get_taskbar_rect(taskbar_hwnd) else {
        return;
    };
    let width = {
        let guard = lock_state();
        guard
            .as_ref()
            .map(|state| compute_layout(state).width)
            .unwrap_or_else(|| sc(160))
    };
    let delta = pt.x - start_x;
    let offset = if right_anchor {
        start_offset - delta
    } else {
        start_offset + delta
    };
    let max = (rect.right - rect.left - width).max(0);
    {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            state.anchor_offset = Some(offset.clamp(0, max));
        }
    }
    apply_anchor_during_drag();
}

/// Same as `apply_anchor`, but allowed to run while a drag is in progress.
fn apply_anchor_during_drag() {
    let mut guard = lock_state();
    let Some(state) = guard.as_mut() else {
        return;
    };
    let Some(taskbar) = state.taskbar_hwnd else {
        return;
    };
    let Some(rect) = native_interop::get_taskbar_rect(taskbar) else {
        return;
    };
    let layout = compute_layout(state);
    let max = (rect.right - rect.left - layout.width).max(0);
    let offset = state.anchor_offset.unwrap_or_default().clamp(0, max);
    state.anchor_offset = Some(offset);
    let left = if state.anchor_taskbar_right {
        rect.right - rect.left - layout.width - offset
    } else {
        offset
    };
    let hwnd = state.hwnd.to_hwnd();
    native_interop::move_window(
        hwnd,
        left,
        rect.bottom - rect.top - layout.height,
        layout.width,
        layout.height,
    );
}

fn end_drag(hwnd: HWND) {
    let dragging = {
        let mut guard = lock_state();
        match guard.as_mut() {
            Some(state) if state.dragging => {
                state.dragging = false;
                true
            }
            _ => false,
        }
    };
    if !dragging {
        return;
    }
    unsafe {
        let _ = ReleaseCapture();
    }
    let mut pt = POINT::default();
    unsafe {
        let _ = GetCursorPos(&mut pt);
    }
    if let Some((index, _)) = taskbar_at_point(pt) {
        let current = {
            let guard = lock_state();
            guard
                .as_ref()
                .map(|state| state.taskbar_index)
                .unwrap_or(0)
        };
        if index != current {
            attach_to_taskbar(hwnd, index);
        }
    }
    save_state_settings();
    update_display();
}

fn toggle_widget_visibility() {
    let mut guard = lock_state();
    let Some(state) = guard.as_mut() else {
        return;
    };
    state.widget_visible = !state.widget_visible;
    drop(guard);
    save_state_settings();
    apply_widget_visibility();
    sync_tray_icon();
}

fn open_detail_url() {
    let url = {
        let guard = lock_state();
        guard
            .as_ref()
            .and_then(|state| state.snapshot.as_ref())
            .map(|snapshot| snapshot.usage.detail_url.clone())
            .unwrap_or_default()
    };
    if url.is_empty() {
        return;
    }
    unsafe {
        let operation = native_interop::wide_str("open");
        let target = native_interop::wide_str(&url);
        let _ = ShellExecuteW(
            HWND::default(),
            PCWSTR::from_raw(operation.as_ptr()),
            PCWSTR::from_raw(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
    }
}

fn set_interval(hwnd: HWND, interval: u32) {
    {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            state.poll_interval_ms = interval;
            state.retry_count = 0;
        }
    }
    save_state_settings();
    unsafe {
        let _ = SetTimer(hwnd, TIMER_POLL, interval, None);
    }
}

fn refresh_usage_now(hwnd: HWND) {
    begin_poll(hwnd);
}

// ------------------------------------------------------------ context menu --

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let menu = CreatePopupMenu().unwrap_or_default();
        let (
            strings,
            interval,
            language_override,
            widget_visible,
            show_user,
            show_addon,
            show_org,
            show_packages,
            show_cycle,
            alert_threshold_percent,
            appearance_theme,
            bar_style,
            text_mode,
            transparent_background,
            anchor_taskbar_right,
            detail_url,
        ) = {
            let guard = lock_state();
            let Some(state) = guard.as_ref() else {
                return;
            };
            (
                state.strings(),
                state.poll_interval_ms,
                state.language_override,
                state.widget_visible,
                state.show_user,
                state.show_addon,
                state.show_org,
                state.show_packages,
                state.show_cycle,
                state.alert_threshold_percent,
                state.appearance_theme,
                state.bar_style,
                state.text_mode,
                state.transparent_background,
                state.anchor_taskbar_right,
                state
                    .snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.usage.detail_url.clone())
                    .unwrap_or_default(),
            )
        };

        append_item(menu, strings.refresh, IDM_REFRESH, MENU_ITEM_FLAGS(0));

        let frequency = CreatePopupMenu().unwrap_or_default();
        append_item(frequency, strings.one_minute, IDM_FREQ_1MIN, checked(interval == POLL_1_MIN));
        append_item(frequency, strings.five_minutes, IDM_FREQ_5MIN, checked(interval == POLL_5_MIN));
        append_item(
            frequency,
            strings.fifteen_minutes,
            IDM_FREQ_15MIN,
            checked(interval == POLL_15_MIN),
        );
        append_item(frequency, strings.one_hour, IDM_FREQ_1HOUR, checked(interval == POLL_1_HOUR));
        append_popup(menu, strings.update_frequency, frequency);

        let content = CreatePopupMenu().unwrap_or_default();
        append_item(content, strings.user_quota, IDM_SHOW_USER, checked(show_user));
        append_item(content, strings.add_on, IDM_SHOW_ADDON, checked(show_addon));
        append_item(content, strings.org_package, IDM_SHOW_ORG, checked(show_org));
        append_item(
            content,
            strings.package,
            IDM_SHOW_PACKAGES,
            checked(show_packages),
        );
        append_item(content, strings.cycle_row_label, IDM_SHOW_CYCLE, checked(show_cycle));
        append_popup(menu, strings.content, content);

        let appearance = CreatePopupMenu().unwrap_or_default();
        append_item(
            appearance,
            strings.theme_system,
            IDM_THEME_SYSTEM,
            checked(appearance_theme == AppearanceTheme::System),
        );
        append_item(
            appearance,
            strings.theme_light,
            IDM_THEME_LIGHT,
            checked(appearance_theme == AppearanceTheme::Light),
        );
        append_item(
            appearance,
            strings.theme_dark,
            IDM_THEME_DARK,
            checked(appearance_theme == AppearanceTheme::Dark),
        );
        append_item(
            appearance,
            strings.bar_continuous,
            IDM_BAR_CONTINUOUS,
            checked(bar_style == BarStyle::Continuous),
        );
        append_item(
            appearance,
            strings.bar_segmented,
            IDM_BAR_SEGMENTED,
            checked(bar_style == BarStyle::Segmented),
        );
        append_item(
            appearance,
            strings.mode_used,
            IDM_MODE_USED,
            checked(text_mode == TextMode::Used),
        );
        append_item(
            appearance,
            strings.mode_remaining,
            IDM_MODE_REMAINING,
            checked(text_mode == TextMode::Remaining),
        );
        append_item(
            appearance,
            strings.transparent_background,
            IDM_TRANSPARENT_BACKGROUND,
            checked(transparent_background),
        );
        append_popup(menu, strings.appearance, appearance);

        let alerts = CreatePopupMenu().unwrap_or_default();
        append_item(alerts, strings.alert_off, IDM_ALERT_OFF, checked(alert_threshold_percent == 0));
        for (id, threshold) in [
            (IDM_ALERT_10, 10u8),
            (IDM_ALERT_25, 25),
            (IDM_ALERT_50, 50),
        ] {
            let label = format!("{threshold}%");
            append_item(alerts, &label, id, checked(alert_threshold_percent == threshold));
        }
        append_popup(menu, strings.quota_alerts, alerts);

        let language = CreatePopupMenu().unwrap_or_default();
        append_item(
            language,
            strings.system_default,
            IDM_LANG_SYSTEM,
            checked(language_override.is_none()),
        );
        for (id, value) in [
            (IDM_LANG_ENGLISH, LanguageId::English),
            (IDM_LANG_CHINESE, LanguageId::SimplifiedChinese),
        ] {
            append_item(
                language,
                value.native_name(),
                id,
                checked(language_override == Some(value)),
            );
        }
        append_popup(menu, strings.language, language);

        let details_flags = if detail_url.is_empty() {
            MF_GRAYED
        } else {
            MENU_ITEM_FLAGS(0)
        };
        append_item(menu, strings.quota_details, IDM_OPEN_QODER, details_flags);

        let settings = CreatePopupMenu().unwrap_or_default();
        append_item(
            settings,
            strings.start_with_windows,
            IDM_START_WITH_WINDOWS,
            checked(is_startup_enabled()),
        );
        append_item(
            settings,
            strings.anchor_right,
            IDM_ANCHOR_TASKBAR_RIGHT,
            checked(anchor_taskbar_right),
        );
        append_item(settings, strings.reset_position, IDM_RESET_POSITION, MENU_ITEM_FLAGS(0));
        append_popup(menu, strings.settings, settings);

        append_item(
            menu,
            strings.show_widget,
            IDM_TOGGLE_WIDGET,
            checked(widget_visible),
        );
        append_item(menu, strings.exit, IDM_EXIT, MENU_ITEM_FLAGS(0));

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let _ = TrackPopupMenu(menu, TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, None);
        let _ = DestroyMenu(menu);
    }
}

unsafe fn checked(active: bool) -> MENU_ITEM_FLAGS {
    if active {
        MF_CHECKED
    } else {
        MENU_ITEM_FLAGS(0)
    }
}

unsafe fn append_item(menu: HMENU, label: &str, id: usize, flags: MENU_ITEM_FLAGS) {
    let wide = native_interop::wide_str(label);
    let _ = AppendMenuW(menu, flags, id, PCWSTR::from_raw(wide.as_ptr()));
}

unsafe fn append_popup(menu: HMENU, label: &str, submenu: HMENU) {
    let wide = native_interop::wide_str(label);
    let _ = AppendMenuW(menu, MF_POPUP, submenu.0 as usize, PCWSTR::from_raw(wide.as_ptr()));
}

fn handle_menu_command(hwnd: HWND, id: usize) {
    macro_rules! toggle {
        ($field:ident) => {{
            let mut guard = lock_state();
            if let Some(state) = guard.as_mut() {
                state.$field = !state.$field;
            }
            drop(guard);
            save_state_settings();
            update_display();
        }};
    }

    match id {
        IDM_REFRESH => refresh_usage_now(hwnd),
        IDM_FREQ_1MIN => set_interval(hwnd, POLL_1_MIN),
        IDM_FREQ_5MIN => set_interval(hwnd, POLL_5_MIN),
        IDM_FREQ_15MIN => set_interval(hwnd, POLL_15_MIN),
        IDM_FREQ_1HOUR => set_interval(hwnd, POLL_1_HOUR),
        IDM_SHOW_USER => toggle!(show_user),
        IDM_SHOW_ADDON => toggle!(show_addon),
        IDM_SHOW_ORG => toggle!(show_org),
        IDM_SHOW_PACKAGES => toggle!(show_packages),
        IDM_SHOW_CYCLE => toggle!(show_cycle),
        IDM_TRANSPARENT_BACKGROUND => toggle!(transparent_background),
        IDM_ANCHOR_TASKBAR_RIGHT => {
            let target = {
                let guard = lock_state();
                guard.as_ref().map(|state| !state.anchor_taskbar_right)
            };
            pin_anchor(target);
            toggle!(anchor_taskbar_right);
        }
        IDM_TOGGLE_WIDGET => toggle_widget_visibility(),
        IDM_OPEN_QODER => open_detail_url(),
        IDM_START_WITH_WINDOWS => {
            let enable = !is_startup_enabled();
            set_startup_enabled(enable);
            diagnose::log(format!("startup enabled={enable}"));
        }
        IDM_RESET_POSITION => {
            {
                let mut guard = lock_state();
                if let Some(state) = guard.as_mut() {
                    state.anchor_offset = None;
                    state.anchor_taskbar_right = false;
                }
            }
            save_state_settings();
            update_display();
        }
        IDM_ALERT_OFF => set_alert_threshold(0),
        IDM_ALERT_10 => set_alert_threshold(10),
        IDM_ALERT_25 => set_alert_threshold(25),
        IDM_ALERT_50 => set_alert_threshold(50),
        IDM_THEME_SYSTEM => set_appearance(AppearanceTheme::System),
        IDM_THEME_LIGHT => set_appearance(AppearanceTheme::Light),
        IDM_THEME_DARK => set_appearance(AppearanceTheme::Dark),
        IDM_BAR_CONTINUOUS => set_bar_style(BarStyle::Continuous),
        IDM_BAR_SEGMENTED => set_bar_style(BarStyle::Segmented),
        IDM_MODE_USED => set_text_mode(TextMode::Used),
        IDM_MODE_REMAINING => set_text_mode(TextMode::Remaining),
        IDM_LANG_SYSTEM => set_language(None),
        IDM_LANG_ENGLISH => set_language(Some(LanguageId::English)),
        IDM_LANG_CHINESE => set_language(Some(LanguageId::SimplifiedChinese)),
        IDM_EXIT => unsafe {
            PostMessageW(hwnd, WM_CLOSE, WPARAM(0), LPARAM(0)).ok();
        },
        _ => {}
    }
}

fn set_alert_threshold(threshold: u8) {
    {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            if state.alert_threshold_percent != threshold {
                state.alert_threshold_percent = threshold;
                state.notified_alerts.clear();
            }
        }
    }
    save_state_settings();
}

fn set_appearance(theme: AppearanceTheme) {
    {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            state.appearance_theme = theme;
            state.is_dark = appearance_is_dark(theme);
        }
    }
    save_state_settings();
    update_display();
}

fn set_bar_style(style: BarStyle) {
    {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            state.bar_style = style;
        }
    }
    save_state_settings();
    update_display();
}

fn set_text_mode(mode: TextMode) {
    {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            state.text_mode = mode;
        }
    }
    save_state_settings();
    update_display();
}

fn set_language(language: Option<LanguageId>) {
    {
        let mut guard = lock_state();
        if let Some(state) = guard.as_mut() {
            state.language_override = language;
            state.language = localization::resolve_language(language);
        }
    }
    save_state_settings();
    let title = {
        let guard = lock_state();
        guard
            .as_ref()
            .map(|state| state.strings().window_title.to_string())
            .unwrap_or_default()
    };
    let hwnd = lock_state()
        .as_ref()
        .map(|state| state.hwnd.to_hwnd())
        .unwrap_or(HWND::default());
    let wide = native_interop::wide_str(&title);
    unsafe {
        let _ = SetWindowTextW(hwnd, PCWSTR::from_raw(wide.as_ptr()));
    }
    update_display();
}

// ----------------------------------------------------------------- messaging --

unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER => {
            match wparam.0 {
                TIMER_POLL => begin_poll(hwnd),
                TIMER_COUNTDOWN => {
                    check_theme_change();
                    update_display();
                }
                TIMER_CREDENTIAL_WATCH => on_watch_tick(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_USAGE_UPDATED => {
            update_display();
            schedule_countdown_timer(hwnd);
            LRESULT(0)
        }
        WM_APP_TRAY => {
            match tray_icon::handle_message(lparam) {
                tray_icon::TrayAction::ToggleWidget => toggle_widget_visibility(),
                tray_icon::TrayAction::ShowContextMenu => show_context_menu(hwnd),
                tray_icon::TrayAction::None => {}
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            handle_menu_command(hwnd, wparam.0 & 0xFFFF);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            begin_drag(hwnd);
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            update_drag();
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            end_drag(hwnd);
            LRESULT(0)
        }
        WM_RBUTTONUP | WM_CONTEXTMENU => {
            show_context_menu(hwnd);
            LRESULT(0)
        }
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            let layout = {
                let guard = lock_state();
                guard.as_ref().map(compute_layout)
            };
            if let Some(layout) = layout {
                paint_hwnd(hdc, &layout);
            }
            let _ = EndPaint(hwnd, &ps);
            LRESULT(0)
        }
        WM_DPICHANGED_MSG => {
            refresh_dpi();
            update_display();
            LRESULT(0)
        }
        WM_DISPLAYCHANGE => {
            position_at_taskbar();
            update_display();
            LRESULT(0)
        }
        WM_SETTINGCHANGE | WM_SYSCOLORCHANGE => {
            check_theme_change();
            LRESULT(0)
        }
        WM_DESTROY => {
            cleanup(hwnd);
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn cleanup(hwnd: HWND) {
    let hook = {
        let mut guard = lock_state();
        guard.as_mut().and_then(|state| state.win_event_hook.take())
    };
    if let Some(hook) = hook {
        native_interop::unhook_win_event(hook);
    }
    tray_icon::remove_all(hwnd);
}

// ---------------------------------------------------------------------- run --

pub fn run() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let dpi = GetDpiForSystem();
        if dpi > 0 {
            CURRENT_DPI.store(dpi, Ordering::Relaxed);
        }
    }

    let preview_mode = cli::options().preview;
    diagnose::log(if preview_mode {
        "preview window starting"
    } else {
        "widget starting"
    });

    let is_relaunch = std::env::var(ENV_RELAUNCH).is_ok();
    let _single_instance = if preview_mode {
        None
    } else {
        match acquire_single_instance_lock(is_relaunch) {
            Some(handle) => Some(handle),
            None => return,
        }
    };

    let settings = load_settings();
    let language_override = settings
        .language
        .as_deref()
        .and_then(LanguageId::from_code);
    let language = localization::resolve_language(language_override);
    let is_dark = appearance_is_dark(settings.appearance_theme);

    let class_name = native_interop::wide_str("QoderUsage");
    let title = native_interop::wide_str(language.strings().window_title);

    unsafe {
        let hinstance = GetModuleHandleW(PCWSTR::null())
            .map(HINSTANCE::from)
            .unwrap_or_default();
        let wc = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wnd_proc),
            hInstance: hinstance.into(),
            hCursor: LoadCursorW(HINSTANCE::default(), IDC_ARROW).unwrap_or_default(),
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            lpszClassName: PCWSTR::from_raw(class_name.as_ptr()),
            ..Default::default()
        };
        if RegisterClassExW(&wc) == 0 {
            diagnose::log("window class registration failed");
            return;
        }

        let extended = if preview_mode {
            WS_EX_APPWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE
        } else {
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE
        };
        let Ok(hwnd) = CreateWindowExW(
            extended,
            PCWSTR::from_raw(class_name.as_ptr()),
            PCWSTR::from_raw(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            sc(220),
            sc(WIDGET_HEIGHT),
            HWND::default(),
            HMENU::default(),
            hinstance,
            None,
        ) else {
            diagnose::log("window creation failed");
            return;
        };

        {
            let mut guard = lock_state();
            *guard = Some(AppState {
                hwnd: SendHwnd::from_hwnd(hwnd),
                taskbar_hwnd: None,
                tray_notify_hwnd: None,
                win_event_hook: None,
                embedded: false,
                preview_mode,
                widget_visible: settings.widget_visible,
                is_dark,
                appearance_theme: settings.appearance_theme,
                bar_style: settings.bar_style,
                text_mode: settings.text_mode,
                transparent_background: settings.transparent_background,
                language,
                language_override,
                snapshot: None,
                status: Status::Loading,
                show_user: settings.show_user,
                show_addon: settings.show_addon,
                show_org: settings.show_org,
                show_packages: settings.show_packages,
                show_cycle: settings.show_cycle,
                alert_threshold_percent: settings.alert_threshold_percent,
                notified_alerts: settings.notified_alerts.into_iter().collect(),
                poll_interval_ms: settings.poll_interval_ms,
                retry_count: 0,
                credential_watch: None,
                taskbar_index: settings.taskbar_index,
                anchor_offset: settings.anchor_offset,
                anchor_taskbar_right: settings.anchor_taskbar_right,
                dragging: false,
                drag_start_mouse_x: 0,
                drag_start_offset: 0,
            });
        }

        if preview_mode {
            diagnose::log("preview window is floating");
        } else if attach_to_taskbar(hwnd, settings.taskbar_index) {
            diagnose::log("attached to taskbar");
        } else {
            diagnose::log("taskbar unavailable, widget floats on the desktop");
        }

        update_display();
        let _ = SetTimer(hwnd, TIMER_POLL, FIRST_POLL_DELAY_MS, None);
        let _ = SetTimer(hwnd, TIMER_COUNTDOWN, COUNTDOWN_TICK_MS, None);
        if !preview_mode {
            let _ = SetTimer(hwnd, TIMER_CREDENTIAL_WATCH, CREDENTIAL_WATCH_MS, None);
        }
        begin_poll(hwnd);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, HWND::default(), 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        cleanup(hwnd);
    }
}

fn acquire_single_instance_lock(relaunch: bool) -> Option<HANDLE> {
    let name = native_interop::wide_str("Global\\QoderUsage");
    unsafe {
        let Ok(handle) = CreateMutexW(None, true, PCWSTR::from_raw(name.as_ptr())) else {
            diagnose::log("single-instance mutex unavailable");
            return None;
        };
        if GetLastError() != ERROR_ALREADY_EXISTS {
            return Some(handle);
        }
        if !relaunch {
            diagnose::log("another instance is already running");
            return None;
        }
        let waited = WaitForSingleObject(handle, 10_000);
        if waited != WAIT_OBJECT_0 && waited != WAIT_ABANDONED {
            diagnose::log("relaunch aborted: previous instance still running");
            return None;
        }
        Some(handle)
    }
}

