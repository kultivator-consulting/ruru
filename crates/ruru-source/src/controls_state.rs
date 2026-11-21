//! Runtime control overrides — the small writable file that makes operator tuning survive a
//! restart.
//!
//! Camera controls describe the *device*, so they belong to `ruru-source`, not `ruru-mcp`
//! (which only drives the change). But the daemon's own config under `/etc` is read-only to
//! it in the hardened deployment, so a persisted override can't be written back into
//! `[camera.controls]`. Instead each override is recorded here as a `name → value` pair in a
//! writable state file, and layered over the config's locked controls on every open.

use anyhow::{Context, Result};
use indexmap::IndexMap;
use std::path::Path;

/// Operator-set control values, in the order they were first set. Order is preserved (an
/// `IndexMap`, not a sorted map) so that override-only controls apply after the config's —
/// a gate set before the control it gates keeps working.
pub type Overrides = IndexMap<String, i64>;

/// Load overrides from disk. A missing file is normal (nothing tuned yet); a corrupt one is
/// logged and ignored rather than fatal — a bad override must never stop the camera coming up.
pub fn load(path: &Path) -> Overrides {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!(
                path = %path.display(), error = %e,
                "ignoring unreadable control overrides — starting from the config defaults"
            );
            Overrides::new()
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Overrides::new(),
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "could not read control overrides");
            Overrides::new()
        }
    }
}

/// Persist overrides atomically: write a sibling temp file then rename, so a crash mid-write
/// can't leave a half-written file that fails to parse on the next boot.
pub fn save(path: &Path, overrides: &Overrides) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(overrides).context("serializing control overrides")?;
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("renaming {} into place", tmp.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_is_empty_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let got = load(&dir.path().join("nope.json"));
        assert!(got.is_empty());
    }

    #[test]
    fn round_trips_and_preserves_insertion_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controls.json");
        let mut ov = Overrides::new();
        ov.insert("focus_automatic_continuous".into(), 0);
        ov.insert("focus_absolute".into(), 55);
        save(&path, &ov).unwrap();

        let got = load(&path);
        assert_eq!(got.get("focus_absolute"), Some(&55));
        let order: Vec<&str> = got.keys().map(String::as_str).collect();
        assert_eq!(order, ["focus_automatic_continuous", "focus_absolute"]);
    }

    #[test]
    fn corrupt_file_is_ignored_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("controls.json");
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load(&path).is_empty());
    }
}
