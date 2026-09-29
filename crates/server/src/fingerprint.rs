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
    let dir = file.parent()?;
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "py") && p.is_file())
        .collect();
    if !files.contains(&file) {
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
    let mut hasher = Sha256::new();
    for path in files {
        let name = path.file_name()?.to_string_lossy().into_owned();
        let content = std::fs::read(&path).ok()?;
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

/// The newest modification time among a module's files.
fn newest(source_dir: &Path, module: &str) -> Option<SystemTime> {
    module_files(source_dir, module)?
        .iter()
        .filter_map(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
        .max()
}

/// (source dir, module) → (newest file time seen, fingerprint).
type Cache = HashMap<(String, String), (Option<SystemTime>, Option<String>)>;

/// Fingerprints of the server's own modules, recomputed when a file changes.
#[derive(Default)]
pub struct Fingerprints {
    cache: Mutex<Cache>,
}

impl Fingerprints {
    pub fn get(&self, source_dir: &str, module: &str) -> Option<String> {
        let dir = Path::new(source_dir);
        let stamp = newest(dir, module);
        let key = (source_dir.to_string(), module.to_string());
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((seen, hash)) = cache.get(&key) {
            if *seen == stamp {
                return hash.clone();
            }
        }
        let hash = module_fingerprint(dir, module);
        cache.insert(key, (stamp, hash.clone()));
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
