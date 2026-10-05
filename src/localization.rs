use windows::Win32::Globalization::GetUserDefaultUILanguage;

/// `PRIMARYLANGID` is a C macro; the windows crate exposes only the raw LANGID.
const fn primary_language_id(language_id: u16) -> u16 {
    language_id & 0x3FF
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Strings {
    pub window_title: &'static str,
    pub refresh: &'static str,
    pub update_frequency: &'static str,
    pub one_minute: &'static str,
    pub five_minutes: &'static str,
    pub fifteen_minutes: &'static str,
    pub one_hour: &'static str,
    pub quota_row_label: &'static str,
    pub cycle_row_label: &'static str,
    pub user_quota: &'static str,
    pub add_on: &'static str,
    pub org_package: &'static str,
    pub package: &'static str,
    pub content: &'static str,
    pub quota_details: &'static str,
    pub appearance: &'static str,
    pub theme_system: &'static str,
    pub theme_light: &'static str,
    pub theme_dark: &'static str,
    pub bar_continuous: &'static str,
    pub bar_segmented: &'static str,
    pub mode_used: &'static str,
    pub mode_remaining: &'static str,
    pub transparent_background: &'static str,
    pub quota_alerts: &'static str,
    pub alert_off: &'static str,
    pub alert_remaining: &'static str,
    pub settings: &'static str,
    pub start_with_windows: &'static str,
    pub reset_position: &'static str,
    pub anchor_right: &'static str,
    pub language: &'static str,
    pub system_default: &'static str,
    pub show_widget: &'static str,
    pub exit: &'static str,
    pub not_logged_in: &'static str,
    pub credential_title: &'static str,
    pub credential_body: &'static str,
    pub quota_alert_body: &'static str,
    pub expired: &'static str,
    pub remaining: &'static str,
    pub unavailable: &'static str,
    pub network_error: &'static str,
    pub error_marker: &'static str,
    pub days_short: &'static str,
    pub hours_short: &'static str,
    pub minutes_short: &'static str,
    pub loading: &'static str,
    pub enterprise_hint: &'static str,
    /// Shown when the account reports no numeric quota buckets but has consumed
    /// everything it is allowed to use.
    pub exhausted: &'static str,
    /// Label for the aggregate percentage reported by the usage API.
    pub total_usage: &'static str,
}

pub const ENGLISH: Strings = Strings {
    window_title: "Qoder Usage",
    refresh: "Refresh now",
    update_frequency: "Update frequency",
    one_minute: "1 minute",
    five_minutes: "5 minutes",
    fifteen_minutes: "15 minutes",
    one_hour: "1 hour",
    quota_row_label: "Quota",
    cycle_row_label: "Renews",
    user_quota: "Plan credits",
    add_on: "Add-on",
    org_package: "Org package",
    package: "Package",
    content: "Display",
    quota_details: "Quota details",
    appearance: "Appearance",
    theme_system: "Theme: System",
    theme_light: "Theme: Light",
    theme_dark: "Theme: Dark",
    bar_continuous: "Continuous bars",
    bar_segmented: "Segmented bars",
    mode_used: "Used mode",
    mode_remaining: "Remaining mode",
    transparent_background: "Transparent background",
    quota_alerts: "Quota alerts",
    alert_off: "Off",
    alert_remaining: "% remaining",
    settings: "Settings",
    start_with_windows: "Start with Windows",
    reset_position: "Reset position",
    anchor_right: "Anchor to taskbar right",
    language: "Language",
    system_default: "System default",
    show_widget: "Show widget",
    exit: "Exit",
    not_logged_in: "No Qoder desktop credentials found",
    credential_title: "Qoder login required",
    credential_body: "Open the Qoder desktop app to refresh its token, then click Refresh.",
    quota_alert_body: "{remaining}% credits left in {label}.",
    expired: "expired",
    remaining: "left",
    unavailable: "n/a",
    network_error: "NET",
    error_marker: "!",
    days_short: "d",
    hours_short: "h",
    minutes_short: "m",
    loading: "…",
    enterprise_hint: "Enterprise account: view usage in browser",
    exhausted: "Quota exhausted",
    total_usage: "Total",
};

pub const SIMPLIFIED_CHINESE: Strings = Strings {
    window_title: "Qoder 用量",
    refresh: "立即刷新",
    update_frequency: "刷新频率",
    one_minute: "1 分钟",
    five_minutes: "5 分钟",
    fifteen_minutes: "15 分钟",
    one_hour: "1 小时",
    quota_row_label: "额度",
    cycle_row_label: "到期",
    user_quota: "用户额度",
    add_on: "附加包",
    org_package: "组织资源包",
    package: "资源包",
    content: "显示内容",
    quota_details: "额度明细",
    appearance: "外观",
    theme_system: "主题：跟随系统",
    theme_light: "主题：浅色",
    theme_dark: "主题：深色",
    bar_continuous: "连续进度条",
    bar_segmented: "分段进度条",
    mode_used: "已用模式",
    mode_remaining: "余量模式",
    transparent_background: "透明背景",
    quota_alerts: "额度提醒",
    alert_off: "关闭",
    alert_remaining: "剩余 %",
    settings: "设置",
    start_with_windows: "开机自动启动",
    reset_position: "重置位置",
    anchor_right: "锚定任务栏右侧",
    language: "语言",
    system_default: "跟随系统",
    show_widget: "显示小组件",
    exit: "退出",
    not_logged_in: "未找到 Qoder 桌面应用凭据",
    credential_title: "需要登录 Qoder",
    credential_body: "请打开 Qoder 桌面应用刷新令牌后，点击「立即刷新」。",
    quota_alert_body: "{label}仅剩 {remaining}% 额度。",
    expired: "已到期",
    remaining: "剩",
    unavailable: "无",
    network_error: "网络",
    error_marker: "!",
    days_short: "天",
    hours_short: "时",
    minutes_short: "分",
    loading: "…",
    enterprise_hint: "企业账户：用量需在浏览器查看",
    exhausted: "额度已用尽",
    total_usage: "总用量",
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LanguageId {
    English,
    SimplifiedChinese,
}

pub const LANGUAGES: [LanguageId; 2] = [LanguageId::English, LanguageId::SimplifiedChinese];

impl LanguageId {
    pub fn strings(self) -> Strings {
        match self {
            Self::English => ENGLISH,
            Self::SimplifiedChinese => SIMPLIFIED_CHINESE,
        }
    }

    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::SimplifiedChinese => "zh-CN",
        }
    }

    pub fn native_name(self) -> &'static str {
        match self {
            Self::English => "English",
            Self::SimplifiedChinese => "简体中文",
        }
    }

    pub fn from_code(code: &str) -> Option<Self> {
        LANGUAGES.iter().find(|language| language.code() == code).copied()
    }
}

pub fn resolve_language(override_language: Option<LanguageId>) -> LanguageId {
    override_language.unwrap_or_else(system_language)
}

fn system_language() -> LanguageId {
    let language_id = unsafe { GetUserDefaultUILanguage() };
    match primary_language_id(language_id) {
        0x04 => LanguageId::SimplifiedChinese,
        _ => LanguageId::English,
    }
}
