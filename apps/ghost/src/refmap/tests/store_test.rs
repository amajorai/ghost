//! Snapshot store tests: memory and sealed backends, expiry, legacy cleanup.

use std::sync::atomic::Ordering;

use super::super::store::*;
use super::super::{Snapshot, MAX_SNAPSHOTS, SNAPSHOT_SEQ};
use super::sample;

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
        let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
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
