//! Which files study may read and write, for hosts that run it on a server for other people's
//! documents. Both limits come from the environment and are off unless set:
//!
//! - `STUDY_ROOT=<dir>`: every file study reads or writes (documents, imports, images, shaders,
//!   scripts, exports) must resolve inside this directory, after following symlinks.
//! - `STUDY_WRITABLE=<dir>[:<dir>...]` (separated like `PATH`): study saves documents, creates new
//!   ones and exports only inside these directories. Documents elsewhere open read-only.
//!
//! Relative directories are resolved against the working directory when study starts.

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

struct Policy {
    root: Option<PathBuf>,
    writable: Option<Vec<PathBuf>>,
}

fn policy() -> &'static Policy {
    static P: OnceLock<Policy> = OnceLock::new();
    P.get_or_init(|| {
        let var = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty());
        Policy {
            root: var("STUDY_ROOT").map(|r| resolve(Path::new(&r)).unwrap_or_else(|_| PathBuf::from(r))),
            writable: var("STUDY_WRITABLE").map(|v| {
                std::env::split_paths(&v).filter(|d| !d.as_os_str().is_empty()).map(|d| resolve(&d).unwrap_or(d)).collect()
            }),
        }
    })
}

/// True when either limit is set.
pub fn confined() -> bool {
    let p = policy();
    p.root.is_some() || p.writable.is_some()
}

/// `path` made absolute with every symlink resolved, including for paths that don't exist yet
/// (the existing part is resolved, the rest appended). Fails on a broken symlink.
pub fn resolve(path: &Path) -> Result<PathBuf, String> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(|e| e.to_string())?.join(path)
    };
    let mut cur = PathBuf::new();
    let mut exists = true;
    for c in abs.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => cur.push(c.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                cur.pop();
            }
            Component::Normal(name) => {
                cur.push(name);
                if exists && std::fs::symlink_metadata(&cur).is_ok() {
                    cur = std::fs::canonicalize(&cur).map_err(|e| format!("{}: {e}", path.display()))?;
                } else {
                    exists = false;
                }
            }
        }
    }
    Ok(strip_verbatim(cur))
}

/// Windows' `\\?\C:\...` form from `canonicalize`, as the plain `C:\...` every other path uses.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    match p.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => p,
    }
}

fn inside(path: &Path, dir: &Path) -> bool {
    path.starts_with(dir)
}

/// Whether study may read `path`. The error says why not.
pub fn check_read(path: &Path) -> Result<(), String> {
    let Some(root) = &policy().root else { return Ok(()) };
    let p = resolve(path)?;
    if inside(&p, root) {
        Ok(())
    } else {
        Err(format!("{} is outside the workspace", path.display()))
    }
}

/// Whether study may create or change `path`. The error says why not.
pub fn check_write(path: &Path) -> Result<(), String> {
    check_read(path)?;
    let Some(dirs) = &policy().writable else { return Ok(()) };
    let p = resolve(path)?;
    if dirs.iter().any(|d| inside(&p, d)) {
        Ok(())
    } else {
        let what = if p.exists() { "is read-only" } else { "can't be created there" };
        Err(format!("{} {what}. Only files in {} can be changed.", display(&p), writable_dirs()))
    }
}

/// Why a document at `path` can't be changed, if it can't.
pub fn read_only_reason(path: &Path) -> Option<String> {
    policy().writable.as_ref()?;
    check_write(path).err().map(|_| {
        format!("{} is read-only. Copy it into {} to edit it.", display(path), writable_dirs())
    })
}

/// The writable directories, as an agent should name them: "deliverables/" or "a/ or b/".
fn writable_dirs() -> String {
    let dirs = policy().writable.as_deref().unwrap_or_default();
    dirs.iter().map(|d| format!("{}/", display(d))).collect::<Vec<_>>().join(" or ")
}

/// `path` relative to `STUDY_ROOT` with `/` separators when it's inside it; otherwise as given.
pub fn display(path: &Path) -> String {
    relative(path).unwrap_or_else(|| path.display().to_string())
}

/// `path` relative to `STUDY_ROOT`, with `/` separators, if a root is set and contains it.
pub fn relative(path: &Path) -> Option<String> {
    let root = policy().root.as_ref()?;
    let p = resolve(path).ok()?;
    let rel = p.strip_prefix(root).ok()?;
    Some(rel.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_missing_tails_and_parent_dirs() {
        let dir = std::env::temp_dir().join("study-access-test");
        std::fs::create_dir_all(dir.join("a")).unwrap();
        let base = resolve(&dir).unwrap();
        assert_eq!(resolve(&dir.join("a/../a/new/x.pen")).unwrap(), base.join("a").join("new").join("x.pen"));
        assert_eq!(resolve(&dir.join("a/new/../../b")).unwrap(), base.join("b"));
        assert!(resolve(&dir.join("a/../../study-access-test/a")).unwrap().starts_with(&base));
    }

    #[cfg(unix)]
    #[test]
    fn follows_symlinks_out_of_a_directory() {
        let dir = std::env::temp_dir().join("study-access-link");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in")).unwrap();
        std::os::unix::fs::symlink("/etc", dir.join("in/escape")).unwrap();
        let p = resolve(&dir.join("in/escape/hosts")).unwrap();
        assert!(!p.starts_with(resolve(&dir).unwrap()));
        std::os::unix::fs::symlink(dir.join("missing"), dir.join("in/broken")).unwrap();
        assert!(resolve(&dir.join("in/broken/x")).is_err());
    }
}
