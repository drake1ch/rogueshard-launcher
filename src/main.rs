// Rogue Shard Launcher: a window (ui/index.html in a WebView) over the launcher's folder:
//
//   Launcher.exe   this
//   Rogue Shard.exe  the Rogue Shard engine (rsr.exe until launcher-13; cargo builds it as rsr.exe)
//   patcher\       ModPatcher: MSL-E's own code and the .NET runtime (RogueShardTools/ModPatcher)
//   mods\          the player's .sml mods (load_order.txt optional)
//   cache\         data.win built from them, build.key, patch.log
//   keys.ini       the speed hotkeys, read by the engine
//   launcher.ini   language, "rebuild every launch", the hotkeys
//   last_run.log   the engine's output of the last game
//
// Play: when mods\ holds .sml files and the build is stale (mods, load order or vanilla file
// changed, not the patcher: cache\build.key, the same key as `rsr launch`) or "rebuild" is on, ModPatcher
// builds cache\data.win, as the MSL-E window would, showing each mod's progress; then the game
// starts on the engine with that data.win. The game's own folder is never written.

#![windows_subsystem = "windows"]

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};

use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::window::WindowBuilder;
use wry::WebViewBuilder;

mod update;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

const WIKI: &str = "https://drake1ch.github.io/rogueshard-wiki/";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const SPEEDS: [f64; 5] = [1.0, 3.0, 5.0, 10.0, 20.0]; // the default presets, each one changeable
const SPEED_RANGE: (f64, f64) = (0.25, 100.0);
const DEFAULT_KEYS: [&str; 5] = ["Ctrl+1", "Ctrl+2", "Ctrl+3", "Ctrl+4", "Ctrl+5"];

enum Ev {
    Ipc(String),
    Js(String),
    GameStarted,
    GameEnded(bool), // true: closed normally
    Quit,            // an updated launcher took over
}

struct Settings {
    lang: String,
    rebuild: bool,
    keys: Vec<String>,
    speeds: Vec<f64>, // the speed of each hotkey ("speed<i>=" in launcher.ini)
    disabled: Vec<String>, // mods switched off (file names without .sml)
    game: Option<PathBuf>,  // the game's folder chosen by hand ("game=" in launcher.ini), else Steam's
    auto_mod: bool,         // the mod from the GitHub releases (update.rs)
    auto_launcher: bool,    // the launcher itself from them
}

struct App {
    root: PathBuf,
    game: Option<PathBuf>,
    settings: Settings,
    busy: bool, // patching or the game running
}

fn root_dir() -> PathBuf {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())).unwrap_or_else(|| PathBuf::from("."))
}

// --- the game, through Steam (as rsr launch) -------------------------------------------------

// HKCU\Software\Valve\Steam\SteamPath read through the registry API (reg.exe from a windowed
// process came back empty: the launcher found no game while `rsr launch` did)
#[cfg(windows)]
fn steam_path() -> Option<PathBuf> {
    #[link(name = "advapi32")]
    extern "system" {
        fn RegGetValueW(hkey: isize, sub: *const u16, value: *const u16, flags: u32, ty: *mut u32, data: *mut u8, len: *mut u32) -> i32;
    }
    const HKCU: isize = 0x8000_0001u32 as i32 as isize;
    const RRF_RT_REG_SZ: u32 = 2;
    let w = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (sub, val) = (w(r"Software\Valve\Steam"), w("SteamPath"));
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe { RegGetValueW(HKCU, sub.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr() as *mut u8, &mut len) };
    if r != 0 {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1);
    Some(PathBuf::from(String::from_utf16_lossy(&buf[..n]).replace('/', "\\")))
}
#[cfg(not(windows))]
fn steam_path() -> Option<PathBuf> {
    None
}

fn find_game() -> Option<PathBuf> {
    // Steam's own folder (registry, else where it usually is), its libraries, and the game
    let mut steams: Vec<PathBuf> = steam_path().into_iter().collect();
    for d in [r"C:\Program Files (x86)\Steam", r"C:\Program Files\Steam", r"D:\Steam", r"D:\SteamLibrary", r"E:\Steam", r"E:\SteamLibrary"] {
        steams.push(PathBuf::from(d));
    }
    let mut libs = vec![];
    for steam in &steams {
        libs.extend(libraries(steam));
    }
    for lib in libs {
        let apps = lib.join("steamapps");
        if apps.join("appmanifest_625960.acf").exists() {
            let dir = apps.join("common").join("Stoneshard");
            // the game's folder: its exe or either data file (an MSL-E player may have only
            // vanilla.win beside a removed data.win)
            if dir.join("StoneShard.exe").exists() || dir.join("data.win").exists() || dir.join("vanilla.win").exists() {
                return Some(PathBuf::from(dir.to_string_lossy().replace('/', "\\")));
            }
        }
    }
    None
}

// A Stoneshard folder: its exe or either data file (an MSL-E player may have only vanilla.win
// beside a removed data.win). A folder picked one level too high (common) finds Stoneshard in it.
fn game_dir_in(d: &Path) -> Option<PathBuf> {
    let ok = |d: &Path| d.join("StoneShard.exe").exists() || d.join("data.win").exists() || d.join("vanilla.win").exists();
    if ok(d) {
        return Some(d.to_path_buf());
    }
    let sub = d.join("Stoneshard");
    ok(&sub).then_some(sub)
}

// The game: the folder chosen by hand while it still holds the game, else Steam's.
fn the_game(s: &Settings) -> Option<PathBuf> {
    s.game.as_deref().and_then(game_dir_in).or_else(find_game)
}

// a Steam folder and the libraries its libraryfolders.vdf lists
fn libraries(steam: &Path) -> Vec<PathBuf> {
    let mut out = vec![steam.to_path_buf()];
    if let Ok(t) = std::fs::read_to_string(steam.join("steamapps").join("libraryfolders.vdf")) {
        for l in t.lines() {
            if let Some(rest) = l.trim().strip_prefix("\"path\"") {
                out.push(PathBuf::from(rest.trim().trim_matches('"').replace("\\\\", "\\")));
            }
        }
    }
    out
}

// The unmodded data.win: vanilla.win when the game has one, else data.win if MSL never patched it.
fn vanilla_of(game: &Path) -> Result<PathBuf, String> {
    let v = game.join("vanilla.win");
    if v.exists() {
        return Ok(v);
    }
    let d = game.join("data.win");
    let bytes = std::fs::read(&d).map_err(|e| format!("{}: {e}", d.display()))?;
    if bytes.windows(9).any(|w| w == b"msl_print") {
        return Err(format!("{} already has mods in it: restore the game's own data.win (Steam: Properties, Installed files, Verify integrity), or put an unmodded copy beside it as vanilla.win", d.display()));
    }
    Ok(d)
}

// --- settings ---------------------------------------------------------------------------------

fn load_settings(root: &Path) -> Settings {
    let mut s = Settings { lang: "ru".into(), rebuild: false, disabled: vec![], game: None, auto_mod: true, auto_launcher: true, keys: DEFAULT_KEYS.iter().map(|k| k.to_string()).collect(), speeds: SPEEDS.to_vec() };
    if let Ok(t) = std::fs::read_to_string(root.join("launcher.ini")) {
        for l in t.lines() {
            let Some((k, v)) = l.split_once('=') else { continue };
            let (k, v) = (k.trim(), v.trim());
            match k {
                "lang" if v == "ru" || v == "en" => s.lang = v.into(),
                "rebuild" => s.rebuild = v == "1",
                "game" if !v.is_empty() => s.game = Some(PathBuf::from(v)),
                "automod" => s.auto_mod = v != "0",
                "autolauncher" => s.auto_launcher = v != "0",
                "disabled" => s.disabled = v.split('|').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect(),
                _ => {
                    if let Some(i) = k.strip_prefix("speed").and_then(|i| i.parse::<usize>().ok()) {
                        if let (true, Some(v)) = (i < 5, parse_speed(v)) {
                            s.speeds[i] = v;
                        }
                    }
                    if let Some(i) = k.strip_prefix("key").and_then(|i| i.parse::<usize>().ok()) {
                        if i < 5 && !v.is_empty() {
                            s.keys[i] = v.into();
                        }
                    }
                }
            }
        }
    } else {
        // first start: English unless Windows is Russian
        let ru = std::env::var("LANG").map(|l| l.starts_with("ru")).unwrap_or(false) || system_is_russian();
        s.lang = if ru { "ru" } else { "en" }.into();
    }
    s
}

#[cfg(windows)]
fn system_is_russian() -> bool {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetUserDefaultUILanguage() -> u16;
    }
    // LANGID: primary language in the low 10 bits; Russian 0x19, Ukrainian 0x22, Belarusian 0x23
    let p = unsafe { GetUserDefaultUILanguage() } & 0x3FF;
    matches!(p, 0x19 | 0x22 | 0x23)
}
#[cfg(not(windows))]
fn system_is_russian() -> bool {
    false
}

fn save_settings(root: &Path, s: &Settings) {
    let mut t = format!("lang={}\nrebuild={}\ndisabled={}\nautomod={}\nautolauncher={}\n", s.lang, if s.rebuild { 1 } else { 0 }, s.disabled.join("|"), s.auto_mod as u8, s.auto_launcher as u8);
    if let Some(g) = &s.game {
        t += &format!("game={}\n", g.display());
    }
    for (i, k) in s.keys.iter().enumerate() {
        t += &format!("key{i}={k}\n");
    }
    for (i, v) in s.speeds.iter().enumerate() {
        t += &format!("speed{i}={}\n", speed_text(*v));
    }
    let _ = std::fs::write(root.join("launcher.ini"), t);
    // the engine's own file: "<speed> = <combo>"
    let mut k = String::from("# speed hotkeys, written by the launcher\n");
    for (sp, key) in s.speeds.iter().zip(&s.keys) {
        k += &format!("{} = {key}\n", speed_text(*sp));
    }
    let _ = std::fs::write(root.join("keys.ini"), k);
}

// A speed typed by the player: "2", "1.5" or "1,5", an optional "x"; within SPEED_RANGE.
fn parse_speed(v: &str) -> Option<f64> {
    let x: &[char] = &['x', 'X', '\u{445}', '\u{425}'];
    let v = v.trim().trim_start_matches(x).trim_end_matches(x).trim().replace(',', ".");
    let n = v.parse::<f64>().ok()?;
    (n.is_finite() && n >= SPEED_RANGE.0 && n <= SPEED_RANGE.1).then(|| (n * 100.0).round() / 100.0)
}

// 2.0 -> "2", 1.5 -> "1.5"
fn speed_text(v: f64) -> String {
    let t = format!("{:.2}", v);
    t.trim_end_matches('0').trim_end_matches('.').to_string()
}

// --- the build --------------------------------------------------------------------------------

fn mods_of(root: &Path) -> Vec<PathBuf> {
    let dir = root.join("mods");
    let _ = std::fs::create_dir_all(&dir);
    let mut v: Vec<PathBuf> = std::fs::read_dir(&dir).map(|rd| rd.flatten().map(|e| e.path()).filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("sml"))).collect()).unwrap_or_default();
    v.sort();
    // load_order.txt first, as ModPatcher orders them
    if let Ok(t) = std::fs::read_to_string(dir.join("load_order.txt")) {
        let names: Vec<String> = t.lines().map(|l| l.trim().to_lowercase()).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
        v.sort_by_key(|p| names.iter().position(|n| *n == stem(p).to_lowercase()).unwrap_or(usize::MAX));
    }
    v
}

// the mods that go into the build: every mod of mods_of but those switched off
fn enabled_mods(root: &Path, disabled: &[String]) -> Vec<PathBuf> {
    mods_of(root).into_iter().filter(|p| !disabled.iter().any(|d| d.eq_ignore_ascii_case(&stem(p)))).collect()
}

// load_order.txt: every mod of the folder in the launcher's order
fn write_order(root: &Path, names: &[String]) {
    let _ = std::fs::write(root.join("mods").join("load_order.txt"), names.join("\n") + "\n");
}

fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
}

fn stamp(p: &Path) -> String {
    let m = std::fs::metadata(p).ok();
    let t = m.as_ref().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).unwrap_or(0);
    format!("{} {} {}", p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), m.map(|m| m.len()).unwrap_or(0), t)
}

// cache\build.key, written the same way by `rsr launch`. The patcher is not in it: a launcher
// update replaces patcher\, and that alone must not rebuild data.win (only the mods, their order
// or the vanilla file do)
fn build_key(root: &Path, vanilla: &Path, mods: &[PathBuf]) -> String {
    let mut mods = mods.to_vec();
    mods.sort();
    let mut key = vec![format!("vanilla {} {}", vanilla.display(), stamp(vanilla))];
    key.extend(mods.iter().map(|p| format!("mod {}", stamp(p))));
    key.push(format!("order {}", std::fs::read_to_string(root.join("mods").join("load_order.txt")).unwrap_or_default().trim()));
    key.join("\n")
}

// a key written before launcher-11 has a "patcher" line: ignored, so that cache stays fresh
fn is_fresh(root: &Path, key: &str) -> bool {
    let old = std::fs::read_to_string(root.join("cache").join("build.key")).unwrap_or_default();
    let old: Vec<&str> = old.lines().filter(|l| !l.starts_with("patcher ")).collect();
    root.join("cache").join("data.win").exists() && old.join("\n") == key
}

// --- JS ---------------------------------------------------------------------------------------

fn js_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o += "\\\"",
            '\\' => o += "\\\\",
            '\n' => o += "\\n",
            '\r' => {}
            c if (c as u32) < 0x20 => o += &format!("\\u{:04x}", c as u32),
            c => o.push(c),
        }
    }
    o + "\""
}

fn js_list(v: &[String]) -> String {
    format!("[{}]", v.iter().map(|s| js_str(s)).collect::<Vec<_>>().join(","))
}

fn send_js(proxy: &EventLoopProxy<Ev>, js: String) {
    let _ = proxy.send_event(Ev::Js(js));
}

fn log(proxy: &EventLoopProxy<Ev>, cls: &str, text: &str) {
    send_js(proxy, format!("rs.log([[{},{}]])", js_str(cls), js_str(text)));
}

fn date_of(p: &Path) -> String {
    // dd.mm hh:mm, local time through the file's age (no time zone crate): the build's own date
    let Ok(m) = std::fs::metadata(p).and_then(|m| m.modified()) else { return String::new() };
    let st: std::time::SystemTime = m;
    let secs = st.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0) + local_offset();
    let days = secs.div_euclid(86400);
    let (h, mi) = ((secs.rem_euclid(86400)) / 3600, (secs.rem_euclid(3600)) / 60);
    // civil date from days since 1970 (Howard Hinnant)
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    format!("{d:02}.{mo:02} {h:02}:{mi:02}")
}

#[cfg(windows)]
fn local_offset() -> i64 {
    #[repr(C)]
    struct Tzi {
        bias: i32,
        _rest: [u8; 168],
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn GetTimeZoneInformation(t: *mut Tzi) -> u32;
    }
    let mut t = Tzi { bias: 0, _rest: [0; 168] };
    let r = unsafe { GetTimeZoneInformation(&mut t) };
    // daylight bias at offset 168 (i32) when daylight time is on (r == 2)
    let dl = if r == 2 { i32::from_le_bytes([t._rest[164], t._rest[165], t._rest[166], t._rest[167]]) } else { 0 };
    -((t.bias + dl) as i64) * 60
}
#[cfg(not(windows))]
fn local_offset() -> i64 {
    0
}

impl App {
    // the whole state for rs.update
    fn state_js(&self) -> String {
        let names: Vec<String> = mods_of(&self.root).iter().map(|p| stem(p)).collect();
        let mods = enabled_mods(&self.root, &self.settings.disabled);
        let (game_dir, vanilla, fresh) = match &self.game {
            Some(g) => match vanilla_of(g) {
                Ok(v) => {
                    let k = build_key(&self.root, &v, &mods);
                    (g.display().to_string(), v.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(), is_fresh(&self.root, &k))
                }
                Err(_) => (g.display().to_string(), String::new(), false),
            },
            None => (String::new(), String::new(), false),
        };
        let data = self.root.join("cache").join("data.win");
        let built = if data.exists() { format!("cache\\data.win · {}", date_of(&data)) } else { String::new() };
        format!(
            "rs.update({{lang:{},disabled:{},mods:{},gameDir:{},vanilla:{},built:{},fresh:{},rebuild:{},keys:{},speeds:[{}],autoMod:{},autoLauncher:{},launcherBuild:{}}})",
            js_str(&self.settings.lang),
            js_list(&self.settings.disabled),
            js_list(&names),
            js_str(&game_dir),
            js_str(&vanilla),
            js_str(&built),
            fresh,
            self.settings.rebuild,
            js_list(&self.settings.keys),
            self.settings.speeds.iter().map(|v| speed_text(*v)).collect::<Vec<_>>().join(","),
            self.settings.auto_mod,
            self.settings.auto_launcher,
            update::LAUNCHER_BUILD
        )
    }
}

fn debug(root: &Path, msg: &str) {
    if std::env::var_os("RSR_LAUNCHER_DEBUG").is_some() {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(root.join("launcher_debug.log")) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

fn open(target: &str) {
    let mut c = Command::new("explorer.exe");
    c.arg(target);
    let _ = c.spawn();
}

// Play, on a thread of its own: the build if needed, then the game until it closes.
fn play(app: Arc<Mutex<App>>, proxy: EventLoopProxy<Ev>) {
    let (root, game, rebuild, disabled) = {
        let a = app.lock().unwrap();
        (a.root.clone(), a.game.clone(), a.settings.rebuild, a.settings.disabled.clone())
    };
    let done = |phase: &str| {
        app.lock().unwrap().busy = false;
        send_js(&proxy, format!("rs.update({{phase:{}}})", js_str(phase)));
        let js = app.lock().unwrap().state_js();
        send_js(&proxy, js);
    };
    debug(&root, &format!("play: game {:?}, rebuild {rebuild}", game));
    let Some(game) = game else {
        done("noGame");
        return;
    };
    let mods = enabled_mods(&root, &disabled);
    let data: Option<PathBuf>;
    if !mods.is_empty() {
        let vanilla = match vanilla_of(&game) {
            Ok(v) => v,
            Err(e) => {
                log(&proxy, "e", &e);
                done("failed");
                return;
            }
        };
        let key = build_key(&root, &vanilla, &mods);
        let out = root.join("cache").join("data.win");
        debug(&root, &format!("play: vanilla {}, {} mods, fresh {}", vanilla.display(), mods.len(), is_fresh(&root, &key)));
        if rebuild || !is_fresh(&root, &key) {
            if !patch(&root, &vanilla, &mods, &disabled, &out, &key, &proxy) {
                done("failed");
                return;
            }
        } else {
            log(&proxy, "o", "data.win: cache");
        }
        data = Some(out);
    } else {
        // no mods: the game as it is, its data.win, else the vanilla.win kept beside it
        let d = game.join("data.win");
        data = Some(if d.exists() { d } else { game.join("vanilla.win") });
    }
    // the game
    send_js(&proxy, "rs.update({phase:\"running\",modStates:{}})".into());
    let rsr = update::game_exe(&root);
    let mut c = Command::new(&rsr);
    c.current_dir(&root).arg("launch");
    if let Some(d) = &data {
        c.arg("--data").arg(d);
    }
    // the game's own saves and settings (%LOCALAPPDATA%\StoneShard), as the real game
    c.arg("--game").arg(&game).arg("--real-saves");
    if let Ok(f) = std::fs::File::create(root.join("last_run.log")) {
        if let Ok(f2) = f.try_clone() {
            c.stdout(f).stderr(f2);
        }
    }
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    log(&proxy, "", &format!("Stoneshard: {}", data.as_ref().map(|d| d.display().to_string()).unwrap_or_default()));
    let _ = proxy.send_event(Ev::GameStarted);
    let steam = steam_session(&game);
    debug(&root, &format!("play: Steam session {}", steam.is_some()));
    let ok = match c.status() {
        Ok(s) if s.success() => {
            log(&proxy, "o", "game closed");
            done("closed");
            true
        }
        Ok(s) => {
            log(&proxy, "e", &format!("the game ended with code {}: last_run.log", s.code().unwrap_or(-1)));
            done("crashed");
            false
        }
        Err(e) => {
            log(&proxy, "e", &format!("could not start {}: {e}", rsr.display()));
            done("crashed");
            false
        }
    };
    steam_end(steam);
    let _ = proxy.send_event(Ev::GameEnded(ok));
}

// The launcher window's HWND, the owner of the folder dialog.
#[cfg(windows)]
fn window_handle(w: &tao::window::Window) -> isize {
    use tao::platform::windows::WindowExtWindows;
    w.hwnd() as isize
}
#[cfg(not(windows))]
fn window_handle(_: &tao::window::Window) -> isize {
    0
}

// Windows' folder dialog (SHBrowseForFolderW, the new style with an edit box): the chosen folder,
// None when cancelled.
#[cfg(windows)]
fn pick_folder(owner: isize, start: Option<&Path>) -> Option<PathBuf> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[repr(C)]
    struct BrowseInfo {
        owner: isize,
        root: *const c_void,
        display: *mut u16,
        title: *const u16,
        flags: u32,
        callback: Option<unsafe extern "system" fn(isize, u32, isize, isize) -> i32>,
        param: isize,
        image: i32,
    }
    #[link(name = "shell32")]
    extern "system" {
        fn SHBrowseForFolderW(bi: *const BrowseInfo) -> *mut c_void;
        fn SHGetPathFromIDListW(pidl: *const c_void, path: *mut u16) -> i32;
    }
    #[link(name = "ole32")]
    extern "system" {
        fn CoInitializeEx(r: *const c_void, mode: u32) -> i32;
        fn CoTaskMemFree(p: *mut c_void);
    }
    #[link(name = "user32")]
    extern "system" {
        fn SendMessageW(h: isize, msg: u32, w: usize, l: isize) -> isize;
    }
    // BFFM_INITIALIZED: open at the current folder (BFFM_SETSELECTIONW, lParam = the path)
    unsafe extern "system" fn cb(h: isize, msg: u32, _l: isize, data: isize) -> i32 {
        if msg == 1 && data != 0 {
            SendMessageW(h, 0x400 + 103, 1, data);
        }
        0
    }
    let title: Vec<u16> = "Stoneshard (StoneShard.exe, data.win)".encode_utf16().chain([0]).collect();
    let start_w: Vec<u16> = start.map(|p| p.as_os_str().encode_wide().chain([0]).collect()).unwrap_or_default();
    let mut display = vec![0u16; 260];
    let bi = BrowseInfo {
        owner,
        root: std::ptr::null(),
        display: display.as_mut_ptr(),
        title: title.as_ptr(),
        flags: 0x1 | 0x40, // BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE
        callback: Some(cb),
        param: if start_w.is_empty() { 0 } else { start_w.as_ptr() as isize },
        image: 0,
    };
    unsafe {
        CoInitializeEx(std::ptr::null(), 0x2); // apartment threaded, as the dialog needs
        let pidl = SHBrowseForFolderW(&bi);
        if pidl.is_null() {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let ok = SHGetPathFromIDListW(pidl, buf.as_mut_ptr()) != 0;
        CoTaskMemFree(pidl);
        let n = buf.iter().position(|&c| c == 0).unwrap_or(0);
        (ok && n > 0).then(|| PathBuf::from(String::from_utf16_lossy(&buf[..n])))
    }
}
#[cfg(not(windows))]
fn pick_folder(_: isize, _: Option<&Path>) -> Option<PathBuf> {
    None
}

// While the game runs the launcher tells Steam that Stoneshard (625960) is running, through the
// game's own steam_api64.dll: Steam counts the play time and shows the activity, with no launch
// options, and Steam's own Play still starts the vanilla game. Steam not running or the DLL
// missing: nothing happens (SteamAPI_Init returns false).
const STEAM_APPID: &str = "625960";

#[cfg(windows)]
fn steam_session(game: &Path) -> Option<usize> {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn LoadLibraryW(name: *const u16) -> *mut c_void;
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }
    let dll = game.join("steam_api64.dll");
    if !dll.exists() {
        return None;
    }
    std::env::set_var("SteamAppId", STEAM_APPID);
    std::env::set_var("SteamGameId", STEAM_APPID);
    let w: Vec<u16> = dll.as_os_str().encode_wide().chain([0]).collect();
    unsafe {
        let m = LoadLibraryW(w.as_ptr());
        if m.is_null() {
            return None;
        }
        let init = GetProcAddress(m, b"SteamAPI_Init\0".as_ptr());
        if init.is_null() {
            return None;
        }
        let init: extern "C" fn() -> bool = std::mem::transmute(init);
        if init() { Some(m as usize) } else { None }
    }
}

#[cfg(windows)]
fn steam_end(s: Option<usize>) {
    use std::ffi::c_void;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetProcAddress(module: *mut c_void, name: *const u8) -> *mut c_void;
    }
    let Some(m) = s else { return };
    unsafe {
        let f = GetProcAddress(m as *mut c_void, b"SteamAPI_Shutdown\0".as_ptr());
        if !f.is_null() {
            let f: extern "C" fn() = std::mem::transmute(f);
            f();
        }
    }
}

#[cfg(not(windows))]
fn steam_session(_: &Path) -> Option<usize> {
    None
}

#[cfg(not(windows))]
fn steam_end(_: Option<usize>) {}

// ModPatcher over the mods, its output in the log and each mod's progress in the list.
fn patch(root: &Path, vanilla: &Path, mods: &[PathBuf], disabled: &[String], out: &Path, key: &str, proxy: &EventLoopProxy<Ev>) -> bool {
    let cache = root.join("cache");
    let _ = std::fs::create_dir_all(&cache);
    let _ = std::fs::remove_file(cache.join("build.key"));
    let names: Vec<String> = mods.iter().map(|p| stem(p)).collect();
    let states: Vec<String> = names.iter().map(|n| format!("{}:\"wait\"", js_str(n))).collect();
    send_js(proxy, format!("rs.clearLog();rs.update({{phase:\"loading\",progress:0,current:\"\",modStates:{{{}}}}})", states.join(",")));
    let patcher = root.join("patcher").join("ModPatcher.exe");
    let mut c = Command::new(&patcher);
    c.arg("--msl").arg(root.join("patcher")).arg("--vanilla").arg(vanilla).arg("--mods").arg(root.join("mods")).arg("--out").arg(out);
    c.arg("--order").arg(root.join("mods").join("load_order.txt")).arg("--log").arg(cache.join("patch.log"));
    if !disabled.is_empty() {
        c.arg("--skip").arg(disabled.join("|"));
    }
    c.stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(windows)]
    c.creation_flags(CREATE_NO_WINDOW);
    let _ = std::fs::remove_file(cache.join("patch.log"));
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => {
            log(proxy, "e", &format!("could not start {}: {e}", patcher.display()));
            return false;
        }
    };
    let t0 = std::time::Instant::now();
    let err = child.stderr.take().unwrap();
    let p2 = proxy.clone();
    let errs = std::thread::spawn(move || {
        let mut last = String::new();
        for line in BufReader::new(err).lines().map_while(Result::ok) {
            // warnings (the compiler's "Cannot infer the instance type ...") stay in patch.log;
            // the log shows errors only
            let l = line.trim();
            if l.contains(" ERR ") || l.starts_with("error") {
                log(&p2, "e", l.trim_start_matches("[msl] ").trim_start_matches("ERR ").trim_start_matches("error: "));
                last = line;
            }
        }
        last
    });
    let total = names.len().max(1) as f64;
    let mut done = 0usize;
    let mut current = String::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines().map_while(Result::ok) {
        if let Some(m) = line.strip_prefix("@patching ") {
            current = m.to_string();
            send_js(proxy, format!("S.modStates[{0}]=\"run\";rs.update({{phase:\"patching\",current:{0},progress:{1}}})", js_str(m), done as f64 / total));
            log(proxy, "", &format!("{m}..."));
        } else if let Some(m) = line.strip_prefix("@done ") {
            done += 1;
            let saving = if done == names.len() { ",phase:\"saving\"" } else { "" };
            send_js(proxy, format!("S.modStates[{}]=\"done\";rs.update({{progress:{}{saving}}})", js_str(m), done as f64 / total));
        }
        // the patcher's own lines (loading, saving) go to patch.log only
    }
    let ok = child.wait().map(|s| s.success()).unwrap_or(false) && out.exists();
    let _ = errs.join();
    if ok {
        let _ = std::fs::write(cache.join("build.key"), key);
        log(proxy, "o", &format!("data.win: {} mods, {:.0} s", names.len(), t0.elapsed().as_secs_f64()));
    } else {
        if !current.is_empty() {
            send_js(proxy, format!("S.modStates[{}]=\"bad\"", js_str(&current)));
        }
        log(proxy, "e", &format!("patch.log: {}", cache.join("patch.log").display()));
    }
    ok
}

fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut o = String::with_capacity(b.len() * 4 / 3 + 4);
    for ch in b.chunks(3) {
        let n = (ch[0] as u32) << 16 | (*ch.get(1).unwrap_or(&0) as u32) << 8 | *ch.get(2).unwrap_or(&0) as u32;
        o.push(T[(n >> 18) as usize & 63] as char);
        o.push(T[(n >> 12) as usize & 63] as char);
        o.push(if ch.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        o.push(if ch.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    o
}

fn main() {
    let root = root_dir();
    update::cleanup(&root);
    {
        // right after an update the old launcher may still be closing: Launcher.old.exe goes once it can
        let r = root.clone();
        std::thread::spawn(move || {
            for _ in 0..40 {
                if !r.join("Launcher.old.exe").exists() {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
                update::cleanup(&r);
            }
        });
    }
    let settings = load_settings(&root);
    // RSR_NO_UPDATE=1: no update check (tests)
    let mut update_due = std::env::var_os("RSR_NO_UPDATE").is_none();
    // --updated: started by an update; no second launcher update now (a release whose exe has the
    // wrong LAUNCHER_BUILD would otherwise update again on every start)
    let just_updated = std::env::args().any(|a| a == "--updated");
    save_settings(&root, &settings); // keys.ini exists from the first start
    // --autoplay: Play as soon as the page is up (tests of the whole path without a click)
    let mut autoplay = std::env::args().any(|a| a == "--autoplay");
    // closed while the game runs: the window goes, the process stays until the game ends
    let mut game_running = false;
    let mut exit_after_game = false;
    let app = Arc::new(Mutex::new(App { root: root.clone(), game: the_game(&settings), settings, busy: false }));

    let html = include_str!("../ui/index.html").replace("src=\"logo.png\"", &format!("src=\"data:image/png;base64,{}\"", base64(include_bytes!("../ui/logo.png"))));
    let event_loop = EventLoopBuilder::<Ev>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    // the window's icon: the logo (icon/icon64.rgba, 64x64 RGBA, made with app.ico)
    let icon = tao::window::Icon::from_rgba(include_bytes!("../icon/icon64.rgba").to_vec(), 64, 64).ok();
    let window = WindowBuilder::new()
        .with_title("Rogue Shard Launcher")
        .with_inner_size(tao::dpi::LogicalSize::new(980.0, 660.0))
        .with_min_inner_size(tao::dpi::LogicalSize::new(820.0, 600.0))
        .with_window_icon(icon)
        .build(&event_loop)
        .expect("window");
    let ipc_proxy = proxy.clone();
    let ipc_self = proxy.clone();
    // WebView2's own files in cache\webview, not beside the exe
    let mut web_context = wry::WebContext::new(Some(root.join("cache").join("webview")));
    let webview = WebViewBuilder::new_with_web_context(&mut web_context)
        .with_html(html)
        .with_background_color((0x0e, 0x10, 0x22, 0xff))
        .with_devtools(std::env::var_os("RSR_LAUNCHER_DEBUG").is_some())
        .with_ipc_handler(move |req| {
            let _ = ipc_proxy.send_event(Ev::Ipc(req.body().clone()));
        })
        .build(&window)
        .expect("WebView2: install the Microsoft Edge WebView2 Runtime");

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if game_running {
                    window.set_visible(false);
                    exit_after_game = true;
                } else {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(Ev::Quit) => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(Ev::GameStarted) => {
                game_running = true;
            }
            Event::UserEvent(Ev::GameEnded(ok)) => {
                game_running = false;
                let _ = ok;
                if exit_after_game {
                    *control_flow = ControlFlow::Exit;
                } else {
                    window.set_visible(true);
                    window.set_focus();
                }
            }
            Event::UserEvent(Ev::Js(js)) => {
                let _ = webview.evaluate_script(&js);
            }
            Event::UserEvent(Ev::Ipc(msg)) => {
                if std::env::var_os("RSR_LAUNCHER_DEBUG").is_some() {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(root.join("launcher_debug.log")) {
                        let _ = writeln!(f, "ipc {msg}");
                    }
                }
                let mut a = app.lock().unwrap();
                let (cmd, arg) = msg.split_once(':').map(|(c, v)| (c.to_string(), v.to_string())).unwrap_or((msg.clone(), String::new()));
                match cmd.as_str() {
                    "ready" | "refresh" => {
                        if a.game.is_none() {
                            a.game = the_game(&a.settings);
                        }
                        let js = a.state_js();
                        let _ = webview.evaluate_script(&js);
                        if cmd == "ready" && autoplay {
                            autoplay = false;
                            update_due = false;
                            let _ = ipc_self.send_event(Ev::Ipc("play".into()));
                        }
                        // the first time the page is up: updates from GitHub, Play waits for them
                        if cmd == "ready" && update_due && !a.busy && (a.settings.auto_mod || a.settings.auto_launcher) {
                            update_due = false;
                            a.busy = true;
                            let (app2, p2, root2) = (app.clone(), proxy.clone(), a.root.clone());
                            let (am, al) = (a.settings.auto_mod, a.settings.auto_launcher);
                            let _ = webview.evaluate_script("rs.update({phase:'updating'})");
                            std::thread::spawn(move || {
                                let lg = |c: &str, t: &str| log(&p2, c, t);
                                match update::releases() {
                                    Ok(rs) => {
                                        if al && !just_updated && update::update_launcher(&root2, &rs, &lg) {
                                            let _ = Command::new(root2.join("Launcher.exe")).arg("--updated").current_dir(&root2).spawn();
                                            let _ = p2.send_event(Ev::Quit);
                                            return;
                                        }
                                        if am {
                                            update::update_mod(&root2, &rs, &lg);
                                        }
                                    }
                                    Err(e) => lg("e", &format!("updates: {e}")),
                                }
                                app2.lock().unwrap().busy = false;
                                send_js(&p2, "rs.update({phase:'ready'})".into());
                                let js = app2.lock().unwrap().state_js();
                                send_js(&p2, js);
                            });
                        }
                    }
                    "lang" => {
                        a.settings.lang = if arg == "en" { "en" } else { "ru" }.into();
                        save_settings(&a.root, &a.settings);
                        let js = a.state_js();
                        let _ = webview.evaluate_script(&js);
                    }
                    "automod" | "autolauncher" => {
                        if cmd == "automod" {
                            a.settings.auto_mod = arg == "1";
                        } else {
                            a.settings.auto_launcher = arg == "1";
                        }
                        save_settings(&a.root, &a.settings);
                        let js = a.state_js();
                        let _ = webview.evaluate_script(&js);
                    }
                    "rebuild" => {
                        a.settings.rebuild = arg == "1";
                        save_settings(&a.root, &a.settings);
                        let js = a.state_js();
                        let _ = webview.evaluate_script(&js);
                    }
                    "toggle" if !a.busy => {
                        if let Some(i) = a.settings.disabled.iter().position(|d| d.eq_ignore_ascii_case(&arg)) {
                            a.settings.disabled.remove(i);
                        } else {
                            a.settings.disabled.push(arg.clone());
                        }
                        save_settings(&a.root, &a.settings);
                        let js = a.state_js();
                        let _ = webview.evaluate_script(&js);
                    }
                    "move" if !a.busy => {
                        // move:<name>:<new index>; the whole order goes to load_order.txt
                        if let Some((name, to)) = arg.rsplit_once(':') {
                            let mut names: Vec<String> = mods_of(&a.root).iter().map(|p| stem(p)).collect();
                            if let (Some(from), Ok(to)) = (names.iter().position(|n| n == name), to.parse::<usize>()) {
                                let n = names.remove(from);
                                names.insert(to.min(names.len()), n);
                                write_order(&a.root, &names);
                            }
                        }
                        let js = a.state_js();
                        let _ = webview.evaluate_script(&js);
                    }
                    "keys" => {
                        let k: Vec<String> = arg.split('|').map(|s| s.trim().to_string()).collect();
                        if k.len() == 5 && k.iter().all(|s| !s.is_empty()) {
                            a.settings.keys = k;
                            save_settings(&a.root, &a.settings);
                        }
                    }
                    "speeds" => {
                        let v: Vec<Option<f64>> = arg.split('|').map(parse_speed).collect();
                        if v.len() == 5 && v.iter().all(|x| x.is_some()) {
                            a.settings.speeds = v.into_iter().flatten().collect();
                            save_settings(&a.root, &a.settings);
                        }
                        let js = a.state_js();
                        let _ = webview.evaluate_script(&js);
                    }
                    "open" => {
                        let target = match arg.as_str() {
                            "mods" => a.root.join("mods").display().to_string(),
                            "saves" => {
                                let d = std::env::var("LOCALAPPDATA").map(PathBuf::from).unwrap_or_default().join("StoneShard");
                                let _ = std::fs::create_dir_all(&d);
                                d.display().to_string()
                            }
                            "logs" => a.root.display().to_string(),
                            _ => WIKI.to_string(),
                        };
                        open(&target);
                    }
                    "pickgame" if !a.busy => {
                        // the folder dialog on its own thread: a modal loop on this one would run
                        // the event loop's handler inside itself
                        let (app2, p2) = (app.clone(), proxy.clone());
                        let owner = window_handle(&window);
                        let start = a.game.clone();
                        drop(a);
                        std::thread::spawn(move || {
                            let Some(d) = pick_folder(owner, start.as_deref()) else { return };
                            let mut a = app2.lock().unwrap();
                            match game_dir_in(&d) {
                                Some(g) => {
                                    log(&p2, "o", &format!("Stoneshard: {}", g.display()));
                                    a.settings.game = Some(g.clone());
                                    a.game = Some(g);
                                    save_settings(&a.root, &a.settings);
                                }
                                None => log(&p2, "e", &format!("{}: no StoneShard.exe or data.win here", d.display())),
                            }
                            let js = a.state_js();
                            drop(a);
                            send_js(&p2, "rs.update({phase:'ready'})".into());
                            send_js(&p2, js);
                        });
                    }
                    "play" if !a.busy => {
                        a.busy = true;
                        let (app2, p2) = (app.clone(), proxy.clone());
                        drop(a);
                        std::thread::spawn(move || play(app2, p2));
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    });
}

#[cfg(test)]
mod speed_tests {
    use super::*;

    #[test]
    fn typed_speeds_are_read() {
        assert_eq!(parse_speed("2"), Some(2.0));
        assert_eq!(parse_speed(" x1,5 "), Some(1.5));
        assert_eq!(parse_speed("4x"), Some(4.0));
        assert_eq!(parse_speed("\u{445}7"), Some(7.0));
        assert_eq!(parse_speed("0"), None);
        assert_eq!(parse_speed("500"), None);
        assert_eq!(parse_speed("abc"), None);
        assert_eq!(speed_text(2.0), "2");
        assert_eq!(speed_text(1.5), "1.5");
        assert_eq!(speed_text(0.25), "0.25");
    }
}
