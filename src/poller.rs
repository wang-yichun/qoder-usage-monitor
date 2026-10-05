use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::cli;
use crate::credits::{self, parse_system_time};
use crate::models::{QoderUsage, Quota, QuotaKind, QuotaRow, Snapshot};

const REGIONS: &[(&str, &str)] = &[
    ("global", "https://openapi.qoder.sh"),
    ("cn", "https://openapi.qoder.com.cn"),
];
const USAGE_PATH: &str = "/sash/api/v2/me/usage";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PollError {
    NoCredentials,
    AuthRequired,
    NetworkUnavailable,
    RateLimited,
    ServerError,
    RequestFailed,
}

impl PollError {
    pub fn category(self) -> &'static str {
        match self {
            Self::NoCredentials => "no-credentials",
            Self::AuthRequired => "auth",
            Self::NetworkUnavailable => "network",
            Self::RateLimited => "rate-limited",
            Self::ServerError => "server",
            Self::RequestFailed => "request",
        }
    }

    /// Auth failures only clear up when the desktop app rewrites its token file,
    /// so the widget watches that file instead of hammering the API.
    pub fn is_credential_failure(self) -> bool {
        matches!(self, Self::NoCredentials | Self::AuthRequired)
    }
}

/// Region that last answered successfully, so polling doesn't probe both every time.
static REGION_CACHE: Mutex<Option<&'static str>> = Mutex::new(None);

pub fn credential_signature() -> Option<String> {
    credits::credential_signature()
}

pub fn poll() -> Result<Snapshot, PollError> {
    let options = cli::options();
    if options.demo {
        return Ok(demo_snapshot().map_err(|_| PollError::RequestFailed)?);
    }

    let tokens = credential_candidates();
    if tokens.is_empty() {
        return Err(PollError::NoCredentials);
    }

    let mut last_error = PollError::NoCredentials;
    for region in region_order(options.region.as_deref()) {
        for (token, source) in &tokens {
            match fetch_usage(region.1, token) {
                Ok(usage) => {
                    remember_region(region.0);
                    return Ok(Snapshot {
                        usage,
                        fetched_at: SystemTime::now(),
                        source: source.clone(),
                        region: region.0,
                    });
                }
                Err(error) => last_error = error,
            }
        }
    }
    Err(last_error)
}

fn region_order(pinned: Option<&str>) -> Vec<(&'static str, &'static str)> {
    let find = |name: &str| REGIONS.iter().find(|(region, _)| *region == name).copied();
    if let Some(pinned) = pinned.and_then(find) {
        return vec![pinned];
    }
    let guard = REGION_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    let cached: Option<&'static str> = match *guard {
        Some(region) => Some(region),
        None => None,
    };
    let mut ordered: Vec<(&'static str, &'static str)> = REGIONS.to_vec();
    if let Some(cached) = cached {
        if let Some(index) = ordered.iter().position(|region| region.0 == cached) {
            let entry = ordered.remove(index);
            ordered.insert(0, entry);
        }
    }
    ordered
}

fn remember_region(region: &'static str) {
    let mut cached = REGION_CACHE.lock().unwrap_or_else(|error| error.into_inner());
    *cached = Some(region);
}

/// (token, human readable origin) pairs ordered by preference.
fn credential_candidates() -> Vec<(String, String)> {
    let mut candidates = Vec::new();
    if let Some(token) = cli::options().token.clone() {
        candidates.push((token, "--token".to_string()));
    }
    if let Some(auth) = credits::load_desktop_auth() {
        let label = match auth.token_expires_at {
            Some(expiry) => format!(
                "桌面应用令牌（auth.v1.dat，有效期至 {}）",
                crate::native_interop::format_local(expiry)
            ),
            None => "桌面应用令牌（auth.v1.dat）".to_string(),
        };
        candidates.push((auth.token, label));
    }
    candidates
}

/// ureq only compiles the native-tls backend, it never selects it by default,
/// so the agent has to be handed a connector explicitly. Schannel then gives us
/// the Windows certificate store, which matches how the desktop app validates.
fn build_agent() -> Result<ureq::Agent, PollError> {
    let connector = native_tls::TlsConnector::new().map_err(|_| PollError::NetworkUnavailable)?;
    Ok(ureq::AgentBuilder::new()
        .timeout(REQUEST_TIMEOUT)
        .tls_connector(std::sync::Arc::new(connector))
        .build())
}

fn fetch_usage(base_url: &str, token: &str) -> Result<QoderUsage, PollError> {
    let agent = build_agent()?;
    let url = format!("{base_url}{USAGE_PATH}");
    let response = agent
        .get(&url)
        .set("Authorization", &format!("Bearer {token}"))
        .set("Accept", "application/json")
        .call();
    let response = match response {
        Ok(response) => response,
        Err(ureq::Error::Status(status, _)) => {
            crate::diagnose::log(format!("{url} -> HTTP {status}"));
            return Err(classify_status(status));
        }
        Err(error) => {
            crate::diagnose::log(error.to_string());
            return Err(PollError::NetworkUnavailable);
        }
    };
    let body: Value = response
        .into_json()
        .map_err(|_| PollError::RequestFailed)?;
    parse_usage(&body)
}

fn classify_status(status: u16) -> PollError {
    match status {
        401 | 403 => PollError::AuthRequired,
        429 => PollError::RateLimited,
        500..=599 => PollError::ServerError,
        _ => PollError::RequestFailed,
    }
}

fn is_object(value: &Value) -> bool {
    value.is_object()
}

fn pick_str(object: Option<&Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        object
            .and_then(|value| value.get(*key))
            .and_then(Value::as_str)
            .map(str::to_string)
    })
}

fn pick_num(object: Option<&Value>, keys: &[&str]) -> Option<f64> {
    keys.iter().find_map(|key| {
        object
            .and_then(|value| value.get(*key))
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
    })
}

fn pick_bool(object: Option<&Value>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|key| {
        object
            .and_then(|value| value.get(*key))
            .and_then(Value::as_bool)
    })
}

/// The API mixes 0..1 and 0..100 styles depending on the field and version.
fn normalize_percentage(raw: Option<f64>, used: f64, total: f64) -> f64 {
    match raw {
        Some(value) if (0.0..=1.0).contains(&value) => value * 100.0,
        Some(value) if (0.0..=100.0).contains(&value) => value,
        _ => {
            if total > 0.0 {
                (used / total * 100.0).clamp(0.0, 100.0)
            } else {
                0.0
            }
        }
    }
}

fn normalize_quota(raw: Option<&Value>) -> Option<Quota> {
    let quota = raw.filter(|value| is_object(value))?;
    let total = pick_num(Some(quota), &["total"]).unwrap_or(0.0);
    let used = pick_num(Some(quota), &["used"]).unwrap_or(0.0);
    let remaining = pick_num(Some(quota), &["remaining"])
        .filter(|value| *value >= 0.0)
        .unwrap_or_else(|| (total - used).max(0.0));
    Some(Quota {
        total,
        used,
        remaining,
        used_percentage: normalize_percentage(
            pick_num(Some(quota), &["percentage"]),
            used,
            total,
        ),
        unit: pick_str(Some(quota), &["unit"]).unwrap_or_else(|| "credits".to_string()),
        available: pick_bool(Some(quota), &["available"]).unwrap_or(true),
        expires_at: parse_system_time(quota.get("expires_at").or_else(|| quota.get("expiresAt"))),
    })
}

pub fn parse_usage(body: &Value) -> Result<QoderUsage, PollError> {
    if !is_object(body) {
        return Err(PollError::RequestFailed);
    }

    if body.get("displayMode").and_then(Value::as_str) == Some("enterprise") {
        let enterprise = body.get("enterpriseUsage");
        return Ok(QoderUsage {
            enterprise: true,
            detail_url: pick_str(enterprise, &["detailUrl", "detail_url"]).unwrap_or_default(),
            ..Default::default()
        });
    }

    let usage = body
        .get("qoderUsage")
        .filter(|value| is_object(value))
        .ok_or(PollError::RequestFailed)?;

    let mut rows = Vec::new();
    if let Some(quota) = normalize_quota(usage.get("user_quota").or_else(|| usage.get("userQuota")))
    {
        rows.push(QuotaRow {
            kind: QuotaKind::User,
            key: "user".to_string(),
            label: String::new(),
            quota,
        });
    }
    if let Some(quota) =
        normalize_quota(usage.get("add_on_quota").or_else(|| usage.get("addOnQuota")))
    {
        if quota.total > 0.0 {
            rows.push(QuotaRow {
                kind: QuotaKind::AddOn,
                key: "addon".to_string(),
                label: String::new(),
                quota,
            });
        }
    }

    let org_raw = usage
        .get("org_resource_package")
        .or_else(|| usage.get("orgResourcePackage"))
        .or_else(|| usage.get("shared_quota"))
        .or_else(|| usage.get("sharedQuota"));
    if let Some(raw) = org_raw.filter(|value| is_object(value)) {
        let cap = pick_num(Some(raw), &["cap", "total"]).unwrap_or(0.0);
        if let Some(mut quota) = normalize_quota(Some(raw)) {
            if cap > 0.0 {
                let used = quota.used;
                quota.total = cap;
                quota.remaining = (cap - used).max(0.0);
                quota.used_percentage = normalize_percentage(None, used, cap);
            }
            if quota.total > 0.0 {
                quota.available = pick_bool(Some(raw), &["available"]).unwrap_or(cap > 0.0);
                rows.push(QuotaRow {
                    kind: QuotaKind::Org,
                    key: "org".to_string(),
                    label: String::new(),
                    quota,
                });
            }
        }
    }

    let packages = usage
        .get("dedicated_resource_packages")
        .or_else(|| usage.get("dedicatedResourcePackages"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for package in packages {
        if !is_object(&package) {
            continue;
        }
        let id = pick_str(Some(&package), &["id"]).unwrap_or_default();
        let Some(mut quota) = normalize_quota(Some(&package)) else {
            continue;
        };
        if quota.total <= 0.0 {
            continue;
        }
        quota.available = pick_bool(Some(&package), &["available"]).unwrap_or(false);
        let name = pick_str(Some(&package), &["name"])
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| id.clone());
        let key = if id.is_empty() {
            format!("pkg:{name}")
        } else {
            format!("pkg:{id}")
        };
        rows.push(QuotaRow {
            kind: QuotaKind::Package,
            key,
            label: name,
            quota,
        });
    }

    Ok(QoderUsage {
        enterprise: false,
        detail_url: pick_str(Some(usage), &["upgrade_url", "upgradeUrl"])
            .unwrap_or_default(),
        user_type: pick_str(Some(usage), &["user_type", "userType"]).unwrap_or_default(),
        quota_exceeded: pick_bool(Some(usage), &["is_quota_exceeded", "isQuotaExceeded"])
            .unwrap_or(false),
        total_usage_percentage: pick_num(Some(usage), &["total_usage_percentage", "totalUsagePercentage"])
            .map(|value| normalize_percentage(Some(value), 0.0, 0.0)),
        cycle_expires_at: parse_system_time(
            usage.get("expires_at").or_else(|| usage.get("expiresAt")),
        ),
        rows,
    })
}

fn demo_snapshot() -> Result<Snapshot, PollError> {
    let body = serde_json::json!({
        "displayMode": "qoder",
        "qoderUsage": {
            "user_type": "pro",
            "usage_type": "credits",
            "is_quota_exceeded": false,
            "total_usage_percentage": 0.42,
            "expires_at": epoch_millis_in(26 * 86_400 + 5 * 3_600),
            "user_quota": { "total": 1000, "used": 523.5, "remaining": 476.5, "percentage": 0.5235, "unit": "credits" },
            "add_on_quota": { "total": 500, "used": 75, "remaining": 425, "unit": "credits" },
            "org_resource_package": { "cap": 4000, "used": 1200, "available": true },
            "dedicated_resource_packages": [
                {
                    "id": "pkg-booster-01",
                    "name": "加速包",
                    "total": 200,
                    "used": 30,
                    "remaining": 170,
                    "percentage": 0.15,
                    "unit": "credits",
                    "expires_at": epoch_millis_in(29 * 86_400),
                    "available": true
                }
            ]
        }
    });
    Ok(Snapshot {
        usage: parse_usage(&body)?,
        fetched_at: SystemTime::now(),
        source: "演示数据".to_string(),
        region: "demo",
    })
}

fn epoch_millis_in(seconds_from_now: u64) -> u64 {
    let now = SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    (now + seconds_from_now) * 1000
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_qoder_quota_payload() {
        let usage = parse_usage(&serde_json::json!({
            "displayMode": "qoder",
            "qoderUsage": {
                "userType": "personal_professional_trial",
                "isQuotaExceeded": false,
                "totalUsagePercentage": 0.51,
                "expiresAt": 1_800_000_000,
                "userQuota": { "total": 300, "used": 253, "remaining": 47, "percentage": 0.85 },
                "addOnQuota": { "total": 200, "used": 0, "remaining": 200 },
            }
        }))
        .unwrap();
        assert_eq!(usage.rows.len(), 2);
        assert_eq!(usage.user_type, "personal_professional_trial");
        assert!((usage.total_usage_percentage.unwrap() - 51.0).abs() < 0.001);
        assert!(usage.cycle_expires_at.is_some());
        let user = &usage.rows[0];
        assert!((user.quota.used_percentage - 85.0).abs() < 0.001);
        assert!((user.quota.remaining - 47.0).abs() < 0.001);
    }

    #[test]
    fn enterprise_mode_has_no_quota_rows() {
        let usage = parse_usage(&serde_json::json!({
            "displayMode": "enterprise",
            "enterpriseUsage": { "openMode": "link", "detailUrl": "https://example.test/usage" }
        }))
        .unwrap();
        assert!(usage.enterprise);
        assert_eq!(usage.detail_url, "https://example.test/usage");
        assert!(usage.rows.is_empty());
    }

    #[test]
    fn derives_percentage_from_used_and_total() {
        let quota = normalize_quota(Some(&serde_json::json!({ "total": 100, "used": 25 }))).unwrap();
        assert!((quota.used_percentage - 25.0).abs() < 0.001);
        assert!((quota.remaining - 75.0).abs() < 0.001);
    }
}
