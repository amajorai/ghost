// Snapshot + element-ref store.
//
// Ported in spirit from lahfir/agent-desktop's snapshot/ref system: a `ghost_snapshot`
// captures the focused accessibility tree once, assigns stable `@eN` references to its
// interactive/named nodes in depth-first order, and keeps the whole walked tree sealed
// under Ghost's component key (the tree is on-screen text and field values; see
// `SnapshotStore`), so the next per-call `ghost mcp` process can resolve its refs. Subsequent action tools (`ghost_click`/`ghost_type`)
// then act on `@eN` instead of re-describing the element, which cuts token usage on dense
// apps and makes targeting deterministic.
//
// Re-identification is optimistic: when an action resolves a ref, ghost re-captures the
// focused tree and finds the node matching (role, name), preferring an exact `Bounds`
// match. If the focused app's pid no longer matches the snapshot, or no candidate is
// found, the action fails with a `STALE_REF` error telling the agent to re-snapshot.

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use ghost_eyes::{AXTreeNode, Bounds};

/// How many snapshots to keep.
const MAX_SNAPSHOTS: usize = 10;

/// Monotonic per-process counter so two snapshots in the same millisecond get distinct ids.
static SNAPSHOT_SEQ: AtomicU64 = AtomicU64::new(0);

/// A node in a captured skeleton tree. Mirrors `AXTreeNode` but carries an optional
/// `@eN` ref and keeps every walked child (the skeleton *view* prunes by depth at
/// render time; the stored tree is whole so drill-down can re-render deeper).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkeletonNode {
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub ref_id: Option<String>,
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identifier: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bounds: Option<Bounds>,
    pub children: Vec<SkeletonNode>,
}

/// A captured snapshot: the tree plus the app/pid it was taken from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub pid: i32,
    pub created_ms: i64,
    pub tree: SkeletonNode,
}

impl Snapshot {
    /// Build a snapshot from a freshly captured AX tree, assigning `@eN` refs DFS.
    pub fn build(root: &AXTreeNode, app: Option<String>, pid: i32, now_ms: i64) -> Self {
        // The process id keeps ids unique across the per-call Ghost processes
        // that share the sealed snapshot directory.
        let seq = SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed);
        let id = format!("s{now_ms}-{}{seq:06}", std::process::id());
        let mut counter: u32 = 0;
        let tree = build_node(root, &mut counter);
        Self {
            id,
            app,
            pid,
            created_ms: now_ms,
            tree,
        }
    }

    /// The total number of refs assigned in this snapshot.
    pub fn ref_count(&self) -> usize {
        count_refs(&self.tree)
    }

    /// Find a node by its `@eN` ref.
    pub fn find_ref(&self, ref_id: &str) -> Option<&SkeletonNode> {
        find_ref_in(&self.tree, ref_id)
    }
}

/// A node deserves a ref if it is directly actionable, or if it is a named container
/// with children (a useful `--root` drill-down target).
fn deserves_ref(node: &AXTreeNode) -> bool {
    if is_actionable(node) {
        return true;
    }
    node.title.as_deref().is_some_and(|t| !t.is_empty()) && !node.children.is_empty()
}

fn is_actionable(node: &AXTreeNode) -> bool {
    let r = node.role.to_lowercase();
    r.contains("button")
        || r.contains("link")
        || r.contains("textfield")
        || r.contains("text field")
        || r.contains("checkbox")
        || r.contains("combo")
        || r.contains("slider")
        || r.contains("tab")
        || r.contains("menu")
        || r.contains("cell")
        || r.contains("row")
        || r.contains("edit")
}

fn build_node(node: &AXTreeNode, counter: &mut u32) -> SkeletonNode {
    // Pre-order: a parent gets its ref before its children, so refs read in DFS order.
    let ref_id = if deserves_ref(node) {
        *counter += 1;
        Some(format!("@e{counter}"))
    } else {
        None
    };
    let children = node
        .children
        .iter()
        .map(|c| build_node(c, counter))
        .collect();
    SkeletonNode {
        ref_id,
        role: node.role.clone(),
        name: node.title.clone(),
        value: node.value.clone(),
        identifier: node.identifier.clone(),
        bounds: node.bounds.clone(),
        children,
    }
}

fn count_refs(node: &SkeletonNode) -> usize {
    let mut n = usize::from(node.ref_id.is_some());
    for c in &node.children {
        n += count_refs(c);
    }
    n
}

fn find_ref_in<'a>(node: &'a SkeletonNode, ref_id: &str) -> Option<&'a SkeletonNode> {
    if node.ref_id.as_deref() == Some(ref_id) {
        return Some(node);
    }
    for c in &node.children {
        if let Some(found) = find_ref_in(c, ref_id) {
            return Some(found);
        }
    }
    None
}

/// Render a skeleton node to compact JSON, pruning at `max_depth` but always reporting
/// `children_count` so the agent knows where it can drill down with `root`.
pub fn render(node: &SkeletonNode, max_depth: u32) -> Value {
    render_inner(node, 0, max_depth)
}

fn render_inner(node: &SkeletonNode, depth: u32, max_depth: u32) -> Value {
    let mut obj = serde_json::Map::new();
    if let Some(r) = &node.ref_id {
        obj.insert("ref".into(), json!(r));
    }
    obj.insert("role".into(), json!(node.role));
    if let Some(n) = &node.name {
        obj.insert("name".into(), json!(n));
    }
    if let Some(v) = &node.value {
        obj.insert("value".into(), json!(v));
    }
    if let Some(b) = &node.bounds {
        obj.insert(
            "bounds".into(),
            json!({ "x": b.x, "y": b.y, "w": b.width, "h": b.height }),
        );
    }
    if !node.children.is_empty() {
        obj.insert("children_count".into(), json!(node.children.len()));
        if depth < max_depth {
            let kids: Vec<Value> = node
                .children
                .iter()
                .map(|c| render_inner(c, depth + 1, max_depth))
                .collect();
            obj.insert("children".into(), json!(kids));
        }
    }
    Value::Object(obj)
}

/// Outcome of resolving a ref against the currently focused tree.
pub enum Resolved {
    /// The element was re-identified; click at this screen-space center.
    At { x: i32, y: i32 },
    /// The element could not be re-identified — the agent should re-snapshot.
    Stale(String),
}

/// Re-identify a stored ref entry in a freshly captured tree, returning its current
/// center. `entry` is the `SkeletonNode` the ref pointed at in the snapshot.
pub fn reidentify(current_root: &AXTreeNode, entry: &SkeletonNode) -> Resolved {
    let role = entry.role.to_lowercase();
    let mut candidates: Vec<&AXTreeNode> = vec![];
    collect_matches(current_root, &role, &entry.name, &mut candidates);

    if candidates.is_empty() {
        return Resolved::Stale(format!(
            "STALE_REF: {} ({}{}) is no longer present in the focused window; re-run ghost_snapshot",
            entry.ref_id.as_deref().unwrap_or("element"),
            entry.role,
            entry.name.as_deref().map(|n| format!(" '{n}'")).unwrap_or_default(),
        ));
    }

    // Prefer the candidate whose bounds are unchanged; otherwise take the first match.
    let chosen = candidates
        .iter()
        .find(|c| c.bounds.is_some() && c.bounds == entry.bounds)
        .copied()
        .unwrap_or(candidates[0]);

    match &chosen.bounds {
        Some(b) => Resolved::At {
            x: b.x + b.width as i32 / 2,
            y: b.y + b.height as i32 / 2,
        },
        None => Resolved::Stale(format!(
            "STALE_REF: {} matched but has no bounds to click; re-run ghost_snapshot",
            entry.ref_id.as_deref().unwrap_or("element"),
        )),
    }
}

fn collect_matches<'a>(
    node: &'a AXTreeNode,
    role: &str,
    name: &Option<String>,
    out: &mut Vec<&'a AXTreeNode>,
) {
    let role_ok = node.role.to_lowercase() == role;
    let name_ok = match name {
        Some(n) => node.title.as_deref() == Some(n.as_str()),
        None => node.title.is_none(),
    };
    if role_ok && name_ok {
        out.push(node);
    }
    for c in &node.children {
        collect_matches(c, role, name, out);
    }
}

// ─── Snapshot store (sealed files, memory without a key) ───────────────────────

/// How long a snapshot stays usable. A snapshot only has to outlive the few tool
/// calls that act on its refs; after that it is stale screen content.
const SNAPSHOT_TTL_MS: i64 = 10 * 60 * 1000;

/// Content store name of sealed snapshots (KDF input: never rename).
const SNAPSHOT_STORE: &str = "ghost-snapshots";
/// AAD purpose of one sealed snapshot; the snapshot id is bound in as well.
const SNAPSHOT_PURPOSE: &str = "ghost-snapshot";
/// File extension of a sealed snapshot (`<id>.snap`).
const SNAPSHOT_EXT: &str = "snap";
/// A temp file older than this is a crashed write and is removed.
const STALE_TEMP_MS: u128 = 60 * 1000;

#[derive(Default)]
struct MemoryState {
    /// Oldest first; ids are chronological (`s<millis>-<seq>`).
    snapshots: Vec<(i64, Snapshot)>,
    latest: Option<String>,
}

/// Process-wide fallback state, used only when Ghost has no sealing key.
static MEMORY: std::sync::OnceLock<std::sync::Mutex<MemoryState>> = std::sync::OnceLock::new();

/// What a sealed snapshot file holds once opened.
#[derive(Serialize, Deserialize)]
struct StoredSnapshot {
    saved_ms: i64,
    snapshot: Snapshot,
}

enum Backend {
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
    backend: Backend,
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

    fn with_backend(cipher: Option<ryu_crypto::ContentCipher>, dir: std::path::PathBuf) -> Self {
        let backend = match cipher {
            Some(cipher) => Backend::Sealed { dir, cipher },
            None => Backend::Memory(MEMORY.get_or_init(Default::default)),
        };
        Self { backend }
    }

    #[cfg(test)]
    fn isolated() -> Self {
        Self {
            backend: Backend::Memory(Box::leak(Box::default())),
        }
    }

    #[cfg(test)]
    fn sealed_at(dir: &std::path::Path, root: &[u8; 32]) -> Self {
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

    fn save_at(&self, snapshot: &Snapshot, now: i64) -> Result<()> {
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

    fn load_at(&self, id: &str, now: i64) -> Result<Snapshot> {
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

    fn load_latest_at(&self, now: i64) -> Result<Snapshot> {
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

    fn prune_expired_at(&self, now: i64) {
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

fn lock(state: &std::sync::Mutex<MemoryState>) -> std::sync::MutexGuard<'_, MemoryState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn now_ms() -> i64 {
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

fn remove_legacy_snapshots_in(dir: &std::path::Path) {
    remove_legacy_snapshot_files(dir);
}

/// Delete plaintext snapshot files an earlier build persisted (`<id>.json` and
/// the `latest` pointer), then the directory when it is empty.
fn remove_legacy_snapshot_files(dir: &std::path::Path) {
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

fn is_valid_snapshot_id(id: &str) -> bool {
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

#[cfg(test)]
mod tests {
    use super::*;
    use ghost_eyes::{AXTreeNode, Bounds};
    use serde_json::json;

    fn node(
        role: &str,
        title: Option<&str>,
        bounds: Option<Bounds>,
        children: Vec<AXTreeNode>,
    ) -> AXTreeNode {
        AXTreeNode {
            role: role.into(),
            title: title.map(|s| s.into()),
            value: None,
            identifier: None,
            bounds,
            children,
            enabled: true,
            focused: false,
            hidden: false,
        }
    }

    fn bounds(x: i32, y: i32, w: u32, h: u32) -> Bounds {
        Bounds {
            x,
            y,
            width: w,
            height: h,
        }
    }

    fn sample() -> AXTreeNode {
        node(
            "window",
            Some("Main"),
            Some(bounds(0, 0, 800, 600)),
            vec![
                node(
                    "group",
                    None,
                    None,
                    vec![
                        node("button", Some("Send"), Some(bounds(10, 20, 80, 30)), vec![]),
                        node(
                            "button",
                            Some("Cancel"),
                            Some(bounds(100, 20, 80, 30)),
                            vec![],
                        ),
                    ],
                ),
                node(
                    "textfield",
                    Some("Search"),
                    Some(bounds(10, 100, 200, 24)),
                    vec![],
                ),
            ],
        )
    }

    #[test]
    fn refs_assigned_in_dfs_preorder() {
        let snap = Snapshot::build(&sample(), Some("App".into()), 42, 1000);
        // window(named+children)=@e1, button Send=@e2, button Cancel=@e3, textfield=@e4.
        // The unnamed "group" gets no ref.
        assert_eq!(snap.tree.ref_id.as_deref(), Some("@e1"));
        assert_eq!(snap.ref_count(), 4);
        let send = snap.find_ref("@e2").expect("@e2");
        assert_eq!(send.name.as_deref(), Some("Send"));
        let field = snap.find_ref("@e4").expect("@e4");
        assert_eq!(field.role, "textfield");
    }

    #[test]
    fn render_prunes_depth_but_reports_children_count() {
        let snap = Snapshot::build(&sample(), None, 1, 1000);
        let shallow = render(&snap.tree, 0);
        assert_eq!(shallow["children_count"], json!(2));
        assert!(
            shallow.get("children").is_none(),
            "depth 0 must not emit children"
        );
        let deep = render(&snap.tree, 3);
        assert!(deep["children"].is_array());
    }

    #[test]
    fn reidentify_prefers_exact_bounds_then_falls_back() {
        let snap = Snapshot::build(&sample(), None, 1, 1000);
        let send = snap.find_ref("@e2").unwrap();

        // Unchanged tree → exact bounds match → center of (10,20,80,30) = (50,35).
        match reidentify(&sample(), send) {
            Resolved::At { x, y } => {
                assert_eq!((x, y), (50, 35));
            }
            Resolved::Stale(e) => panic!("unexpected stale: {e}"),
        }

        // Moved button (same role+name, different bounds) → still re-identified at new center.
        let moved = node(
            "window",
            Some("Main"),
            None,
            vec![node(
                "button",
                Some("Send"),
                Some(bounds(200, 200, 80, 30)),
                vec![],
            )],
        );
        match reidentify(&moved, send) {
            Resolved::At { x, y } => assert_eq!((x, y), (240, 215)),
            Resolved::Stale(e) => panic!("unexpected stale: {e}"),
        }
    }

    #[test]
    fn reidentify_stale_when_absent() {
        let snap = Snapshot::build(&sample(), None, 1, 1000);
        let send = snap.find_ref("@e2").unwrap();
        let other = node(
            "window",
            Some("Other"),
            None,
            vec![node(
                "button",
                Some("Quit"),
                Some(bounds(0, 0, 10, 10)),
                vec![],
            )],
        );
        assert!(matches!(reidentify(&other, send), Resolved::Stale(_)));
    }

    #[test]
    fn store_round_trip_and_prune() {
        let store = SnapshotStore::isolated();

        let mut last_id = String::new();
        let mut first_id = String::new();
        for i in 0..(MAX_SNAPSHOTS + 5) {
            let snap = Snapshot::build(&sample(), None, 1, 1000 + i as i64);
            if i == 0 {
                first_id = snap.id.clone();
            }
            store.save_at(&snap, 1000 + i as i64).unwrap();
            last_id = snap.id;
        }
        let now = 1000 + MAX_SNAPSHOTS as i64 + 5;
        // latest pointer resolves to the final save.
        let latest = store.load_latest_at(now).unwrap();
        assert_eq!(latest.id, last_id);

        // Pruned to the cap: the oldest is gone.
        let Backend::Memory(state) = &store.backend else {
            panic!("isolated store is in memory")
        };
        assert_eq!(lock(state).snapshots.len(), MAX_SNAPSHOTS);
        assert!(store.load_at(&first_id, now).is_err());
    }

    #[test]
    fn snapshots_expire_after_the_ttl() {
        let store = SnapshotStore::isolated();
        let snap = Snapshot::build(&sample(), None, 1, 5000);
        store.save_at(&snap, 5000).unwrap();
        assert!(store.load_at(&snap.id, 5000 + SNAPSHOT_TTL_MS).is_ok());
        assert!(store.load_at(&snap.id, 5001 + SNAPSHOT_TTL_MS).is_err());
        assert!(store.load_latest_at(5001 + SNAPSHOT_TTL_MS).is_err());
    }

    fn temp_dir(label: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ghost-{label}-{}-{}",
            std::process::id(),
            SNAPSHOT_SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    const KEY: [u8; 32] = [7; 32];

    #[test]
    fn a_sealed_snapshot_resolves_refs_in_a_fresh_store() {
        let tmp = temp_dir("sealed-cross");
        let dir = tmp.join("snapshots");
        let snap = Snapshot::build(&sample(), Some("Mail".into()), 42, 1_000);
        // The process that ran ghost_snapshot.
        SnapshotStore::sealed_at(&dir, &KEY)
            .save_at(&snap, 1_000)
            .unwrap();

        // A later per-call Ghost process: nothing shared but the directory.
        let next = SnapshotStore::sealed_at(&dir, &KEY);
        let latest = next.load_latest_at(2_000).unwrap();
        assert_eq!(latest.id, snap.id);
        let cancel = latest.find_ref("@e3").expect("@e3 survives the process");
        assert_eq!(cancel.name.as_deref(), Some("Cancel"));
        assert_eq!(
            next.load_at(&snap.id, 2_000).unwrap().app.as_deref(),
            Some("Mail")
        );

        // On disk: one content envelope, no on-screen text, owner-only modes.
        let file = dir.join(format!("{}.snap", snap.id));
        let bytes = std::fs::read(&file).unwrap();
        assert!(ryu_crypto::ContentCipher::is_sealed_bytes(&bytes));
        for needle in ["Cancel", "Search", "Mail", "textfield"] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{needle} must not be on disk in plaintext"
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode =
                |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&file), 0o600);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sealed_snapshots_expire_and_are_capped_on_disk() {
        let tmp = temp_dir("sealed-prune");
        let dir = tmp.join("snapshots");
        let store = SnapshotStore::sealed_at(&dir, &KEY);
        let mut ids = Vec::new();
        for i in 0..(MAX_SNAPSHOTS + 3) {
            let snap = Snapshot::build(&sample(), None, 1, 10_000 + i as i64);
            store.save_at(&snap, 10_000 + i as i64).unwrap();
            ids.push(snap.id);
        }
        let count = || {
            std::fs::read_dir(&dir)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().ends_with(".snap"))
                .count()
        };
        assert_eq!(count(), MAX_SNAPSHOTS);
        assert!(store.load_at(&ids[0], 10_020).is_err());
        assert_eq!(
            store.load_latest_at(10_020).unwrap().id,
            *ids.last().unwrap()
        );
        // Past the TTL every file is deleted on the next use.
        let late = 10_000 + MAX_SNAPSHOTS as i64 + 3 + SNAPSHOT_TTL_MS;
        assert!(store.load_latest_at(late).is_err());
        assert_eq!(count(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn expired_sealed_snapshots_are_deleted_without_a_snapshot_call() {
        let tmp = temp_dir("sealed-sweep");
        let dir = tmp.join("snapshots");
        let snap = Snapshot::build(&sample(), None, 1, 20_000);
        SnapshotStore::sealed_at(&dir, &KEY)
            .save_at(&snap, 20_000)
            .unwrap();
        let file = dir.join(format!("{}.snap", snap.id));
        // A fresh process sweeping before expiry keeps the snapshot.
        SnapshotStore::sealed_at(&dir, &KEY).prune_expired_at(20_000 + SNAPSHOT_TTL_MS);
        assert!(file.exists());
        // The sweep after expiry deletes it with no load or save.
        SnapshotStore::sealed_at(&dir, &KEY).prune_expired_at(20_001 + SNAPSHOT_TTL_MS);
        assert!(!file.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn snapshots_under_another_key_or_in_plaintext_are_not_loaded() {
        let tmp = temp_dir("sealed-foreign");
        let dir = tmp.join("snapshots");
        let snap = Snapshot::build(&sample(), None, 1, 1_000);
        SnapshotStore::sealed_at(&dir, &[9; 32])
            .save_at(&snap, 1_000)
            .unwrap();
        let plain = Snapshot::build(&sample(), None, 1, 1_001);
        std::fs::write(
            dir.join(format!("{}.snap", plain.id)),
            serde_json::to_vec(&StoredSnapshot {
                saved_ms: 1_001,
                snapshot: plain.clone(),
            })
            .unwrap(),
        )
        .unwrap();

        let store = SnapshotStore::sealed_at(&dir, &KEY);
        assert!(store.load_latest_at(1_500).is_err());
        assert!(store.load_at(&plain.id, 1_500).is_err());
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn a_sealed_file_renamed_to_another_id_does_not_open() {
        let tmp = temp_dir("sealed-rename");
        let dir = tmp.join("snapshots");
        let store = SnapshotStore::sealed_at(&dir, &KEY);
        let snap = Snapshot::build(&sample(), None, 1, 1_000);
        store.save_at(&snap, 1_000).unwrap();
        let other = "s999-1";
        std::fs::rename(
            dir.join(format!("{}.snap", snap.id)),
            dir.join(format!("{other}.snap")),
        )
        .unwrap();
        assert!(store.load_at(other, 1_500).is_err());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn without_a_key_snapshots_stay_in_memory() {
        let tmp = temp_dir("no-key");
        let dir = tmp.join("snapshots");
        let store = SnapshotStore::with_backend(None, dir.clone());
        let snap = Snapshot::build(&sample(), None, 1, now_ms());
        store.save(&snap).unwrap();
        assert_eq!(store.load(&snap.id).unwrap().id, snap.id);
        assert!(!dir.exists(), "nothing is written without a key");
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn startup_cleanup_removes_legacy_files_without_a_snapshot_call() {
        let tmp = temp_dir("startup-legacy");
        let dir = tmp.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s1000-1.json"), r#"{"secret":"typed text"}"#).unwrap();
        std::fs::write(dir.join("latest"), "s1000-1").unwrap();
        // The function `main` runs before dispatching any command.
        remove_legacy_snapshots_in(&dir);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn legacy_snapshot_files_are_deleted() {
        let tmp = std::env::temp_dir().join(format!("ghost-legacy-snap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let dir = tmp.join("snapshots");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("s1000-1.json"), r#"{"secret":"typed text"}"#).unwrap();
        std::fs::write(dir.join("latest"), "s1000-1").unwrap();
        std::fs::write(dir.join("keep.txt"), "not ours").unwrap();
        remove_legacy_snapshot_files(&dir);
        assert!(!dir.join("s1000-1.json").exists());
        assert!(!dir.join("latest").exists());
        assert!(dir.join("keep.txt").exists());
        std::fs::remove_file(dir.join("keep.txt")).unwrap();
        remove_legacy_snapshot_files(&dir);
        assert!(!dir.exists());
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn store_rejects_path_like_snapshot_ids() {
        let store = SnapshotStore::isolated();

        for id in [
            ".",
            "..",
            "../outside",
            "/tmp/outside",
            "s123-not-a-sequence",
        ] {
            assert!(
                store.load(id).is_err(),
                "snapshot id {id:?} must be rejected"
            );
        }
    }
}
