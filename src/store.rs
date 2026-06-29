//! Encrypted server store + login lockout state.
//!
//! Servers are serialized to JSON and encrypted with `age` (scrypt passphrase).
//! The master password is never stored; we only keep it in memory after unlock.

use std::io::{Read, Write};
use std::path::PathBuf;

use age::secrecy::SecretString;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct Server {
    pub name: String,
    pub host: String,
    pub user: String,
    #[serde(default)]
    pub group: String,
    #[serde(default)]
    pub password: String,
    /// Path to a private key file for `ssh -i`. Empty = no key file.
    #[serde(default)]
    pub keyfile: String,
}

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ssh-manager")
}

pub fn store_path() -> PathBuf {
    config_dir().join("servers.age")
}

fn lockout_path() -> PathBuf {
    config_dir().join("lockout.json")
}

pub fn store_exists() -> bool {
    store_path().exists()
}

/// Encrypt `servers` to disk under `password`.
pub fn save(servers: &[Server], password: &SecretString) -> std::io::Result<()> {
    let json = serde_json::to_vec(servers)?;
    let enc = encrypt(&json, password)?;
    let path = store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, enc)
}

/// Decrypt and load servers. Returns the typed decrypt error so callers can
/// distinguish "wrong password" from "file missing".
pub fn load(password: &SecretString) -> Result<Vec<Server>, LoadError> {
    let path = store_path();
    if !path.exists() {
        return Ok(Vec::new());
    }
    let enc = std::fs::read(path).map_err(LoadError::Io)?;
    let json = decrypt(&enc, password).map_err(|_| LoadError::WrongPassword)?;
    serde_json::from_slice(&json).map_err(|_| LoadError::Corrupt)
}

#[derive(Debug)]
pub enum LoadError {
    Io(std::io::Error),
    WrongPassword,
    Corrupt,
}

fn encrypt(plaintext: &[u8], password: &SecretString) -> std::io::Result<Vec<u8>> {
    let encryptor = age::Encryptor::with_user_passphrase(password.clone());
    let mut out = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut out)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    writer.write_all(plaintext)?;
    writer.finish()?;
    Ok(out)
}

fn decrypt(ciphertext: &[u8], password: &SecretString) -> std::io::Result<Vec<u8>> {
    let decryptor = age::Decryptor::new(ciphertext)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;
    let identity = age::scrypt::Identity::new(password.clone());
    let mut reader = decryptor
        .decrypt(std::iter::once(&identity as &dyn age::Identity))
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let mut out = Vec::new();
    reader.read_to_end(&mut out)?;
    Ok(out)
}

// ---- Lockout: 3 tries, then lock 1m, 2m, 4m, ... (doubling). Persisted so a
// relaunch can't reset the counter. ----

#[derive(Serialize, Deserialize, Default)]
struct Lockout {
    fails: u32,
    /// Unix seconds when the current lock ends. 0 = not locked.
    locked_until: u64,
    /// How many full lockout cycles have elapsed (drives the doubling).
    cycle: u32,
}

const TRIES_PER_CYCLE: u32 = 3;

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn read_lockout() -> Lockout {
    std::fs::read(lockout_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn write_lockout(l: &Lockout) {
    if let Ok(b) = serde_json::to_vec(l) {
        if let Some(parent) = lockout_path().parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::write(lockout_path(), b);
    }
}

/// Seconds remaining on an active lock, or 0 if entry is allowed.
pub fn lock_remaining() -> u64 {
    let l = read_lockout();
    l.locked_until.saturating_sub(now_secs())
}

/// Record one wrong-password attempt; engages a lock after TRIES_PER_CYCLE fails.
/// Returns seconds locked (0 if not yet locked).
pub fn record_failure() -> u64 {
    let mut l = read_lockout();
    l.fails += 1;
    if l.fails >= TRIES_PER_CYCLE {
        // 1m << cycle: 60, 120, 240, ...
        let mins = 1u64 << l.cycle;
        l.locked_until = now_secs() + mins * 60;
        l.fails = 0;
        l.cycle += 1;
        write_lockout(&l);
        return mins * 60;
    }
    write_lockout(&l);
    0
}

/// Clear lockout state after a successful unlock.
pub fn record_success() {
    write_lockout(&Lockout::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_encrypt_decrypt() {
        let pw = SecretString::from("hunter2".to_string());
        let servers = vec![Server {
            name: "box".into(),
            host: "1.2.3.4".into(),
            user: "root".into(),
            group: "prod".into(),
            password: "secret".into(),
            keyfile: "/home/me/.ssh/id_ed25519".into(),
        }];
        let enc = encrypt(&serde_json::to_vec(&servers).unwrap(), &pw).unwrap();
        let dec: Vec<Server> = serde_json::from_slice(&decrypt(&enc, &pw).unwrap()).unwrap();
        assert_eq!(servers, dec);
    }

    #[test]
    fn wrong_password_fails() {
        let pw = SecretString::from("right".to_string());
        let enc = encrypt(b"[]", &pw).unwrap();
        assert!(decrypt(&enc, &SecretString::from("wrong".to_string())).is_err());
    }

    #[test]
    fn lockout_doubles() {
        // pure arithmetic check of the doubling schedule
        for cycle in 0..5u32 {
            let mins = 1u64 << cycle;
            assert_eq!(mins, [1, 2, 4, 8, 16][cycle as usize]);
        }
    }
}
