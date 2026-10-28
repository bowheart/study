//! Self-update from GitHub releases.
//!
//! Release builds (CI sets `STUDY_RELEASE=1` when building a `v*` tag) check the latest release
//! at startup and every few hours, download the asset for this platform into
//! `~/.study/updates/<version>/`, and let the window offer "Install and Restart" or "Install on
//! next launch". Installing swaps the app in place: on macOS the whole `Study.app` bundle, on
//! Windows `study.exe` (a running exe can be renamed out of the way). Downloads made by the app
//! itself aren't quarantined by macOS, so no signing is needed.
//!
//! For testing, `STUDY_UPDATE_FEED` points at another feed (a URL or a local JSON file in the
//! GitHub release format, whose asset URLs may also be local paths) and enables updates in any build.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

const FEED: &str = "https://api.github.com/repos/bowheart/study/releases/latest";
pub const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone, Debug)]
pub struct Update {
    pub version: String,
    /// The downloaded app: `Study.app` on macOS, `study.exe` on Windows.
    pub staged: PathBuf,
}

pub fn enabled() -> bool {
    option_env!("STUDY_RELEASE") == Some("1") || std::env::var_os("STUDY_UPDATE_FEED").is_some()
}

pub fn current() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

fn asset_name() -> Option<&'static str> {
    if cfg!(target_os = "macos") {
        Some("study-macos-arm64.tar.gz")
    } else if cfg!(windows) {
        Some("study-windows-x64.exe")
    } else {
        None
    }
}

fn updates_dir() -> PathBuf {
    render::data_dir().join("updates")
}

/// `a` is a newer semantic version than `b`.
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> { s.trim_start_matches('v').split(['.', '-']).take(3).map(|p| p.parse().unwrap_or(0)).collect() };
    parse(a) > parse(b)
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let mut r = ureq::get(url).header("User-Agent", "study-updater").call().map_err(|e| e.to_string())?;
        r.body_mut().with_config().limit(512 << 20).read_to_vec().map_err(|e| e.to_string())
    } else {
        std::fs::read(url).map_err(|e| format!("{url}: {e}"))
    }
}

/// Looks for a newer release and downloads it. `Ok(None)` = already up to date.
pub fn check() -> Result<Option<Update>, String> {
    let asset = asset_name().ok_or("updates aren't available on this platform")?;
    let feed = std::env::var("STUDY_UPDATE_FEED").unwrap_or_else(|_| FEED.to_string());
    let release: Value = serde_json::from_slice(&fetch(&feed)?).map_err(|e| e.to_string())?;
    let version = release["tag_name"].as_str().ok_or("the release has no tag")?.trim_start_matches('v').to_string();
    if !newer(&version, current()) {
        return Ok(None);
    }
    let url = release["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|x| x["name"] == asset))
        .and_then(|x| x["browser_download_url"].as_str())
        .ok_or_else(|| format!("release {version} has no {asset}"))?;
    let dir = updates_dir().join(&version);
    let staged = dir.join(if cfg!(target_os = "macos") { "Study.app" } else { "study.exe" });
    if !dir.join(".complete").exists() {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let bytes = fetch(url)?;
        if cfg!(target_os = "macos") {
            tar::Archive::new(flate2::read::GzDecoder::new(&bytes[..])).unpack(&dir).map_err(|e| e.to_string())?;
        } else {
            std::fs::write(&staged, &bytes).map_err(|e| e.to_string())?;
        }
        if !staged.exists() {
            return Err(format!("the download didn't contain {}", staged.display()));
        }
        std::fs::write(dir.join(".complete"), "").map_err(|e| e.to_string())?;
    }
    Ok(Some(Update { version, staged }))
}

/// Replaces the running app with the staged one. Returns the executable to relaunch.
pub fn install(u: &Update) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    if cfg!(target_os = "macos") {
        let bundle = exe.ancestors().nth(3).filter(|p| p.extension().is_some_and(|e| e == "app")).ok_or("study isn't running from an app bundle; reinstall it from a release to get updates")?.to_path_buf();
        let old = bundle.with_extension("app.old");
        let _ = std::fs::remove_dir_all(&old);
        std::fs::rename(&bundle, &old).map_err(|e| format!("could not move {} aside: {e}", bundle.display()))?;
        let copied = std::process::Command::new("ditto").arg(&u.staged).arg(&bundle).status();
        if !copied.is_ok_and(|s| s.success()) {
            let _ = std::fs::remove_dir_all(&bundle);
            let _ = std::fs::rename(&old, &bundle);
            return Err("could not copy the new version into place".into());
        }
        let _ = std::fs::remove_dir_all(&old);
        Ok(bundle.join("Contents/MacOS/study"))
    } else {
        let old = exe.with_file_name(format!("study.old-{}.exe", std::process::id()));
        std::fs::rename(&exe, &old).map_err(|e| format!("could not move {} aside: {e}", exe.display()))?;
        if let Err(e) = std::fs::copy(&u.staged, &exe) {
            let _ = std::fs::rename(&old, &exe);
            return Err(format!("could not copy the new version into place: {e}"));
        }
        Ok(exe)
    }
}

/// Starts the (new) app, detached from this process.
pub fn relaunch(exe: &Path, file: Option<&Path>) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    if let Some(f) = file {
        cmd.arg(f);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008); // DETACHED_PROCESS
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

fn pending_file() -> PathBuf {
    updates_dir().join("pending.json")
}

/// "Install on next launch".
pub fn set_pending(u: &Update) {
    let _ = std::fs::create_dir_all(updates_dir());
    let _ = std::fs::write(pending_file(), json!({"version": u.version, "staged": u.staged}).to_string());
}

/// At startup: installs an update deferred to this launch. Returns the executable to relaunch
/// into (the caller should exit), or None to continue starting normally.
pub fn install_pending() -> Option<PathBuf> {
    let text = std::fs::read_to_string(pending_file()).ok()?;
    let _ = std::fs::remove_file(pending_file());
    let v: Value = serde_json::from_str(&text).ok()?;
    let u = Update { version: v["version"].as_str()?.to_string(), staged: PathBuf::from(v["staged"].as_str()?) };
    if !newer(&u.version, current()) || !u.staged.exists() {
        return None;
    }
    install(&u).ok()
}

/// Removes leftovers from earlier updates (old Windows exes, installed staging folders).
pub fn cleanup() {
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(Path::to_path_buf)) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("study.old-") && name.ends_with(".exe") {
                let _ = std::fs::remove_file(entry.path()); // fails harmlessly while that process still runs
            }
        }
    }
    for entry in std::fs::read_dir(updates_dir()).into_iter().flatten().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if entry.path().is_dir() && !newer(&name, current()) {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn versions_compare_numerically() {
        assert!(super::newer("0.10.0", "0.9.3"));
        assert!(super::newer("v1.0.0", "0.99.0"));
        assert!(!super::newer("0.1.0", "0.1.0"));
        assert!(!super::newer("0.1.0", "0.2.0"));
    }
}
