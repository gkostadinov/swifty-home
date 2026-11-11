//! The secret store: one private file per slot, listed without being read, and
//! read only with a live grant.

use std::collections::BTreeSet;
use std::io::ErrorKind;
use std::path::Path;

use swifty_hub::secrets::{FileSecretStore, MemorySecretStore, SecretStore, MAX_VALUE_BYTES};
use swifty_net::LiveGrant;
use swifty_plugin_api::Secret;

const SOCKET: &str = "local_key/bf0123456789abcdef";
const OTHER_SOCKET: &str = "local_key/bf0123456789abcde2";

fn grant() -> LiveGrant {
    LiveGrant::when_live(true).expect("a live configuration grants")
}

fn hidden(text: &str) -> Secret {
    text.to_string().into()
}

fn stored(store: &dyn SecretStore, instance: &str, slot: &str) -> Option<String> {
    store.read(grant(), instance, slot).unwrap().map(|value| value.expose().to_string())
}

fn names(list: &[&str]) -> BTreeSet<String> {
    list.iter().map(|name| name.to_string()).collect()
}

/// What any store must do, whatever it keeps its values in.
fn keeps_what_a_store_must(store: &dyn SecretStore) {
    assert!(store.slots("hue").unwrap().is_empty());
    assert_eq!(stored(store, "hue", "app_key"), None);

    store.write("hue", "app_key", &hidden("first-value")).unwrap();
    assert_eq!(store.slots("hue").unwrap(), names(&["app_key"]));
    assert_eq!(stored(store, "hue", "app_key").as_deref(), Some("first-value"));
    store.write("hue", "app_key", &hidden("second-value")).unwrap();
    assert_eq!(stored(store, "hue", "app_key").as_deref(), Some("second-value"), "a write replaces the value");
    assert!(store.slots("hue2").unwrap().is_empty(), "instances are apart");
    assert_eq!(stored(store, "hue2", "app_key"), None);

    store.write("tuya", SOCKET, &hidden("MDEyMzQ1Njc4OWFiY2RlZg==")).unwrap();
    store.write("tuya", OTHER_SOCKET, &hidden("ZmVkY2JhOTg3NjU0MzIxMA==")).unwrap();
    assert_eq!(store.slots("tuya").unwrap(), names(&[SOCKET, OTHER_SOCKET]), "a slot per row");
    assert_eq!(stored(store, "tuya", OTHER_SOCKET).as_deref(), Some("ZmVkY2JhOTg3NjU0MzIxMA=="));

    store.remove("tuya", SOCKET).unwrap();
    assert_eq!(store.slots("tuya").unwrap(), names(&[OTHER_SOCKET]));
    assert_eq!(stored(store, "tuya", SOCKET), None);
    store.remove("tuya", SOCKET).unwrap();
    store.remove("plex", "token").unwrap();

    store.write("tuya", "login", &hidden("x")).unwrap();
    store.remove_instance("tuya").unwrap();
    assert!(store.slots("tuya").unwrap().is_empty());
    store.remove_instance("tuya").unwrap();
    assert_eq!(store.slots("hue").unwrap(), names(&["app_key"]), "only that instance went");

    let value = hidden("v");
    for (instance, slot) in [
        ("../hue", "app_key"),
        ("Hue", "app_key"),
        ("hue/x", "app_key"),
        ("", "app_key"),
        ("hue", "../../escape"),
        ("hue", "app-key"),
        ("hue", "local_key/a/b"),
        ("hue", "local_key/.."),
        ("hue", ""),
    ] {
        let refused = store.write(instance, slot, &value).unwrap_err();
        assert_eq!(refused.kind(), ErrorKind::InvalidInput, "{instance} {slot}");
        assert_eq!(store.read(grant(), instance, slot).unwrap_err().kind(), ErrorKind::InvalidInput);
        assert_eq!(store.remove(instance, slot).unwrap_err().kind(), ErrorKind::InvalidInput);
    }
    for instance in ["../hue", "Hue", ""] {
        assert_eq!(store.slots(instance).unwrap_err().kind(), ErrorKind::InvalidInput, "{instance}");
        assert_eq!(store.remove_instance(instance).unwrap_err().kind(), ErrorKind::InvalidInput, "{instance}");
    }
    assert_eq!(store.write("hue", "app_key", &hidden("")).unwrap_err().kind(), ErrorKind::InvalidInput);
    let too_long = hidden(&"x".repeat(MAX_VALUE_BYTES + 1));
    assert_eq!(store.write("hue", "app_key", &too_long).unwrap_err().kind(), ErrorKind::InvalidInput);
    assert_eq!(stored(store, "hue", "app_key").as_deref(), Some("second-value"), "a refused write changes nothing");
}

#[test]
fn the_file_store_keeps_what_a_store_must() {
    let dir = tempfile::tempdir().unwrap();
    keeps_what_a_store_must(&FileSecretStore::new(dir.path().join("secrets")));
}

#[test]
fn the_memory_store_keeps_what_a_store_must() {
    keeps_what_a_store_must(&MemorySecretStore::default());
}

/// Every entry under `dir`, as paths relative to it.
fn tree(dir: &Path) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(dir).unwrap().to_string_lossy().replace('\\', "/");
            if path.is_dir() {
                pending.push(path.clone());
                found.insert(format!("{relative}/"));
            } else {
                found.insert(relative);
            }
        }
    }
    found
}

#[test]
fn the_file_store_keeps_one_file_per_slot_and_nothing_else() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data").join("secrets");
    let store = FileSecretStore::new(&root);
    assert_eq!(store.root(), root);
    store.write("hue", "app_key", &hidden("app-key-value")).unwrap();
    store.write("hue", "app_key", &hidden("app-key-value-2")).unwrap();
    store.write("tuya", SOCKET, &hidden("MDEyMzQ1Njc4OWFiY2RlZg==")).unwrap();
    assert_eq!(
        tree(&root),
        names(&["hue/", "hue/app_key", "tuya/", "tuya/local_key/", &format!("tuya/{SOCKET}")]),
        "no temporary file is left behind"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("hue").join("app_key")).unwrap(),
        "app-key-value-2",
        "the exact value"
    );

    store.remove("tuya", SOCKET).unwrap();
    assert_eq!(tree(&root), names(&["hue/", "hue/app_key", "tuya/"]), "a row's directory goes with its last row");
    store.remove_instance("hue").unwrap();
    assert_eq!(tree(&root), names(&["tuya/"]));
}

/// A file only its owner may read, as the store would have written it.
fn write_private(path: &Path, content: &[u8]) {
    std::fs::write(path, content).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[test]
fn listing_reads_no_value() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileSecretStore::new(dir.path());
    store.write("hue", "app_key", &hidden("app-key-value")).unwrap();
    // Something no read would accept: listing still names it, because it opens nothing.
    write_private(&dir.path().join("hue").join("token"), &vec![b'x'; MAX_VALUE_BYTES + 1]);
    write_private(&dir.path().join("hue").join("login"), &[0xff, 0xfe, 0x00]);
    assert_eq!(store.slots("hue").unwrap(), names(&["app_key", "login", "token"]));
    assert_eq!(store.read(grant(), "hue", "token").unwrap_err().kind(), ErrorKind::InvalidData);
    assert!(store.read(grant(), "hue", "login").is_err());
    // An empty file, a stray file and a write in progress are not slots that hold a value.
    write_private(&dir.path().join("hue").join("ca"), b"");
    write_private(&dir.path().join("hue").join(".app_key.0123.tmp"), b"half");
    write_private(&dir.path().join("hue").join("Notes.txt"), b"x");
    assert_eq!(store.slots("hue").unwrap(), names(&["app_key", "login", "token"]));
    assert!(store.read(grant(), "hue", "ca").unwrap().is_none(), "an empty slot holds nothing");
}

#[test]
fn a_dry_run_hub_has_no_grant_to_read_with() {
    // `SecretStore::read` takes a `LiveGrant`, and a dry-run process cannot make one.
    // The crate's documentation has a test that a read without one does not build.
    assert!(LiveGrant::when_live(false).is_none());
    let store = MemorySecretStore::default();
    store.write("hue", "app_key", &hidden("app-key-value")).unwrap();
    assert_eq!(store.slots("hue").unwrap(), names(&["app_key"]), "what a dry-run hub can know");
}

#[test]
fn no_value_is_ever_printed() {
    let memory = MemorySecretStore::default();
    memory.write("hue", "app_key", &hidden("app-key-value")).unwrap();
    let printed = format!("{memory:?}");
    assert!(printed.contains("app_key") && printed.contains("hue"), "{printed}");
    assert!(!printed.contains("app-key-value"), "{printed}");

    let dir = tempfile::tempdir().unwrap();
    let file = FileSecretStore::new(dir.path());
    file.write("hue", "app_key", &hidden("app-key-value")).unwrap();
    assert!(!format!("{file:?}").contains("app-key-value"));
    let read = file.read(grant(), "hue", "app_key").unwrap().unwrap();
    assert!(!format!("{read:?} {read}").contains("app-key-value"));
}

#[cfg(unix)]
mod unix {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    fn mode(path: &Path) -> u32 {
        std::fs::symlink_metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn files_are_0600_and_directories_0700() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("secrets");
        let store = FileSecretStore::new(&root);
        store.write("hue", "app_key", &hidden("app-key-value")).unwrap();
        store.write("tuya", SOCKET, &hidden("MDEyMzQ1Njc4OWFiY2RlZg==")).unwrap();
        for directory in [root.clone(), root.join("hue"), root.join("tuya"), root.join("tuya/local_key")] {
            assert_eq!(mode(&directory), 0o700, "{}", directory.display());
        }
        for file in [root.join("hue/app_key"), root.join("tuya").join(SOCKET)] {
            assert_eq!(mode(&file), 0o600, "{}", file.display());
        }

        // A directory someone opened up is closed again on the next write.
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(root.join("hue"), std::fs::Permissions::from_mode(0o750)).unwrap();
        store.write("hue", "app_key", &hidden("app-key-value-2")).unwrap();
        assert_eq!(mode(&root), 0o700);
        assert_eq!(mode(&root.join("hue")), 0o700);
        assert_eq!(mode(&root.join("hue/app_key")), 0o600, "a replaced file is still private");
    }

    #[test]
    fn a_slot_others_could_read_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        store.write("hue", "app_key", &hidden("app-key-value")).unwrap();
        let path = dir.path().join("hue/app_key");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_eq!(store.read(grant(), "hue", "app_key").unwrap_err().kind(), ErrorKind::PermissionDenied);
        assert_eq!(store.slots("hue").unwrap(), names(&["app_key"]));
    }

    #[test]
    fn a_slot_nobody_may_open_is_still_listed() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path());
        store.write("hue", "app_key", &hidden("app-key-value")).unwrap();
        let path = dir.path().join("hue/app_key");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        assert_eq!(store.slots("hue").unwrap(), names(&["app_key"]), "listing opens no file");
        if std::fs::File::open(&path).is_err() {
            // Not running as root: the file really cannot be read.
            assert!(store.read(grant(), "hue", "app_key").is_err());
        }
    }

    #[test]
    fn a_symbolic_link_is_not_a_slot() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileSecretStore::new(dir.path().join("secrets"));
        store.write("hue", "token", &hidden("token-value")).unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, "not-a-slot-value").unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&elsewhere, dir.path().join("secrets/hue/app_key")).unwrap();
        assert_eq!(store.slots("hue").unwrap(), names(&["token"]));
        assert_eq!(store.read(grant(), "hue", "app_key").unwrap_err().kind(), ErrorKind::InvalidData);
    }
}
