// Updates from the GitHub releases of the launcher's public repository (REPO):
//
// - the mod: the most recently published release tagged "v<version>" ("v1.0.13.0", or
//   "v1.0.13.0-dev2" for a build of an unreleased version) with a RogueShard.sml asset. The file
//   goes to mods\RogueShard.sml and mods\RogueShard.version keeps the release's version. When the
//   mod is missing, or the version file is missing or differs, the release's mod is downloaded
//   (the user's rule, 21.11.2026: the mod is always the one of the newest release).
// - the launcher: the newest release tagged "launcher-<n>" with a .zip asset, n above
//   LAUNCHER_BUILD. Its Launcher.exe, rsr.exe, README.txt and patcher\ replace ours (the running
//   Launcher.exe is renamed to Launcher.old.exe first, which Windows allows, and is deleted on
//   the next start); mods\, launcher.ini, keys.ini and cache\ stay. Then the new launcher starts.
//
// No HTTP or JSON crate: curl.exe and tar.exe (in Windows 10 1803 and later) do the network
// and the zip, and the API's JSON is read by the small parser below.
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub const REPO: &str = "drake1ch/rogueshard-launcher";
// this launcher's own number, the N of its "launcher-N" release (and of its Discord post)
pub const LAUNCHER_BUILD: u32 = 5;
const AGENT: &str = "RogueShardLauncher";

// --- JSON -------------------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
pub enum J {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<J>),
    Obj(Vec<(String, J)>),
}

impl J {
    pub fn get(&self, k: &str) -> Option<&J> {
        match self {
            J::Obj(v) => v.iter().find(|(n, _)| n == k).map(|(_, j)| j),
            _ => None,
        }
    }
    pub fn str(&self) -> &str {
        match self {
            J::Str(s) => s,
            _ => "",
        }
    }
    pub fn truthy(&self) -> bool {
        matches!(self, J::Bool(true))
    }
    pub fn arr(&self) -> &[J] {
        match self {
            J::Arr(v) => v,
            _ => &[],
        }
    }
}

pub fn parse(s: &str) -> Option<J> {
    let b = s.as_bytes();
    let mut i = 0;
    let v = value(b, &mut i)?;
    ws(b, &mut i);
    (i == b.len()).then_some(v)
}

fn ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && b[*i].is_ascii_whitespace() {
        *i += 1;
    }
}

fn value(b: &[u8], i: &mut usize) -> Option<J> {
    ws(b, i);
    match *b.get(*i)? {
        b'{' => {
            *i += 1;
            let mut v = vec![];
            ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Some(J::Obj(v));
            }
            loop {
                ws(b, i);
                let J::Str(k) = string(b, i)? else { return None };
                ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return None;
                }
                *i += 1;
                v.push((k, value(b, i)?));
                ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b'}' => {
                        *i += 1;
                        return Some(J::Obj(v));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *i += 1;
            let mut v = vec![];
            ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Some(J::Arr(v));
            }
            loop {
                v.push(value(b, i)?);
                ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b']' => {
                        *i += 1;
                        return Some(J::Arr(v));
                    }
                    _ => return None,
                }
            }
        }
        b'"' => string(b, i),
        b't' if b[*i..].starts_with(b"true") => {
            *i += 4;
            Some(J::Bool(true))
        }
        b'f' if b[*i..].starts_with(b"false") => {
            *i += 5;
            Some(J::Bool(false))
        }
        b'n' if b[*i..].starts_with(b"null") => {
            *i += 4;
            Some(J::Null)
        }
        _ => {
            let st = *i;
            while *i < b.len() && matches!(b[*i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
                *i += 1;
            }
            std::str::from_utf8(&b[st..*i]).ok()?.parse().ok().map(J::Num)
        }
    }
}

fn string(b: &[u8], i: &mut usize) -> Option<J> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let mut out: Vec<u16> = vec![];
    let mut raw: Vec<u8> = vec![];
    let flush = |raw: &mut Vec<u8>, out: &mut Vec<u16>| {
        out.extend(String::from_utf8_lossy(raw).encode_utf16());
        raw.clear();
    };
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        match c {
            b'"' => {
                flush(&mut raw, &mut out);
                return Some(J::Str(String::from_utf16_lossy(&out)));
            }
            b'\\' => {
                flush(&mut raw, &mut out);
                let e = *b.get(*i)?;
                *i += 1;
                match e {
                    b'n' => out.push(10),
                    b't' => out.push(9),
                    b'r' => out.push(13),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'u' => {
                        let h = std::str::from_utf8(b.get(*i..*i + 4)?).ok()?;
                        out.push(u16::from_str_radix(h, 16).ok()?);
                        *i += 4;
                    }
                    c => out.push(c as u16),
                }
            }
            c => raw.push(c),
        }
    }
}

// --- versions ---------------------------------------------------------------------------------

// "1.0.13.0" or "1.0.13.0-dev24": the numbers, then 1 for a release and 0 for a dev build, then
// the dev number; a release sorts after every dev build of its version
pub fn ver_key(s: &str) -> (Vec<u32>, u32, u32) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let (nums, dev) = match s.split_once("-dev") {
        Some((n, d)) => (n, Some(d.trim().parse().unwrap_or(0))),
        None => (s.split('-').next().unwrap_or(s), None),
    };
    let mut v: Vec<u32> = nums.split('.').map(|x| x.trim().parse().unwrap_or(0)).collect();
    while v.last() == Some(&0) && v.len() > 1 {
        v.pop();
    }
    (v, if dev.is_some() { 0 } else { 1 }, dev.unwrap_or(0))
}

// "2026-11-20T12:03:19Z" in seconds since 1970 (UTC)
pub fn iso_secs(s: &str) -> Option<i64> {
    let n = |a: usize, b: usize| s.get(a..b)?.parse::<i64>().ok();
    let (y, mo, d, h, mi, se) = (n(0, 4)?, n(5, 7)?, n(8, 10)?, n(11, 13)?, n(14, 16)?, n(17, 19)?);
    // days from civil (Howard Hinnant)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * (if mo > 2 { mo - 3 } else { mo + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe - 719468) * 86400 + h * 3600 + mi * 60 + se)
}

// --- GitHub -----------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Release {
    pub tag: String,
    pub published: String,
    pub assets: Vec<(String, String)>, // name, download url
}

fn hidden(c: &mut Command) -> &mut Command {
    #[cfg(windows)]
    c.creation_flags(0x0800_0000);
    c
}

pub fn releases() -> Result<Vec<Release>, String> {
    // RSR_UPDATE_URL: another release list (tests: a file:// JSON whose assets are file:// too)
    let url = std::env::var("RSR_UPDATE_URL").unwrap_or_else(|_| format!("https://api.github.com/repos/{REPO}/releases?per_page=30"));
    let out = hidden(Command::new("curl.exe").args(["-sSfL", "--max-time", "20", "-A", AGENT, "-H", "Accept: application/vnd.github+json", &url]))
        .output()
        .map_err(|e| format!("curl.exe: {e}"))?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    Ok(releases_of(&String::from_utf8_lossy(&out.stdout)))
}

pub fn releases_of(json: &str) -> Vec<Release> {
    let Some(j) = parse(json) else { return vec![] };
    j.arr()
        .iter()
        .filter(|r| !r.get("draft").is_some_and(J::truthy) && !r.get("prerelease").is_some_and(J::truthy))
        .map(|r| Release {
            tag: r.get("tag_name").map(|t| t.str().to_string()).unwrap_or_default(),
            published: r.get("published_at").map(|t| t.str().to_string()).unwrap_or_default(),
            assets: r
                .get("assets")
                .map(|a| a.arr().iter().map(|x| (x.get("name").map(|n| n.str().to_string()).unwrap_or_default(), x.get("browser_download_url").map(|n| n.str().to_string()).unwrap_or_default())).collect())
                .unwrap_or_default(),
        })
        .collect()
}

fn download(url: &str, out: &Path) -> Result<(), String> {
    let part = out.with_extension("part");
    let _ = std::fs::remove_file(&part);
    let st = hidden(Command::new("curl.exe").args(["-sSfL", "--retry", "2", "--max-time", "900", "-A", AGENT, "-o"]).arg(&part).arg(url))
        .status()
        .map_err(|e| format!("curl.exe: {e}"))?;
    if !st.success() || std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0) == 0 {
        let _ = std::fs::remove_file(&part);
        return Err(format!("download failed: {url}"));
    }
    let _ = std::fs::remove_file(out);
    std::fs::rename(&part, out).map_err(|e| format!("{}: {e}", out.display()))
}

// --- the mod ----------------------------------------------------------------------------------

// the newest mod release: (version, release)
// the most recently published mod release (ISO dates order as strings)
pub fn latest_mod(rs: &[Release]) -> Option<(String, &Release)> {
    rs.iter()
        .filter(|r| r.tag.starts_with(['v', 'V']) && r.assets.iter().any(|(n, _)| n.eq_ignore_ascii_case("RogueShard.sml")))
        .map(|r| (r.tag[1..].to_string(), r))
        .max_by(|a, b| a.1.published.cmp(&b.1.published))
}

// Whether the release's mod replaces the one in mods\: none there, no version file, or another version.
pub fn mod_wanted(sml: &Path, installed: Option<&str>, version: &str) -> bool {
    !sml.exists() || installed.map(str::trim) != Some(version)
}

pub fn update_mod(root: &Path, rs: &[Release], log: &dyn Fn(&str, &str)) {
    let Some((version, r)) = latest_mod(rs) else { return };
    let dir = root.join("mods");
    let sml = dir.join("RogueShard.sml");
    let vfile = dir.join("RogueShard.version");
    let installed = std::fs::read_to_string(&vfile).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    if !mod_wanted(&sml, installed.as_deref(), &version) {
        log("", &format!("Rogue Shard {}: up to date", installed.as_deref().unwrap_or(&version)));
        return;
    }
    let url = &r.assets.iter().find(|(n, _)| n.eq_ignore_ascii_case("RogueShard.sml")).unwrap().1;
    log("", &format!("Rogue Shard {version}: downloading..."));
    let _ = std::fs::create_dir_all(&dir);
    match download(url, &sml) {
        Ok(()) => {
            let _ = std::fs::write(&vfile, &version);
            log("o", &format!("Rogue Shard {version}: installed"));
        }
        Err(e) => log("e", &format!("Rogue Shard {version}: {e}")),
    }
}

// --- the launcher -----------------------------------------------------------------------------

pub fn latest_launcher(rs: &[Release]) -> Option<(u32, &Release, String)> {
    rs.iter()
        .filter_map(|r| {
            let n: u32 = r.tag.strip_prefix("launcher-")?.parse().ok()?;
            let url = r.assets.iter().find(|(name, _)| name.to_lowercase().ends_with(".zip"))?.1.clone();
            Some((n, r, url))
        })
        .max_by_key(|x| x.0)
}

// true: the files are replaced and the new launcher should start
pub fn update_launcher(root: &Path, rs: &[Release], log: &dyn Fn(&str, &str)) -> bool {
    let Some((n, _, url)) = latest_launcher(rs) else { return false };
    if n <= LAUNCHER_BUILD {
        return false;
    }
    log("", &format!("Launcher #{n}: downloading..."));
    let work = root.join("cache").join("update");
    let _ = std::fs::remove_dir_all(&work);
    let _ = std::fs::create_dir_all(&work);
    let zip = work.join("launcher.zip");
    if let Err(e) = download(&url, &zip) {
        log("e", &format!("Launcher #{n}: {e}"));
        return false;
    }
    let x = work.join("x");
    let _ = std::fs::create_dir_all(&x);
    let ok = hidden(Command::new("tar.exe").arg("-xf").arg(&zip).arg("-C").arg(&x)).status().map(|s| s.success()).unwrap_or(false);
    let Some(src) = ok.then(|| find_launcher_dir(&x)).flatten() else {
        log("e", &format!("Launcher #{n}: the archive has no Launcher.exe"));
        return false;
    };
    match install(root, &src) {
        Ok(()) => {
            log("o", &format!("Launcher #{n}: installed, restarting"));
            true
        }
        Err(e) => {
            log("e", &format!("Launcher #{n}: {e}"));
            false
        }
    }
}

fn find_launcher_dir(d: &Path) -> Option<PathBuf> {
    if d.join("Launcher.exe").exists() {
        return Some(d.to_path_buf());
    }
    std::fs::read_dir(d).ok()?.flatten().map(|e| e.path()).filter(|p| p.is_dir()).find_map(|p| find_launcher_dir(&p))
}

// the new files over ours: the running exe is moved aside first (Windows lets a running exe be
// renamed, not overwritten); mods, settings and the cache stay
fn install(root: &Path, src: &Path) -> Result<(), String> {
    let me = root.join("Launcher.exe");
    let old = root.join("Launcher.old.exe");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(&me, &old).map_err(|e| format!("Launcher.exe: {e}"))?;
    if let Err(e) = std::fs::copy(src.join("Launcher.exe"), &me) {
        let _ = std::fs::rename(&old, &me);
        return Err(format!("Launcher.exe: {e}"));
    }
    for f in ["rsr.exe", "README.txt"] {
        if src.join(f).exists() {
            std::fs::copy(src.join(f), root.join(f)).map_err(|e| format!("{f}: {e}"))?;
        }
    }
    if src.join("patcher").is_dir() {
        copy_dir(&src.join("patcher"), &root.join("patcher")).map_err(|e| format!("patcher: {e}"))?;
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)?.flatten() {
        let p = e.path();
        if p.is_dir() {
            copy_dir(&p, &to.join(e.file_name()))?;
        } else {
            std::fs::copy(&p, to.join(e.file_name()))?;
        }
    }
    Ok(())
}

// left over from an update: the old exe and the download
pub fn cleanup(root: &Path) {
    let _ = std::fs::remove_file(root.join("Launcher.old.exe"));
    let _ = std::fs::remove_dir_all(root.join("cache").join("update"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_order_dev_builds_before_their_release() {
        assert!(ver_key("1.0.13.0") > ver_key("1.0.13.0-dev24"));
        assert!(ver_key("1.0.13.0-dev24") > ver_key("1.0.13.0-dev3"));
        assert!(ver_key("1.0.13.0-dev1") > ver_key("1.0.12.8"));
        assert!(ver_key("v1.0.13.1") > ver_key("1.0.13.0"));
        assert_eq!(ver_key("1.0.13"), ver_key("1.0.13.0"));
    }

    #[test]
    fn github_release_list_is_read() {
        let json = r#"[{"tag_name":"launcher-6","draft":false,"prerelease":false,"published_at":"2026-11-21T10:00:00Z","assets":[{"name":"RogueShard_Launcher-6.zip","browser_download_url":"https://x/l6.zip"}]},
            {"tag_name":"v1.0.13.0","draft":false,"prerelease":false,"published_at":"2026-11-20T12:03:19Z","body":"Исп \"q\"","assets":[{"name":"RogueShard.sml","browser_download_url":"https://x/m.sml"}]},
            {"tag_name":"v1.0.14.0","draft":true,"prerelease":false,"published_at":null,"assets":[{"name":"RogueShard.sml","browser_download_url":"https://x/d.sml"}]},
            {"tag_name":"v1.0.12.8","draft":false,"prerelease":false,"published_at":"2026-11-01T00:00:00Z","assets":[{"name":"RogueShard.sml","browser_download_url":"https://x/o.sml"}]}]"#;
        let rs = releases_of(json);
        assert_eq!(rs.len(), 3);
        let (v, r) = latest_mod(&rs).unwrap();
        assert_eq!(v, "1.0.13.0");
        assert_eq!(r.assets[0].1, "https://x/m.sml");
        let (n, _, url) = latest_launcher(&rs).unwrap();
        assert_eq!((n, url.as_str()), (6, "https://x/l6.zip"));
        assert_eq!(iso_secs("1970-01-02T00:00:01Z"), Some(86401));
        assert_eq!(iso_secs("2026-11-20T12:03:19Z"), Some(1795176199));
    }

    #[test]
    fn the_mod_is_downloaded_when_missing_or_of_another_version() {
        let d = std::env::temp_dir().join("rs_launcher_update_test");
        let _ = std::fs::create_dir_all(&d);
        let sml = d.join("RogueShard.sml");
        let _ = std::fs::remove_file(&sml);
        assert!(mod_wanted(&sml, Some("1.0.13.0"), "1.0.13.0"));
        std::fs::write(&sml, b"x").unwrap();
        assert!(!mod_wanted(&sml, Some("1.0.13.0"), "1.0.13.0"));
        assert!(!mod_wanted(&sml, Some("1.0.13.0-dev2\n"), "1.0.13.0-dev2"));
        assert!(mod_wanted(&sml, Some("1.0.13.0-dev24"), "1.0.13.0"));
        assert!(mod_wanted(&sml, Some("1.0.14.0"), "1.0.13.0"));
        assert!(mod_wanted(&sml, None, "1.0.13.0"));
    }
}
