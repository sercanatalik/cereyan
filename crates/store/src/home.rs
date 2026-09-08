use std::path::{Path, PathBuf};

/// Environment variable that overrides the default home.
pub const HOME_ENV: &str = "CEREYAN_HOME";

/// Resolve the runtime home: an explicit flag value, then `CEREYAN_HOME`,
/// then `~/.cereyan`. A leading `~` is expanded. The directory is not
/// created here.
pub fn resolve_home(flag: Option<&Path>) -> PathBuf {
    if let Some(p) = flag {
        return expand_tilde(p);
    }
    if let Some(v) = std::env::var_os(HOME_ENV) {
        if !v.is_empty() {
            return expand_tilde(Path::new(&v));
        }
    }
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".cereyan")
}

fn expand_tilde(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = s.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest);
    }
    p.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flag_beats_environment() {
        // Environment access in tests is process-wide; only assert the
        // flag path, which ignores the environment entirely.
        let p = resolve_home(Some(Path::new("/b")));
        assert_eq!(p, PathBuf::from("/b"));
    }

    #[test]
    fn tilde_expands() {
        let p = resolve_home(Some(Path::new("~/x")));
        assert!(p.ends_with("x"));
        assert!(!p.to_string_lossy().starts_with('~'));
    }
}
