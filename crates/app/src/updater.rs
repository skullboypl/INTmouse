//! Update client, modelled on LiveDelay: the site publishes `version.json` (a cached mirror of
//! the newest GitHub release), the app reads it, downloads the exe and installs it only after
//! its SHA-256 matches. No account, no licence, no telemetry: the request carries nothing but
//! the standard HTTP headers.
//!
//! * `internet.check_updates = false` = ZERO requests.
//! * Auto-install is OFF unless `internet.auto_install = true`; otherwise the user presses
//!   "Aktualizuj" in the banner.
//! * A release without a verified exe (no `exe`/`sha256` in `version.json`) is only announced:
//!   the banner then opens the release page instead of installing.
//! * Everything here blocks: call from a background thread, never from the input hot path.
//!
//! Swap sequence (Windows allows renaming a running exe, not overwriting it):
//! `install_on_restart` copies the verified download to `<exe>.new` (+ a
//! `.new.sha256` sidecar). On the next start `apply_pending_on_startup` renames
//! `<exe>` → `<exe>.old` (kept for rollback), `<exe>.new` → `<exe>`, then the
//! caller relaunches with `restart_into`. `rollback` swaps `.old` back.

use crate::http::{Transport, UreqTransport};
use anyhow::{anyhow, bail, Context, Result};
use inputshare_core::{AppConfig, InternetConfig};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Where the site publishes the newest release (cached mirror of the GitHub release).
pub const UPDATE_URL: &str = "https://intmouse.vxh.pl/version.json";
/// Overrides [`UPDATE_URL`] (local testing against a dev server).
pub const ENV_UPDATE_URL: &str = "INTMOUSE_UPDATE_URL";
/// Upper bound for a downloaded installer.
pub const MAX_DOWNLOAD_BYTES: u64 = 256 * 1024 * 1024;

/// What the update banner needs to know about a newer release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateInfo {
    pub latest: String,
    /// Release page, opened when the exe cannot be installed automatically.
    pub page: String,
    /// Exe download address; empty = nothing safe to install (see [`UpdateInfo::can_download`]).
    pub url: String,
    pub sha256: String,
    pub notes: String,
}

impl UpdateInfo {
    /// `true` only for an https (or loopback) exe with a well-formed SHA-256.
    pub fn can_download(&self) -> bool {
        is_acceptable_download_url(&self.url) && is_sha256_hex(&self.sha256)
    }
}

/// `version.json` as the site serves it. Every field is optional on the wire.
#[derive(Debug, Deserialize)]
struct VersionDoc {
    #[serde(default)]
    version: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    exe: Option<String>,
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateStatus {
    /// Disabled by the user (`internet.check_updates = false`). No request was made.
    Off,
    UpToDate,
    Available(UpdateInfo),
    /// Downloaded, verified and staged; applied on next start.
    Staged(UpdateInfo),
    Error(String),
}

impl UpdateStatus {
    /// Short Polish status line for the UI.
    pub fn note(&self) -> String {
        match self {
            UpdateStatus::Off => "Aktualizacje wyłączone".into(),
            UpdateStatus::UpToDate => "Masz najnowszą wersję".into(),
            UpdateStatus::Available(i) => format!("Dostępna wersja {}", i.latest),
            UpdateStatus::Staged(i) => {
                format!("Wersja {} pobrana — zainstaluje się po restarcie", i.latest)
            }
            UpdateStatus::Error(e) => format!("Błąd sprawdzania aktualizacji: {e}"),
        }
    }
}

pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// The address of `version.json`: `INTMOUSE_UPDATE_URL` when set and acceptable, else [`UPDATE_URL`].
pub fn update_url() -> String {
    std::env::var(ENV_UPDATE_URL)
        .ok()
        .map(|u| u.trim().to_string())
        .filter(|u| is_acceptable_download_url(u))
        .unwrap_or_else(|| UPDATE_URL.to_string())
}

/// `(numeric parts, pre-release text)`; `v` prefix and `+build` ignored.
fn parse_version(v: &str) -> Option<(Vec<u64>, Option<String>)> {
    let v = v.trim().trim_start_matches(['v', 'V']);
    let v = v.split('+').next()?;
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p.to_string())),
        None => (v, None),
    };
    let nums: Option<Vec<u64>> = core.split('.').map(|p| p.parse().ok()).collect();
    let nums = nums?;
    (!nums.is_empty()).then_some((nums, pre))
}

/// Semver order of two pre-release texts (`beta.9` < `beta.10` < `rc.1`): dot-separated
/// identifiers, numbers compare numerically and sort before words, a longer list wins a tie.
fn cmp_prerelease(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering::{Equal, Greater, Less};
    for (x, y) in a.split('.').zip(b.split('.')) {
        let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(i), Ok(j)) => i.cmp(&j),
            (Ok(_), Err(_)) => Less,
            (Err(_), Ok(_)) => Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if ord != Equal {
            return ord;
        }
    }
    a.split('.').count().cmp(&b.split('.').count())
}

/// `true` when `latest` is strictly newer than `current` (unparsable → `false`).
pub fn is_newer(latest: &str, current: &str) -> bool {
    let (Some((mut a, apre)), Some((mut b, bpre))) = (parse_version(latest), parse_version(current)) else {
        return false;
    };
    let n = a.len().max(b.len());
    a.resize(n, 0);
    b.resize(n, 0);
    match a.cmp(&b) {
        std::cmp::Ordering::Greater => true,
        std::cmp::Ordering::Less => false,
        // same numbers: a release is newer than its own pre-releases, betas compare by number
        std::cmp::Ordering::Equal => match (apre, bpre) {
            (None, Some(_)) => true,
            (Some(x), Some(y)) => cmp_prerelease(&x, &y) == std::cmp::Ordering::Greater,
            _ => false,
        },
    }
}

fn is_sha256_hex(s: &str) -> bool {
    s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// https only; plain http solely for loopback (local development server).
pub fn is_acceptable_download_url(url: &str) -> bool {
    if url.starts_with("https://") {
        return true;
    }
    ["http://localhost", "http://127.0.0.1", "http://[::1]"]
        .iter()
        .any(|p| url.strip_prefix(p).is_some_and(|rest| rest.is_empty() || rest.starts_with([':', '/'])))
}

// ───────────────────────────── check ─────────────────────────────

/// Check using the real config and the running version. Blocking; call from a background thread.
pub fn check(cfg: &AppConfig) -> UpdateStatus {
    check_with(&UreqTransport::api(), &update_url(), &cfg.internet, current_version())
}

pub fn check_with(
    transport: &dyn Transport,
    version_url: &str,
    internet: &InternetConfig,
    current: &str,
) -> UpdateStatus {
    if !internet.check_updates {
        return UpdateStatus::Off;
    }
    let resp = match transport.get(version_url) {
        Ok(r) => r,
        Err(e) => return UpdateStatus::Error(e),
    };
    if !resp.is_success() {
        return UpdateStatus::Error(format!("HTTP {}", resp.status));
    }
    match parse_check_response(&resp.body, current) {
        Ok(Some(info)) => UpdateStatus::Available(info),
        Ok(None) => UpdateStatus::UpToDate,
        Err(e) => UpdateStatus::Error(e.to_string()),
    }
}

/// `Ok(None)` = not newer (also when the site has no release yet and sends an empty version).
/// A newer version needs something to act on: a release page or a verified exe.
pub fn parse_check_response(body: &str, current: &str) -> Result<Option<UpdateInfo>> {
    let doc: VersionDoc = serde_json::from_str(body).context("invalid update response")?;
    if !is_newer(&doc.version, current) {
        return Ok(None);
    }
    let page = if doc.url.starts_with("https://") { doc.url } else { String::new() };
    let mut info = UpdateInfo {
        latest: doc.version.trim().trim_start_matches(['v', 'V']).to_string(),
        page,
        url: doc.exe.unwrap_or_default(),
        sha256: doc.sha256.unwrap_or_default().trim().to_string(),
        notes: doc.notes.unwrap_or_default(),
    };
    // An exe without a usable hash or on a plain-http host is never installed, only announced.
    if !info.can_download() {
        info.url.clear();
        info.sha256.clear();
    }
    if info.page.is_empty() && info.url.is_empty() {
        bail!("incomplete update response (no release page and no verified exe)");
    }
    Ok(Some(info))
}

// ───────────────────────────── download + verify ─────────────────────────────

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut f = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

/// `Ok(())` only when the file's SHA-256 equals `expected_hex` (case-insensitive).
pub fn verify_sha256(path: &Path, expected_hex: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    if actual.eq_ignore_ascii_case(expected_hex.trim()) {
        Ok(())
    } else {
        bail!("SHA-256 mismatch (expected {}, got {actual})", expected_hex.trim())
    }
}

fn safe_name(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect()
}

/// Download to the temp dir and verify. Returns the verified file's path.
pub fn download_and_verify(info: &UpdateInfo) -> Result<PathBuf> {
    let dir = std::env::temp_dir().join("inputshare-update");
    download_and_verify_with(&UreqTransport::download_agent(), info, &dir)
}

pub fn download_and_verify_with(transport: &dyn Transport, info: &UpdateInfo, dir: &Path) -> Result<PathBuf> {
    if !is_sha256_hex(&info.sha256) {
        bail!("refusing to download without a valid sha256");
    }
    if !is_acceptable_download_url(&info.url) {
        bail!("refusing non-https download url");
    }
    std::fs::create_dir_all(dir)?;
    let name = format!("INTmouse-{}.exe", safe_name(&info.latest));
    let final_path = dir.join(&name);
    let part = dir.join(format!("{name}.part"));
    let _ = std::fs::remove_file(&part);
    let _ = std::fs::remove_file(&final_path);

    transport
        .download(&info.url, &part, MAX_DOWNLOAD_BYTES)
        .map_err(|e| anyhow!("download failed: {e}"))?;
    if let Err(e) = verify_sha256(&part, &info.sha256) {
        let _ = std::fs::remove_file(&part);
        return Err(e);
    }
    std::fs::rename(&part, &final_path)?;
    Ok(final_path)
}

// ───────────────────────────── swap / rollback ─────────────────────────────

fn with_suffix(exe: &Path, suffix: &str) -> PathBuf {
    let mut s = exe.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

pub fn new_path(exe: &Path) -> PathBuf {
    with_suffix(exe, ".new")
}
pub fn old_path(exe: &Path) -> PathBuf {
    with_suffix(exe, ".old")
}
fn sidecar_path(exe: &Path) -> PathBuf {
    with_suffix(exe, ".new.sha256")
}

/// Stage a verified download next to the running exe (applied on next start).
pub fn install_on_restart(downloaded: &Path) -> Result<()> {
    stage_update(downloaded, &std::env::current_exe()?)
}

pub fn stage_update(downloaded: &Path, exe: &Path) -> Result<()> {
    let sha = sha256_file(downloaded)?;
    let new = new_path(exe);
    let tmp = with_suffix(exe, ".new.tmp");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(downloaded, &tmp).with_context(|| format!("write {}", tmp.display()))?;
    let _ = std::fs::remove_file(&new);
    std::fs::rename(&tmp, &new)?;
    std::fs::write(sidecar_path(exe), &sha)?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApplyOutcome {
    NothingPending,
    /// Swap done; relaunch `exe` (see [`restart_into`]) and exit this process.
    Applied { exe: PathBuf },
}

/// Call very early in `main`, before any UI or hook is created.
pub fn apply_pending_on_startup() -> Result<ApplyOutcome> {
    apply_pending(&std::env::current_exe()?)
}

pub fn apply_pending(exe: &Path) -> Result<ApplyOutcome> {
    let new = new_path(exe);
    if !new.exists() {
        return Ok(ApplyOutcome::NothingPending);
    }
    let sidecar = sidecar_path(exe);
    // Never run something that was not verified by `stage_update`.
    let expected = std::fs::read_to_string(&sidecar).unwrap_or_default();
    if let Err(e) = verify_sha256(&new, &expected) {
        let _ = std::fs::remove_file(&new);
        let _ = std::fs::remove_file(&sidecar);
        return Err(e.context("staged update failed verification and was discarded"));
    }

    let old = old_path(exe);
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).context("could not move current exe to .old")?;
    if let Err(e) = std::fs::rename(&new, exe) {
        // Put the previous version back so the app still starts.
        let _ = std::fs::rename(&old, exe);
        return Err(anyhow!(e).context("could not move new exe into place (restored previous)"));
    }
    let _ = std::fs::remove_file(&sidecar);
    Ok(ApplyOutcome::Applied { exe: exe.to_path_buf() })
}

/// Restore the previous version (`.old`). `Ok(false)` = nothing to roll back to.
/// The replaced build is kept as `.bad`. Restart the app afterwards.
pub fn rollback() -> Result<bool> {
    rollback_at(&std::env::current_exe()?)
}

pub fn rollback_at(exe: &Path) -> Result<bool> {
    let old = old_path(exe);
    if !old.exists() {
        return Ok(false);
    }
    let bad = with_suffix(exe, ".bad");
    let _ = std::fs::remove_file(&bad);
    let had_exe = exe.exists();
    if had_exe {
        std::fs::rename(exe, &bad)?;
    }
    if let Err(e) = std::fs::rename(&old, exe) {
        if had_exe {
            let _ = std::fs::rename(&bad, exe);
        }
        return Err(anyhow!(e).context("rollback failed"));
    }
    Ok(true)
}

/// Spawn `exe` again with the current arguments; the caller must then exit.
pub fn restart_into(exe: &Path) -> Result<()> {
    std::process::Command::new(exe)
        .args(std::env::args_os().skip(1))
        .spawn()
        .with_context(|| format!("could not relaunch {}", exe.display()))?;
    Ok(())
}

// ───────────────────────────── check + (optional) auto-install ─────────────────────────────

/// Check, and only when `internet.auto_install` is on also download, verify and
/// stage the update. Blocking; background thread only.
pub fn check_and_stage(cfg: &AppConfig) -> UpdateStatus {
    let exe = std::env::current_exe().unwrap_or_default();
    check_and_stage_with(
        &UreqTransport::api(),
        &UreqTransport::download_agent(),
        &update_url(),
        &cfg.internet,
        current_version(),
        &std::env::temp_dir().join("inputshare-update"),
        &exe,
    )
}

pub fn check_and_stage_with(
    api: &dyn Transport,
    downloader: &dyn Transport,
    version_url: &str,
    internet: &InternetConfig,
    current: &str,
    dl_dir: &Path,
    exe: &Path,
) -> UpdateStatus {
    let status = check_with(api, version_url, internet, current);
    let UpdateStatus::Available(info) = status else {
        return status;
    };
    if !internet.auto_install || !info.can_download() {
        return UpdateStatus::Available(info);
    }
    let staged = download_and_verify_with(downloader, &info, dl_dir).and_then(|p| {
        let r = stage_update(&p, exe);
        let _ = std::fs::remove_file(&p);
        r
    });
    match staged {
        Ok(()) => UpdateStatus::Staged(info),
        Err(e) => UpdateStatus::Error(format!("{e:#}")),
    }
}

// ───────────────────────────── tests ─────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::mock::MockTransport;

    const VERSION_URL: &str = "https://site.test/version.json";

    struct TempDir(PathBuf);
    impl TempDir {
        fn new(tag: &str) -> Self {
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let p = std::env::temp_dir().join(format!("inputshare-test-{tag}-{}-{nanos}", std::process::id()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sha_of(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn internet(check: bool, auto: bool) -> InternetConfig {
        InternetConfig { check_updates: check, auto_install: auto }
    }

    /// `version.json` of a release that ships a signed exe.
    fn reply_json(version: &str, bytes: &[u8]) -> String {
        format!(
            r#"{{"version":"{version}","tag":"v{version}","url":"https://github.com/o/r/releases/tag/v{version}","zip":null,"exe":"https://dl.test/INTmouse.exe","sha256":"{}","notes":"fixes"}}"#,
            sha_of(bytes)
        )
    }

    fn info_for(version: &str, bytes: &[u8]) -> UpdateInfo {
        parse_check_response(&reply_json(version, bytes), "0.0.1").unwrap().unwrap()
    }

    #[test]
    fn version_comparison() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v1.0.0", "0.9.9"));
        assert!(is_newer("0.1.1", "0.1"));
        assert!(is_newer("0.1.0", "0.1.0-beta.1"));
        assert!(!is_newer("0.1.0", "0.1.0"));
        assert!(!is_newer("0.1.0-beta", "0.1.0"));
        assert!(!is_newer("0.0.9", "0.1.0"));
        assert!(!is_newer("garbage", "0.1.0"));
        assert!(!is_newer("", "0.1.0"));
        assert!(!is_newer("1.0.0", "garbage"));
    }

    #[test]
    fn beta_versions_are_ordered_like_semver() {
        assert!(is_newer("0.2.0-beta.2", "0.2.0-beta.1"));
        assert!(is_newer("0.2.0-beta.10", "0.2.0-beta.9"));
        assert!(is_newer("0.2.0-rc.1", "0.2.0-beta.9"));
        assert!(is_newer("0.2.0-beta.1.1", "0.2.0-beta.1"));
        assert!(is_newer("0.2.0", "0.2.0-rc.1"));
        assert!(is_newer("v0.2.0-beta.1", "0.1.0"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0-beta.1"));
        assert!(!is_newer("0.2.0-beta.1", "0.2.0-beta.2"));
        assert!(!is_newer("0.2.0-rc.1", "0.2.0"));
        // the beta already installed is not offered again as a "newer" release of itself
        assert!(!is_newer("0.1.0-beta.4", "0.1.0"));
    }

    #[test]
    fn download_url_policy() {
        assert!(is_acceptable_download_url("https://x.test/a.exe"));
        assert!(is_acceptable_download_url("http://localhost:3000/a.exe"));
        assert!(is_acceptable_download_url("http://127.0.0.1/a.exe"));
        assert!(!is_acceptable_download_url("http://evil.test/a.exe"));
        assert!(!is_acceptable_download_url("http://localhost.evil.test/a.exe"));
        assert!(!is_acceptable_download_url("ftp://x.test/a.exe"));
    }

    #[test]
    fn update_url_defaults_to_the_site_mirror() {
        // The override variable is not set in the test environment.
        assert_eq!(UPDATE_URL, "https://intmouse.vxh.pl/version.json");
        assert!(is_acceptable_download_url(UPDATE_URL));
    }

    #[test]
    fn check_off_makes_zero_requests() {
        let net = MockTransport::new();
        let s = check_with(&net, VERSION_URL, &internet(false, false), "0.1.0");
        assert_eq!(s, UpdateStatus::Off);
        assert_eq!(net.call_count(), 0);
    }

    #[test]
    fn check_reads_version_json_and_reports_available() {
        let net = MockTransport::new();
        net.push_ok(200, &reply_json("0.2.0", b"exe"));
        let s = check_with(&net, VERSION_URL, &internet(true, false), "0.1.0");
        match s {
            UpdateStatus::Available(i) => {
                assert_eq!(i.latest, "0.2.0");
                assert_eq!(i.notes, "fixes");
                assert_eq!(i.page, "https://github.com/o/r/releases/tag/v0.2.0");
                assert!(i.can_download());
            }
            other => panic!("unexpected {other:?}"),
        }
        assert_eq!(net.calls.lock().unwrap()[0], "GET https://site.test/version.json");
        assert_eq!(net.call_count(), 1, "one plain GET, nothing else is sent");
    }

    #[test]
    fn up_to_date_empty_release_and_failures() {
        let cfg = internet(true, false);
        let net = MockTransport::new();
        net.push_ok(200, r#"{"version":"0.1.0"}"#);
        assert_eq!(check_with(&net, VERSION_URL, &cfg, "0.1.0"), UpdateStatus::UpToDate);
        // the site has no release yet: empty version means nothing to do
        net.push_ok(200, r#"{"version":""}"#);
        assert_eq!(check_with(&net, VERSION_URL, &cfg, "0.1.0"), UpdateStatus::UpToDate);
        net.push_ok(503, "");
        assert!(matches!(check_with(&net, VERSION_URL, &cfg, "0.1.0"), UpdateStatus::Error(_)));
        net.push_err("dns");
        assert!(matches!(check_with(&net, VERSION_URL, &cfg, "0.1.0"), UpdateStatus::Error(_)));
        net.push_ok(200, "not json");
        assert!(matches!(check_with(&net, VERSION_URL, &cfg, "0.1.0"), UpdateStatus::Error(_)));
    }

    #[test]
    fn newer_release_without_exe_is_announced_with_its_page_only() {
        let body = r#"{"version":"0.9.0","url":"https://github.com/o/r/releases/tag/v0.9.0","exe":null,"sha256":null}"#;
        let info = parse_check_response(body, "0.1.0").unwrap().unwrap();
        assert!(!info.can_download());
        assert_eq!(info.page, "https://github.com/o/r/releases/tag/v0.9.0");
        assert!(info.url.is_empty());
    }

    #[test]
    fn exe_without_a_valid_hash_or_over_plain_http_is_never_offered_for_install() {
        let page = r#""url":"https://github.com/o/r/releases/tag/v0.9.0""#;
        let no_hash = format!(r#"{{"version":"0.9.0",{page},"exe":"https://dl.test/x.exe"}}"#);
        let short_hash = format!(r#"{{"version":"0.9.0",{page},"exe":"https://dl.test/x.exe","sha256":"abc"}}"#);
        let http = format!(
            r#"{{"version":"0.9.0",{page},"exe":"http://evil.test/x.exe","sha256":"{}"}}"#,
            sha_of(b"x")
        );
        for body in [no_hash, short_hash, http] {
            let info = parse_check_response(&body, "0.1.0").unwrap().unwrap();
            assert!(!info.can_download(), "{body}");
            assert!(info.url.is_empty() && info.sha256.is_empty(), "{body}");
            assert!(!info.page.is_empty());
        }
    }

    #[test]
    fn newer_release_with_nothing_to_act_on_is_an_error() {
        assert!(parse_check_response(r#"{"version":"0.9.0"}"#, "0.1.0").is_err());
        // a plain-http page is not trusted either
        assert!(parse_check_response(r#"{"version":"0.9.0","url":"http://evil.test/p"}"#, "0.1.0").is_err());
    }

    #[test]
    fn sha256_verification_on_local_file() {
        let d = TempDir::new("sha");
        let f = d.path().join("a.bin");
        std::fs::write(&f, b"hello").unwrap();
        // well-known SHA-256("hello")
        let good = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        assert_eq!(sha256_file(&f).unwrap(), good);
        assert!(verify_sha256(&f, good).is_ok());
        assert!(verify_sha256(&f, &good.to_ascii_uppercase()).is_ok());
        assert!(verify_sha256(&f, &"0".repeat(64)).is_err());
        assert!(verify_sha256(&d.path().join("missing"), good).is_err());
    }

    #[test]
    fn download_and_verify_accepts_matching_hash() {
        let d = TempDir::new("dl-ok");
        let net = MockTransport::new();
        *net.download_bytes.lock().unwrap() = b"new-exe-bytes".to_vec();
        let info = info_for("0.2.0", b"new-exe-bytes");
        let p = download_and_verify_with(&net, &info, d.path()).unwrap();
        assert_eq!(std::fs::read(&p).unwrap(), b"new-exe-bytes");
        assert!(!d.path().join("inputshare-0.2.0.exe.part").exists());
    }

    #[test]
    fn download_and_verify_rejects_tampered_file_and_leaves_nothing() {
        let d = TempDir::new("dl-bad");
        let net = MockTransport::new();
        *net.download_bytes.lock().unwrap() = b"TAMPERED".to_vec();
        let info = info_for("0.2.0", b"new-exe-bytes");
        let err = download_and_verify_with(&net, &info, d.path()).unwrap_err();
        assert!(err.to_string().contains("mismatch"), "{err}");
        assert_eq!(std::fs::read_dir(d.path()).unwrap().count(), 0);
    }

    #[test]
    fn download_refuses_bad_url_or_hash_before_any_request() {
        let d = TempDir::new("dl-pre");
        let net = MockTransport::new();
        let mut info = UpdateInfo {
            latest: "0.2.0".into(),
            page: String::new(),
            url: "http://evil.test/x".into(),
            sha256: sha_of(b"x"),
            notes: String::new(),
        };
        assert!(download_and_verify_with(&net, &info, d.path()).is_err());
        info.url = "https://ok.test/x".into();
        info.sha256 = "abc".into();
        assert!(download_and_verify_with(&net, &info, d.path()).is_err());
        assert_eq!(net.call_count(), 0);
    }

    fn fake_install(dir: &Path) -> PathBuf {
        let exe = dir.join("INTmouse.exe");
        std::fs::write(&exe, b"v1").unwrap();
        exe
    }

    #[test]
    fn stage_apply_and_rollback_keep_previous_version() {
        let d = TempDir::new("swap");
        let exe = fake_install(d.path());
        let dl = d.path().join("downloaded.exe");
        std::fs::write(&dl, b"v2").unwrap();

        assert_eq!(apply_pending(&exe).unwrap(), ApplyOutcome::NothingPending);

        stage_update(&dl, &exe).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), b"v1", "staging must not touch the live exe");
        assert!(new_path(&exe).exists());

        assert_eq!(apply_pending(&exe).unwrap(), ApplyOutcome::Applied { exe: exe.clone() });
        assert_eq!(std::fs::read(&exe).unwrap(), b"v2");
        assert_eq!(std::fs::read(old_path(&exe)).unwrap(), b"v1");
        assert!(!new_path(&exe).exists());
        assert_eq!(apply_pending(&exe).unwrap(), ApplyOutcome::NothingPending);

        assert!(rollback_at(&exe).unwrap());
        assert_eq!(std::fs::read(&exe).unwrap(), b"v1");
        assert_eq!(std::fs::read(with_suffix(&exe, ".bad")).unwrap(), b"v2");
        assert!(!rollback_at(&exe).unwrap(), "nothing left to roll back to");
    }

    #[test]
    fn tampered_staged_file_is_discarded_not_applied() {
        let d = TempDir::new("tamper");
        let exe = fake_install(d.path());
        let dl = d.path().join("downloaded.exe");
        std::fs::write(&dl, b"v2").unwrap();
        stage_update(&dl, &exe).unwrap();
        std::fs::write(new_path(&exe), b"evil").unwrap();
        assert!(apply_pending(&exe).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"v1");
        assert!(!new_path(&exe).exists());
        assert!(!old_path(&exe).exists());
    }

    #[test]
    fn staged_file_without_sidecar_is_not_applied() {
        let d = TempDir::new("nosidecar");
        let exe = fake_install(d.path());
        std::fs::write(new_path(&exe), b"v2").unwrap();
        assert!(apply_pending(&exe).is_err());
        assert_eq!(std::fs::read(&exe).unwrap(), b"v1");
    }

    #[test]
    fn auto_install_off_by_default_downloads_nothing() {
        assert!(!InternetConfig::default().auto_install);
        let d = TempDir::new("auto-off");
        let exe = fake_install(d.path());
        let api = MockTransport::new();
        api.push_ok(200, &reply_json("0.2.0", b"v2"));
        let dl = MockTransport::new();
        *dl.download_bytes.lock().unwrap() = b"v2".to_vec();
        let s = check_and_stage_with(&api, &dl, VERSION_URL, &internet(true, false), "0.1.0", &d.path().join("dl"), &exe);
        assert!(matches!(s, UpdateStatus::Available(_)));
        assert_eq!(dl.call_count(), 0);
        assert!(!new_path(&exe).exists());
    }

    #[test]
    fn auto_install_on_stages_verified_update() {
        let d = TempDir::new("auto-on");
        let exe = fake_install(d.path());
        let api = MockTransport::new();
        api.push_ok(200, &reply_json("0.2.0", b"v2"));
        let dl = MockTransport::new();
        *dl.download_bytes.lock().unwrap() = b"v2".to_vec();
        let s = check_and_stage_with(&api, &dl, VERSION_URL, &internet(true, true), "0.1.0", &d.path().join("dl"), &exe);
        assert!(matches!(s, UpdateStatus::Staged(_)), "{s:?}");
        assert_eq!(std::fs::read(new_path(&exe)).unwrap(), b"v2");
        assert_eq!(std::fs::read(&exe).unwrap(), b"v1");
    }

    #[test]
    fn auto_install_with_wrong_hash_reports_error_and_stages_nothing() {
        let d = TempDir::new("auto-bad");
        let exe = fake_install(d.path());
        let api = MockTransport::new();
        api.push_ok(200, &reply_json("0.2.0", b"v2"));
        let dl = MockTransport::new();
        *dl.download_bytes.lock().unwrap() = b"other".to_vec();
        let s = check_and_stage_with(&api, &dl, VERSION_URL, &internet(true, true), "0.1.0", &d.path().join("dl"), &exe);
        assert!(matches!(s, UpdateStatus::Error(_)), "{s:?}");
        assert!(!new_path(&exe).exists());
    }

    #[test]
    fn auto_install_never_downloads_a_release_without_a_verified_exe() {
        let d = TempDir::new("auto-page");
        let exe = fake_install(d.path());
        let api = MockTransport::new();
        api.push_ok(200, r#"{"version":"0.2.0","url":"https://github.com/o/r/releases/tag/v0.2.0"}"#);
        let dl = MockTransport::new();
        let s = check_and_stage_with(&api, &dl, VERSION_URL, &internet(true, true), "0.1.0", &d.path().join("dl"), &exe);
        assert!(matches!(s, UpdateStatus::Available(_)), "{s:?}");
        assert_eq!(dl.call_count(), 0);
    }
}
