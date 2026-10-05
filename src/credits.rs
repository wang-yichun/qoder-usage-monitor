use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use serde_json::Value;
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Cryptography::{
    CryptUnprotectData, CRYPT_INTEGER_BLOB,
};

/// Credential copied out of the Qoder desktop app's encrypted auth store.
pub struct DesktopAuth {
    pub token: String,
    pub token_expires_at: Option<SystemTime>,
}

/// Electron safeStorage header: "v10" + 12-byte GCM nonce.
const SAFE_STORAGE_PREFIX: usize = 3;
const GCM_NONCE_LEN: usize = 12;
const GCM_TAG_LEN: usize = 16;

/// The Electron app data folder has been published under two names across
/// releases, so both are probed for the credential store.
fn app_data_dir() -> Option<PathBuf> {
    let appdata = std::env::var("APPDATA").ok()?;
    ["com.qoder.app.stable", "com.qoder.app"]
        .iter()
        .map(|name| PathBuf::from(&appdata).join(name))
        .find(|dir| dir.join("Local State").is_file())
}

pub fn auth_file_path() -> Option<PathBuf> {
    Some(app_data_dir()?.join("auth.v1.dat"))
}

pub fn local_state_path() -> Option<PathBuf> {
    Some(app_data_dir()?.join("Local State"))
}

/// Changes when the desktop app refreshes its tokens, so the widget can resume
/// polling without waiting for the regular interval.
pub fn credential_signature() -> Option<String> {
    let path = auth_file_path()?;
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map(|value| value.as_secs())
        .unwrap_or(0);
    Some(format!("{modified}:{}", meta.len()))
}

fn read_aes_key(local_state: &Path) -> Option<Vec<u8>> {
    let text = std::fs::read_to_string(local_state).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let encoded = value.get("os_crypt")?.get("encrypted_key")?.as_str()?;
    let blob = base64::engine::general_purpose::STANDARD.decode(encoded).ok()?;
    let wrapped = blob.get(5..)?;
    dpapi_unprotect(wrapped)
}

fn dpapi_unprotect(data: &[u8]) -> Option<Vec<u8>> {
    unsafe {
        let mut input = CRYPT_INTEGER_BLOB {
            cbData: data.len() as u32,
            pbData: data.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB::default();
        CryptUnprotectData(&mut input, None, None, None, None, 0, &mut output).ok()?;
        if output.pbData.is_null() || output.cbData == 0 {
            return None;
        }
        let key = std::slice::from_raw_parts(output.pbData, output.cbData as usize).to_vec();
        let _ = LocalFree(HLOCAL(output.pbData as *mut _));
        Some(key)
    }
}

fn aes_gcm_decrypt(key: &[u8], blob: &[u8]) -> Option<Vec<u8>> {
    use aes_gcm::aead::Aead;
    use aes_gcm::{Aes256Gcm, KeyInit, Nonce};

    if blob.len() < SAFE_STORAGE_PREFIX + GCM_NONCE_LEN + GCM_TAG_LEN {
        return None;
    }
    if &blob[..SAFE_STORAGE_PREFIX] != b"v10" {
        return None;
    }
    let nonce_start = SAFE_STORAGE_PREFIX;
    let cipher_start = nonce_start + GCM_NONCE_LEN;
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let nonce = Nonce::from_slice(&blob[nonce_start..cipher_start]);
    // The trailing 16-byte GCM tag is already positioned after the ciphertext.
    cipher.decrypt(nonce, &blob[cipher_start..]).ok()
}

pub fn load_desktop_auth() -> Option<DesktopAuth> {
    let key = read_aes_key(&local_state_path()?)?;
    let blob = std::fs::read(auth_file_path()?).ok()?;
    let plain = aes_gcm_decrypt(&key, &blob)?;
    let text = String::from_utf8(plain).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let token = value.get("token")?.as_str()?.trim();
    if token.is_empty() {
        return None;
    }
    Some(DesktopAuth {
        token: token.to_string(),
        token_expires_at: parse_system_time(value.get("expiresAt")),
    })
}

/// Accept both ISO-8601 strings and epoch seconds / milliseconds numbers.
pub fn parse_system_time(value: Option<&Value>) -> Option<SystemTime> {
    let value = value?;
    if let Some(number) = value.as_f64() {
        if !number.is_finite() || number <= 0.0 {
            return None;
        }
        let seconds = if number < 1e12 { number } else { number / 1000.0 };
        return UNIX_EPOCH.checked_add(Duration::from_secs_f64(seconds));
    }
    let text = value.as_str()?;
    parse_iso8601(text)
}

pub fn parse_iso8601(text: &str) -> Option<SystemTime> {
    let text = text.trim();
    let (date_part, offset_part) = split_offset(text);
    let (year, month, day, hour, minute, second, nanos) = parse_date_time(date_part)?;
    let days = days_from_civil(year, month, day);
    let mut seconds = days as i64 * 86_400 + hour as i64 * 3_600 + minute as i64 * 60 + second as i64;
    seconds -= offset_part;
    if seconds < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::new(seconds as u64, nanos))
}

/// Returns the naive date/time text plus the UTC offset in seconds.
fn split_offset(text: &str) -> (&str, i64) {
    let bytes = text.as_bytes();
    let search_from = bytes.len().saturating_sub(6);
    if let Some(index) = text[search_from..].rfind(['Z', '+', '-']) {
        let index = search_from + index;
        let sign = match bytes[index] {
            b'-' => -1,
            _ => 1,
        };
        let tail = &text[index + 1..];
        let offset = match tail.split_once(':') {
            Some((h, m)) => {
                let hours: i64 = h.parse().unwrap_or(0);
                let minutes: i64 = m.parse().unwrap_or(0);
                hours * 3_600 + minutes * 60
            }
            None if tail.len() == 4 => {
                let hours: i64 = tail[..2].parse().unwrap_or(0);
                let minutes: i64 = tail[2..].parse().unwrap_or(0);
                hours * 3_600 + minutes * 60
            }
            _ => 0,
        };
        return (&text[..index], sign * offset);
    }
    // No zone marker: timestamps written without a zone are local wall clock.
    (text, local_offset_secs())
}

/// Seconds east of UTC for the current machine timezone.
fn local_offset_secs() -> i64 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};

    unsafe {
        let mut info = TIME_ZONE_INFORMATION::default();
        GetTimeZoneInformation(&mut info);
        -(info.Bias as i64 * 60)
    }
}

fn parse_date_time(text: &str) -> Option<(i64, u32, u32, u32, u32, u32, u32)> {
    let date_part = text.get(..10)?;
    let year: i64 = date_part.get(..4)?.parse().ok()?;
    let month: u32 = date_part.get(5..7)?.parse().ok()?;
    let day: u32 = date_part.get(8..10)?.parse().ok()?;
    let rest = &text[10..];
    if rest.is_empty() {
        return Some((year, month, day, 0, 0, 0, 0));
    }
    let time_text = rest.get(1..).filter(|_| matches!(&rest[..1], "T" | "t" | " "))?;
    let (time_text, nanos) = match time_text.split_once('.') {
        Some((_, tail)) => {
            let digits: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
            let mut scaled = digits.clone();
            scaled.push_str(&"0".repeat(9usize.saturating_sub(digits.len())));
            (
                &time_text[..time_text.len() - tail.len() - 1],
                scaled.parse::<u32>().unwrap_or(0),
            )
        }
        None => (time_text, 0),
    };
    let mut parts = time_text.split(':');
    let hour: u32 = parts.next()?.parse().ok()?;
    let minute: u32 = parts.next().unwrap_or("0").parse().ok()?;
    let second: u32 = parts.next().unwrap_or("0").parse().ok()?;
    Some((year, month, day, hour, minute, second, nanos))
}

/// Howard Hinnant's civil-days algorithm.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = month as i64;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day as i64 - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rfc3339_utc() {
        let value = parse_iso8601("2026-11-04T00:00:00Z").unwrap();
        assert_eq!(value.duration_since(UNIX_EPOCH).unwrap().as_secs(), 1_793_750_400);
    }

    #[test]
    fn parses_offset_timestamp() {
        let utc = parse_iso8601("2026-01-01T00:00:00Z").unwrap();
        let shifted = parse_iso8601("2026-01-01T08:00:00+08:00").unwrap();
        assert_eq!(
            utc.duration_since(UNIX_EPOCH).unwrap().as_secs(),
            shifted.duration_since(UNIX_EPOCH).unwrap().as_secs()
        );
    }

    #[test]
    fn parses_epoch_millis() {
        let value = parse_system_time(Some(&serde_json::json!(1_700_000_000_000u64))).unwrap();
        assert_eq!(value.duration_since(UNIX_EPOCH).unwrap().as_secs(), 1_700_000_000);
    }
}
