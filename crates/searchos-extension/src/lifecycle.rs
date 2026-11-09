//! Extension lifecycle & registry (RFC-011 11.3, 11.5).

use std::collections::HashMap;
use std::path::Path;
use std::sync::RwLock;
use std::time::Instant;

use crate::manifest::{ExtensionManifest, RuntimeKind};

/// Lifecycle states (RFC-011 11.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtensionState {
    Discovered,
    ManifestValidated,
    CompatibilityChecked,
    PermissionChecked,
    Initialized,
    HealthChecked,
    Registered,
    Active,
    Degraded,
    Quarantined,
    Disabled,
}

impl ExtensionState {
    /// Whether the extension is usable for new tasks.
    pub fn usable(&self) -> bool {
        matches!(self, Self::Active | Self::Degraded)
    }
}

/// Validation failure detail.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{stage}: {message}")]
pub struct ValidationError {
    pub stage: &'static str,
    pub message: String,
}

/// Result of one discovery pass. Per-manifest failures are isolated: one
/// bad manifest never prevents later manifests from loading (Part 24 §24.6,
/// INV-30-12).
#[derive(Debug, Clone, Default)]
pub struct DiscoveryReport {
    pub loaded: Vec<String>,
    pub quarantined: Vec<String>,
    pub errors: Vec<String>,
}

/// A tracked extension.
#[derive(Debug, Clone)]
pub struct ExtensionRecord {
    pub manifest: ExtensionManifest,
    pub state: ExtensionState,
    pub loaded_at: Option<Instant>,
    pub last_error: Option<String>,
}

/// Registry managing extension lifecycle.
#[derive(Debug, Default)]
pub struct ExtensionRegistry {
    records: RwLock<HashMap<String, ExtensionRecord>>,
}

impl ExtensionRegistry {
    /// Empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Full lifecycle promotion of a manifest: validation → compatibility →
    /// permission → initialize → health check → registered (RFC-011 11.3).
    pub fn install(&self, manifest: ExtensionManifest) -> Result<ExtensionState, ValidationError> {
        // Stage: Manifest Validation.
        if let Err(e) = manifest.validate() {
            return Err(ValidationError {
                stage: "manifest_validation",
                message: e,
            });
        }
        // Stage: Compatibility Check (requires_core vs current host).
        if let Some(req) = &manifest.requires_core {
            let host = env!("CARGO_PKG_VERSION");
            if version_lt(host, req) {
                return Err(ValidationError {
                    stage: "compatibility_check",
                    message: format!("requires core >= {req}, host is {host}"),
                });
            }
        }
        // Stage: Permission Check (least privilege; wasm wildcard denial).
        if manifest.runtime == RuntimeKind::Wasm
            && manifest.permissions.network.contains(&"*".to_string())
        {
            return Err(ValidationError {
                stage: "permission_check",
                message: "wasm wildcard network denied".into(),
            });
        }
        // Initialize + Health Check + Registered: recorded as Registered;
        // native extensions ship with the binary so they are immediately
        // usable (RFC-011 11.2).
        let state = if manifest.healthcheck.unwrap_or(false) {
            ExtensionState::Registered
        } else {
            ExtensionState::Active
        };
        let record = ExtensionRecord {
            manifest: manifest.clone(),
            state,
            loaded_at: Some(Instant::now()),
            last_error: None,
        };
        self.records
            .write()
            .unwrap()
            .insert(manifest.id.clone(), record);
        Ok(state)
    }

    /// Discovers manifest.yaml files with the permissive default runtime
    /// policy (used by tests and tools).
    pub fn discover(&self, dir: &Path) -> Result<DiscoveryReport, String> {
        self.discover_with_policy(dir, true, true)
    }

    /// Discovers manifests one-by-one, quarantining only the failing entry.
    /// `allow_native` / `wasm_enabled` are runtime config fields, not no-ops.
    pub fn discover_with_policy(
        &self,
        dir: &Path,
        allow_native: bool,
        wasm_enabled: bool,
    ) -> Result<DiscoveryReport, String> {
        let mut report = DiscoveryReport::default();
        if !dir.exists() {
            return Ok(report);
        }
        let mut entries: Vec<_> = std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        for e in entries {
            let p = e.path().join("manifest.yaml");
            if !p.exists() {
                continue;
            }
            let label = e.file_name().to_string_lossy().to_string();
            let yaml = match std::fs::read_to_string(&p) {
                Ok(y) => y,
                Err(err) => {
                    let message = format!("{label}: {err}");
                    report.errors.push(message.clone());
                    report.quarantined.push(label.clone());
                    self.quarantine_stub(&label, &message);
                    continue;
                }
            };
            let manifest = match ExtensionManifest::from_yaml(&yaml) {
                Ok(m) => m,
                Err(err) => {
                    let message = format!("{label}: {err}");
                    report.errors.push(message.clone());
                    report.quarantined.push(label.clone());
                    self.quarantine_stub(&label, &message);
                    continue;
                }
            };
            let policy_error = match manifest.runtime {
                RuntimeKind::Native if !allow_native => Some("native extensions disabled"),
                RuntimeKind::Wasm if !wasm_enabled => Some("wasm extensions disabled"),
                _ => None,
            };
            if let Some(reason) = policy_error {
                let message = format!("{}: {reason}", manifest.id);
                report.errors.push(message.clone());
                report.quarantined.push(manifest.id.clone());
                self.quarantine_manifest(manifest, &message);
                continue;
            }
            match self.install(manifest.clone()) {
                Ok(_) => report.loaded.push(manifest.id),
                Err(ve) => {
                    let message = format!("{}: {ve}", manifest.id);
                    report.errors.push(message.clone());
                    report.quarantined.push(manifest.id.clone());
                    self.quarantine_manifest(manifest, &message);
                }
            }
        }
        Ok(report)
    }

    fn quarantine_stub(&self, label: &str, error: &str) {
        let id = format!("ext.quarantined.{label}");
        let manifest = ExtensionManifest {
            id: id.clone(),
            name: label.to_string(),
            version: "0.0.0".into(),
            kind: crate::manifest::ExtensionType::Intelligence,
            runtime: RuntimeKind::Declarative,
            entry: Some("quarantined.yaml".into()),
            permissions: Default::default(),
            requires_core: None,
            healthcheck: Some(false),
        };
        self.insert_quarantined(manifest, error);
    }

    fn quarantine_manifest(&self, manifest: ExtensionManifest, error: &str) {
        self.insert_quarantined(manifest, error);
    }

    fn insert_quarantined(&self, manifest: ExtensionManifest, error: &str) {
        self.records.write().unwrap().insert(
            manifest.id.clone(),
            ExtensionRecord {
                manifest,
                state: ExtensionState::Quarantined,
                loaded_at: None,
                last_error: Some(error.to_string()),
            },
        );
    }

    /// Marks an extension active (post health check).
    pub fn activate(&self, id: &str) -> Result<(), String> {
        let mut map = self.records.write().unwrap();
        let r = map.get_mut(id).ok_or("unknown extension")?;
        r.state = ExtensionState::Active;
        Ok(())
    }

    /// Degrades (fails but survivable).
    pub fn degrade(&self, id: &str, reason: &str) {
        if let Some(r) = self.records.write().unwrap().get_mut(id) {
            r.state = ExtensionState::Degraded;
            r.last_error = Some(reason.into());
        }
    }

    /// Quarantines (repeated failures).
    pub fn quarantine(&self, id: &str, reason: &str) {
        if let Some(r) = self.records.write().unwrap().get_mut(id) {
            r.state = ExtensionState::Quarantined;
            r.last_error = Some(reason.into());
        }
    }

    /// Disables manually.
    pub fn disable(&self, id: &str, reason: &str) {
        if let Some(r) = self.records.write().unwrap().get_mut(id) {
            r.state = ExtensionState::Disabled;
            r.last_error = Some(reason.into());
        }
    }

    /// Looks up a record.
    pub fn get(&self, id: &str) -> Option<ExtensionRecord> {
        self.records.read().unwrap().get(id).cloned()
    }

    /// Quarantined records for `extension list --quarantined` / dashboard.
    pub fn quarantined(&self) -> Vec<ExtensionRecord> {
        self.list()
            .into_iter()
            .filter(|r| r.state == ExtensionState::Quarantined)
            .collect()
    }

    /// Lists all records sorted by id (for `extension list`).
    pub fn list(&self) -> Vec<ExtensionRecord> {
        let mut v: Vec<ExtensionRecord> = self.records.read().unwrap().values().cloned().collect();
        v.sort_by(|a, b| a.manifest.id.cmp(&b.manifest.id));
        v
    }

    /// 扩展不能直接修改核心 SQLite Schema / 绕过成本账本 (RFC-011 11.5):
    /// capability helpers enforce this at the API level.
    pub fn can_modify_core_schema(_id: &str) -> bool {
        false
    }

    pub fn can_bypass_cost_ledger(_id: &str) -> bool {
        false
    }
}

/// Naive semver less-than (x.y.z only).
fn version_lt(host: &str, required: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .map(|p| p.parse().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    let h = parse(host);
    let r = parse(required);
    for i in 0..3 {
        let hv = h.get(i).copied().unwrap_or(0);
        let rv = r.get(i).copied().unwrap_or(0);
        if hv != rv {
            return hv < rv;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(id: &str, version: &str, runtime: RuntimeKind) -> ExtensionManifest {
        ExtensionManifest {
            id: id.into(),
            name: "Test".into(),
            version: version.into(),
            kind: crate::manifest::ExtensionType::SearchProvider,
            runtime,
            entry: if runtime == RuntimeKind::Declarative {
                Some("rules.yaml".into())
            } else {
                None
            },
            permissions: Default::default(),
            requires_core: None,
            healthcheck: Some(false),
        }
    }

    #[test]
    fn lifecycle_registers_native() {
        let r = ExtensionRegistry::new();
        let st = r
            .install(manifest("ext.a", "1.0.0", RuntimeKind::Native))
            .unwrap();
        assert_eq!(st, ExtensionState::Active);
        assert!(r.get("ext.a").unwrap().state.usable());
        r.degrade("ext.a", "flaky");
        assert!(
            r.get("ext.a").unwrap().state.usable(),
            "degraded still usable"
        );
        r.quarantine("ext.a", "repeated");
        assert!(!r.get("ext.a").unwrap().state.usable());
        r.disable("ext.a", "manual");
        assert_eq!(r.get("ext.a").unwrap().state, ExtensionState::Disabled);
    }

    #[test]
    fn invalid_manifest_rejected() {
        let r = ExtensionRegistry::new();
        let err = r
            .install(manifest("bad", "1.0.0", RuntimeKind::Native))
            .unwrap_err();
        assert_eq!(err.stage, "manifest_validation");
        assert!(r.get("bad").is_none());
    }

    #[test]
    fn compatibility_gate_blocks_old_core_requirement() {
        let mut m = manifest("ext.old", "1.0.0", RuntimeKind::Native);
        m.requires_core = Some("99.0.0".into());
        let r = ExtensionRegistry::new();
        let err = r.install(m).unwrap_err();
        assert_eq!(err.stage, "compatibility_check");
    }

    #[test]
    fn core_schema_and_ledger_never_modifiable() {
        assert!(!ExtensionRegistry::can_modify_core_schema("ext.any"));
        assert!(!ExtensionRegistry::can_bypass_cost_ledger("ext.any"));
    }

    #[test]
    fn version_lt_logic() {
        assert!(version_lt("0.1.0", "1.0.0"));
        assert!(!version_lt("1.2.3", "1.2.2"));
        assert!(!version_lt("1.0.0", "1.0.0"));
    }

    /// Part 30 §30.11: one bad manifest quarantines only itself; later
    /// manifests still load and failures are observable.
    #[test]
    fn bad_manifest_quarantines_only_itself() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("bad");
        let good = dir.path().join("good");
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::create_dir_all(&good).unwrap();
        std::fs::write(bad.join("manifest.yaml"), "this: is: not: an extension").unwrap();
        std::fs::write(
            good.join("manifest.yaml"),
            r#"
id: ext.good
name: Good Extension
version: 1.0.0
type: search_provider
runtime: native
permissions:
  network: []
healthcheck: false
"#,
        )
        .unwrap();
        let registry = ExtensionRegistry::new();
        let report = registry.discover(dir.path()).unwrap();
        assert!(report.loaded.contains(&"ext.good".to_string()));
        assert!(report.quarantined.iter().any(|q| q.contains("bad")));
        assert!(!report.errors.is_empty());
        assert_eq!(
            registry.get("ext.good").unwrap().state,
            ExtensionState::Active
        );
        assert!(registry
            .quarantined()
            .iter()
            .any(|r| r.manifest.id.contains("bad")));
    }
}
