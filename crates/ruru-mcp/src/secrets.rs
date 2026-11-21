//! The `0600` secrets file and the three cryptographic operations over it.
//!
//! Deliberately different mechanisms for the two HTTP boundaries, matching the epic:
//! the `/mcp` bearer key is a high-entropy token we only ever store hashed (SHA-256,
//! constant-time compare); the admin password is human-chosen and so gets a slow
//! password hash (argon2id). The cookie key encrypts the admin session cookie.
//!
//! Nothing here is ever logged, and the file is created `0600`.

use anyhow::{Context, Result, bail};
use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum_extra::extract::cookie::Key;
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::Path;
use subtle::ConstantTimeEq;

/// Length of the cookie encryption key. `cookie::Key` needs at least 64 bytes.
const COOKIE_KEY_LEN: usize = 64;
/// Bearer token entropy — 256 bits, as the epic specifies.
const BEARER_KEY_LEN: usize = 32;
/// Human-facing prefix so a leaked token is recognisable in logs/history and greppable.
const BEARER_PREFIX: &str = "ruru_";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Secrets {
    /// Base64 of 64 random bytes. Encrypts the admin session cookie.
    cookie_key: String,
    /// argon2id PHC string for the admin password. `None` until `set-password` is run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    admin_password_phc: Option<String>,
    /// Base64 of the SHA-256 of the bearer token. `None` until a key is minted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bearer_key_sha256: Option<String>,
    /// Admin session generation. Every session cookie carries the generation it was issued
    /// under; bumping this (on any password change) revokes all outstanding sessions.
    #[serde(default)]
    session_generation: u64,
}

impl Secrets {
    /// Load an existing secrets file. Errors if it is missing — callers that want to
    /// bootstrap use [`Secrets::load_or_init`].
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).with_context(|| {
            format!(
                "reading secrets {}. Run `ruru-mcp init` (or set-password / mint-key) first.",
                path.display()
            )
        })?;
        let secrets: Secrets =
            toml::from_str(&text).with_context(|| format!("parsing secrets {}", path.display()))?;
        secrets.validate()?;
        Ok(secrets)
    }

    /// Load the file, creating it with a fresh cookie key if it does not yet exist.
    pub fn load_or_init(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        let secrets = Self {
            cookie_key: STANDARD.encode(random_bytes::<COOKIE_KEY_LEN>()),
            admin_password_phc: None,
            bearer_key_sha256: None,
            session_generation: 0,
        };
        secrets.save(path)?;
        Ok(secrets)
    }

    fn validate(&self) -> Result<()> {
        let key = STANDARD
            .decode(&self.cookie_key)
            .context("cookie_key is not valid base64")?;
        if key.len() < COOKIE_KEY_LEN {
            bail!("cookie_key must decode to at least {COOKIE_KEY_LEN} bytes");
        }
        if let Some(phc) = &self.admin_password_phc {
            PasswordHash::new(phc)
                .map_err(|e| anyhow::anyhow!("admin_password_phc invalid: {e}"))?;
        }
        if let Some(b64) = &self.bearer_key_sha256 {
            let raw = STANDARD
                .decode(b64)
                .context("bearer_key_sha256 is not valid base64")?;
            if raw.len() != 32 {
                bail!("bearer_key_sha256 must decode to 32 bytes (a SHA-256 digest)");
            }
        }
        Ok(())
    }

    /// Write the file with `0600` permissions, creating the parent directory if needed.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating secrets directory {}", parent.display()))?;
        }
        let text = toml::to_string_pretty(self).context("serialising secrets")?;

        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .with_context(|| format!("opening secrets {} for write", path.display()))?;
        file.write_all(text.as_bytes())
            .with_context(|| format!("writing secrets {}", path.display()))?;
        // create() with mode only applies on creation; enforce on an existing file too.
        std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o600))
            .with_context(|| format!("setting 0600 on {}", path.display()))?;
        Ok(())
    }

    // --- cookie -----------------------------------------------------------

    pub fn cookie_key(&self) -> Result<Key> {
        let raw = STANDARD
            .decode(&self.cookie_key)
            .context("decoding cookie_key")?;
        Ok(Key::from(&raw))
    }

    // --- admin password (argon2id) ---------------------------------------

    pub fn has_password(&self) -> bool {
        self.admin_password_phc.is_some()
    }

    pub fn set_password(&mut self, password: &str) -> Result<()> {
        if password.is_empty() {
            bail!("password must not be empty");
        }
        let salt = SaltString::generate(&mut argon2::password_hash::rand_core::OsRng);
        let phc = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| anyhow::anyhow!("hashing password: {e}"))?
            .to_string();
        self.admin_password_phc = Some(phc);
        // A new password must not leave sessions minted under the old one alive.
        self.session_generation = self.session_generation.wrapping_add(1);
        Ok(())
    }

    /// The current admin session generation (see the field docs).
    pub fn session_generation(&self) -> u64 {
        self.session_generation
    }

    /// Constant-time-ish by construction: argon2 verification does not early-return on the
    /// hash comparison. Returns false when no password has been set.
    pub fn verify_password(&self, password: &str) -> bool {
        let Some(phc) = &self.admin_password_phc else {
            return false;
        };
        let Ok(parsed) = PasswordHash::new(phc) else {
            return false;
        };
        Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok()
    }

    // --- bearer key (SHA-256) --------------------------------------------

    pub fn has_bearer(&self) -> bool {
        self.bearer_key_sha256.is_some()
    }

    /// Mint a fresh bearer token, store only its SHA-256, and return the raw token — the
    /// one and only time it is ever available. Any previous key is replaced (rotate).
    pub fn mint_bearer(&mut self) -> String {
        let token = format!(
            "{BEARER_PREFIX}{}",
            URL_SAFE_NO_PAD.encode(random_bytes::<BEARER_KEY_LEN>())
        );
        let digest = Sha256::digest(token.as_bytes());
        self.bearer_key_sha256 = Some(STANDARD.encode(digest));
        token
    }

    /// Constant-time comparison of SHA-256(presented) against the stored digest. Returns
    /// false when no key has been minted.
    pub fn verify_bearer(&self, presented: &str) -> bool {
        let Some(b64) = &self.bearer_key_sha256 else {
            return false;
        };
        let Ok(stored) = STANDARD.decode(b64) else {
            return false;
        };
        let digest = Sha256::digest(presented.as_bytes());
        // Both are fixed 32-byte SHA-256 outputs; ct_eq keeps the comparison
        // length-independent regardless.
        digest.as_slice().ct_eq(stored.as_slice()).into()
    }
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut buf = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut buf);
    buf
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh() -> Secrets {
        Secrets {
            cookie_key: STANDARD.encode(random_bytes::<COOKIE_KEY_LEN>()),
            admin_password_phc: None,
            bearer_key_sha256: None,
            session_generation: 0,
        }
    }

    #[test]
    fn password_round_trip() {
        let mut s = fresh();
        assert!(!s.has_password());
        assert!(!s.verify_password("hunter2"));

        s.set_password("hunter2").unwrap();
        assert!(s.has_password());
        assert!(s.verify_password("hunter2"));
        assert!(!s.verify_password("hunter3"));
    }

    #[test]
    fn stored_password_is_a_hash_not_plaintext() {
        let mut s = fresh();
        s.set_password("super-secret-pw").unwrap();
        let phc = s.admin_password_phc.as_ref().unwrap();
        assert!(
            phc.starts_with("$argon2id$"),
            "should be an argon2id PHC string"
        );
        assert!(!phc.contains("super-secret-pw"));
    }

    #[test]
    fn setting_a_password_bumps_the_session_generation() {
        let mut s = fresh();
        let before = s.session_generation();
        s.set_password("first-password").unwrap();
        let after_first = s.session_generation();
        s.set_password("second-password").unwrap();
        assert!(after_first != before && s.session_generation() != after_first);
    }

    #[test]
    fn a_secrets_file_without_a_generation_still_loads() {
        // Files written before session generations existed must keep working.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-secrets.toml");
        let key = STANDARD.encode(random_bytes::<COOKIE_KEY_LEN>());
        std::fs::write(&path, format!("cookie_key = \"{key}\"\n")).unwrap();
        assert_eq!(Secrets::load(&path).unwrap().session_generation(), 0);
    }

    #[test]
    fn bearer_mint_and_verify() {
        let mut s = fresh();
        assert!(!s.has_bearer());

        let token = s.mint_bearer();
        assert!(token.starts_with("ruru_"));
        assert!(s.has_bearer());
        assert!(s.verify_bearer(&token));
        assert!(!s.verify_bearer("ruru_wrong"));
        assert!(!s.verify_bearer(""));

        // Only the hash is stored, never the token.
        assert!(!s.bearer_key_sha256.as_ref().unwrap().contains(&token[5..]));
    }

    #[test]
    fn rotating_invalidates_the_previous_token() {
        let mut s = fresh();
        let first = s.mint_bearer();
        let second = s.mint_bearer();
        assert_ne!(first, second);
        assert!(!s.verify_bearer(&first), "old token must stop working");
        assert!(s.verify_bearer(&second));
    }

    #[test]
    fn save_is_0600_and_round_trips() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("mcp-secrets.toml");

        let mut s = fresh();
        s.set_password("pw").unwrap();
        let token = s.mint_bearer();
        s.save(&path).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "secrets file must be 0600, got {mode:o}");

        let loaded = Secrets::load(&path).unwrap();
        assert!(loaded.verify_password("pw"));
        assert!(loaded.verify_bearer(&token));
        loaded.cookie_key().unwrap();
    }

    #[test]
    fn load_or_init_creates_then_reuses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mcp-secrets.toml");
        let a = Secrets::load_or_init(&path).unwrap();
        let b = Secrets::load_or_init(&path).unwrap();
        assert_eq!(
            a.cookie_key, b.cookie_key,
            "cookie key must be stable across loads"
        );
    }
}
