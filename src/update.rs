//! Self-updater: checks GitHub for a newer release and swaps in its exe.
//!
//! Windows lets a running exe be renamed (not overwritten), so installing is:
//! download `kennel.exe.new` next to us, rename ourselves to `kennel.exe.old`,
//! rename the download into place. The next launch deletes the `.old` file.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const REPO: &str = "CharoenwitKunna/kennel";
const ASSET: &str = "kennel.exe";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// e.g. "0.2.0" (tag without the leading "v").
    pub version: String,
    /// Direct download of the release's exe.
    pub download: String,
    /// The release page, for its notes.
    pub page: String,
}

fn curl() -> Command {
    let mut c = Command::new("curl.exe");
    c.args(["-sSfL", "--max-time", "120", "-H", "User-Agent: kennel-updater"]);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c
}

/// The latest release, if it is newer than this build. `Ok(None)` = up to date
/// (or no releases published yet).
pub fn check() -> Result<Option<Release>, String> {
    let url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let out = curl()
        .args(["-w", "\n%{http_code}", "-H", "Accept: application/vnd.github+json", &url])
        .output()
        .map_err(|e| format!("couldn't run curl: {e}"))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let (body, code) = text.rsplit_once('\n').unwrap_or(("", text.as_ref()));
    match code.trim() {
        "404" => return Ok(None), // no releases yet
        "200" => {}
        "" => return Err("no connection to GitHub".into()),
        c => return Err(format!("GitHub answered HTTP {c}")),
    }
    let v: Value = serde_json::from_str(body).map_err(|e| format!("bad reply from GitHub: {e}"))?;
    Ok(parse_release(&v).filter(|r| is_newer(&r.version, CURRENT)))
}

pub fn parse_release(v: &Value) -> Option<Release> {
    if v["draft"].as_bool() == Some(true) || v["prerelease"].as_bool() == Some(true) {
        return None;
    }
    let version = v["tag_name"].as_str()?.trim_start_matches(['v', 'V']).to_string();
    let download = v["assets"]
        .as_array()?
        .iter()
        .find(|a| a["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(ASSET)))?["browser_download_url"]
        .as_str()?
        .to_string();
    let page = v["html_url"].as_str().unwrap_or_default().to_string();
    Some(Release { version, download, page })
}

fn parts(v: &str) -> Vec<u64> {
    v.split(['.', '-', '+']).map_while(|p| p.parse().ok()).collect()
}

/// "0.10.0" > "0.9.3"; anything unparseable is not newer.
pub fn is_newer(remote: &str, local: &str) -> bool {
    let (r, l) = (parts(remote), parts(local));
    !r.is_empty() && r > l
}

fn sibling(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Download the release and put it where `exe` is. Takes effect on restart.
pub fn install(r: &Release, exe: &Path) -> Result<(), String> {
    let new = sibling(exe, ".new");
    let old = sibling(exe, ".old");
    let out = curl().arg("-o").arg(&new).arg(&r.download).output().map_err(|e| format!("couldn't run curl: {e}"))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&new);
        return Err(format!("download failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let head = std::fs::read(&new).map(|b| b.len() > 1_000_000 && b.starts_with(b"MZ")).unwrap_or(false);
    if !head {
        let _ = std::fs::remove_file(&new);
        return Err("the download isn't a Windows program".into());
    }
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("couldn't move the current version aside: {e}"))?;
    if let Err(e) = std::fs::rename(&new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("couldn't put the new version in place: {e}"));
    }
    Ok(())
}

/// Remove what a previous update left behind.
pub fn cleanup(exe: &Path) {
    let _ = std::fs::remove_file(sibling(exe, ".old"));
    let _ = std::fs::remove_file(sibling(exe, ".new"));
}

/// Start `exe` again once this process has exited (and saved its state).
pub fn relaunch_after_exit(exe: &Path) -> std::io::Result<()> {
    let pid = std::process::id();
    let path = exe.display().to_string().replace('\'', "''");
    let script = format!("Wait-Process -Id {pid} -ErrorAction SilentlyContinue; Start-Process -FilePath '{path}'");
    let mut c = Command::new("powershell.exe");
    c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    c.spawn().map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_compare() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("0.10.0", "0.9.3"));
        assert!(is_newer("1.0", "0.9.9"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("nightly", "0.1.0"));
    }

    #[test]
    fn release_from_github_json() {
        let v: Value = serde_json::from_str(
            r#"{"tag_name":"v0.2.0","draft":false,"prerelease":false,"html_url":"https://github.com/x/y/releases/tag/v0.2.0",
                "assets":[{"name":"notes.txt","browser_download_url":"a"},{"name":"kennel.exe","browser_download_url":"https://dl/kennel.exe"}]}"#,
        )
        .unwrap();
        let r = parse_release(&v).unwrap();
        assert_eq!((r.version.as_str(), r.download.as_str()), ("0.2.0", "https://dl/kennel.exe"));
        let no_exe: Value = serde_json::from_str(r#"{"tag_name":"v0.3.0","assets":[]}"#).unwrap();
        assert_eq!(parse_release(&no_exe), None);
    }
}
