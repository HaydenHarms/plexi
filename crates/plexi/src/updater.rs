//! Update checks against GitHub releases, and installing an update.
//!
//! On launch Plexi asks the GitHub API for the latest release. If it is newer, the user
//! is offered the update. Installing depends on how this copy was installed:
//!
//! - Inno Setup install (an `unins000.exe` beside the exe): download the new setup.exe
//!   and run it silently; it closes Plexi, installs over it and starts it again.
//! - Portable exe or zip: download the new portable exe, move the running exe aside
//!   (Windows allows renaming a running exe), put the new one in its place and restart.
//! - MSI installs and macOS: open the release page so the right file can be downloaded.
//!
//! Every download is checked against the release's `SHA256SUMS.txt` before it is used.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const REPO: &str = "HaydenHarms/plexi";
const USER_AGENT: &str = concat!("plexi/", env!("CARGO_PKG_VERSION"), " (update check)");

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    /// Version without the leading `v`, e.g. `0.3.0`.
    pub version: String,
    /// The release page.
    pub page: String,
    /// (file name, download URL) of every attached file.
    pub assets: Vec<(String, String)>,
}

impl Release {
    fn asset(&self, name: &str) -> Option<&str> {
        self.assets.iter().find(|(n, _)| n == name).map(|(_, u)| u.as_str())
    }
}

/// How this copy of Plexi was installed, which decides how it updates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InstallKind {
    /// Inno Setup; `all_users` when it lives under Program Files.
    Setup { all_users: bool },
    /// The portable exe or zip.
    Portable,
    /// An MSI, a Mac app, or a build run from source: update by hand.
    Manual,
}

pub fn install_kind() -> InstallKind {
    let Ok(exe) = std::env::current_exe() else { return InstallKind::Manual };
    if !cfg!(windows) {
        return InstallKind::Manual;
    }
    let dir = exe.parent().unwrap_or(Path::new("."));
    if dir.join("unins000.exe").exists() {
        let pf = std::env::var_os("ProgramFiles").map(PathBuf::from);
        let all_users = pf.is_some_and(|pf| exe.starts_with(pf));
        return InstallKind::Setup { all_users };
    }
    if crate::config::is_portable() {
        return InstallKind::Portable;
    }
    InstallKind::Manual
}

/// Whether version `a` is newer than `b` (dotted numbers; anything after `-` is a
/// pre-release and sorts before the plain version).
pub fn newer(a: &str, b: &str) -> bool {
    fn parse(v: &str) -> (Vec<u64>, bool) {
        let v = v.trim().trim_start_matches('v');
        let (core, pre) = match v.split_once('-') {
            Some((c, _)) => (c, true),
            None => (v, false),
        };
        let mut nums: Vec<u64> = core.split('.').map(|p| p.parse().unwrap_or(0)).collect();
        nums.resize(3, 0);
        (nums, pre)
    }
    let ((na, pa), (nb, pb)) = (parse(a), parse(b));
    match na.cmp(&nb) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Equal => pb && !pa,
    }
}

fn agent(timeout: Option<Duration>) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_global(timeout)
        .user_agent(USER_AGENT)
        .build()
        .into()
}

fn get_text(url: &str) -> Result<String, String> {
    let mut resp = agent(Some(Duration::from_secs(20)))
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| e.to_string())?;
    resp.body_mut().read_to_string().map_err(|e| e.to_string())
}

/// The latest published release, if it is newer than this build.
pub fn check() -> Result<Option<Release>, String> {
    let body = get_text(&format!("https://api.github.com/repos/{REPO}/releases/latest"))?;
    let release = parse_release(&body)?;
    Ok(newer(&release.version, env!("CARGO_PKG_VERSION")).then_some(release))
}

fn parse_release(json: &str) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("release has no tag")?;
    let assets = v["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| Some((x["name"].as_str()?.to_owned(), x["browser_download_url"].as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();
    Ok(Release {
        version: tag.trim_start_matches('v').to_owned(),
        page: v["html_url"].as_str().unwrap_or("https://github.com/HaydenHarms/plexi/releases").to_owned(),
        assets,
    })
}

/// The file this install updates from, if it can update itself.
pub fn asset_name(kind: InstallKind, version: &str) -> Option<String> {
    match kind {
        InstallKind::Setup { .. } => Some(format!("Plexi-{version}-setup.exe")),
        InstallKind::Portable => Some(format!("Plexi-{version}-portable.exe")),
        InstallKind::Manual => None,
    }
}

/// The SHA-256 the release lists for `name` in its `SHA256SUMS.txt`.
fn expected_hash(release: &Release, name: &str) -> Result<String, String> {
    let url = release.asset("SHA256SUMS.txt").ok_or("the release has no checksums")?;
    let sums = get_text(url)?;
    sums.lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .find(|(_, file)| file.trim().trim_start_matches('*') == name)
        .map(|(hash, _)| hash.trim().to_ascii_lowercase())
        .ok_or_else(|| format!("no checksum for {name}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Download the update for `kind` into the temp folder and verify it. `progress`
/// gets the fraction done (0..=1) as it goes.
pub fn download(release: &Release, kind: InstallKind, mut progress: impl FnMut(f32)) -> Result<PathBuf, String> {
    use sha2::{Digest, Sha256};
    let name = asset_name(kind, &release.version).ok_or("this copy of Plexi can't update itself")?;
    let url = release.asset(&name).ok_or_else(|| format!("the release has no {name}"))?;
    let want = expected_hash(release, &name)?;

    let mut resp = agent(None).get(url).call().map_err(|e| e.to_string())?;
    let total: Option<u64> =
        resp.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok());
    let dir = std::env::temp_dir().join("plexi-update");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let path = dir.join(&name);
    let mut file = std::fs::File::create(&path).map_err(|e| e.to_string())?;
    let mut reader = resp.body_mut().as_reader();
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut done = 0u64;
    loop {
        let n = reader.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if let Some(t) = total.filter(|t| *t > 0) {
            progress((done as f32 / t as f32).min(1.0));
        }
    }
    file.flush().map_err(|e| e.to_string())?;
    drop(file);
    let got = hex(&hasher.finalize());
    if got != want {
        let _ = std::fs::remove_file(&path);
        return Err("the download didn't match its checksum".into());
    }
    Ok(path)
}

/// Start installing the downloaded update. On success the caller should quit right
/// away: the installer or the new exe takes over.
pub fn apply(kind: InstallKind, downloaded: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    match kind {
        InstallKind::Setup { all_users } => {
            let mut cmd = std::process::Command::new(downloaded);
            cmd.args(["/SILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CLOSEAPPLICATIONS", "/relaunch=1"]);
            cmd.arg(if all_users { "/ALLUSERS" } else { "/CURRENTUSER" });
            cmd.spawn().map_err(|e| format!("couldn't start the installer: {e}"))?;
        }
        InstallKind::Portable => {
            let old = old_exe_path(&exe);
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&exe, &old).map_err(|e| format!("couldn't replace {}: {e}", exe.display()))?;
            if let Err(e) = std::fs::copy(downloaded, &exe) {
                let _ = std::fs::rename(&old, &exe); // put the working copy back
                return Err(format!("couldn't write the new version: {e}"));
            }
            std::process::Command::new(&exe).spawn().map_err(|e| format!("couldn't restart Plexi: {e}"))?;
        }
        InstallKind::Manual => return Err("this copy of Plexi can't update itself".into()),
    }
    Ok(())
}

fn old_exe_path(exe: &Path) -> PathBuf {
    exe.with_extension("old.exe")
}

/// Remove the previous exe a portable update left behind.
pub fn clean_up_after_update() {
    if let Ok(exe) = std::env::current_exe() {
        let _ = std::fs::remove_file(old_exe_path(&exe));
    }
    let _ = std::fs::remove_dir_all(std::env::temp_dir().join("plexi-update"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare() {
        assert!(newer("0.3.0", "0.2.0"));
        assert!(newer("v0.2.1", "0.2.0"));
        assert!(newer("1.0.0", "0.99.9"));
        assert!(newer("0.10.0", "0.9.0"), "numeric, not alphabetical");
        assert!(!newer("0.2.0", "0.2.0"));
        assert!(!newer("0.1.9", "0.2.0"));
        assert!(newer("0.3.0", "0.3.0-beta.1"), "a release beats its pre-release");
        assert!(!newer("0.3.0-beta.1", "0.3.0"));
    }

    /// Talks to GitHub: `cargo test -p plexi -- --ignored live`.
    #[test]
    #[ignore]
    fn live_latest_release_and_checksums() {
        let body = get_text(&format!("https://api.github.com/repos/{REPO}/releases/latest")).unwrap();
        let r = parse_release(&body).unwrap();
        println!("latest {} with {} files", r.version, r.assets.len());
        for kind in [InstallKind::Setup { all_users: false }, InstallKind::Portable] {
            let name = asset_name(kind, &r.version).unwrap();
            assert!(r.asset(&name).is_some(), "{name} is attached");
            let hash = expected_hash(&r, &name).unwrap();
            assert_eq!(hash.len(), 64, "{name}: {hash}");
        }
    }

    /// Downloads the latest portable exe (~20 MB) and checks it against SHA256SUMS.txt.
    #[test]
    #[ignore]
    fn live_download_is_verified() {
        let body = get_text(&format!("https://api.github.com/repos/{REPO}/releases/latest")).unwrap();
        let r = parse_release(&body).unwrap();
        let mut last = 0.0;
        let path = download(&r, InstallKind::Portable, |f| last = f).unwrap();
        assert!(last > 0.99, "progress reached the end ({last})");
        assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
        std::fs::remove_file(path).ok();
    }

    #[test]
    fn release_json_parses() {
        let json = r#"{"tag_name":"v0.3.0","html_url":"https://github.com/HaydenHarms/plexi/releases/tag/v0.3.0",
            "assets":[{"name":"Plexi-0.3.0-setup.exe","browser_download_url":"https://x/setup.exe"},
                      {"name":"SHA256SUMS.txt","browser_download_url":"https://x/sums"}]}"#;
        let r = parse_release(json).unwrap();
        assert_eq!(r.version, "0.3.0");
        assert_eq!(r.asset("Plexi-0.3.0-setup.exe"), Some("https://x/setup.exe"));
        assert_eq!(asset_name(InstallKind::Portable, &r.version).as_deref(), Some("Plexi-0.3.0-portable.exe"));
        assert_eq!(asset_name(InstallKind::Manual, &r.version), None);
    }
}
