//! Snapshot schema + version handling.
//!
//! Honors architectural commitment #5 — `schema_version` lives in every
//! snapshot file; the reader rejects unknown versions with a clear error rather
//! than crashing.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::detection::DeviceType;

/// Current snapshot schema version. Bump when the structure changes; the
/// reader will refuse future versions explicitly.
pub const SCHEMA_VERSION: u32 = 3;

/// Setting keys we track in a snapshot — matches v1's `$Script:SnapshotSettingKeys`.
pub fn tracked_setting_keys() -> &'static [(&'static str, &'static str)] {
    &[
        ("global", "window_animation_scale"),
        ("global", "transition_animation_scale"),
        ("global", "animator_duration_scale"),
        ("global", "hdmi_control_enabled"),
        ("global", "hdmi_control_auto_wakeup_enabled"),
        ("global", "hdmi_control_auto_device_off_enabled"),
        ("global", "hdmi_system_audio_control_enabled"),
        ("secure", "match_content_frame_rate"),
        ("secure", "long_press_timeout"),
        ("global", "encoded_surround_output"),
        ("global", "encoded_surround_output_enabled_formats"),
    ]
}

/// On-disk snapshot file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub schema_version: u32,
    /// ISO-8601 UTC timestamp.
    pub saved_at: String,
    /// Optional user-given name for the snapshot (e.g. "before debloat").
    /// Added in schema v2 — `serde(default)` loads v1 snapshots with `None`.
    #[serde(default)]
    pub label: Option<String>,
    pub device_name: String,
    pub device_serial: String,
    pub device_type: DeviceType,
    pub android_version: String,
    pub disabled_packages: Vec<String>,
    pub current_launcher: Option<String>,
    /// Key format: `"<namespace>.<key>"` (e.g. `"global.window_animation_scale"`).
    /// Values are the raw strings the device returned.
    pub settings: BTreeMap<String, String>,
    #[serde(default)]
    pub absent_settings: Vec<String>,
}

/// Errors that arise from snapshot parsing / application.
#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("snapshot schema version {found} is newer than this build supports (max {supported})")]
    UnsupportedSchema { found: u32, supported: u32 },
    #[error("snapshot is missing required field: {0}")]
    MissingField(&'static str),
    #[error("snapshot JSON is malformed: {0}")]
    Malformed(String),
}

impl Snapshot {
    /// Parse a snapshot from JSON. Rejects unknown future versions and
    /// nonsense low values (0, anything larger than u32). Uses a single
    /// `serde_json::Value` parse + `from_value` rather than parsing the
    /// string twice.
    pub fn from_json(json: &str) -> Result<Self, SnapshotError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| SnapshotError::Malformed(e.to_string()))?;
        let schema = value
            .get("schema_version")
            .and_then(|v| v.as_u64())
            .ok_or(SnapshotError::MissingField("schema_version"))?;
        if schema == 0 || schema > u64::from(SCHEMA_VERSION) {
            return Err(SnapshotError::UnsupportedSchema {
                found: u32::try_from(schema).unwrap_or(u32::MAX),
                supported: SCHEMA_VERSION,
            });
        }
        let mut snapshot: Self =
            serde_json::from_value(value).map_err(|e| SnapshotError::Malformed(e.to_string()))?;
        if schema < 3 {
            // Older captures omitted both absent and unreadable settings. Neither
            // omission authorizes deleting a value on the target device.
            snapshot.absent_settings.clear();
            snapshot.schema_version = SCHEMA_VERSION;
        }
        if snapshot
            .absent_settings
            .iter()
            .any(|key| snapshot.settings.contains_key(key))
        {
            return Err(SnapshotError::Malformed(
                "setting cannot be both present and absent".into(),
            ));
        }
        Ok(snapshot)
    }

    pub fn to_json(&self) -> Result<String, SnapshotError> {
        serde_json::to_string_pretty(self).map_err(|e| SnapshotError::Malformed(e.to_string()))
    }
}

/// What the engine plans to do when applying a snapshot — computed before any
/// ADB calls are made. The host layer executes these against the ADB driver.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotApplyPlan {
    /// Packages to be `pm disable-user`d (currently installed and enabled).
    pub packages_to_disable: Vec<String>,
    /// Packages already in the disabled state — no-op, but counted.
    pub packages_already_disabled: Vec<String>,
    /// Packages on the snapshot list but not present on the target device.
    pub packages_not_installed: Vec<String>,
    /// The launcher to set as default (None = no launcher change).
    pub launcher_to_set: Option<String>,
    /// Settings whose current device value differs from the snapshot —
    /// these will be written. Same key format as `Snapshot::settings`.
    pub settings_to_write: BTreeMap<String, String>,
    pub settings_to_delete: Vec<String>,
    /// Settings already at the snapshot's value on the device — no-op, counted
    /// so the preview doesn't overstate the work.
    pub settings_already_set: Vec<String>,
    /// Set when the snapshot's device type doesn't match the target's.
    pub cross_device_warning: Option<String>,
}

/// Inputs the engine needs to compute an apply plan, expressed as facts
/// about the device's *current* state (no I/O — caller fetches and passes in).
#[derive(Debug, Clone)]
pub struct ApplyPlanInputs<'a> {
    pub target_device_type: DeviceType,
    pub currently_disabled: &'a [String],
    pub currently_installed: &'a [String],
    /// Successfully read current tracked values. Missing keys are absent;
    /// callers must not substitute an empty map for a failed read.
    pub current_settings: &'a BTreeMap<String, String>,
}

/// Compute the plan for applying `snap` to a device in `inputs`' state.
/// Per commitment #2: this function is pure; the host layer executes the plan.
pub fn compute_apply_plan(snap: &Snapshot, inputs: &ApplyPlanInputs<'_>) -> SnapshotApplyPlan {
    let disabled_set: std::collections::HashSet<&str> = inputs
        .currently_disabled
        .iter()
        .map(String::as_str)
        .collect();
    let installed_set: std::collections::HashSet<&str> = inputs
        .currently_installed
        .iter()
        .map(String::as_str)
        .collect();

    let mut to_disable = Vec::new();
    let mut already_disabled = Vec::new();
    let mut not_installed = Vec::new();

    for pkg in &snap.disabled_packages {
        let s = pkg.as_str();
        if !installed_set.contains(s) {
            not_installed.push(pkg.clone());
        } else if disabled_set.contains(s) {
            already_disabled.push(pkg.clone());
        } else {
            to_disable.push(pkg.clone());
        }
    }

    let cross_device_warning = if snap.device_type != inputs.target_device_type {
        Some(format!(
            "Snapshot was taken from a {} device; current device is detected as {}.",
            snap.device_type.label(),
            inputs.target_device_type.label()
        ))
    } else {
        None
    };

    // Only write settings whose current value differs from the snapshot.
    let mut settings_to_write = BTreeMap::new();
    let mut settings_already_set = Vec::new();
    for (key, value) in &snap.settings {
        if inputs.current_settings.get(key) == Some(value) {
            settings_already_set.push(key.clone());
        } else {
            settings_to_write.insert(key.clone(), value.clone());
        }
    }

    let mut settings_to_delete = Vec::new();
    for key in &snap.absent_settings {
        if inputs.current_settings.contains_key(key) {
            settings_to_delete.push(key.clone());
        } else {
            settings_already_set.push(key.clone());
        }
    }

    SnapshotApplyPlan {
        packages_to_disable: to_disable,
        packages_already_disabled: already_disabled,
        packages_not_installed: not_installed,
        launcher_to_set: snap.current_launcher.clone(),
        settings_to_write,
        settings_to_delete,
        settings_already_set,
        cross_device_warning,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn sample_snapshot() -> Snapshot {
        let mut settings = BTreeMap::new();
        settings.insert(
            "global.window_animation_scale".to_string(),
            "0.5".to_string(),
        );
        Snapshot {
            schema_version: SCHEMA_VERSION,
            saved_at: "2026-05-27T12:00:00Z".to_string(),
            label: None,
            device_name: "Living Room TV".to_string(),
            device_serial: "192.168.42.71:5555".to_string(),
            device_type: DeviceType::Shield,
            android_version: "11".to_string(),
            disabled_packages: vec!["com.foo".into(), "com.bar".into(), "com.missing".into()],
            current_launcher: Some("com.spocky.projengmenu".to_string()),
            settings,
            absent_settings: Vec::new(),
        }
    }

    #[test]
    fn roundtrip_json() {
        let snap = sample_snapshot();
        let json = snap.to_json().unwrap();
        let parsed = Snapshot::from_json(&json).unwrap();
        assert_eq!(parsed.device_name, snap.device_name);
        assert_eq!(parsed.disabled_packages, snap.disabled_packages);
    }

    #[test]
    fn v1_snapshot_loads_with_no_label() {
        // A pre-label (schema v1) file must still parse — label defaults to None.
        let payload = r#"{
            "schema_version": 1,
            "saved_at": "2026-05-27T12:00:00Z",
            "device_name": "Old Shield",
            "device_serial": "x",
            "device_type": "shield",
            "android_version": "11",
            "disabled_packages": [],
            "current_launcher": null,
            "settings": {}
        }"#;
        let snap = Snapshot::from_json(payload).unwrap();
        assert_eq!(snap.label, None);
        assert_eq!(snap.device_name, "Old Shield");
    }

    #[test]
    fn label_roundtrips() {
        let mut snap = sample_snapshot();
        snap.label = Some("before debloat".to_string());
        let parsed = Snapshot::from_json(&snap.to_json().unwrap()).unwrap();
        assert_eq!(parsed.label.as_deref(), Some("before debloat"));
    }

    #[test]
    fn absent_and_empty_settings_roundtrip_and_plan_separately() {
        let mut snap = sample_snapshot();
        let empty_key = "global.encoded_surround_output_enabled_formats".to_string();
        let absent_key = "global.encoded_surround_output".to_string();
        snap.settings.insert(empty_key.clone(), String::new());
        snap.absent_settings.push(absent_key.clone());
        let parsed = Snapshot::from_json(&snap.to_json().unwrap()).unwrap();
        assert_eq!(parsed.settings.get(&empty_key), Some(&String::new()));
        assert_eq!(parsed.absent_settings, std::slice::from_ref(&absent_key));
        let current = BTreeMap::from([
            (absent_key.clone(), "3".into()),
            ("global.unrelated".into(), "keep".into()),
        ]);
        let plan = compute_apply_plan(
            &parsed,
            &ApplyPlanInputs {
                target_device_type: DeviceType::Shield,
                currently_disabled: &[],
                currently_installed: &[],
                current_settings: &current,
            },
        );
        assert_eq!(plan.settings_to_delete, [absent_key]);
        assert_eq!(plan.settings_to_write.get(&empty_key), Some(&String::new()));
        assert!(!plan.settings_to_write.contains_key("global.unrelated"));
        let absent = BTreeMap::new();
        let unchanged = compute_apply_plan(
            &parsed,
            &ApplyPlanInputs {
                target_device_type: DeviceType::Shield,
                currently_disabled: &[],
                currently_installed: &[],
                current_settings: &absent,
            },
        );
        assert!(unchanged.settings_to_delete.is_empty());
        assert!(unchanged
            .settings_already_set
            .contains(&"global.encoded_surround_output".into()));
    }

    #[test]
    fn legacy_omissions_never_become_deletions() {
        for version in [1, 2] {
            let mut snap = sample_snapshot();
            snap.schema_version = version;
            let mut json = serde_json::to_value(snap).unwrap();
            json.as_object_mut().unwrap().remove("absent_settings");
            let migrated = Snapshot::from_json(&json.to_string()).unwrap();
            assert_eq!(migrated.schema_version, SCHEMA_VERSION);
            assert!(migrated.absent_settings.is_empty());
            let current = BTreeMap::from([("global.encoded_surround_output".into(), "3".into())]);
            let plan = compute_apply_plan(
                &migrated,
                &ApplyPlanInputs {
                    target_device_type: DeviceType::Shield,
                    currently_disabled: &[],
                    currently_installed: &[],
                    current_settings: &current,
                },
            );
            assert!(plan.settings_to_delete.is_empty());
        }
    }

    #[test]
    fn rejects_conflicting_present_and_absent_values() {
        let mut snap = sample_snapshot();
        snap.absent_settings
            .push("global.window_animation_scale".into());
        assert!(Snapshot::from_json(&snap.to_json().unwrap()).is_err());
    }

    #[test]
    fn rejects_zero_schema_version() {
        let payload = r#"{
            "schema_version": 0,
            "saved_at": "2026-05-27T12:00:00Z",
            "device_name": "x",
            "device_serial": "x",
            "device_type": "shield",
            "android_version": "11",
            "disabled_packages": [],
            "current_launcher": null,
            "settings": {}
        }"#;
        let err = Snapshot::from_json(payload).unwrap_err();
        assert!(matches!(
            err,
            SnapshotError::UnsupportedSchema {
                found: 0,
                supported: 3
            }
        ));
    }

    #[test]
    fn rejects_future_schema() {
        let payload = r#"{
            "schema_version": 999,
            "saved_at": "2026-05-27T12:00:00Z",
            "device_name": "x",
            "device_serial": "x",
            "device_type": "shield",
            "android_version": "11",
            "disabled_packages": [],
            "current_launcher": null,
            "settings": {}
        }"#;
        let err = Snapshot::from_json(payload).unwrap_err();
        match err {
            SnapshotError::UnsupportedSchema {
                found: 999,
                supported: 3,
            } => {}
            other => panic!("wrong error: {other:?}"),
        }
    }

    #[test]
    fn rejects_missing_schema_version() {
        let payload = r#"{
            "saved_at": "2026-05-27T12:00:00Z",
            "device_name": "x",
            "disabled_packages": []
        }"#;
        let err = Snapshot::from_json(payload).unwrap_err();
        assert!(matches!(err, SnapshotError::MissingField("schema_version")));
    }

    #[test]
    fn rejects_malformed_json() {
        let err = Snapshot::from_json("not json").unwrap_err();
        assert!(matches!(err, SnapshotError::Malformed(_)));
    }

    #[test]
    fn apply_plan_categorizes_packages() {
        let snap = sample_snapshot();
        let installed = vec!["com.foo".into(), "com.bar".into()];
        let disabled = vec!["com.bar".into()];
        let no_settings = BTreeMap::new();
        let inputs = ApplyPlanInputs {
            target_device_type: DeviceType::Shield,
            currently_disabled: &disabled,
            currently_installed: &installed,
            current_settings: &no_settings,
        };
        let plan = compute_apply_plan(&snap, &inputs);
        assert_eq!(plan.packages_to_disable, vec!["com.foo"]);
        assert_eq!(plan.packages_already_disabled, vec!["com.bar"]);
        assert_eq!(plan.packages_not_installed, vec!["com.missing"]);
        assert!(plan.cross_device_warning.is_none());
        // No current settings known → everything is a write.
        assert_eq!(plan.settings_to_write.len(), 1);
        assert!(plan.settings_already_set.is_empty());
    }

    #[test]
    fn apply_plan_skips_settings_already_at_target() {
        let snap = sample_snapshot(); // has global.window_animation_scale = "0.5"
        let installed: Vec<String> = vec![];
        let disabled: Vec<String> = vec![];
        let mk = |val: &str| {
            let mut m = BTreeMap::new();
            m.insert("global.window_animation_scale".to_string(), val.to_string());
            m
        };

        let matched = mk("0.5");
        let plan = compute_apply_plan(
            &snap,
            &ApplyPlanInputs {
                target_device_type: DeviceType::Shield,
                currently_disabled: &disabled,
                currently_installed: &installed,
                current_settings: &matched,
            },
        );
        assert!(plan.settings_to_write.is_empty());
        assert_eq!(
            plan.settings_already_set,
            vec!["global.window_animation_scale"]
        );

        // A different current value → it IS written.
        let differs = mk("1.0");
        let plan = compute_apply_plan(
            &snap,
            &ApplyPlanInputs {
                target_device_type: DeviceType::Shield,
                currently_disabled: &disabled,
                currently_installed: &installed,
                current_settings: &differs,
            },
        );
        assert_eq!(plan.settings_to_write.len(), 1);
        assert!(plan.settings_already_set.is_empty());
    }

    #[test]
    fn apply_plan_warns_on_cross_device() {
        let snap = sample_snapshot();
        let installed: Vec<String> = vec![];
        let disabled: Vec<String> = vec![];
        let no_settings = BTreeMap::new();
        let inputs = ApplyPlanInputs {
            target_device_type: DeviceType::GoogleTv,
            currently_disabled: &disabled,
            currently_installed: &installed,
            current_settings: &no_settings,
        };
        let plan = compute_apply_plan(&snap, &inputs);
        assert!(plan.cross_device_warning.is_some());
        assert!(plan.cross_device_warning.unwrap().contains("Nvidia Shield"));
    }
}
