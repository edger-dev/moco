//! The `Place` contract, held by every place: the same suite runs against the
//! file-system place and the in-memory place.
//!
//! implements: a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447@a7089331ac0dff49deb5acc9afce92b2a9d8a9d5022f66af915f3a2332d6f447

#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use moco_place::{
    Capabilities, Child, FsPlace, Hash, Key, MemPlace, Place, PlaceError, Verified, put_bytes,
};

/// The key a sharded store files `bytes` under.
fn key_for(bytes: &[u8], ext: &str) -> Key {
    let h = Hash::of_content(bytes);
    Key::new(format!("store/{}/{}{ext}", h.shard(), h.as_hex())).unwrap()
}

moco_place::place_contract!(fs, {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    (dir, place)
});
moco_place::place_contract!(mem, ((), MemPlace::new()));

/// On disk: a corrupted file reads as corrupt, temporaries and other
/// dot-names never show, and a stray file whose name names no hash is left
/// out of the inventory.
#[test]
fn a_file_system_place_sees_corruption_and_hides_its_own_files() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    let key = key_for(b"original", ".md");
    put_bytes(&place, &key, b"original").unwrap();
    std::fs::write(dir.path().join(key.as_str()), b"rotted!!").unwrap();
    assert_eq!(
        place.verify(&key).unwrap(),
        Verified::Corrupt {
            got: Hash::of_content(b"rotted!!")
        }
    );

    let shard = dir.path().join(key.parent().unwrap().as_str());
    std::fs::write(shard.join(".tmp-123"), b"half").unwrap();
    std::fs::write(shard.join("README"), b"stray").unwrap();
    let listed = place.list(Some(&key.parent().unwrap())).unwrap();
    assert!(
        listed
            .iter()
            .all(|c| !matches!(c, Child::Object { key, .. } if key.name().starts_with('.')))
    );
    let inv = place.inventory().unwrap();
    assert_eq!(inv.len(), 1, "{inv:?}");
    assert_eq!(
        inv[0].hash,
        Hash::of_content(b"rotted!!"),
        "the inventory states what is there now"
    );
}

/// A read-only place refuses a put; an offline one says so.
#[test]
fn a_place_that_takes_no_writes_refuses_a_put() {
    let ro = MemPlace::with_capabilities(Capabilities {
        online: true,
        writable: false,
        cold: false,
    });
    let key = key_for(b"x", "");
    assert!(matches!(
        put_bytes(&ro, &key, b"x"),
        Err(PlaceError::NotWritable)
    ));
    let off = MemPlace::with_capabilities(Capabilities {
        online: false,
        writable: true,
        cold: false,
    });
    assert!(matches!(
        put_bytes(&off, &key, b"x"),
        Err(PlaceError::Offline)
    ));
    assert!(matches!(off.get(&key, None), Err(PlaceError::Offline)));
}

/// A disk that is not mounted is not an empty place: its mount point lacks
/// the place's marker, so the place is offline and nothing lands on the host
/// disk underneath.
#[test]
fn an_unmounted_disk_is_offline_not_empty() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("usb");
    let place = FsPlace::create(&root).unwrap();
    assert!(place.capabilities().online);
    put_bytes(&place, &key_for(b"photo", ""), b"photo").unwrap();

    // Unplugged: the mount point is an empty directory again.
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::create_dir(&root).unwrap();
    assert!(!place.capabilities().online);
    assert!(matches!(
        put_bytes(&place, &key_for(b"more", ""), b"more"),
        Err(PlaceError::Offline)
    ));
    assert!(matches!(place.inventory(), Err(PlaceError::Offline)));
    assert!(matches!(place.list(None), Err(PlaceError::Offline)));
    assert!(matches!(
        place.exists(&key_for(b"photo", "")),
        Err(PlaceError::Offline)
    ));
    assert_eq!(
        std::fs::read_dir(&root).unwrap().count(),
        0,
        "nothing written underneath"
    );

    assert!(matches!(
        FsPlace::open(&root),
        Err(PlaceError::NotAPlace { .. })
    ));
    assert!(matches!(
        FsPlace::open(dir.path().join("never")),
        Err(PlaceError::NotAPlace { .. })
    ));
    assert!(FsPlace::open(dir.path().join("usb2")).is_err());
    FsPlace::create(dir.path().join("usb2")).unwrap();
    assert!(FsPlace::open(dir.path().join("usb2")).is_ok());
}

/// Something other than a file at a key's path is not the object.
#[test]
fn a_directory_at_a_key_is_not_the_object() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    let key = key_for(b"blocked", "");
    std::fs::create_dir_all(dir.path().join(key.as_str())).unwrap();
    assert!(put_bytes(&place, &key, b"blocked").is_err());
    assert!(!place.exists(&key).unwrap());
}

/// A store moved to another disk behind a symlink is still walked.
#[cfg(unix)]
#[test]
fn a_symlinked_directory_is_followed() {
    let dir = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let elsewhere = FsPlace::create(other.path()).unwrap();
    let key = key_for(b"moved", "");
    put_bytes(&elsewhere, &key, b"moved").unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    std::os::unix::fs::symlink(other.path().join("store"), dir.path().join("store")).unwrap();
    assert_eq!(
        place.list(None).unwrap(),
        vec![Child::Prefix(Key::new("store").unwrap())]
    );
    assert_eq!(place.inventory().unwrap().len(), 1);
}

/// Crashed puts leave temporaries; a sweep clears old ones.
#[test]
fn a_sweep_clears_old_temporaries() {
    let dir = tempfile::tempdir().unwrap();
    let place = FsPlace::create(dir.path()).unwrap();
    let key = key_for(b"x", "");
    put_bytes(&place, &key, b"x").unwrap();
    let shard = dir.path().join(key.parent().unwrap().as_str());
    std::fs::write(shard.join(".tmp-1-2-3"), b"half").unwrap();
    assert_eq!(
        place
            .sweep_temporaries(std::time::Duration::from_secs(3600))
            .unwrap(),
        0
    );
    assert_eq!(
        place.sweep_temporaries(std::time::Duration::ZERO).unwrap(),
        1
    );
    assert!(!shard.join(".tmp-1-2-3").exists());
    assert!(place.exists(&key).unwrap());
}

/// On a cold place the read-everything defaults refuse rather than run up a
/// bill; such a place verifies itself or not at all.
#[test]
fn the_defaults_refuse_on_a_cold_place() {
    let cold = MemPlace::with_capabilities(Capabilities {
        online: true,
        writable: true,
        cold: true,
    });
    let key = key_for(b"archived", "");
    put_bytes(&cold, &key, b"archived").unwrap();
    assert!(matches!(cold.verify(&key), Err(PlaceError::Cold)));
    assert!(matches!(cold.inventory(), Err(PlaceError::Cold)));
}

/// Places are usable behind a pointer, as a working copy holds its remotes.
#[test]
fn a_place_behind_a_pointer_is_a_place() {
    let shared: std::sync::Arc<dyn Place> = std::sync::Arc::new(MemPlace::new());
    let boxed: Box<dyn Place> = Box::new(MemPlace::new());
    let key = key_for(b"p", "");
    put_bytes(&shared, &key, b"p").unwrap();
    put_bytes(&boxed, &key, b"p").unwrap();
    put_bytes(&&*boxed, &key, b"p").unwrap();
    assert!(shared.exists(&key).unwrap() && boxed.exists(&key).unwrap());
}
