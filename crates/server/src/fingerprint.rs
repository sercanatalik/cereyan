//! Module fingerprints: what code a run executed, and whether a worker's
//! checkout matches the server's. Code is never stored, only its hash.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use sha2::{Digest, Sha256};

/// The file a dotted module name resolves to. A flow records the directory of
/// its module's own file, so `a.b` is `source_dir/b.py` there; a root-relative
/// `source_dir/a/b.py` and the package forms are tried too.
pub fn module_file(source_dir: &Path, module: &str) -> PathBuf {
    let rel = module.replace('.', "/");
    let last = module.rsplit('.').next().unwrap_or(module);
    let candidates = [
        source_dir.join(format!("{rel}.py")),
        source_dir.join(&rel).join("__init__.py"),
        source_dir.join(format!("{last}.py")),
        source_dir.join("__init__.py"),
    ];
    candidates
        .iter()
        .find(|p| p.is_file())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

/// The `.py` files that make up a module for fingerprinting: its own file and
/// every other `.py` file in the same directory, sorted by name. The directory
/// is the unit because a module's helpers usually sit beside it.
fn module_files(source_dir: &Path, module: &str) -> Option<Vec<PathBuf>> {
    let file = module_file(source_dir, module);
    files_in_dir(&file)
}

/// The `.py` files in the directory holding `file`, sorted by name.
fn files_in_dir(file: &Path) -> Option<Vec<PathBuf>> {
    let dir = file.parent()?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "py") && p.is_file())
        .collect();
    if !files.iter().any(|p| p == file) {
        return None;
    }
    files.sort();
    Some(files)
}

/// SHA-256, hex, over each file of the module as `name \0 content \0`, in name
/// order. `None` when the module cannot be found. The same function runs on the
/// server and, through `_core`, on every worker, so equal code gives equal hashes
/// whatever directory the checkout sits in.
pub fn module_fingerprint(source_dir: &Path, module: &str) -> Option<String> {
    let files = module_files(source_dir, module)?;
    hash_files(&files)
}

/// Hash exactly these files, in the order given. Shared by the public entry
/// point and the cache, so both hash one file list rather than each listing the
/// directory for themselves.
fn hash_files(files: &[PathBuf]) -> Option<String> {
    let mut hasher = Sha256::new();
    for path in files {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let content = std::fs::read(path).ok()?;
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(&content);
        hasher.update([0]);
    }
    Some(hex(&hasher.finalize()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A directory's identity for change detection: mtime and size. Adding,
/// removing or renaming an entry moves the mtime; editing a file in place does
/// not, which is why the cached file stamps are checked too.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct DirStamp {
    mtime: Option<SystemTime>,
    len: u64,
}

fn dir_stamp(path: &Path) -> DirStamp {
    match std::fs::metadata(path) {
        Ok(m) => DirStamp {
            mtime: m.modified().ok(),
            len: m.len(),
        },
        Err(_) => DirStamp {
            mtime: None,
            len: 0,
        },
    }
}

fn file_stamp(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).ok().and_then(|m| m.modified().ok())
}

/// What the cache remembers about one module.
struct Entry {
    /// Identity of the module's directory: detects entries added or removed.
    dir: DirStamp,
    /// The exact `.py` list that produced `hash`, or `None` when the module
    /// could not be resolved. Caching this is what lets a hit skip `read_dir`.
    files: Option<Vec<PathBuf>>,
    /// Each cached file's mtime: detects in-place edits, which a directory
    /// stamp alone would miss.
    stamps: Vec<Option<SystemTime>>,
    hash: Option<String>,
}

impl Entry {
    /// Is this entry still valid, given the directory's current stamp?
    /// Never lists the directory: the file set is already cached.
    fn still_valid(&self, stamp: &DirStamp) -> bool {
        if self.dir != *stamp {
            return false;
        }
        // Same paths, same order, same mtimes: the hash would be identical.
        match &self.files {
            None => true,
            Some(files) => {
                files.len() == self.stamps.len()
                    && files
                        .iter()
                        .zip(&self.stamps)
                        .all(|(p, seen)| file_stamp(p) == *seen)
            }
        }
    }
}

/// (source dir, module) → what is known about it.
type Cache = HashMap<(String, String), Entry>;

/// Fingerprints of the server's own modules, recomputed when a file changes.
#[derive(Default)]
pub struct Fingerprints {
    cache: Mutex<Cache>,
}

impl Fingerprints {
    pub fn get(&self, source_dir: &str, module: &str) -> Option<String> {
        let key = (source_dir.to_string(), module.to_string());
        // Resolve where the module's code actually lives. This is not
        // necessarily `source_dir`: a dotted module such as `etl.orders` is
        // `source_dir/etl/orders.py`, and it is that directory whose contents
        // the fingerprint covers.
        let file = module_file(Path::new(source_dir), module);
        let dir = file.parent()?;
        let stamp = dir_stamp(dir);
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        // Validate from what is already cached. This is the hot path: a worker
        // heartbeat asks for every flow's fingerprint every few seconds, and it
        // must not cost a directory listing each time.
        if let Some(entry) = cache.get(&key) {
            if entry.still_valid(&stamp) {
                return entry.hash.clone();
            }
        }
        // Stale or absent: list the directory once, and use that one list for
        // both the stamp and the hash.
        let files = files_in_dir(&file);
        let stamps = files
            .as_deref()
            .map(|fs| fs.iter().map(|p| file_stamp(p)).collect())
            .unwrap_or_default();
        let hash = files.as_deref().and_then(hash_files);
        cache.insert(
            key,
            Entry {
                dir: stamp,
                files,
                stamps,
                hash: hash.clone(),
            },
        );
        hash
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_code_in_different_directories_gives_equal_hashes() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for dir in [a.path(), b.path()] {
            std::fs::create_dir_all(dir.join("etl")).unwrap();
            std::fs::write(dir.join("etl/orders.py"), "def f(): pass\n").unwrap();
            std::fs::write(dir.join("etl/helpers.py"), "X = 1\n").unwrap();
            std::fs::write(dir.join("etl/notes.txt"), "ignored").unwrap();
        }
        let ha = module_fingerprint(a.path(), "etl.orders").unwrap();
        assert_eq!(Some(ha.clone()), module_fingerprint(b.path(), "etl.orders"));
        // A helper beside the module changes the fingerprint; other files do not.
        std::fs::write(b.path().join("etl/helpers.py"), "X = 2\n").unwrap();
        assert_ne!(Some(ha.clone()), module_fingerprint(b.path(), "etl.orders"));
        std::fs::write(a.path().join("etl/notes.txt"), "changed").unwrap();
        assert_eq!(Some(ha), module_fingerprint(a.path(), "etl.orders"));
        assert_eq!(module_fingerprint(a.path(), "etl.missing"), None);
        // A flow in a package records the package directory as its source dir.
        assert_eq!(
            module_fingerprint(&a.path().join("etl"), "etl.orders"),
            module_fingerprint(a.path(), "etl.orders")
        );
    }

    #[test]
    fn the_cache_follows_edits() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("pipeline.py"), "A = 1\n").unwrap();
        let prints = Fingerprints::default();
        let dir = d.path().to_str().unwrap();
        let first = prints.get(dir, "pipeline").unwrap();
        assert_eq!(prints.get(dir, "pipeline").unwrap(), first);
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(d.path().join("pipeline.py"), "A = 2\n").unwrap();
        assert_ne!(prints.get(dir, "pipeline").unwrap(), first);
    }
}

// Unix only: the tests seal a directory with permission bits, which Windows
// does not have.
#[cfg(all(test, unix))]
mod cache_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    /// A directory made unreadable but still traversable: `read_dir` fails,
    /// while `metadata` on the directory and its files still succeeds. That
    /// separates "listed the directory" from "stat'ed what was already known",
    /// so a cache hit can be proven not to list.
    struct Sealed(PathBuf);

    impl Sealed {
        fn new(dir: &Path) -> Option<Self> {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o111)).ok()?;
            Some(Self(dir.to_path_buf()))
        }
    }

    impl Drop for Sealed {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }

    fn fixture() -> (tempfile::TempDir, String) {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join("etl")).unwrap();
        std::fs::write(d.path().join("etl/orders.py"), "def f(): pass\n").unwrap();
        std::fs::write(d.path().join("etl/helpers.py"), "X = 1\n").unwrap();
        let dir = d.path().to_str().unwrap().to_string();
        (d, dir)
    }

    /// The whole point of the cache: a hit must not list the directory. Sealing
    /// the module's directory makes any listing fail, so a hash still returned
    /// under seal can only have come from the cache.
    #[test]
    fn a_cache_hit_does_not_list_the_directory() {
        let (d, dir) = fixture();
        let prints = Fingerprints::default();
        let first = prints.get(&dir, "etl.orders").expect("hashed");
        assert_eq!(prints.get(&dir, "etl.orders"), Some(first.clone()));

        // The module's code lives in the `etl` subdirectory, so that is what
        // must be sealed.
        let sealed = Sealed::new(&d.path().join("etl")).expect("seal");
        assert_eq!(
            prints.get(&dir, "etl.orders"),
            Some(first),
            "a cache hit listed the directory"
        );
        // A miss would now fail, proving the seal is effective.
        let other = Fingerprints::default();
        assert_eq!(
            other.get(&dir, "etl.orders"),
            None,
            "seal did not take effect"
        );
        drop(sealed);
    }

    #[test]
    fn editing_a_beside_file_invalidates_the_cache() {
        let (d, dir) = fixture();
        let prints = Fingerprints::default();
        let first = prints.get(&dir, "etl.orders").unwrap();
        // Re-stamp so the edit is visible at any mtime granularity.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(d.path().join("etl/helpers.py"), "X = 2\n").unwrap();
        assert_ne!(prints.get(&dir, "etl.orders").unwrap(), first);
    }

    #[test]
    fn adding_a_beside_file_invalidates_the_cache() {
        let (d, dir) = fixture();
        let prints = Fingerprints::default();
        let first = prints.get(&dir, "etl.orders").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(d.path().join("etl/extra.py"), "Y = 1\n").unwrap();
        assert_ne!(
            prints.get(&dir, "etl.orders").unwrap(),
            first,
            "a new .py file beside the module must change the fingerprint"
        );
    }

    #[test]
    fn removing_a_beside_file_invalidates_the_cache() {
        let (d, dir) = fixture();
        let prints = Fingerprints::default();
        let first = prints.get(&dir, "etl.orders").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::remove_file(d.path().join("etl/helpers.py")).unwrap();
        assert_ne!(prints.get(&dir, "etl.orders").unwrap(), first);
    }

    #[test]
    fn a_non_python_file_does_not_invalidate_the_cache() {
        let (d, dir) = fixture();
        let prints = Fingerprints::default();
        let first = prints.get(&dir, "etl.orders").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(d.path().join("etl/notes.txt"), "changed").unwrap();
        assert_eq!(
            prints.get(&dir, "etl.orders").unwrap(),
            first,
            "a non-.py file must not change the fingerprint"
        );
    }

    #[test]
    fn the_same_module_in_two_directories_is_cached_separately() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        for (d, body) in [(a.path(), "A = 1\n"), (b.path(), "B = 2\n")] {
            std::fs::create_dir_all(d.join("etl")).unwrap();
            std::fs::write(d.join("etl/orders.py"), body).unwrap();
        }
        let prints = Fingerprints::default();
        let (da, db) = (a.path().to_str().unwrap(), b.path().to_str().unwrap());
        let ha = prints.get(da, "etl.orders").unwrap();
        let hb = prints.get(db, "etl.orders").unwrap();
        assert_ne!(ha, hb, "different content must hash differently");

        // Editing one must not disturb the other's cached entry.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(a.path().join("etl/orders.py"), "A = 2\n").unwrap();
        assert_ne!(prints.get(da, "etl.orders").unwrap(), ha);
        assert_eq!(prints.get(db, "etl.orders").unwrap(), hb);
    }

    #[test]
    fn a_missing_module_stays_missing() {
        let (d, dir) = fixture();
        let prints = Fingerprints::default();
        assert_eq!(prints.get(&dir, "etl.absent"), None);
        assert_eq!(prints.get(&dir, "etl.absent"), None);
        // And a module that appears later is picked up.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(d.path().join("etl/absent.py"), "Z = 1\n").unwrap();
        assert!(prints.get(&dir, "etl.absent").is_some());
    }
}
