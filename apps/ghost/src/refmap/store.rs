//! Snapshot store (sealed files, memory without a key).

use std::sync::atomic::Ordering;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{Snapshot, MAX_SNAPSHOTS, SNAPSHOT_SEQ};

/// How long a snapshot stays usable. A snapshot only has to outlive the few tool
/// calls that act on its refs; after that it is stale screen content.
pub(super) const SNAPSHOT_TTL_MS: i64 = 10 * 60 * 1000;

/// Content store name of sealed snapshots (KDF input: never rename).
const SNAPSHOT_STORE: &str = "ghost-snapshots";
/// AAD purpose of one sealed snapshot; the snapshot id is bound in as well.
const SNAPSHOT_PURPOSE: &str = "ghost-snapshot";
/// File extension of a sealed snapshot (`<id>.snap`).
const SNAPSHOT_EXT: &str = "snap";
/// A temp file older than this is a crashed write and is removed.
const STALE_TEMP_MS: u128 = 60 * 1000;

#[derive(Default)]
pub(super) struct MemoryState {
    /// Oldest first; ids are chronological (`s<millis>-<seq>`).
    pub(super) snapshots: Vec<(i64, Snapshot)>,
    latest: Option<String>,
}

/// Process-wide fallback state, used only when Ghost has no sealing key.
static MEMORY: std::sync::OnceLock<std::sync::Mutex<MemoryState>> = std::sync::OnceLock::new();

/// What a sealed snapshot file holds once opened.
#[derive(Serialize, Deserialize)]
pub(super) struct StoredSnapshot {
    pub(super) saved_ms: i64,
    pub(super) snapshot: Snapshot,
}

pub(super) enum Backend {
    /// One `RYU-CB01` file per snapshot under Ghost's component subkey, bound to
    /// the snapshot id. Shared by every Ghost process of this data dir, so a
    /// snapshot taken by one per-call `ghost mcp` resolves refs in the next.
    Sealed {
        dir: std::path::PathBuf,
        cipher: ryu_crypto::ContentCipher,
    },
    /// No key: the snapshot lives only in this process and is never written.
    Memory(&'static std::sync::Mutex<MemoryState>),
}

/// Ghost's sealing cipher, resolved once per process.
static PROCESS_CIPHER: std::sync::OnceLock<Result<ryu_crypto::ComponentCipher, String>> =
    std::sync::OnceLock::new();

fn process_cipher() -> &'static Result<ryu_crypto::ComponentCipher, String> {
    PROCESS_CIPHER.get_or_init(|| {
        ryu_crypto::component_cipher_for_process(ryu_crypto::components::GHOST)
            .map_err(|error| format!("{error:#}"))
    })
}

/// Resolve Ghost's sealing key now. `main` calls this after taking a delivered
/// `RYU_SEAL_KEY` and before it scrubs `RYU_MASTER_KEY` from the environment, so a
/// Ghost started outside Core with only the master key still seals its snapshots.
pub fn resolve_process_cipher() {
    let _ = process_cipher();
}

/// Snapshot store. Snapshots hold the accessibility tree of the focused window
/// (visible text and field values). With Ghost's sealing key (delivered by Core,
/// or derived from `RYU_MASTER_KEY`) each snapshot is sealed to
/// `services/ghost/snapshots/<id>.snap` (0700 dir, 0600 files); without one it is
/// kept in this process's memory only and never written. Either way at most
/// [`MAX_SNAPSHOTS`] are kept, for at most [`SNAPSHOT_TTL_MS`], pruned on every
/// use. Plaintext snapshot files left by earlier builds are deleted at startup.
pub struct SnapshotStore {
    pub(super) backend: Backend,
}

impl SnapshotStore {
    pub fn open() -> Result<Self> {
        static LEGACY_SWEEP: std::sync::Once = std::sync::Once::new();
        let dir = ghost_core::paths::snapshots_dir();
        LEGACY_SWEEP.call_once(|| remove_legacy_snapshot_files(&dir));
        let cipher = match process_cipher() {
            Ok(cipher) => Some(cipher.content_cipher(SNAPSHOT_STORE)),
            Err(error) => {
                static WARNED: std::sync::Once = std::sync::Once::new();
                WARNED.call_once(|| {
                    tracing::warn!(
                        "ghost: no sealing key ({error}); snapshots stay in this process only"
                    );
                });
                None
            }
        };
        Ok(Self::with_backend(cipher, dir))
    }

    pub(super) fn with_backend(
        cipher: Option<ryu_crypto::ContentCipher>,
        dir: std::path::PathBuf,
    ) -> Self {
        let backend = match cipher {
            Some(cipher) => Backend::Sealed { dir, cipher },
            None => Backend::Memory(MEMORY.get_or_init(Default::default)),
        };
        Self { backend }
    }

    #[cfg(test)]
    pub(super) fn isolated() -> Self {
        Self {
            backend: Backend::Memory(Box::leak(Box::default())),
        }
    }

    #[cfg(test)]
    pub(super) fn sealed_at(dir: &std::path::Path, root: &[u8; 32]) -> Self {
        Self {
            backend: Backend::Sealed {
                dir: dir.to_path_buf(),
                cipher: ryu_crypto::ContentCipher::from_root(root, SNAPSHOT_STORE),
            },
        }
    }

    /// Keep a snapshot, make it the latest, and drop expired or excess ones.
    pub fn save(&self, snapshot: &Snapshot) -> Result<()> {
        self.save_at(snapshot, now_ms())
    }

    pub(super) fn save_at(&self, snapshot: &Snapshot, now: i64) -> Result<()> {
        if !is_valid_snapshot_id(&snapshot.id) {
            anyhow::bail!("invalid snapshot id")
        }
        match &self.backend {
            Backend::Memory(state) => {
                let mut state = lock(state);
                state.snapshots.retain(|(_, s)| s.id != snapshot.id);
                state.snapshots.push((now, snapshot.clone()));
                state.latest = Some(snapshot.id.clone());
                prune_memory(&mut state, now);
                Ok(())
            }
            Backend::Sealed { dir, cipher } => {
                ensure_private_dir(dir)?;
                let payload = serde_json::to_vec(&StoredSnapshot {
                    saved_ms: now,
                    snapshot: snapshot.clone(),
                })?;
                let sealed = cipher
                    .seal_bytes(&payload, &snapshot_context(&snapshot.id))
                    .context("sealing the snapshot")?;
                write_private_atomically(dir, &snapshot.id, &sealed)?;
                prune_sealed(dir, cipher, now);
                Ok(())
            }
        }
    }

    /// Load a snapshot by id.
    pub fn load(&self, id: &str) -> Result<Snapshot> {
        self.load_at(id, now_ms())
    }

    pub(super) fn load_at(&self, id: &str, now: i64) -> Result<Snapshot> {
        if !is_valid_snapshot_id(id) {
            anyhow::bail!("invalid snapshot id")
        }
        let found = match &self.backend {
            Backend::Memory(state) => {
                let mut state = lock(state);
                prune_memory(&mut state, now);
                state
                    .snapshots
                    .iter()
                    .find(|(_, s)| s.id == id)
                    .map(|(_, s)| s.clone())
            }
            Backend::Sealed { dir, cipher } => prune_sealed(dir, cipher, now)
                .into_iter()
                .find(|(_, s)| s.id == id)
                .map(|(_, s)| s),
        };
        found.with_context(|| {
            format!("Snapshot '{id}' not found or expired; run ghost_snapshot again")
        })
    }

    /// Load the most recently saved snapshot.
    pub fn load_latest(&self) -> Result<Snapshot> {
        self.load_latest_at(now_ms())
    }

    pub(super) fn load_latest_at(&self, now: i64) -> Result<Snapshot> {
        let latest = match &self.backend {
            Backend::Memory(state) => {
                let mut state = lock(state);
                prune_memory(&mut state, now);
                let id = state.latest.clone();
                id.and_then(|id| {
                    state
                        .snapshots
                        .iter()
                        .find(|(_, s)| s.id == id)
                        .map(|(_, s)| s.clone())
                })
            }
            Backend::Sealed { dir, cipher } => prune_sealed(dir, cipher, now).pop().map(|(_, s)| s),
        };
        latest.context("No snapshot yet — run ghost_snapshot first")
    }

    /// Delete expired, unreadable and excess snapshots without loading one, so
    /// the ten-minute retention holds even when no snapshot tool is called.
    pub fn prune_expired(&self) {
        self.prune_expired_at(now_ms());
    }

    pub(super) fn prune_expired_at(&self, now: i64) {
        match &self.backend {
            Backend::Memory(state) => prune_memory(&mut lock(state), now),
            Backend::Sealed { dir, cipher } => {
                prune_sealed(dir, cipher, now);
            }
        }
    }
}

/// How often the long-lived `ghost mcp` server deletes expired snapshots.
pub const SNAPSHOT_PRUNE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);

/// Delete expired snapshot files now (startup and the periodic sweep).
pub fn prune_expired_snapshots() {
    match SnapshotStore::open() {
        Ok(store) => store.prune_expired(),
        Err(error) => tracing::warn!("ghost: could not open the snapshot store: {error:#}"),
    }
}

pub(super) fn lock(
    state: &std::sync::Mutex<MemoryState>,
) -> std::sync::MutexGuard<'_, MemoryState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub(super) fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn snapshot_context(id: &str) -> ryu_crypto::ContentContext {
    ryu_crypto::ContentContext::for_id(SNAPSHOT_PURPOSE, id)
}

/// Whether a snapshot saved at `saved` is still usable at `now`.
fn is_fresh(saved: i64, now: i64) -> bool {
    now.saturating_sub(saved) <= SNAPSHOT_TTL_MS
}

/// Drop snapshots older than the TTL, then the oldest beyond the cap.
fn prune_memory(state: &mut MemoryState, now: i64) {
    state.snapshots.retain(|(saved, _)| is_fresh(*saved, now));
    if state.snapshots.len() > MAX_SNAPSHOTS {
        let excess = state.snapshots.len() - MAX_SNAPSHOTS;
        state.snapshots.drain(..excess);
    }
    if let Some(latest) = &state.latest {
        if !state.snapshots.iter().any(|(_, s)| &s.id == latest) {
            state.latest = None;
        }
    }
}

/// Open every sealed snapshot in `dir`, delete the expired, unreadable (wrong
/// key, tampered, plaintext) and excess ones plus crashed temp files, and return
/// the rest oldest first.
fn prune_sealed(
    dir: &std::path::Path,
    cipher: &ryu_crypto::ContentCipher,
    now: i64,
) -> Vec<(i64, Snapshot)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut kept: Vec<(i64, Snapshot, std::path::PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') && name.contains(".tmp-") {
            let stale = entry
                .metadata()
                .and_then(|meta| meta.modified())
                .ok()
                .and_then(|modified| modified.elapsed().ok())
                .is_none_or(|age| age.as_millis() > STALE_TEMP_MS);
            if stale {
                remove_quietly(&path);
            }
            continue;
        }
        let Some(id) = name
            .strip_suffix(SNAPSHOT_EXT)
            .and_then(|stem| stem.strip_suffix('.'))
            .filter(|id| is_valid_snapshot_id(id))
        else {
            continue;
        };
        match open_sealed_snapshot(&path, id, cipher) {
            Some(stored) if is_fresh(stored.saved_ms, now) => {
                kept.push((stored.saved_ms, stored.snapshot, path));
            }
            _ => remove_quietly(&path),
        }
    }
    kept.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.id.cmp(&b.1.id)));
    if kept.len() > MAX_SNAPSHOTS {
        let excess = kept.len() - MAX_SNAPSHOTS;
        for (_, _, path) in kept.drain(..excess) {
            remove_quietly(&path);
        }
    }
    kept.into_iter()
        .map(|(saved, snapshot, _)| (saved, snapshot))
        .collect()
}

/// Open one sealed snapshot file; `None` unless it is sealed, opens under this
/// key for this id, and parses.
fn open_sealed_snapshot(
    path: &std::path::Path,
    id: &str,
    cipher: &ryu_crypto::ContentCipher,
) -> Option<StoredSnapshot> {
    let bytes = std::fs::read(path).ok()?;
    if !ryu_crypto::ContentCipher::is_sealed_bytes(&bytes) {
        return None;
    }
    let opened = cipher.open_bytes(&bytes, &snapshot_context(id)).ok()?;
    let stored: StoredSnapshot = serde_json::from_slice(&opened.value).ok()?;
    (stored.snapshot.id == id).then_some(stored)
}

fn remove_quietly(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(path) {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!("ghost: could not delete a snapshot file: {error}");
        }
    }
}

/// Create `dir` (0700 on Unix) if needed and tighten an existing one.
fn ensure_private_dir(dir: &std::path::Path) -> Result<()> {
    std::fs::create_dir_all(dir).context("creating the snapshot directory")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .context("restricting the snapshot directory")?;
    }
    Ok(())
}

/// Write `<dir>/<id>.snap` through a 0600 temp file and a rename, so a reader
/// never sees a partial file.
fn write_private_atomically(dir: &std::path::Path, id: &str, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let seq = SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed);
    let temp = dir.join(format!(".{id}.tmp-{}-{seq}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| -> Result<()> {
        let mut file = options
            .open(&temp)
            .context("creating a snapshot temp file")?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, dir.join(format!("{id}.{SNAPSHOT_EXT}")))
            .context("publishing the sealed snapshot")?;
        Ok(())
    })();
    if result.is_err() {
        remove_quietly(&temp);
    }
    result
}

/// Startup cleanup: delete plaintext snapshot files earlier builds left in
/// Ghost's data dir, whether or not a snapshot tool is ever called.
pub fn remove_legacy_snapshots_at_startup() {
    remove_legacy_snapshots_in(&ghost_core::paths::snapshots_dir());
}

pub(super) fn remove_legacy_snapshots_in(dir: &std::path::Path) {
    remove_legacy_snapshot_files(dir);
}

/// Delete plaintext snapshot files an earlier build persisted (`<id>.json` and
/// the `latest` pointer), then the directory when it is empty.
pub(super) fn remove_legacy_snapshot_files(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let legacy =
            name == "latest" || name.strip_suffix(".json").is_some_and(is_valid_snapshot_id);
        if legacy {
            if let Err(error) = std::fs::remove_file(&path) {
                tracing::warn!("ghost: could not delete an old snapshot file: {error}");
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

pub(crate) fn is_valid_snapshot_id(id: &str) -> bool {
    let Some(body) = id.strip_prefix('s') else {
        return false;
    };
    let Some((timestamp, sequence)) = body.split_once('-') else {
        return false;
    };
    !timestamp.is_empty()
        && !sequence.is_empty()
        && timestamp.bytes().all(|byte| byte.is_ascii_digit())
        && sequence.bytes().all(|byte| byte.is_ascii_digit())
}
