//! Integrates local application settings.
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Error, Result};

const KEYS: [&str; 5] = [
    "http.proxy",
    "http.proxyKerberosServicePrincipal",
    "http.proxySupport",
    "cursor.general.disableHttp2",
    "http.experimental.systemCertificatesV2",
];

fn read_at(path: &Path) -> Result<BTreeMap<String, Value>> {
    let Some(data) = read_raw(path)? else {
        return Ok(BTreeMap::new());
    };
    parse_settings(&data)
}

fn read_raw(path: &std::path::Path) -> Result<Option<String>> {
    match fs::read_to_string(path) {
        Ok(data) => Ok(Some(data)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn parse_settings(data: &str) -> Result<BTreeMap<String, Value>> {
    if data.trim().is_empty() {
        return Ok(BTreeMap::new());
    }
    json5::from_str(data)
        .map_err(|error| Error::Config(format!("parse Cursor settings JSONC: {error}")))
}

fn write_settings_at(path: &std::path::Path, settings: &BTreeMap<String, Value>) -> Result<()> {
    let mut data = serde_json::to_vec_pretty(settings)?;
    data.push(b'\n');
    write_at(path, &data)
}

fn write_at(path: &std::path::Path, data: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension("json.tmp");
    fs::write(&temp, data)?;
    fs::rename(temp, path)?;
    Ok(())
}

fn backup_path(settings_path: &std::path::Path) -> PathBuf {
    settings_path.with_file_name("cursor-agent-service-settings-backup.json")
}

#[derive(Serialize, Deserialize)]
struct SettingsBackup {
    original: BTreeMap<String, Option<Value>>,
    managed: BTreeMap<String, Value>,
    #[serde(default)]
    original_file: Option<String>,
    #[serde(default)]
    original_file_present: Option<bool>,
}

fn managed_values(proxy_url: &str) -> BTreeMap<String, Value> {
    [
        (KEYS[0], Value::String(proxy_url.into())),
        (KEYS[1], Value::String(proxy_url.into())),
        (KEYS[2], Value::String("on".into())),
        (KEYS[3], Value::Bool(true)),
        (KEYS[4], Value::Bool(true)),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), v))
    .collect()
}

fn settings_conflict(current: &BTreeMap<String, Value>, backup: &SettingsBackup) -> bool {
    let managed_changed = KEYS.iter().any(|key| {
        backup
            .managed
            .get(*key)
            .is_some_and(|value| current.get(*key) != Some(value))
    });
    if managed_changed {
        return true;
    }
    let Some(original) = backup
        .original_file
        .as_deref()
        .and_then(|source| parse_settings(source).ok())
    else {
        return false;
    };
    let keys = current
        .keys()
        .chain(original.keys())
        .collect::<std::collections::HashSet<_>>();
    keys.into_iter()
        .any(|key| !KEYS.contains(&key.as_str()) && current.get(key) != original.get(key))
}

fn no_proxy_bypasses_cursor(value: Option<&Value>) -> bool {
    fn contains_cursor_host(value: &Value) -> bool {
        match value {
            Value::String(value) => value.split([',', ';', ' ', '\n', '\t']).any(|entry| {
                let entry = entry
                    .trim()
                    .trim_start_matches('.')
                    .trim_start_matches('*')
                    .to_ascii_lowercase();
                entry == "cursor.sh" || entry.ends_with(".cursor.sh")
            }),
            Value::Array(values) => values.iter().any(contains_cursor_host),
            _ => false,
        }
    }
    value.is_some_and(contains_cursor_host)
}

pub fn cursor_proxy_bypassed_by_no_proxy(settings_path: &Path) -> Result<bool> {
    let settings = read_at(settings_path)?;
    Ok(no_proxy_bypasses_cursor(settings.get("http.noProxy")))
}

pub fn write_proxy_settings(settings_path: &Path, proxy_url: &str) -> Result<()> {
    write_proxy_settings_at(settings_path, proxy_url)
}

fn write_proxy_settings_at(settings_path: &std::path::Path, proxy_url: &str) -> Result<()> {
    let current_file = read_raw(settings_path)?;
    let mut settings = current_file
        .as_deref()
        .map(parse_settings)
        .transpose()?
        .unwrap_or_default();
    let backup_path = backup_path(settings_path);
    let (original, original_file, original_file_present) = if backup_path.exists() {
        let backup: SettingsBackup = serde_json::from_slice(&fs::read(&backup_path)?)?;
        (
            backup.original,
            backup.original_file,
            backup.original_file_present,
        )
    } else {
        (
            KEYS.iter()
                .map(|key| ((*key).to_owned(), settings.get(*key).cloned()))
                .collect::<BTreeMap<_, _>>(),
            current_file.clone(),
            Some(current_file.is_some()),
        )
    };
    let managed = managed_values(proxy_url);
    if let Some(parent) = backup_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup = serde_json::to_vec_pretty(&SettingsBackup {
        original,
        managed: managed.clone(),
        original_file,
        original_file_present,
    })?;
    write_at(&backup_path, &backup)?;
    for (key, value) in managed {
        settings.insert(key, value);
    }
    write_settings_at(settings_path, &settings)
}

pub fn clear_proxy_settings(settings_path: &Path) -> Result<()> {
    let backup_path = backup_path(settings_path);
    clear_proxy_settings_at(settings_path, &backup_path)
}

fn clear_proxy_settings_at(
    settings_path: &std::path::Path,
    backup_path: &std::path::Path,
) -> Result<()> {
    if backup_path.exists() {
        let backup: SettingsBackup = serde_json::from_slice(&fs::read(backup_path)?)?;
        let current_file = read_raw(settings_path)?;
        let mut settings = current_file
            .as_deref()
            .map(parse_settings)
            .transpose()?
            .unwrap_or_default();
        let has_conflict = settings_conflict(&settings, &backup);
        if !has_conflict {
            match backup.original_file_present {
                Some(true) if backup.original_file.is_some() => {
                    write_at(
                        settings_path,
                        backup.original_file.as_deref().unwrap().as_bytes(),
                    )?;
                    fs::remove_file(backup_path)?;
                    return Ok(());
                }
                Some(false) => {
                    if settings_path.exists() {
                        fs::remove_file(settings_path)?;
                    }
                    fs::remove_file(backup_path)?;
                    return Ok(());
                }
                Some(true) => {}
                None => {}
            }
        }
        // Restore only settings that still contain our exact managed value. User edits win.
        for key in KEYS {
            if backup
                .managed
                .get(key)
                .is_some_and(|value| settings.get(key) == Some(value))
            {
                match backup.original.get(key).cloned().flatten() {
                    Some(value) => {
                        settings.insert(key.into(), value);
                    }
                    None => {
                        settings.remove(key);
                    }
                }
            }
        }
        write_settings_at(settings_path, &settings)?;
        if has_conflict {
            tracing::warn!("Cursor settings changed since setup; retained the original backup");
        } else {
            fs::remove_file(backup_path)?;
        }
        return Ok(());
    } else {
        let mut settings = read_raw(settings_path)?
            .as_deref()
            .map(parse_settings)
            .transpose()?
            .unwrap_or_default();
        // Cleanup for settings created by older versions that had no backup.
        for key in KEYS {
            let matches_managed = match key {
                "http.proxy" | "http.proxyKerberosServicePrincipal" => {
                    settings.get(key).and_then(Value::as_str).is_some_and(|v| {
                        v.starts_with("http://127.0.0.1:") || v.starts_with("http://[::1]:")
                    })
                }
                "http.proxySupport" => settings.get(key) == Some(&Value::String("on".into())),
                _ => settings.get(key) == Some(&Value::Bool(true)),
            };
            if matches_managed {
                settings.remove(key);
            }
        }
        write_settings_at(settings_path, &settings)?;
    }
    Ok(())
}

pub fn settings_match(settings_path: &Path, proxy_url: &str) -> Result<bool> {
    let settings = read_at(settings_path)?;
    Ok(
        settings.get(KEYS[0]) == Some(&Value::String(proxy_url.into()))
            && settings.get(KEYS[1]) == Some(&Value::String(proxy_url.into()))
            && settings.get(KEYS[2]) == Some(&Value::String("on".into()))
            && settings.get(KEYS[3]) == Some(&Value::Bool(true))
            && settings.get(KEYS[4]) == Some(&Value::Bool(true)),
    )
}

pub fn clear_stale_managed_settings(settings_path: &Path) -> Result<()> {
    let settings = read_at(settings_path)?;
    let managed_signature = settings.get(KEYS[2]) == Some(&Value::String("on".into()))
        && settings.get(KEYS[3]) == Some(&Value::Bool(true))
        && settings.get(KEYS[4]) == Some(&Value::Bool(true));
    let loopback = settings
        .get(KEYS[0])
        .and_then(Value::as_str)
        .and_then(|value| value.parse::<reqwest::Url>().ok())
        .and_then(|url| url.host_str().map(str::to_owned))
        .is_some_and(|host| matches!(host.as_str(), "127.0.0.1" | "localhost" | "::1"));
    if managed_signature && loopback {
        clear_proxy_settings(settings_path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_proxy_detection_only_flags_cursor_hosts() {
        assert!(no_proxy_bypasses_cursor(Some(&serde_json::json!(
            "localhost,*.cursor.sh"
        ))));
        assert!(no_proxy_bypasses_cursor(Some(&serde_json::json!([
            "api2.cursor.sh"
        ]))));
        assert!(!no_proxy_bypasses_cursor(Some(&serde_json::json!(
            "localhost,127.0.0.1"
        ))));
        assert!(!no_proxy_bypasses_cursor(None));
    }

    #[test]
    fn restore_recovers_the_exact_jsonc_file_and_no_proxy_value() {
        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("settings.json");
        let backup_path = backup_path(&settings_path);
        let original = "{\n  // keep this comment\n  \"http.noProxy\": [\"localhost\"],\n  \"other.setting\": true\n}\n";
        fs::write(&settings_path, original).unwrap();

        write_proxy_settings_at(&settings_path, "http://127.0.0.1:3001").unwrap();
        assert!(backup_path.exists());
        clear_proxy_settings_at(&settings_path, &backup_path).unwrap();

        assert_eq!(fs::read_to_string(&settings_path).unwrap(), original);
        assert!(!backup_path.exists());
    }

    #[test]
    fn isolated_profile_supports_repeat_setup_and_exact_restore() {
        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("Cursor/User/settings.json");
        let original = "{\n  // isolated profile\n  \"http.noProxy\": [\"localhost\"]\n}\n";
        fs::create_dir_all(settings_path.parent().unwrap()).unwrap();
        fs::write(&settings_path, original).unwrap();

        write_proxy_settings(&settings_path, "http://127.0.0.1:3001").unwrap();
        write_proxy_settings(&settings_path, "http://127.0.0.1:3002").unwrap();

        assert!(settings_match(&settings_path, "http://127.0.0.1:3002").unwrap());
        assert!(!cursor_proxy_bypassed_by_no_proxy(&settings_path).unwrap());
        clear_proxy_settings(&settings_path).unwrap();

        assert_eq!(fs::read_to_string(settings_path).unwrap(), original);
    }

    #[test]
    fn restore_preserves_user_changes_and_retains_the_original_backup() {
        let directory = tempfile::tempdir().unwrap();
        let settings_path = directory.path().join("settings.json");
        let backup_path = backup_path(&settings_path);
        fs::write(&settings_path, r#"{"other.setting": true}"#).unwrap();

        write_proxy_settings_at(&settings_path, "http://127.0.0.1:3001").unwrap();
        let mut settings = parse_settings(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        settings.insert(
            KEYS[0].into(),
            Value::String("http://127.0.0.1:9000".into()),
        );
        settings.insert("other.setting".into(), Value::Bool(false));
        write_settings_at(&settings_path, &settings).unwrap();

        clear_proxy_settings_at(&settings_path, &backup_path).unwrap();

        let restored = parse_settings(&fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(restored[KEYS[0]], "http://127.0.0.1:9000");
        assert_eq!(restored["other.setting"], false);
        assert!(!restored.contains_key(KEYS[1]));
        assert!(backup_path.exists());
    }
}
