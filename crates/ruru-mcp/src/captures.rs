//! Retained captures.
//!
//! Each user-facing snapshot (`capture_whiteboard` / `capture_region`) is written to disk
//! under an unguessable 128-bit id and served back at `<public_base_url>/captures/<id>.jpg`,
//! so an agent can hand the user a link to the exact frame it read.
//!
//! **The id is the access control.** The serving route is unauthenticated — a link a user
//! clicks in chat has to "just work" in a browser, where a bearer header or session cookie
//! isn't available. That's an acceptable trade here: captures are LAN-only, low-sensitivity
//! whiteboard images, the id is 128 bits of unguessable randomness, and everything is pruned
//! by age + count. This is a deliberate fourth surface, distinct from the three trust
//! boundaries (`/mcp` bearer, admin cookie, source socket).

use crate::AppState;
use crate::config::CapturesConfig;
use anyhow::{Context, Result};
use axum::extract::{Path, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use rand::RngCore;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tracing::warn;

/// Ids are 32 lowercase hex chars (16 random bytes). Fixed-length + hex-only is what makes
/// the serving path trivially safe: nothing that isn't exactly this shape maps to a file.
const ID_LEN: usize = 32;

pub struct CaptureStore {
    dir: PathBuf,
    base_url: String,
    retain: Duration,
    max_count: usize,
}

impl CaptureStore {
    /// Build a store iff retention is configured (both a `dir` and a `public_base_url`);
    /// otherwise `None` (retention off, responses carry no link). Creates the directory.
    pub fn from_config(cfg: &CapturesConfig) -> Result<Option<Self>> {
        let (Some(dir), Some(base)) = (cfg.dir.clone(), cfg.public_base_url.clone()) else {
            return Ok(None);
        };
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("creating captures dir {}", dir.display()))?;
        Ok(Some(Self {
            dir,
            base_url: base.trim_end_matches('/').to_string(),
            retain: Duration::from_secs(cfg.retain_days.saturating_mul(24 * 60 * 60)),
            max_count: cfg.retain_count,
        }))
    }

    /// Write a JPEG, prune, and return its view URL.
    pub fn store(&self, jpeg: &[u8]) -> Result<String> {
        let id = new_id();
        let path = self.dir.join(format!("{id}.jpg"));
        // Write a temp then rename, so a partial write is never served under the real name.
        let tmp = self.dir.join(format!(".{id}.tmp"));
        std::fs::write(&tmp, jpeg).with_context(|| format!("writing {}", tmp.display()))?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("renaming into {}", path.display()))?;
        self.prune();
        Ok(format!("{}/captures/{id}.jpg", self.base_url))
    }

    /// Read a stored capture by id, validating the id so nothing but a 32-hex string can ever
    /// name a path — no `..`, no separators, no absolute paths.
    pub fn read(&self, id: &str) -> Option<Vec<u8>> {
        if !is_valid_id(id) {
            return None;
        }
        std::fs::read(self.dir.join(format!("{id}.jpg"))).ok()
    }

    /// Drop captures past the retention window, then any beyond the count cap (oldest first).
    /// Best-effort: a prune failure is logged, never fatal — a full disk shouldn't 500 a read.
    fn prune(&self) {
        let mut entries: Vec<(PathBuf, SystemTime)> = match std::fs::read_dir(&self.dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|x| x == "jpg"))
                .filter_map(|e| {
                    let mtime = e.metadata().ok()?.modified().ok()?;
                    Some((e.path(), mtime))
                })
                .collect(),
            Err(e) => {
                warn!(error = %e, "could not read captures dir to prune");
                return;
            }
        };

        let now = SystemTime::now();
        entries.retain(|(path, mtime)| {
            let too_old = now
                .duration_since(*mtime)
                .map(|age| age > self.retain)
                .unwrap_or(false);
            if too_old {
                let _ = std::fs::remove_file(path);
            }
            !too_old
        });

        if entries.len() > self.max_count {
            entries.sort_by_key(|(_, mtime)| *mtime); // oldest first
            for (path, _) in entries.iter().take(entries.len() - self.max_count) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

fn new_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let mut s = String::with_capacity(ID_LEN);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(s, "{b:02x}");
    }
    s
}

fn is_valid_id(id: &str) -> bool {
    id.len() == ID_LEN && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Public, unauthenticated: serve a retained capture by id. The route pattern is
/// `/captures/{file}` where `file` is `<id>.jpg`; a bad, unknown, or wrongly-shaped id is a
/// plain 404. `immutable` caching — the bytes at an id never change.
pub async fn serve(State(state): State<AppState>, Path(file): Path<String>) -> Response {
    let Some(store) = state.captures.as_ref() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let id = file.strip_suffix(".jpg").unwrap_or(&file);
    match store.read(id) {
        Some(bytes) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/jpeg"),
                (header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &std::path::Path, retain_days: u64, retain_count: usize) -> CaptureStore {
        CaptureStore::from_config(&CapturesConfig {
            dir: Some(dir.to_path_buf()),
            public_base_url: Some("http://ruru.test".into()),
            retain_days,
            retain_count,
        })
        .unwrap()
        .unwrap()
    }

    #[test]
    fn off_without_dir_or_base_url() {
        let mut cfg = CapturesConfig::default();
        assert!(CaptureStore::from_config(&cfg).unwrap().is_none());
        cfg.dir = Some("/tmp/whatever".into());
        assert!(
            CaptureStore::from_config(&cfg).unwrap().is_none(),
            "dir alone is not enough"
        );
    }

    #[test]
    fn store_writes_and_read_round_trips_via_the_link_id() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), 7, 200);
        let url = s.store(b"\xFF\xD8jpeg-bytes\xFF\xD9").unwrap();
        assert!(url.starts_with("http://ruru.test/captures/"));
        assert!(url.ends_with(".jpg"));
        let id = url
            .rsplit('/')
            .next()
            .unwrap()
            .strip_suffix(".jpg")
            .unwrap();
        assert_eq!(s.read(id).unwrap(), b"\xFF\xD8jpeg-bytes\xFF\xD9");
    }

    #[test]
    fn rejects_traversal_and_malformed_ids() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), 7, 200);
        // Plant a file outside that a traversal id would reach.
        std::fs::write(dir.path().join("secret.jpg"), b"nope").unwrap();
        assert!(s.read("../secret").is_none());
        assert!(s.read("secret").is_none()); // wrong length/shape
        assert!(s.read("../../etc/passwd").is_none());
        assert!(s.read(&"g".repeat(32)).is_none()); // 32 chars but not hex
    }

    #[test]
    fn prune_enforces_the_count_cap() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path(), 7, 3);
        let mut urls = Vec::new();
        for _ in 0..6 {
            urls.push(s.store(b"x").unwrap());
            // mtime resolution can tie; nudge so "oldest first" is deterministic.
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let live = std::fs::read_dir(dir.path())
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .path()
                    .extension()
                    .is_some_and(|x| x == "jpg")
            })
            .count();
        assert_eq!(live, 3, "only the 3 newest survive the count cap");
        // The three most recent links still resolve; the oldest don't.
        let id = |u: &str| {
            u.rsplit('/')
                .next()
                .unwrap()
                .strip_suffix(".jpg")
                .unwrap()
                .to_string()
        };
        assert!(s.read(&id(&urls[5])).is_some());
        assert!(s.read(&id(&urls[0])).is_none());
    }
}
