use std::time::SystemTime;

/// One credits bucket as reported by the Qoder usage API.
#[derive(Clone, Debug)]
pub struct Quota {
    pub total: f64,
    pub used: f64,
    pub remaining: f64,
    /// Percentage of the quota already consumed, 0..100.
    pub used_percentage: f64,
    pub unit: String,
    pub available: bool,
    pub expires_at: Option<SystemTime>,
}

impl Quota {
    pub fn remaining_percentage(&self) -> f64 {
        100.0 - self.used_percentage.clamp(0.0, 100.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QuotaKind {
    User,
    AddOn,
    Org,
    Package,
}

#[derive(Clone, Debug)]
pub struct QuotaRow {
    pub kind: QuotaKind,
    /// Stable identity used for visibility settings and alert deduplication.
    pub key: String,
    pub label: String,
    pub quota: Quota,
}

#[derive(Clone, Debug, Default)]
pub struct QoderUsage {
    pub enterprise: bool,
    pub detail_url: String,
    pub user_type: String,
    pub quota_exceeded: bool,
    pub total_usage_percentage: Option<f64>,
    /// When the current credits period rolls over.
    pub cycle_expires_at: Option<SystemTime>,
    pub rows: Vec<QuotaRow>,
}

#[derive(Clone, Debug)]
pub struct Snapshot {
    pub usage: QoderUsage,
    pub fetched_at: SystemTime,
    /// Human readable origin of the credential, never contains the token.
    pub source: String,
    pub region: &'static str,
}
