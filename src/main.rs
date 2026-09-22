//! auxbookmark - Paper Plane xUI (PPx) aux: extension for Web Bookmarks
//!
//! Multi-browser Chromium bookmarks (Brave, Chrome, Edge) integration.
//!
//! Commands:
//!   list    <virtual_path> [output_file]
//!   get     <virtual_path> <dest_local_file>
//!   makedir <parent_path>  <folder_name>
//!   deldir  <virtual_path>
//!   delete  <parent_path>  <item_name> [item_name2 ...]
//!   move    <src_path>     <dest_path>
//!   copy    <src_path>     <dest_path>
//!   rename  <src_path>     <dest_path>

use std::env;
use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::SystemTime;
use anyhow::{anyhow, Context, Result};
use chrono::{DateTime, Local, Utc};
use serde_json::{json, Value};

const TRASH_FOLDER_NAME: &str = "ゴミ箱";
const ISOLATE_FOLDER_NAME: &str = "リンク切れ";
const HISTORY_FOLDER_NAME: &str = "履歴";
const BOOKMARK_BAR_JP: &str = "ブックマーク バー";
const OTHER_BOOKMARKS_JP: &str = "その他のブックマーク";
const BOOKMARK_MENU_JP: &str = "ブックマーク メニュー";

// ==============================================================================
// 外部ツール非依存の UUID v4 & Firefox GUID 生成
// ==============================================================================

fn generate_guid() -> String {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id() as u128;
    static mut COUNTER: u64 = 0;
    let cnt = unsafe {
        COUNTER += 1;
        COUNTER as u128
    };

    let mut state = now ^ (pid << 32) ^ (cnt << 64);
    let mut rand_bytes = [0u8; 16];
    for b in rand_bytes.iter_mut() {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        *b = (state >> 56) as u8;
    }

    // UUID v4: byte 6 の上位4bitを 0100 (version 4), byte 8 の上位2bitを 10 (variant RFC4122)
    rand_bytes[6] = (rand_bytes[6] & 0x0f) | 0x40;
    rand_bytes[8] = (rand_bytes[8] & 0x3f) | 0x80;

    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        rand_bytes[0], rand_bytes[1], rand_bytes[2], rand_bytes[3],
        rand_bytes[4], rand_bytes[5],
        rand_bytes[6], rand_bytes[7],
        rand_bytes[8], rand_bytes[9],
        rand_bytes[10], rand_bytes[11], rand_bytes[12], rand_bytes[13], rand_bytes[14], rand_bytes[15]
    )
}

fn generate_firefox_guid() -> String {
    const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789-_";
    let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let pid = std::process::id() as u128;
    static mut CNT: u64 = 0;
    let c = unsafe {
        CNT += 1;
        CNT as u128
    };
    let mut state = now ^ (pid << 48) ^ (c << 32);

    let mut out = String::with_capacity(12);
    for _ in 0..12 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let idx = ((state >> 58) as usize) % CHARS.len();
        out.push(CHARS[idx] as char);
    }
    out
}

// ==============================================================================
// Windows 公式 winsqlite3.dll FFI バインディング (外部クレート/Cコンパイラ不要)
// ==============================================================================

unsafe extern "system" {
    fn LoadLibraryA(lpLibFileName: *const c_char) -> *mut c_void;
    fn GetProcAddress(hModule: *mut c_void, lpProcName: *const c_char) -> *mut c_void;
}

struct SqliteLib {
    open_v2: unsafe extern "C" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int,
    prepare_v2: unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void, *mut *const c_char) -> c_int,
    step: unsafe extern "C" fn(*mut c_void) -> c_int,
    column_int64: unsafe extern "C" fn(*mut c_void, c_int) -> i64,
    column_text: unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char,
    bind_int64: unsafe extern "C" fn(*mut c_void, c_int, i64) -> c_int,
    bind_text: unsafe extern "C" fn(*mut c_void, c_int, *const c_char, c_int, *const c_void) -> c_int,
    bind_null: unsafe extern "C" fn(*mut c_void, c_int) -> c_int,
    finalize: unsafe extern "C" fn(*mut c_void) -> c_int,
    exec: unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void, *const c_void, *mut *mut c_char) -> c_int,
    close: unsafe extern "C" fn(*mut c_void) -> c_int,
    last_insert_rowid: unsafe extern "C" fn(*mut c_void) -> i64,
}

impl SqliteLib {
    fn load() -> Result<Self> {
        unsafe {
            let dll_name = CString::new("winsqlite3.dll").unwrap();
            let h_mod = LoadLibraryA(dll_name.as_ptr());
            if h_mod.is_null() {
                return Err(anyhow!("Failed to load winsqlite3.dll from Windows System32"));
            }

            macro_rules! get_sym {
                ($name:ident, $type:ty) => {{
                    let s_name = CString::new(stringify!($name)).unwrap();
                    let ptr = GetProcAddress(h_mod, s_name.as_ptr());
                    if ptr.is_null() {
                        return Err(anyhow!("Failed to locate sqlite symbol: {}", stringify!($name)));
                    }
                    std::mem::transmute::<*mut c_void, $type>(ptr)
                }};
            }

            Ok(SqliteLib {
                open_v2: get_sym!(sqlite3_open_v2, unsafe extern "C" fn(*const c_char, *mut *mut c_void, c_int, *const c_char) -> c_int),
                prepare_v2: get_sym!(sqlite3_prepare_v2, unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut *mut c_void, *mut *const c_char) -> c_int),
                step: get_sym!(sqlite3_step, unsafe extern "C" fn(*mut c_void) -> c_int),
                column_int64: get_sym!(sqlite3_column_int64, unsafe extern "C" fn(*mut c_void, c_int) -> i64),
                column_text: get_sym!(sqlite3_column_text, unsafe extern "C" fn(*mut c_void, c_int) -> *const c_char),
                bind_int64: get_sym!(sqlite3_bind_int64, unsafe extern "C" fn(*mut c_void, c_int, i64) -> c_int),
                bind_text: get_sym!(sqlite3_bind_text, unsafe extern "C" fn(*mut c_void, c_int, *const c_char, c_int, *const c_void) -> c_int),
                bind_null: get_sym!(sqlite3_bind_null, unsafe extern "C" fn(*mut c_void, c_int) -> c_int),
                finalize: get_sym!(sqlite3_finalize, unsafe extern "C" fn(*mut c_void) -> c_int),
                exec: get_sym!(sqlite3_exec, unsafe extern "C" fn(*mut c_void, *const c_char, *const c_void, *const c_void, *mut *mut c_char) -> c_int),
                close: get_sym!(sqlite3_close, unsafe extern "C" fn(*mut c_void) -> c_int),
                last_insert_rowid: get_sym!(sqlite3_last_insert_rowid, unsafe extern "C" fn(*mut c_void) -> i64),
            })
        }
    }
}

// ==============================================================================
// 設定 & プロファイル管理
// ==============================================================================

#[derive(Debug, Clone, PartialEq)]
enum ProfileBackend {
    ChromiumJson,
    FirefoxSqlite,
}

#[derive(Debug, Clone)]
struct Profile {
    name: String,
    bookmark_path: PathBuf,
    backend: ProfileBackend,
}

#[derive(Debug, Clone)]
struct Config {
    profiles: Vec<Profile>,
    backup_interval_minutes: u64,
    backup_keep_generations: usize,
}

fn get_exe_dir() -> PathBuf {
    env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|p| p.to_path_buf()))
        .unwrap_or_else(|| PathBuf::from("."))
}

static DEBUG_MODE: AtomicBool = AtomicBool::new(false);

fn set_debug_mode(enabled: bool) {
    DEBUG_MODE.store(enabled, Ordering::Relaxed);
}

fn log_msg(msg: &str) {
    if !DEBUG_MODE.load(Ordering::Relaxed) {
        return;
    }
    eprintln!("{}", msg);
    let exe_dir = get_exe_dir();
    let log_path = exe_dir.join("auxbookmark.log");
    if let Ok(mut f) = OpenOptions::new().create(true).append(true).open(log_path) {
        let now = Local::now().format("%Y-%m-%d %H:%M:%S");
        let _ = writeln!(f, "[{}] {}", now, msg);
    }
}

fn is_opt(arg: &str, names: &[&str]) -> bool {
    let s = arg.trim();
    let without_prefix = if let Some(r) = s.strip_prefix("--") {
        r
    } else if let Some(r) = s.strip_prefix('-') {
        r
    } else if let Some(r) = s.strip_prefix('/') {
        r
    } else {
        return false;
    };
    names.iter().any(|&n| without_prefix.eq_ignore_ascii_case(n))
}

fn strip_opt_val<'a>(arg: &'a str, names: &[&str]) -> Option<&'a str> {
    let s = arg.trim();
    let rest = if let Some(r) = s.strip_prefix("--") {
        r
    } else if let Some(r) = s.strip_prefix('-') {
        r
    } else if let Some(r) = s.strip_prefix('/') {
        r
    } else {
        return None;
    };

    for &name in names {
        if let Some(after) = rest.get(..name.len()) {
            if after.eq_ignore_ascii_case(name) {
                let remainder = &rest[name.len()..];
                if remainder.starts_with('=') || remainder.starts_with(':') {
                    return Some(&remainder[1..]);
                }
            }
        }
    }
    None
}

fn detect_firefox_profile(base_dir: &Path) -> Option<PathBuf> {
    let ini_path = base_dir.join("profiles.ini");
    if !ini_path.exists() {
        return None;
    }

    let file = File::open(&ini_path).ok()?;
    let reader = BufReader::new(file);
    let mut default_rel_path = None;
    let mut first_profile_path = None;

    for line_res in reader.lines() {
        if let Ok(line) = line_res {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("Default=") {
                if rest.contains("Profiles/") || rest.contains("Profiles\\") {
                    default_rel_path = Some(rest.to_string());
                }
            } else if let Some(rest) = trimmed.strip_prefix("Path=") {
                if first_profile_path.is_none() && (rest.contains("Profiles/") || rest.contains("Profiles\\")) {
                    first_profile_path = Some(rest.to_string());
                }
            }
        }
    }

    let target_rel = default_rel_path.or(first_profile_path)?;
    let p = base_dir.join(target_rel).join("places.sqlite");
    if p.exists() {
        Some(p)
    } else {
        None
    }
}

fn detect_default_profiles() -> Vec<Profile> {
    let mut profiles = Vec::new();

    // 1. Chromium 系 (LOCALAPPDATA)
    if let Some(d) = env::var_os("LOCALAPPDATA") {
        let local_app_data = PathBuf::from(d);

        let brave = local_app_data.join(r"BraveSoftware\Brave-Browser\User Data\Default\Bookmarks");
        if brave.exists() {
            profiles.push(Profile {
                name: "Brave".to_string(),
                bookmark_path: brave,
                backend: ProfileBackend::ChromiumJson,
            });
        }

        let chrome = local_app_data.join(r"Google\Chrome\User Data\Default\Bookmarks");
        if chrome.exists() {
            profiles.push(Profile {
                name: "Chrome".to_string(),
                bookmark_path: chrome,
                backend: ProfileBackend::ChromiumJson,
            });
        }

        let edge = local_app_data.join(r"Microsoft\Edge\User Data\Default\Bookmarks");
        if edge.exists() {
            profiles.push(Profile {
                name: "Edge".to_string(),
                bookmark_path: edge,
                backend: ProfileBackend::ChromiumJson,
            });
        }
    }

    // 2. Firefox / Firefox 互換ブラウザ (APPDATA)
    if let Some(r) = env::var_os("APPDATA") {
        let roaming_path = PathBuf::from(r);

        // Firefox
        if let Some(ff_db) = detect_firefox_profile(&roaming_path.join(r"Mozilla\Firefox")) {
            profiles.push(Profile {
                name: "Firefox".to_string(),
                bookmark_path: ff_db,
                backend: ProfileBackend::FirefoxSqlite,
            });
        }

        // Firefox 互換ブラウザ
        if let Some(compat_db) = detect_firefox_profile(&roaming_path.join("mercury")) {
            profiles.push(Profile {
                name: "Firefox-Compat".to_string(),
                bookmark_path: compat_db,
                backend: ProfileBackend::FirefoxSqlite,
            });
        }
    }

    profiles
}

fn load_config() -> Result<Config> {
    let exe_dir = get_exe_dir();
    let ini_path = exe_dir.join("auxbookmark.ini");

    let mut configured_profiles = Vec::new();
    let mut backup_interval_minutes = 30u64;
    let mut backup_keep_generations = 3usize;
    let mut in_profiles_section = false;

    if ini_path.exists() {
        if let Ok(file) = File::open(&ini_path) {
            let reader = BufReader::new(file);
            for line_res in reader.lines() {
                if let Ok(line) = line_res {
                    let trimmed = line.trim();
                    if trimmed.is_empty() || trimmed.starts_with(';') || trimmed.starts_with('#') {
                        continue;
                    }
                    if trimmed.starts_with('[') && trimmed.ends_with(']') {
                        let sec = trimmed[1..trimmed.len() - 1].trim().to_lowercase();
                        in_profiles_section = sec == "profiles";
                        continue;
                    }
                    if let Some(idx) = trimmed.find('=') {
                        let key = trimmed[..idx].trim();
                        let val = trimmed[idx + 1..].trim().trim_matches('"');
                        if in_profiles_section {
                            let path = PathBuf::from(val);
                            if path.exists() {
                                let is_sqlite = val.ends_with(".sqlite") || val.contains("places.sqlite");
                                let backend = if is_sqlite {
                                    ProfileBackend::FirefoxSqlite
                                } else {
                                    ProfileBackend::ChromiumJson
                                };
                                configured_profiles.push(Profile {
                                    name: key.to_string(),
                                    bookmark_path: path,
                                    backend,
                                });
                            }
                        } else if key.eq_ignore_ascii_case("backup_interval_minutes") {
                            if let Ok(n) = val.parse::<u64>() {
                                backup_interval_minutes = n;
                            }
                        } else if key.eq_ignore_ascii_case("backup_keep_generations") {
                            if let Ok(n) = val.parse::<usize>() {
                                backup_keep_generations = n;
                            }
                        } else if key.eq_ignore_ascii_case("debug") {
                            if val.eq_ignore_ascii_case("true") || val == "1" {
                                set_debug_mode(true);
                            }
                        }
                    }
                }
            }
        }
    }

    let profiles = if !configured_profiles.is_empty() {
        configured_profiles
    } else {
        detect_default_profiles()
    };

    if profiles.is_empty() {
        return Err(anyhow!(
            "No browser bookmarks found (Brave, Chrome, Edge). Please specify your path in auxbookmark.ini."
        ));
    }

    Ok(Config {
        profiles,
        backup_interval_minutes,
        backup_keep_generations,
    })
}

fn find_profile<'a>(config: &'a Config, name: &str) -> Option<&'a Profile> {
    config.profiles.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

// ==============================================================================
// 自動バックアップ
// ==============================================================================

fn prune_old_backups(backup_dir: &Path, profile_name: &str, keep: usize) {
    if keep == 0 {
        return;
    }
    let prefix = format!("{}_Bookmarks_", profile_name);
    let mut files = Vec::new();
    if let Ok(entries) = fs::read_dir(backup_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(file_name) = path.file_name().and_then(|n| n.to_str()) {
                    if file_name.starts_with(&prefix) && file_name.ends_with(".bak") {
                        files.push(path);
                    }
                }
            }
        }
    }
    files.sort();

    if files.len() > keep {
        let remove_count = files.len() - keep;
        for path in &files[..remove_count] {
            let _ = fs::remove_file(path);
            log_msg(&format!("[{}] Pruned old backup: {:?}", profile_name, path));
        }
    }
}

fn maybe_backup(profile: &Profile, interval_mins: u64, keep_generations: usize) -> Result<()> {
    let exe_dir = get_exe_dir();
    let backup_dir = exe_dir.join("backups");
    if !backup_dir.exists() {
        fs::create_dir_all(&backup_dir).context("Failed to create backups directory")?;
    }

    let stamp_file = backup_dir.join(format!(".last_backup_{}", profile.name.to_lowercase()));
    let now_utc = Utc::now().timestamp();

    let mut need_backup = true;
    if stamp_file.exists() {
        if let Ok(content) = fs::read_to_string(&stamp_file) {
            if let Ok(last_ts) = content.trim().parse::<i64>() {
                let interval_secs = (interval_mins as i64) * 60;
                if now_utc - last_ts < interval_secs {
                    need_backup = false;
                }
            }
        }
    }

    if need_backup {
        let now_local = Local::now().format("%Y%m%d_%H%M%S");
        let backup_file_name = format!("{}_Bookmarks_{}.bak", profile.name, now_local);
        let backup_dest = backup_dir.join(backup_file_name);

        fs::copy(&profile.bookmark_path, &backup_dest)
            .context("Failed to create backup of Bookmarks file")?;

        let _ = fs::write(&stamp_file, now_utc.to_string());
        log_msg(&format!("[{}] Backup created: {:?}", profile.name, backup_dest));

        prune_old_backups(&backup_dir, &profile.name, keep_generations);
    }

    Ok(())
}

// ==============================================================================
// ブックマーク JSON 操作 & タイムスタンプ
// ==============================================================================

fn read_bookmarks(path: &Path) -> Result<Value> {
    let content = fs::read_to_string(path).context("Failed to read Bookmarks file")?;
    let val: Value = serde_json::from_str(&content).context("Failed to parse Bookmarks JSON")?;
    Ok(val)
}

fn write_bookmarks(path: &Path, val: &Value) -> Result<()> {
    let json_bytes = serde_json::to_vec_pretty(val).context("Failed to serialize Bookmarks JSON")?;
    let tmp_path = path.with_extension("tmp");

    fs::write(&tmp_path, json_bytes).context("Failed to write temporary Bookmarks file")?;

    if path.exists() {
        let _ = fs::remove_file(path);
    }
    fs::rename(&tmp_path, path).context("Failed to replace Bookmarks file")?;
    Ok(())
}

#[derive(Debug, Clone)]
struct FxRow {
    id: i64,
    b_type: i64,
    title: String,
    url: Option<String>,
    parent: i64,
    position: i64,
    date_added: i64,
    last_modified: i64,
    guid: String,
}

fn read_firefox_bookmarks(path: &Path) -> Result<Value> {
    let sqlite = SqliteLib::load()?;
    let path_str = path.to_str().ok_or_else(|| anyhow!("Invalid path unicode"))?;
    let c_path = CString::new(path_str)?;

    let mut db: *mut c_void = ptr::null_mut();
    // 1 | 0x40 = SQLITE_OPEN_READONLY | SQLITE_OPEN_URI
    let rc = unsafe { (sqlite.open_v2)(c_path.as_ptr(), &mut db, 1 | 0x40, ptr::null()) };
    if rc != 0 {
        return Err(anyhow!("Failed to open Firefox places.sqlite (rc={})", rc));
    }

    let sql = CString::new(
        "SELECT b.id, b.type, b.title, p.url, b.parent, b.position, b.dateAdded, b.lastModified, b.guid \
         FROM moz_bookmarks b \
         LEFT JOIN moz_places p ON b.fk = p.id \
         ORDER BY b.parent ASC, b.position ASC"
    )?;

    let mut stmt: *mut c_void = ptr::null_mut();
    let rc = unsafe { (sqlite.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) };
    if rc != 0 {
        unsafe { (sqlite.close)(db) };
        return Err(anyhow!("Failed to prepare query on places.sqlite (rc={})", rc));
    }

    let mut rows = Vec::new();
    unsafe {
        while (sqlite.step)(stmt) == 100 { // SQLITE_ROW
            let id = (sqlite.column_int64)(stmt, 0);
            let b_type = (sqlite.column_int64)(stmt, 1);
            let title_ptr = (sqlite.column_text)(stmt, 2);
            let title = if !title_ptr.is_null() {
                CStr::from_ptr(title_ptr).to_string_lossy().to_string()
            } else {
                String::new()
            };
            let url_ptr = (sqlite.column_text)(stmt, 3);
            let url = if !url_ptr.is_null() {
                Some(CStr::from_ptr(url_ptr).to_string_lossy().to_string())
            } else {
                None
            };
            let parent = (sqlite.column_int64)(stmt, 4);
            let position = (sqlite.column_int64)(stmt, 5);
            let date_added = (sqlite.column_int64)(stmt, 6);
            let last_modified = (sqlite.column_int64)(stmt, 7);
            let guid_ptr = (sqlite.column_text)(stmt, 8);
            let guid = if !guid_ptr.is_null() {
                CStr::from_ptr(guid_ptr).to_string_lossy().to_string()
            } else {
                generate_firefox_guid()
            };

            rows.push(FxRow {
                id,
                b_type,
                title,
                url,
                parent,
                position,
                date_added,
                last_modified,
                guid,
            });
        }
        (sqlite.finalize)(stmt);
        (sqlite.close)(db);
    }

    fn build_tree(parent_id: i64, rows: &[FxRow]) -> Vec<Value> {
        let mut items = Vec::new();
        for r in rows.iter().filter(|x| x.parent == parent_id) {
            let mut obj = serde_json::Map::new();
            obj.insert("id".to_string(), json!(r.id.to_string()));
            obj.insert("_firefox_id".to_string(), json!(r.id));
            obj.insert("guid".to_string(), json!(r.guid));
            obj.insert("name".to_string(), json!(r.title));
            obj.insert("date_added".to_string(), json!(r.date_added.to_string()));
            obj.insert("date_modified".to_string(), json!(r.last_modified.to_string()));

            if r.b_type == 2 {
                obj.insert("type".to_string(), json!("folder"));
                obj.insert("children".to_string(), json!(build_tree(r.id, rows)));
                items.push(Value::Object(obj));
            } else if r.b_type == 1 {
                obj.insert("type".to_string(), json!("url"));
                obj.insert("url".to_string(), json!(r.url.clone().unwrap_or_default()));
                items.push(Value::Object(obj));
            }
        }
        items
    }

    let bar_children = build_tree(3, &rows);    // toolbar (id: 3)
    let other_children = build_tree(5, &rows);  // unfiled (id: 5)
    let menu_children = build_tree(2, &rows);   // menu (id: 2)

    let val = json!({
        "roots": {
            "bookmark_bar": {
                "id": "3",
                "_firefox_id": 3,
                "name": BOOKMARK_BAR_JP,
                "type": "folder",
                "children": bar_children
            },
            "other": {
                "id": "5",
                "_firefox_id": 5,
                "name": OTHER_BOOKMARKS_JP,
                "type": "folder",
                "children": other_children
            },
            "menu": {
                "id": "2",
                "_firefox_id": 2,
                "name": BOOKMARK_MENU_JP,
                "type": "folder",
                "children": menu_children
            }
        },
        "version": 1
    });

    Ok(val)
}

fn write_firefox_bookmarks(path: &Path, val: &Value) -> Result<()> {
    let sqlite = SqliteLib::load()?;
    let path_str = path.to_str().ok_or_else(|| anyhow!("Invalid path unicode"))?;
    let c_path = CString::new(path_str)?;

    let mut db: *mut c_void = ptr::null_mut();
    // 6 = SQLITE_OPEN_READWRITE | SQLITE_OPEN_CREATE
    let rc = unsafe { (sqlite.open_v2)(c_path.as_ptr(), &mut db, 6, ptr::null()) };
    if rc != 0 {
        return Err(anyhow!("Failed to open places.sqlite for writing (rc={})", rc));
    }

    unsafe {
        let pragma = CString::new("PRAGMA busy_timeout = 5000; BEGIN IMMEDIATE TRANSACTION;").unwrap();
        (sqlite.exec)(db, pragma.as_ptr(), ptr::null(), ptr::null(), ptr::null_mut());
    }

    // 既存のユーザーID (id > 6) を収集
    let mut existing_ids = Vec::new();
    unsafe {
        let sql = CString::new("SELECT id FROM moz_bookmarks WHERE id > 6").unwrap();
        let mut stmt: *mut c_void = ptr::null_mut();
        if (sqlite.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
            while (sqlite.step)(stmt) == 100 {
                existing_ids.push((sqlite.column_int64)(stmt, 0));
            }
            (sqlite.finalize)(stmt);
        }
    }

    let mut retained_ids = Vec::new();
    let now_micros = Utc::now().timestamp_micros();

    fn sync_children(
        sqlite: &SqliteLib,
        db: *mut c_void,
        parent_id: i64,
        children: &[Value],
        retained: &mut Vec<i64>,
        now_micros: i64,
    ) -> Result<()> {
        for (pos, child) in children.iter().enumerate() {
            let n_type = child.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let name = child.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let ff_id = child.get("_firefox_id").and_then(|v| v.as_i64());

            let c_title = CString::new(name).unwrap_or_default();

            if n_type == "folder" {
                let current_folder_id = if let Some(id) = ff_id {
                    retained.push(id);
                    unsafe {
                        let sql = CString::new("UPDATE moz_bookmarks SET parent = ?, position = ?, title = ?, lastModified = ? WHERE id = ?").unwrap();
                        let mut stmt: *mut c_void = ptr::null_mut();
                        if (sqlite.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                            (sqlite.bind_int64)(stmt, 1, parent_id);
                            (sqlite.bind_int64)(stmt, 2, pos as i64);
                            (sqlite.bind_text)(stmt, 3, c_title.as_ptr(), -1, ptr::null());
                            (sqlite.bind_int64)(stmt, 4, now_micros);
                            (sqlite.bind_int64)(stmt, 5, id);
                            (sqlite.step)(stmt);
                            (sqlite.finalize)(stmt);
                        }
                    }
                    id
                } else {
                    // 新規フォルダ
                    let guid = generate_firefox_guid();
                    let c_guid = CString::new(guid).unwrap_or_default();
                    let mut new_id = 0i64;
                    unsafe {
                        let sql = CString::new("INSERT INTO moz_bookmarks (type, parent, position, title, dateAdded, lastModified, guid) VALUES (2, ?, ?, ?, ?, ?, ?)").unwrap();
                        let mut stmt: *mut c_void = ptr::null_mut();
                        if (sqlite.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                            (sqlite.bind_int64)(stmt, 1, parent_id);
                            (sqlite.bind_int64)(stmt, 2, pos as i64);
                            (sqlite.bind_text)(stmt, 3, c_title.as_ptr(), -1, ptr::null());
                            (sqlite.bind_int64)(stmt, 4, now_micros);
                            (sqlite.bind_int64)(stmt, 5, now_micros);
                            (sqlite.bind_text)(stmt, 6, c_guid.as_ptr(), -1, ptr::null());
                            (sqlite.step)(stmt);
                            (sqlite.finalize)(stmt);
                            new_id = (sqlite.last_insert_rowid)(db);
                        }
                    }
                    retained.push(new_id);
                    new_id
                };

                if let Some(sub_children) = child.get("children").and_then(|v| v.as_array()) {
                    sync_children(sqlite, db, current_folder_id, sub_children, retained, now_micros)?;
                }
            } else if n_type == "url" {
                let url = child.get("url").and_then(|v| v.as_str()).unwrap_or("");
                let c_url = CString::new(url).unwrap_or_default();

                let mut place_id = 0i64;
                unsafe {
                    let ins_place = CString::new("INSERT OR IGNORE INTO moz_places (url, title, guid, date_added) VALUES (?, ?, ?, ?)").unwrap();
                    let mut stmt: *mut c_void = ptr::null_mut();
                    if (sqlite.prepare_v2)(db, ins_place.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                        let p_guid = CString::new(generate_firefox_guid()).unwrap_or_default();
                        (sqlite.bind_text)(stmt, 1, c_url.as_ptr(), -1, ptr::null());
                        (sqlite.bind_text)(stmt, 2, c_title.as_ptr(), -1, ptr::null());
                        (sqlite.bind_text)(stmt, 3, p_guid.as_ptr(), -1, ptr::null());
                        (sqlite.bind_int64)(stmt, 4, now_micros);
                        (sqlite.step)(stmt);
                        (sqlite.finalize)(stmt);
                    }

                    let sel_place = CString::new("SELECT id FROM moz_places WHERE url = ?").unwrap();
                    if (sqlite.prepare_v2)(db, sel_place.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                        (sqlite.bind_text)(stmt, 1, c_url.as_ptr(), -1, ptr::null());
                        if (sqlite.step)(stmt) == 100 {
                            place_id = (sqlite.column_int64)(stmt, 0);
                        }
                        (sqlite.finalize)(stmt);
                    }
                }

                if let Some(id) = ff_id {
                    retained.push(id);
                    unsafe {
                        let sql = CString::new("UPDATE moz_bookmarks SET fk = ?, parent = ?, position = ?, title = ?, lastModified = ? WHERE id = ?").unwrap();
                        let mut stmt: *mut c_void = ptr::null_mut();
                        if (sqlite.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                            (sqlite.bind_int64)(stmt, 1, place_id);
                            (sqlite.bind_int64)(stmt, 2, parent_id);
                            (sqlite.bind_int64)(stmt, 3, pos as i64);
                            (sqlite.bind_text)(stmt, 4, c_title.as_ptr(), -1, ptr::null());
                            (sqlite.bind_int64)(stmt, 5, now_micros);
                            (sqlite.bind_int64)(stmt, 6, id);
                            (sqlite.step)(stmt);
                            (sqlite.finalize)(stmt);
                        }
                    }
                } else {
                    let guid = generate_firefox_guid();
                    let c_guid = CString::new(guid).unwrap_or_default();
                    unsafe {
                        let sql = CString::new("INSERT INTO moz_bookmarks (type, fk, parent, position, title, dateAdded, lastModified, guid) VALUES (1, ?, ?, ?, ?, ?, ?, ?)").unwrap();
                        let mut stmt: *mut c_void = ptr::null_mut();
                        if (sqlite.prepare_v2)(db, sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                            (sqlite.bind_int64)(stmt, 1, place_id);
                            (sqlite.bind_int64)(stmt, 2, parent_id);
                            (sqlite.bind_int64)(stmt, 3, pos as i64);
                            (sqlite.bind_text)(stmt, 4, c_title.as_ptr(), -1, ptr::null());
                            (sqlite.bind_int64)(stmt, 5, now_micros);
                            (sqlite.bind_int64)(stmt, 6, now_micros);
                            (sqlite.bind_text)(stmt, 7, c_guid.as_ptr(), -1, ptr::null());
                            (sqlite.step)(stmt);
                            (sqlite.finalize)(stmt);
                            let new_id = (sqlite.last_insert_rowid)(db);
                            retained.push(new_id);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    let roots = val.get("roots");
    if let Some(bar_children) = roots.and_then(|r| r.get("bookmark_bar")).and_then(|b| b.get("children")).and_then(|c| c.as_array()) {
        sync_children(&sqlite, db, 3, bar_children, &mut retained_ids, now_micros)?;
    }
    if let Some(other_children) = roots.and_then(|r| r.get("other")).and_then(|o| o.get("children")).and_then(|c| c.as_array()) {
        sync_children(&sqlite, db, 5, other_children, &mut retained_ids, now_micros)?;
    }
    if let Some(menu_children) = roots.and_then(|r| r.get("menu")).and_then(|m| m.get("children")).and_then(|c| c.as_array()) {
        sync_children(&sqlite, db, 2, menu_children, &mut retained_ids, now_micros)?;
    }

    for old_id in existing_ids {
        if !retained_ids.contains(&old_id) {
            unsafe {
                let del_sql = CString::new("DELETE FROM moz_bookmarks WHERE id = ?").unwrap();
                let mut stmt: *mut c_void = ptr::null_mut();
                if (sqlite.prepare_v2)(db, del_sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                    (sqlite.bind_int64)(stmt, 1, old_id);
                    (sqlite.step)(stmt);
                    (sqlite.finalize)(stmt);
                }
            }
        }
    }

    unsafe {
        let commit = CString::new("COMMIT;").unwrap();
        (sqlite.exec)(db, commit.as_ptr(), ptr::null(), ptr::null(), ptr::null_mut());
        (sqlite.close)(db);
    }

    Ok(())
}

fn read_history_items(profile: &Profile, limit: usize) -> Vec<Value> {
    let sqlite = match SqliteLib::load() {
        Ok(lib) => lib,
        Err(_) => return Vec::new(),
    };

    let history_source_path = match profile.backend {
        ProfileBackend::ChromiumJson => profile.bookmark_path.with_file_name("History"),
        ProfileBackend::FirefoxSqlite => profile.bookmark_path.clone(),
    };

    if !history_source_path.exists() {
        return Vec::new();
    }

    let temp_dir = env::temp_dir();
    let temp_db_path = temp_dir.join(format!("auxbookmark_hist_{}.db", generate_guid()));

    // ブラウザ起動中のロックを回避するため一時ファイルにコピーして読み取る
    if fs::copy(&history_source_path, &temp_db_path).is_err() {
        return Vec::new();
    }

    let c_path = match CString::new(temp_db_path.to_str().unwrap_or_default()) {
        Ok(c) => c,
        Err(_) => {
            let _ = fs::remove_file(&temp_db_path);
            return Vec::new();
        }
    };

    let mut db: *mut c_void = ptr::null_mut();
    // 1 = SQLITE_OPEN_READONLY
    let rc = unsafe { (sqlite.open_v2)(c_path.as_ptr(), &mut db, 1, ptr::null()) };
    if rc != 0 {
        let _ = fs::remove_file(&temp_db_path);
        return Vec::new();
    }

    let sql_str = match profile.backend {
        ProfileBackend::ChromiumJson => format!(
            "SELECT title, url, last_visit_time FROM urls WHERE url NOT LIKE 'file://%' ORDER BY last_visit_time DESC LIMIT {}",
            limit
        ),
        ProfileBackend::FirefoxSqlite => format!(
            "SELECT title, url, last_visit_date FROM moz_places WHERE visit_count > 0 AND url NOT LIKE 'file://%' AND url NOT LIKE 'about:%' ORDER BY last_visit_date DESC LIMIT {}",
            limit
        ),
    };

    let mut items = Vec::new();
    if let Ok(c_sql) = CString::new(sql_str) {
        let mut stmt: *mut c_void = ptr::null_mut();
        unsafe {
            if (sqlite.prepare_v2)(db, c_sql.as_ptr(), -1, &mut stmt, ptr::null_mut()) == 0 {
                let mut idx = 1;
                while (sqlite.step)(stmt) == 100 {
                    let title_ptr = (sqlite.column_text)(stmt, 0);
                    let url_ptr = (sqlite.column_text)(stmt, 1);
                    let time_val = (sqlite.column_int64)(stmt, 2);

                    let url = if !url_ptr.is_null() {
                        CStr::from_ptr(url_ptr).to_string_lossy().to_string()
                    } else {
                        String::new()
                    };

                    if url.is_empty() {
                        continue;
                    }

                    let raw_title = if !title_ptr.is_null() {
                        CStr::from_ptr(title_ptr).to_string_lossy().to_string()
                    } else {
                        String::new()
                    };

                    let title = if raw_title.trim().is_empty() {
                        url.clone()
                    } else {
                        raw_title.trim().to_string()
                    };

                    items.push(json!({
                        "id": format!("hist_{}", idx),
                        "guid": generate_guid(),
                        "name": title,
                        "type": "url",
                        "url": url,
                        "date_added": time_val.to_string(),
                        "date_modified": time_val.to_string()
                    }));
                    idx += 1;
                }
                (sqlite.finalize)(stmt);
            }
            (sqlite.close)(db);
        }
    }

    let _ = fs::remove_file(&temp_db_path);
    items
}

fn read_profile_bookmarks(profile: &Profile) -> Result<Value> {
    let mut val = match profile.backend {
        ProfileBackend::ChromiumJson => read_bookmarks(&profile.bookmark_path)?,
        ProfileBackend::FirefoxSqlite => read_firefox_bookmarks(&profile.bookmark_path)?,
    };

    // 履歴ノードを仮想フォルダとして roots に注入
    let hist_items = read_history_items(profile, 500);
    if let Some(roots) = val.get_mut("roots").and_then(|r| r.as_object_mut()) {
        roots.insert("history".to_string(), json!({
            "id": "history_root",
            "name": HISTORY_FOLDER_NAME,
            "type": "folder",
            "children": hist_items
        }));
    }

    Ok(val)
}

fn write_profile_bookmarks(profile: &Profile, val: &Value) -> Result<()> {
    match profile.backend {
        ProfileBackend::ChromiumJson => {
            // Chromium の Bookmarks JSON に仮想ノード history が混入しないよう除去して保存
            let mut clean_val = val.clone();
            if let Some(roots) = clean_val.get_mut("roots").and_then(|r| r.as_object_mut()) {
                roots.remove("history");
            }
            write_bookmarks(&profile.bookmark_path, &clean_val)
        }
        ProfileBackend::FirefoxSqlite => write_firefox_bookmarks(&profile.bookmark_path, val),
    }
}

fn chromium_time_to_string(val_str: Option<&str>) -> String {
    const EPOCH_OFFSET_MICROS: i64 = 11_644_473_600_000_000;

    if let Some(s) = val_str {
        if let Ok(micros) = s.parse::<i64>() {
            let unix_micros = if micros > EPOCH_OFFSET_MICROS {
                micros - EPOCH_OFFSET_MICROS
            } else if micros > 1_000_000_000_000_000 {
                micros // Unix 時間 (Firefox 等)
            } else {
                micros * 1_000_000
            };
            let unix_secs = unix_micros / 1_000_000;
            let unix_sub_nanos = ((unix_micros % 1_000_000) * 1000) as u32;
            if let Some(dt_utc) = DateTime::from_timestamp(unix_secs, unix_sub_nanos) {
                let dt_local: DateTime<Local> = DateTime::from(dt_utc);
                return dt_local.format("%Y-%m-%d %H:%M:%S").to_string();
            }
        }
    }
    Local::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn chromium_time_to_unix_secs(val_str: Option<&str>) -> Option<i64> {
    const EPOCH_OFFSET_MICROS: i64 = 11_644_473_600_000_000;
    let s = val_str?;
    let micros = s.parse::<i64>().ok()?;
    if micros > EPOCH_OFFSET_MICROS {
        Some((micros - EPOCH_OFFSET_MICROS) / 1_000_000)
    } else if micros > 1_000_000_000_000_000 {
        Some(micros / 1_000_000)
    } else if micros > 0 {
        Some(micros)
    } else {
        None
    }
}

fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

fn now_chromium_time() -> String {
    const EPOCH_OFFSET_MICROS: i64 = 11_644_473_600_000_000;
    let now = Utc::now();
    let unix_micros = now.timestamp_micros();
    let chrom_micros = unix_micros + EPOCH_OFFSET_MICROS;
    chrom_micros.to_string()
}

fn find_max_id(val: &Value) -> u64 {
    let mut max_id = 0u64;
    fn traverse(v: &Value, max_id: &mut u64) {
        if let Some(obj) = v.as_object() {
            if let Some(id_val) = obj.get("id") {
                if let Some(id_str) = id_val.as_str() {
                    if let Ok(num) = id_str.parse::<u64>() {
                        if num > *max_id {
                            *max_id = num;
                        }
                    }
                }
            }
            for (_k, child) in obj {
                traverse(child, max_id);
            }
        } else if let Some(arr) = v.as_array() {
            for item in arr {
                traverse(item, max_id);
            }
        }
    }
    traverse(val, &mut max_id);
    max_id
}

// ==============================================================================
// ファイル名サニタイズ
// ==============================================================================

fn sanitize_title(title: &str) -> String {
    let mut res = String::with_capacity(title.len());
    for c in title.chars() {
        match c {
            '\\' => res.push('￥'),
            '/' => res.push('／'),
            ':' => res.push('：'),
            '*' => res.push('＊'),
            '?' => res.push('？'),
            '"' => res.push('”'),
            '<' => res.push('＜'),
            '>' => res.push('＞'),
            '|' => res.push('｜'),
            '\r' | '\n' | '\t' => res.push(' '),
            _ => res.push(c),
        }
    }
    let trimmed = res.trim();
    if trimmed.is_empty() {
        "(empty)".to_string()
    } else {
        trimmed.to_string()
    }
}

fn get_node_display_name(node: &Value) -> String {
    let raw_name = node.get("name").and_then(|v| v.as_str()).unwrap_or("(empty)");
    let sanitized = sanitize_title(raw_name);
    let n_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    if n_type == "url" {
        format!("{}.url", sanitized)
    } else {
        sanitized
    }
}

// ==============================================================================
// パス解析 & ノード走査
// ==============================================================================

fn parse_virtual_segments(raw: &str) -> Vec<String> {
    let mut s = raw.trim().trim_matches('"').trim_matches('\'').replace('\\', "/");
    let s_lower = s.to_lowercase();

    let prefixes = [
        "aux://s_auxbookmark/",
        "aux:/s_auxbookmark/",
        "aux:s_auxbookmark/",
        "aux://s_bookmark/",
        "aux:/s_bookmark/",
        "aux:s_bookmark/",
    ];

    let mut matched = false;
    for prefix in &prefixes {
        if let Some(pos) = s_lower.find(prefix) {
            s = s[pos + prefix.len()..].to_string();
            matched = true;
            break;
        }
    }

    if !matched {
        if let Some(pos) = s_lower.find("aux://") {
            if let Some(slash) = s[pos + 6..].find('/') {
                s = s[pos + 6 + slash + 1..].to_string();
            } else {
                s.clear();
            }
        } else if let Some(pos) = s_lower.find("aux:") {
            let after = &s[pos + 4..];
            if let Some(slash) = after.find('/') {
                s = after[slash + 1..].to_string();
            } else {
                s.clear();
            }
        }
    }

    let trimmed = s.trim_matches('/');
    if trimmed.is_empty() {
        Vec::new()
    } else {
        trimmed
            .split('/')
            .map(|seg| seg.trim().to_string())
            .filter(|seg| !seg.is_empty())
            .collect()
    }
}

/// ゴミ箱フォルダが存在しない場合は作成する
fn ensure_trash_folder(val: &mut Value) {
    let has_trash = if let Some(other) = val.get("roots").and_then(|r| r.get("other")).and_then(|o| o.get("children")).and_then(|c| c.as_array()) {
        other.iter().any(|child| {
            child.get("type").and_then(|t| t.as_str()) == Some("folder")
                && child.get("name").and_then(|n| n.as_str()) == Some(TRASH_FOLDER_NAME)
        })
    } else {
        false
    };

    if !has_trash {
        let max_id = find_max_id(val);
        if let Some(other_children) = val.get_mut("roots").and_then(|r| r.get_mut("other")).and_then(|o| o.get_mut("children")).and_then(|c| c.as_array_mut()) {
            other_children.push(json!({
                "date_added": now_chromium_time(),
                "date_last_used": "0",
                "date_modified": now_chromium_time(),
                "guid": generate_guid(),
                "id": (max_id + 1).to_string(),
                "name": TRASH_FOLDER_NAME,
                "type": "folder",
                "children": []
            }));
        }
    }
}

/// リンク切れフォルダが存在しない場合は作成する
fn ensure_isolate_folder(val: &mut Value) {
    let has_isolate = if let Some(other) = val.get("roots").and_then(|r| r.get("other")).and_then(|o| o.get("children")).and_then(|c| c.as_array()) {
        other.iter().any(|child| {
            child.get("type").and_then(|t| t.as_str()) == Some("folder")
                && child.get("name").and_then(|n| n.as_str()) == Some(ISOLATE_FOLDER_NAME)
        })
    } else {
        false
    };

    if !has_isolate {
        let max_id = find_max_id(val);
        if let Some(other_children) = val.get_mut("roots").and_then(|r| r.get_mut("other")).and_then(|o| o.get_mut("children")).and_then(|c| c.as_array_mut()) {
            other_children.push(json!({
                "date_added": now_chromium_time(),
                "date_last_used": "0",
                "date_modified": now_chromium_time(),
                "guid": generate_guid(),
                "id": (max_id + 1).to_string(),
                "name": ISOLATE_FOLDER_NAME,
                "type": "folder",
                "children": []
            }));
        }
    }
}

fn get_root_node_mut<'a>(val: &'a mut Value, segment: &str) -> Option<&'a mut Value> {
    if segment == "trash" || segment == TRASH_FOLDER_NAME {
        ensure_trash_folder(val);
        let other = val.get_mut("roots")?.get_mut("other")?;
        let children = other.get_mut("children")?.as_array_mut()?;
        let idx = children.iter().position(|child| {
            child.get("type").and_then(|t| t.as_str()) == Some("folder")
                && child.get("name").and_then(|n| n.as_str()) == Some(TRASH_FOLDER_NAME)
        })?;
        return children.get_mut(idx);
    }
    if segment == "isolate" || segment == ISOLATE_FOLDER_NAME {
        ensure_isolate_folder(val);
        let other = val.get_mut("roots")?.get_mut("other")?;
        let children = other.get_mut("children")?.as_array_mut()?;
        let idx = children.iter().position(|child| {
            child.get("type").and_then(|t| t.as_str()) == Some("folder")
                && child.get("name").and_then(|n| n.as_str()) == Some(ISOLATE_FOLDER_NAME)
        })?;
        return children.get_mut(idx);
    }

    let roots = val.get_mut("roots")?;

    if segment == "bookmark_bar" || segment == BOOKMARK_BAR_JP {
        return roots.get_mut("bookmark_bar");
    }
    if segment == "other" || segment == OTHER_BOOKMARKS_JP {
        return roots.get_mut("other");
    }
    if segment == "menu" || segment == BOOKMARK_MENU_JP {
        return roots.get_mut("menu");
    }

    None
}

fn get_root_node_ref<'a>(val: &'a Value, segment: &str) -> Option<&'a Value> {
    let roots = val.get("roots")?;

    if segment == "bookmark_bar" || segment == BOOKMARK_BAR_JP {
        return roots.get("bookmark_bar");
    }
    if segment == "other" || segment == OTHER_BOOKMARKS_JP {
        return roots.get("other");
    }
    if segment == "menu" || segment == BOOKMARK_MENU_JP {
        return roots.get("menu");
    }
    if segment == "trash" || segment == TRASH_FOLDER_NAME {
        let other = roots.get("other")?;
        let children = other.get("children")?.as_array()?;
        return children.iter().find(|child| {
            child.get("type").and_then(|t| t.as_str()) == Some(TRASH_FOLDER_NAME)
                || child.get("name").and_then(|n| n.as_str()) == Some(TRASH_FOLDER_NAME)
        });
    }
    if segment == "isolate" || segment == ISOLATE_FOLDER_NAME {
        let other = roots.get("other")?;
        let children = other.get("children")?.as_array()?;
        return children.iter().find(|child| {
            child.get("type").and_then(|t| t.as_str()) == Some(ISOLATE_FOLDER_NAME)
                || child.get("name").and_then(|n| n.as_str()) == Some(ISOLATE_FOLDER_NAME)
        });
    }
    if segment == "history" || segment == HISTORY_FOLDER_NAME {
        return roots.get("history");
    }

    None
}

fn navigate_node_ref<'a>(val: &'a Value, segments: &[String]) -> Option<&'a Value> {
    if segments.is_empty() {
        return None;
    }

    let mut current = get_root_node_ref(val, &segments[0])?;

    for seg in &segments[1..] {
        let children = current.get("children")?.as_array()?;
        let found = children.iter().find(|child| {
            get_node_display_name(child) == *seg
        })?;
        current = found;
    }

    Some(current)
}

fn find_parent_and_index_mut<'a>(
    val: &'a mut Value,
    segments: &[String],
) -> Result<(&'a mut Value, usize)> {
    if segments.is_empty() {
        return Err(anyhow!("Target path cannot be empty"));
    }

    if segments.len() == 1 {
        return Err(anyhow!("Cannot modify root folder directly: {}", segments[0]));
    }

    let parent_segs = &segments[..segments.len() - 1];
    let target_name = &segments[segments.len() - 1];

    let mut current = get_root_node_mut(val, &parent_segs[0])
        .ok_or_else(|| anyhow!("Root segment not found: {}", parent_segs[0]))?;

    for seg in &parent_segs[1..] {
        let children = current.get_mut("children")
            .and_then(|c| c.as_array_mut())
            .ok_or_else(|| anyhow!("Path segment is not a folder: {}", seg))?;

        let idx = children.iter().position(|child| {
            get_node_display_name(child) == *seg
        }).ok_or_else(|| anyhow!("Child folder not found: {}", seg))?;

        current = children.get_mut(idx).unwrap();
    }

    let children = current.get_mut("children")
        .and_then(|c| c.as_array_mut())
        .ok_or_else(|| anyhow!("Parent node has no children array"))?;

    let idx = children.iter().position(|child| {
        get_node_display_name(child) == *target_name
    }).ok_or_else(|| anyhow!("Target item not found: {}", target_name))?;

    Ok((current, idx))
}

// ==============================================================================
// サブコマンド実装
// ==============================================================================

/// list サブコマンド
fn cmd_list(config: &Config, virtual_path: &str, output_file: Option<&str>) -> Result<()> {
    let segments = parse_virtual_segments(virtual_path);

    let mut out_writer: Box<dyn Write> = if let Some(out_path) = output_file {
        Box::new(BufWriter::new(File::create(out_path).context("Failed to create output list file")?))
    } else {
        Box::new(BufWriter::new(std::io::stdout()))
    };

    write!(out_writer, ";ListFile\r\n")?;

    // 1. ルート階層（ブラウザ一覧）
    if segments.is_empty() {
        for prof in &config.profiles {
            let mtime = fs::metadata(&prof.bookmark_path)
                .and_then(|m| m.modified())
                .map(|t| {
                    let dt: DateTime<Local> = DateTime::from(t);
                    dt.format("%Y-%m-%d %H:%M:%S").to_string()
                })
                .unwrap_or_else(|_| Local::now().format("%Y-%m-%d %H:%M:%S").to_string());

            write!(out_writer, "\"{}\",A:H10,W:{}\r\n", prof.name, mtime)?;
        }
        let _ = out_writer.flush();
        return Ok(());
    }

    // 2. ブラウザ内部
    let prof_name = &segments[0];
    let prof = find_profile(config, prof_name)
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", prof_name))?;

    let val = read_profile_bookmarks(prof)?;
    let sub_segs = &segments[1..];

    // ブラウザのルート階層（ブックマーク バー、その他のブックマーク、ゴミ箱、ブックマーク メニュー）
    if sub_segs.is_empty() {
        let roots = val.get("roots");
        let now_str = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

        let bar_date = roots
            .and_then(|r| r.get("bookmark_bar"))
            .and_then(|b| b.get("date_modified").and_then(|d| d.as_str()))
            .map(|s| chromium_time_to_string(Some(s)))
            .unwrap_or_else(|| now_str.clone());

        let other_date = roots
            .and_then(|r| r.get("other"))
            .and_then(|b| b.get("date_modified").and_then(|d| d.as_str()))
            .map(|s| chromium_time_to_string(Some(s)))
            .unwrap_or_else(|| now_str.clone());

        let trash_date = roots
            .and_then(|r| r.get("other"))
            .and_then(|o| o.get("children").and_then(|c| c.as_array()))
            .and_then(|arr| arr.iter().find(|item| item.get("name").and_then(|n| n.as_str()) == Some(TRASH_FOLDER_NAME)))
            .and_then(|t| t.get("date_modified").and_then(|d| d.as_str()))
            .map(|s| chromium_time_to_string(Some(s)))
            .unwrap_or_else(|| now_str.clone());

        write!(out_writer, "\"{}\",A:H10,W:{}\r\n", BOOKMARK_BAR_JP, bar_date)?;
        write!(out_writer, "\"{}\",A:H10,W:{}\r\n", OTHER_BOOKMARKS_JP, other_date)?;
        if let Some(menu) = roots.and_then(|r| r.get("menu")) {
            let menu_date = menu
                .get("date_modified")
                .and_then(|d| d.as_str())
                .map(|s| chromium_time_to_string(Some(s)))
                .unwrap_or_else(|| now_str.clone());
            write!(out_writer, "\"{}\",A:H10,W:{}\r\n", BOOKMARK_MENU_JP, menu_date)?;
        }
        write!(out_writer, "\"{}\",A:H10,W:{}\r\n", TRASH_FOLDER_NAME, trash_date)?;
        if let Some(hist) = roots.and_then(|r| r.get("history")) {
            let hist_date = hist
                .get("children")
                .and_then(|c| c.as_array())
                .and_then(|arr| arr.first())
                .and_then(|item| item.get("date_modified").or_else(|| item.get("date_added")))
                .and_then(|d| d.as_str())
                .map(|s| chromium_time_to_string(Some(s)))
                .unwrap_or_else(|| now_str.clone());
            write!(out_writer, "\"{}\",A:H10,W:{}\r\n", HISTORY_FOLDER_NAME, hist_date)?;
        }
        let _ = out_writer.flush();
        return Ok(());
    }

    // サブフォルダ内部の展開
    let current = navigate_node_ref(&val, sub_segs)
        .ok_or_else(|| anyhow!("Path not found: {}", virtual_path))?;

    if let Some(children) = current.get("children").and_then(|c| c.as_array()) {
        let is_other_root = sub_segs.len() == 1
            && (sub_segs[0] == "other" || sub_segs[0] == OTHER_BOOKMARKS_JP);

        for child in children {
            let n_type = child.get("type").and_then(|v| v.as_str()).unwrap_or("");
            let name = child.get("name").and_then(|v| v.as_str()).unwrap_or("");

            if is_other_root && name == TRASH_FOLDER_NAME {
                continue;
            }

            let date_str = chromium_time_to_string(
                child.get("date_modified")
                    .or_else(|| child.get("date_added"))
                    .and_then(|d| d.as_str()),
            );

            if n_type == "folder" {
                let disp = sanitize_title(name);
                write!(out_writer, "\"{}\",A:H10,W:{}\r\n", disp, date_str)?;
            } else if n_type == "url" {
                let disp = format!("{}.url", sanitize_title(name));
                let url_len = child.get("url").and_then(|u| u.as_str()).map(|u| u.len()).unwrap_or(0);
                write!(out_writer, "\"{}\",A:H20,S:{},W:{}\r\n", disp, url_len, date_str)?;
            }
        }
    }

    let _ = out_writer.flush();
    Ok(())
}

/// get サブコマンド (Enter キーでブラウザ起動用のショートカット作成)
fn cmd_get(config: &Config, virtual_path: &str, dest_local_file: &str) -> Result<()> {
    let segments = parse_virtual_segments(virtual_path);
    if segments.len() < 2 {
        return Err(anyhow!("Cannot get root or browser node: {}", virtual_path));
    }

    let prof = find_profile(config, &segments[0])
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", segments[0]))?;

    let val = read_profile_bookmarks(prof)?;
    let node = navigate_node_ref(&val, &segments[1..])
        .ok_or_else(|| anyhow!("Target bookmark not found: {}", virtual_path))?;

    let url = node.get("url").and_then(|u| u.as_str())
        .ok_or_else(|| anyhow!("Node is not a URL item: {}", virtual_path))?;

    let mut file = File::create(dest_local_file).context("Failed to create destination .url shortcut")?;
    write!(file, "[InternetShortcut]\r\nURL={}\r\n", url)?;

    Ok(())
}

/// makedir サブコマンド
fn cmd_makedir(config: &Config, parent_path: &str, folder_name: Option<&str>) -> Result<()> {
    let mut full_path = parent_path.to_string();
    if let Some(f_name) = folder_name {
        if !f_name.is_empty() {
            full_path = format!("{}/{}", full_path.trim_end_matches(['/', '\\']), f_name.trim_start_matches(['/', '\\']));
        }
    }

    let segments = parse_virtual_segments(&full_path);
    if segments.len() < 2 {
        return Err(anyhow!("Cannot create folder at root or browser root directly: {}", full_path));
    }

    let prof = find_profile(config, &segments[0])
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", segments[0]))?;

    let sub_segs = &segments[1..];
    let parent_segs = &sub_segs[..sub_segs.len() - 1];
    let new_folder_name = &sub_segs[sub_segs.len() - 1];

    maybe_backup(prof, config.backup_interval_minutes, config.backup_keep_generations)?;
    let mut val = read_profile_bookmarks(prof)?;

    let max_id = find_max_id(&val);
    let now_micros = now_chromium_time();

    let new_node = json!({
        "date_added": now_micros,
        "date_last_used": "0",
        "date_modified": now_micros,
        "guid": generate_guid(),
        "id": (max_id + 1).to_string(),
        "name": new_folder_name,
        "type": "folder",
        "children": []
    });

    if parent_segs.is_empty() {
        return Err(anyhow!("Cannot create folder directly under browser root: {}", full_path));
    }
    if parent_segs[0] == "history" || parent_segs[0] == HISTORY_FOLDER_NAME {
        return Err(anyhow!("Cannot create folder inside History folder: {}", full_path));
    }

    let mut current = get_root_node_mut(&mut val, &parent_segs[0])
        .ok_or_else(|| anyhow!("Root segment not found: {}", parent_segs[0]))?;

    for seg in &parent_segs[1..] {
        let children = current.get_mut("children")
            .and_then(|c| c.as_array_mut())
            .ok_or_else(|| anyhow!("Path segment is not a folder: {}", seg))?;

        let idx = children.iter().position(|child| {
            get_node_display_name(child) == *seg
        }).ok_or_else(|| anyhow!("Parent folder not found: {}", seg))?;

        current = children.get_mut(idx).unwrap();
    }

    let children = current.get_mut("children")
        .and_then(|c| c.as_array_mut())
        .ok_or_else(|| anyhow!("Target parent is not a folder"))?;

    children.push(new_node);

    write_profile_bookmarks(prof, &val)?;
    log_msg(&format!("[{}] Created folder: {}", prof.name, full_path));
    Ok(())
}

/// 削除ロジック（ゴミ箱退避 or 完全消去）
fn delete_single_node(val: &mut Value, sub_segs: &[String]) -> Result<()> {
    if sub_segs.len() <= 1 {
        return Err(anyhow!("Cannot delete root bookmark categories: {:?}", sub_segs));
    }

    let is_in_trash = sub_segs[0] == "trash" || sub_segs[0] == TRASH_FOLDER_NAME;

    let (parent, idx) = find_parent_and_index_mut(val, sub_segs)?;
    let children = parent.get_mut("children").and_then(|c| c.as_array_mut()).unwrap();
    let removed_node = children.remove(idx);

    if is_in_trash {
        log_msg(&format!("Permanently deleted: {:?}", sub_segs));
    } else {
        let trash_node = get_root_node_mut(val, "trash")
            .ok_or_else(|| anyhow!("Failed to locate Trash folder"))?;
        let trash_children = trash_node.get_mut("children")
            .and_then(|c| c.as_array_mut())
            .ok_or_else(|| anyhow!("Trash node has no children"))?;

        trash_children.push(removed_node);
        log_msg(&format!("Moved to trash: {:?}", sub_segs));
    }

    Ok(())
}

/// delete サブコマンド
fn cmd_delete(config: &Config, parent_path: &str, raw_items: &[String]) -> Result<()> {
    let segments = parse_virtual_segments(parent_path);
    if segments.is_empty() {
        return Err(anyhow!("Cannot delete from root: {}", parent_path));
    }

    let prof = find_profile(config, &segments[0])
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", segments[0]))?;

    maybe_backup(prof, config.backup_interval_minutes, config.backup_keep_generations)?;
    let mut val = read_profile_bookmarks(prof)?;
    let sub_segs = &segments[1..];
    if sub_segs.first().map(|s| s.as_str()) == Some("history")
        || sub_segs.first().map(|s| s.as_str()) == Some(HISTORY_FOLDER_NAME)
    {
        return Err(anyhow!("History folder is read-only. Use copy ('C') to save to bookmarks."));
    }

    let valid_items: Vec<&String> = raw_items.iter().filter(|s| !s.trim().is_empty()).collect();

    if valid_items.is_empty() {
        delete_single_node(&mut val, sub_segs)?;
    } else {
        for item in valid_items {
            let mut full_sub = sub_segs.to_vec();
            full_sub.push(item.clone());
            if let Err(e) = delete_single_node(&mut val, &full_sub) {
                log_msg(&format!("[{}] Failed to delete item {:?}: {:#}", prof.name, full_sub, e));
            }
        }
    }

    write_profile_bookmarks(prof, &val)?;
    Ok(())
}

/// deldir サブコマンド
fn cmd_deldir(config: &Config, virtual_path: &str, folder_name: Option<&str>) -> Result<()> {
    let mut full_path = virtual_path.to_string();
    if let Some(f_name) = folder_name {
        let trimmed = f_name.trim();
        if !trimmed.is_empty() {
            full_path = format!("{}/{}", full_path.trim_end_matches(['/', '\\']), trimmed.trim_start_matches(['/', '\\']));
        }
    }
    let segments = parse_virtual_segments(&full_path);
    if segments.len() <= 2 {
        return Err(anyhow!("Cannot delete browser root folder: {}", full_path));
    }
    if segments.len() > 1 && (segments[1] == "history" || segments[1] == HISTORY_FOLDER_NAME) {
        return Err(anyhow!("Cannot delete History folder: {}", full_path));
    }

    let prof = find_profile(config, &segments[0])
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", segments[0]))?;

    maybe_backup(prof, config.backup_interval_minutes, config.backup_keep_generations)?;
    let mut val = read_profile_bookmarks(prof)?;
    delete_single_node(&mut val, &segments[1..])?;
    write_profile_bookmarks(prof, &val)?;
    Ok(())
}

/// move サブコマンド (同一ブラウザ内移動およびブラウザ間移動の両対応)
fn cmd_move(config: &Config, src_path: &str, dest_path: &str) -> Result<()> {
    let src_segs = parse_virtual_segments(src_path);
    let dest_segs = parse_virtual_segments(dest_path);

    if src_segs.len() <= 2 {
        return Err(anyhow!("Cannot move browser root items: {}", src_path));
    }
    if dest_segs.is_empty() {
        return Err(anyhow!("Destination cannot be root: {}", dest_path));
    }
    if src_segs.len() > 1 && (src_segs[1] == "history" || src_segs[1] == HISTORY_FOLDER_NAME) {
        return Err(anyhow!("History items cannot be moved. Use copy ('C') instead: {}", src_path));
    }
    if dest_segs.len() > 1 && (dest_segs[1] == "history" || dest_segs[1] == HISTORY_FOLDER_NAME) {
        return Err(anyhow!("Cannot move into History folder (History is read-only): {}", dest_path));
    }

    let src_prof = find_profile(config, &src_segs[0])
        .ok_or_else(|| anyhow!("Source browser profile '{}' not found", src_segs[0]))?;
    let dest_prof = find_profile(config, &dest_segs[0])
        .ok_or_else(|| anyhow!("Destination browser profile '{}' not found", dest_segs[0]))?;

    let is_same_profile = src_prof.name.eq_ignore_ascii_case(&dest_prof.name);

    if is_same_profile {
        // 同一ブラウザ内移動
        maybe_backup(src_prof, config.backup_interval_minutes, config.backup_keep_generations)?;
        let mut val = read_profile_bookmarks(src_prof)?;

        let src_sub = &src_segs[1..];
        let mut dest_sub = dest_segs[1..].to_vec();

        let (src_parent, src_idx) = find_parent_and_index_mut(&mut val, src_sub)?;
        let src_children = src_parent.get_mut("children").and_then(|c| c.as_array_mut()).unwrap();
        let moved_node = src_children.remove(src_idx);

        let dest_is_folder = {
            if let Some(d_node) = navigate_node_ref(&val, &dest_sub) {
                d_node.get("type").and_then(|t| t.as_str()) == Some("folder")
            } else {
                false
            }
        };

        if !dest_is_folder && dest_sub.len() > 1 {
            dest_sub.pop();
        }

        let dest_node = if dest_sub.len() == 1 {
            get_root_node_mut(&mut val, &dest_sub[0])
                .ok_or_else(|| anyhow!("Destination root not found: {}", dest_sub[0]))?
        } else {
            let mut current = get_root_node_mut(&mut val, &dest_sub[0])
                .ok_or_else(|| anyhow!("Destination root not found: {}", dest_sub[0]))?;
            for seg in &dest_sub[1..] {
                let children = current.get_mut("children")
                    .and_then(|c| c.as_array_mut())
                    .ok_or_else(|| anyhow!("Destination segment is not a folder: {}", seg))?;
                let idx = children.iter().position(|child| {
                    get_node_display_name(child) == *seg
                }).ok_or_else(|| anyhow!("Destination child not found: {}", seg))?;
                current = children.get_mut(idx).unwrap();
            }
            current
        };

        let dest_children = dest_node.get_mut("children")
            .and_then(|c| c.as_array_mut())
            .ok_or_else(|| anyhow!("Destination node is not a folder"))?;

        dest_children.push(moved_node);

        write_profile_bookmarks(src_prof, &val)?;
        log_msg(&format!("[{}] Moved: {} -> {}", src_prof.name, src_path, dest_path));
    } else {
        // 異なるブラウザ間の移動:
        // 1. 移動元から取り出して保存
        maybe_backup(src_prof, config.backup_interval_minutes, config.backup_keep_generations)?;
        let mut src_val = read_profile_bookmarks(src_prof)?;
        let src_sub = &src_segs[1..];

        let moved_node = {
            let (src_parent, src_idx) = find_parent_and_index_mut(&mut src_val, src_sub)?;
            let src_children = src_parent.get_mut("children").and_then(|c| c.as_array_mut()).unwrap();
            src_children.remove(src_idx)
        };
        write_profile_bookmarks(src_prof, &src_val)?;

        // 2. 移動先へ挿入して保存
        maybe_backup(dest_prof, config.backup_interval_minutes, config.backup_keep_generations)?;
        let mut dest_val = read_profile_bookmarks(dest_prof)?;
        let mut dest_sub = dest_segs[1..].to_vec();

        let dest_is_folder = {
            if let Some(d_node) = navigate_node_ref(&dest_val, &dest_sub) {
                d_node.get("type").and_then(|t| t.as_str()) == Some("folder")
            } else {
                false
            }
        };

        if !dest_is_folder && dest_sub.len() > 1 {
            dest_sub.pop();
        }

        let max_id = find_max_id(&dest_val);
        let mut new_node = moved_node;
        if let Some(obj) = new_node.as_object_mut() {
            obj.insert("id".to_string(), json!((max_id + 1).to_string()));
            obj.insert("guid".to_string(), json!(generate_guid()));
        }

        let dest_node = if dest_sub.len() == 1 {
            get_root_node_mut(&mut dest_val, &dest_sub[0])
                .ok_or_else(|| anyhow!("Destination root not found: {}", dest_sub[0]))?
        } else {
            let mut current = get_root_node_mut(&mut dest_val, &dest_sub[0])
                .ok_or_else(|| anyhow!("Destination root not found: {}", dest_sub[0]))?;
            for seg in &dest_sub[1..] {
                let children = current.get_mut("children")
                    .and_then(|c| c.as_array_mut())
                    .ok_or_else(|| anyhow!("Destination segment is not a folder: {}", seg))?;
                let idx = children.iter().position(|child| {
                    get_node_display_name(child) == *seg
                }).ok_or_else(|| anyhow!("Destination child not found: {}", seg))?;
                current = children.get_mut(idx).unwrap();
            }
            current
        };

        let dest_children = dest_node.get_mut("children")
            .and_then(|c| c.as_array_mut())
            .ok_or_else(|| anyhow!("Destination node is not a folder"))?;

        dest_children.push(new_node);

        write_profile_bookmarks(dest_prof, &dest_val)?;
        log_msg(&format!("Cross-browser moved: {} -> {}", src_path, dest_path));
    }

    Ok(())
}

/// copy サブコマンド (同一ブラウザ内複写およびブラウザ間複写の両対応)
fn cmd_copy(config: &Config, src_path: &str, dest_path: &str) -> Result<()> {
    let src_segs = parse_virtual_segments(src_path);
    let dest_segs = parse_virtual_segments(dest_path);

    if src_segs.len() <= 2 {
        return Err(anyhow!("Cannot copy browser root items: {}", src_path));
    }
    if dest_segs.is_empty() {
        return Err(anyhow!("Destination cannot be root: {}", dest_path));
    }
    if dest_segs.len() > 1 && (dest_segs[1] == "history" || dest_segs[1] == HISTORY_FOLDER_NAME) {
        return Err(anyhow!("Cannot copy into History folder (History is read-only): {}", dest_path));
    }

    let src_prof = find_profile(config, &src_segs[0])
        .ok_or_else(|| anyhow!("Source browser profile '{}' not found", src_segs[0]))?;
    let dest_prof = find_profile(config, &dest_segs[0])
        .ok_or_else(|| anyhow!("Destination browser profile '{}' not found", dest_segs[0]))?;

    let is_same_profile = src_prof.name.eq_ignore_ascii_case(&dest_prof.name);

    let src_val = read_profile_bookmarks(src_prof)?;
    let src_node = navigate_node_ref(&src_val, &src_segs[1..])
        .ok_or_else(|| anyhow!("Source node not found: {}", src_path))?
        .clone();

    maybe_backup(dest_prof, config.backup_interval_minutes, config.backup_keep_generations)?;
    let mut dest_val = if is_same_profile {
        src_val
    } else {
        read_profile_bookmarks(dest_prof)?
    };

    let mut dest_sub = dest_segs[1..].to_vec();
    let dest_is_folder = {
        if let Some(d_node) = navigate_node_ref(&dest_val, &dest_sub) {
            d_node.get("type").and_then(|t| t.as_str()) == Some("folder")
        } else {
            false
        }
    };

    if !dest_is_folder && dest_sub.len() > 1 {
        dest_sub.pop();
    }

    let max_id = find_max_id(&dest_val);
    let mut cloned_node = src_node;
    if let Some(obj) = cloned_node.as_object_mut() {
        obj.insert("id".to_string(), json!((max_id + 1).to_string()));
        obj.insert("guid".to_string(), json!(generate_guid()));
        obj.insert("date_added".to_string(), json!(now_chromium_time()));
        obj.insert("date_modified".to_string(), json!(now_chromium_time()));
    }

    let dest_node = if dest_sub.len() == 1 {
        get_root_node_mut(&mut dest_val, &dest_sub[0])
            .ok_or_else(|| anyhow!("Destination root not found: {}", dest_sub[0]))?
    } else {
        let mut current = get_root_node_mut(&mut dest_val, &dest_sub[0])
            .ok_or_else(|| anyhow!("Destination root not found: {}", dest_sub[0]))?;
        for seg in &dest_sub[1..] {
            let children = current.get_mut("children")
                .and_then(|c| c.as_array_mut())
                .ok_or_else(|| anyhow!("Destination segment is not a folder: {}", seg))?;
            let idx = children.iter().position(|child| {
                get_node_display_name(child) == *seg
            }).ok_or_else(|| anyhow!("Destination child not found: {}", seg))?;
            current = children.get_mut(idx).unwrap();
        }
        current
    };

    let dest_children = dest_node.get_mut("children")
        .and_then(|c| c.as_array_mut())
        .ok_or_else(|| anyhow!("Destination node is not a folder"))?;

    dest_children.push(cloned_node);

    write_profile_bookmarks(dest_prof, &dest_val)?;
    log_msg(&format!("Copied: {} -> {}", src_path, dest_path));
    Ok(())
}

fn export_node_html<W: Write>(
    writer: &mut W,
    node: &Value,
    indent: usize,
    include_trash: bool,
) -> Result<()> {
    let indent_str = "    ".repeat(indent);
    let n_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");

    // ゴミ箱の除外 (クリーンエクスポート時)
    if !include_trash && n_type == "folder" && (name == TRASH_FOLDER_NAME || name.eq_ignore_ascii_case("trash")) {
        return Ok(());
    }

    if n_type == "folder" {
        let add_date = chromium_time_to_unix_secs(node.get("date_added").and_then(|v| v.as_str()));
        let mod_date = chromium_time_to_unix_secs(node.get("date_modified").and_then(|v| v.as_str()));

        let mut date_attrs = String::new();
        if let Some(d) = add_date {
            date_attrs.push_str(&format!(" ADD_DATE=\"{}\"", d));
        }
        if let Some(d) = mod_date {
            date_attrs.push_str(&format!(" LAST_MODIFIED=\"{}\"", d));
        }

        writeln!(writer, "{}<DT><H3{}>{}</H3>", indent_str, date_attrs, escape_html(name))?;
        writeln!(writer, "{}<DL><p>", indent_str)?;

        if let Some(children) = node.get("children").and_then(|v| v.as_array()) {
            for child in children {
                export_node_html(writer, child, indent + 1, include_trash)?;
            }
        }

        writeln!(writer, "{}</DL><p>", indent_str)?;
    } else if n_type == "url" {
        let url = node.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let add_date = chromium_time_to_unix_secs(node.get("date_added").and_then(|v| v.as_str()));
        let mut date_attr = String::new();
        if let Some(d) = add_date {
            date_attr.push_str(&format!(" ADD_DATE=\"{}\"", d));
        }

        writeln!(
            writer,
            "{}<DT><A HREF=\"{}\"{}>{}</A>",
            indent_str,
            escape_html(url),
            date_attr,
            escape_html(name)
        )?;
    }

    Ok(())
}

/// rename サブコマンド (アイテムおよびフォルダの名前変更)
fn cmd_rename(config: &Config, src_path: &str, dest_path: &str) -> Result<()> {
    let src_segs = parse_virtual_segments(src_path);
    let dest_segs = parse_virtual_segments(dest_path);

    if src_segs.len() <= 2 {
        return Err(anyhow!("Cannot rename browser root or root category folders: {}", src_path));
    }
    if dest_segs.is_empty() {
        return Err(anyhow!("Destination cannot be empty: {}", dest_path));
    }
    if src_segs[1] == "history" || src_segs[1] == HISTORY_FOLDER_NAME {
        return Err(anyhow!("History items cannot be renamed (History is read-only): {}", src_path));
    }

    let src_prof = find_profile(config, &src_segs[0])
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", src_segs[0]))?;

    let new_raw_name = dest_segs.last().unwrap();
    if new_raw_name.is_empty() {
        return Err(anyhow!("New name cannot be empty"));
    }

    maybe_backup(src_prof, config.backup_interval_minutes, config.backup_keep_generations)?;
    let mut val = read_profile_bookmarks(src_prof)?;

    let src_sub = &src_segs[1..];
    let (parent, idx) = find_parent_and_index_mut(&mut val, src_sub)?;
    let children = parent.get_mut("children").and_then(|c| c.as_array_mut()).unwrap();
    let target_node = &mut children[idx];

    let old_display = get_node_display_name(target_node);
    let n_type = target_node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let new_title = if n_type == "url" {
        new_raw_name.strip_suffix(".url").unwrap_or(new_raw_name)
    } else {
        new_raw_name.as_str()
    };

    target_node["name"] = json!(new_title);
    let now_micros = now_chromium_time();
    if target_node.get("date_modified").is_some() {
        target_node["date_modified"] = json!(now_micros);
    }

    write_profile_bookmarks(src_prof, &val)?;
    log_msg(&format!("[{}] Renamed: {} -> {}", src_prof.name, old_display, new_title));
    Ok(())
}

/// export サブコマンド (Netscape Bookmark HTML形式への書き出し)
fn cmd_export(
    config: &Config,
    target_prof_or_path: &str,
    output_file: Option<&str>,
    include_trash: bool,
) -> Result<()> {
    let segments = parse_virtual_segments(target_prof_or_path);
    let prof_name = if !segments.is_empty() {
        &segments[0]
    } else if !target_prof_or_path.trim().is_empty() {
        target_prof_or_path.trim()
    } else if !config.profiles.is_empty() {
        &config.profiles[0].name
    } else {
        return Err(anyhow!("No browser profile specified or found for export."));
    };

    let prof = find_profile(config, prof_name)
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", prof_name))?;

    let val = read_profile_bookmarks(prof)?;
    let roots = val.get("roots").ok_or_else(|| anyhow!("Invalid Bookmarks: missing 'roots'"))?;

    let out_path = if let Some(path_str) = output_file {
        PathBuf::from(path_str)
    } else {
        let now_str = Local::now().format("%Y%m%d_%H%M%S");
        let suffix = if include_trash { "all" } else { "clean" };
        let default_name = format!("{}_bookmarks_{}_{}.html", prof.name, now_str, suffix);
        PathBuf::from(default_name)
    };

    let file = File::create(&out_path)
        .context(format!("Failed to create export file: {:?}", out_path))?;
    let mut writer = BufWriter::new(file);

    writeln!(writer, "<!DOCTYPE NETSCAPE-Bookmark-file-1>")?;
    writeln!(writer, "<!-- This is an automatically generated file.")?;
    writeln!(writer, "     It will be read and overwritten.")?;
    writeln!(writer, "     DO NOT EDIT! -->")?;
    writeln!(writer, "<META HTTP-EQUIV=\"Content-Type\" CONTENT=\"text/html; charset=UTF-8\">")?;
    writeln!(writer, "<TITLE>Bookmarks</TITLE>")?;
    writeln!(writer, "<H1>Bookmarks</H1>")?;
    writeln!(writer, "<DL><p>")?;

    if let Some(bar) = roots.get("bookmark_bar") {
        export_node_html(&mut writer, bar, 1, include_trash)?;
    }

    if let Some(other) = roots.get("other") {
        export_node_html(&mut writer, other, 1, include_trash)?;
    }

    if let Some(menu) = roots.get("menu") {
        export_node_html(&mut writer, menu, 1, include_trash)?;
    }

    writeln!(writer, "</DL><p>")?;
    writer.flush()?;

    let mode_str = if include_trash { "all (including trash)" } else { "clean (trash excluded)" };
    println!("Exported [{}] bookmarks ({}) successfully to {:?}", prof.name, mode_str, out_path);
    log_msg(&format!("[{}] Exported bookmarks ({}) to {:?}", prof.name, mode_str, out_path));

    Ok(())
}

// ==============================================================================
// リンクチェック機能 (並行死活監視)
// ==============================================================================

#[derive(Debug, Clone)]
struct LinkItem {
    path_segments: Vec<String>,
    name: String,
    url: String,
}

#[derive(Debug, Clone, PartialEq)]
enum CheckResult {
    Ok(u16),
    Warn(u16, String),
    Dead(u16, String),
}

fn collect_link_items(node: &Value, current_path: &[String], out: &mut Vec<LinkItem>) {
    let n_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");

    // ゴミ箱やリンク切れフォルダ内のアイテムはスキップ
    if n_type == "folder" && (name == TRASH_FOLDER_NAME || name == ISOLATE_FOLDER_NAME) {
        return;
    }

    if n_type == "url" {
        if let Some(url) = node.get("url").and_then(|u| u.as_str()) {
            let u_trim = url.trim();
            if u_trim.starts_with("http://") || u_trim.starts_with("https://") {
                out.push(LinkItem {
                    path_segments: current_path.to_vec(),
                    name: name.to_string(),
                    url: u_trim.to_string(),
                });
            }
        }
    } else if n_type == "folder" {
        if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
            for child in children {
                let mut next_path = current_path.to_vec();
                next_path.push(get_node_display_name(child));
                collect_link_items(child, &next_path, out);
            }
        }
    }
}

fn check_url_with_curl(url: &str, timeout_secs: u64) -> CheckResult {
    let timeout_str = timeout_secs.to_string();
    let ua = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/122.0.0.0 Safari/537.36";

    // 1. まず高速な HEAD リクエスト (-I) を試行
    let head_cmd = Command::new("curl.exe")
        .args(&[
            "-I", "-s", "-L", "-k",
            "--max-time", &timeout_str,
            "-A", ua,
            "-o", "NUL",
            "-w", "%{http_code}",
            url,
        ])
        .output();

    let (mut code_num, mut success) = match head_cmd {
        Ok(out) if out.status.success() => {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let code = s.parse::<u16>().unwrap_or(0);
            (code, code > 0)
        }
        _ => (0, false),
    };

    // 2. HEAD を拒否 (405, 403, 0) された場合、GET の先頭1KB取得でフォールバック
    if code_num == 405 || code_num == 403 || code_num == 0 {
        let get_cmd = Command::new("curl.exe")
            .args(&[
                "-s", "-L", "-k",
                "-r", "0-1024",
                "--max-time", &timeout_str,
                "-A", ua,
                "-o", "NUL",
                "-w", "%{http_code}",
                url,
            ])
            .output();

        if let Ok(out) = get_cmd {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if let Ok(code) = s.parse::<u16>() {
                if code > 0 {
                    code_num = code;
                    success = true;
                }
            }
        }
    }

    if !success || code_num == 0 {
        return CheckResult::Dead(0, "Connection Failed / Timeout".to_string());
    }

    match code_num {
        200..=399 => CheckResult::Ok(code_num),
        401 | 403 | 429 => CheckResult::Warn(code_num, "Access Restricted / Bot Blocked".to_string()),
        404 => CheckResult::Dead(404, "Not Found".to_string()),
        410 => CheckResult::Dead(410, "Gone".to_string()),
        500..=599 => CheckResult::Dead(code_num, "Server Error".to_string()),
        _ => CheckResult::Dead(code_num, format!("HTTP {}", code_num)),
    }
}

/// check サブコマンド (並行死活監視 & 自動退避)
fn cmd_check(
    config: &Config,
    target_prof_or_path: &str,
    to_trash: bool,
    to_isolate: bool,
    timeout_secs: u64,
    thread_count: usize,
) -> Result<()> {
    let segments = parse_virtual_segments(target_prof_or_path);
    let prof_name = if !segments.is_empty() {
        &segments[0]
    } else if !target_prof_or_path.trim().is_empty() {
        target_prof_or_path.trim()
    } else if !config.profiles.is_empty() {
        &config.profiles[0].name
    } else {
        return Err(anyhow!("No browser profile specified for link check."));
    };

    let prof = find_profile(config, prof_name)
        .ok_or_else(|| anyhow!("Browser profile '{}' not found", prof_name))?;

    let val = read_profile_bookmarks(prof)?;
    let sub_segs = if segments.len() > 1 { &segments[1..] } else { &[] };

    let mut targets = Vec::new();

    if sub_segs.is_empty() {
        if let Some(roots) = val.get("roots") {
            if let Some(bar) = roots.get("bookmark_bar") {
                collect_link_items(bar, &[BOOKMARK_BAR_JP.to_string()], &mut targets);
            }
            if let Some(other) = roots.get("other") {
                collect_link_items(other, &[OTHER_BOOKMARKS_JP.to_string()], &mut targets);
            }
            if let Some(menu) = roots.get("menu") {
                collect_link_items(menu, &[BOOKMARK_MENU_JP.to_string()], &mut targets);
            }
        }
    } else {
        let node = navigate_node_ref(&val, sub_segs)
            .ok_or_else(|| anyhow!("Target path not found: {}", target_prof_or_path))?;
        collect_link_items(node, sub_segs, &mut targets);
    }

    let total_count = targets.len();
    if total_count == 0 {
        println!("No Web URL bookmarks found to check.");
        return Ok(());
    }

    println!("===================================================");
    println!("  auxbookmark Link Check: [{}] ({} items)", prof.name, total_count);
    println!("  Timeout: {}s | Concurrency: {} threads", timeout_secs, thread_count);
    let mode_desc = if to_trash {
        "Move broken links to Trash"
    } else if to_isolate {
        "Move broken links to 'リンク切れ' folder"
    } else {
        "Report only (dry-run)"
    };
    println!("  Mode   : {}", mode_desc);
    println!("===================================================");

    let queue = Arc::new(Mutex::new(targets));
    let results = Arc::new(Mutex::new(Vec::new()));

    let mut handles = Vec::new();
    let num_workers = thread_count.min(total_count).max(1);

    for _ in 0..num_workers {
        let q = Arc::clone(&queue);
        let r = Arc::clone(&results);
        let handle = thread::spawn(move || {
            loop {
                let item = {
                    let mut lock = q.lock().unwrap();
                    lock.pop()
                };
                let item = match item {
                    Some(i) => i,
                    None => break,
                };

                let res = check_url_with_curl(&item.url, timeout_secs);
                let mut r_lock = r.lock().unwrap();
                r_lock.push((item, res));
            }
        });
        handles.push(handle);
    }

    for h in handles {
        let _ = h.join();
    }

    let mut checked_results = results.lock().unwrap().clone();
    checked_results.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    let mut ok_count = 0;
    let mut warn_count = 0;
    let mut dead_count = 0;
    let mut dead_items = Vec::new();

    for (item, res) in &checked_results {
        match res {
            CheckResult::Ok(code) => {
                ok_count += 1;
                println!("[ OK ] {:3} | {} ({})", code, item.name, item.url);
            }
            CheckResult::Warn(code, reason) => {
                warn_count += 1;
                println!("[WARN] {:3} | {} ({} - {})", code, item.name, item.url, reason);
            }
            CheckResult::Dead(code, reason) => {
                dead_count += 1;
                dead_items.push(item.clone());
                let code_str = if *code > 0 { code.to_string() } else { "---".to_string() };
                println!("[DEAD] {:3} | {} ({} - {})", code_str, item.name, item.url, reason);
            }
        }
    }

    println!("===================================================");
    println!("Link Check Summary: {} checked", total_count);
    println!("  - OK   : {}", ok_count);
    println!("  - WARN : {} (Maintained)", warn_count);
    println!("  - DEAD : {}", dead_count);
    println!("===================================================");

    // 退避処理
    if (to_trash || to_isolate) && !dead_items.is_empty() {
        maybe_backup(prof, 0, config.backup_keep_generations)?;
        let mut write_val = read_profile_bookmarks(prof)?;
        let mut moved_count = 0;

        for dead in &dead_items {
            if to_trash {
                if let Ok(_) = delete_single_node(&mut write_val, &dead.path_segments) {
                    moved_count += 1;
                }
            } else if to_isolate {
                ensure_isolate_folder(&mut write_val);
                if let Ok((parent, idx)) = find_parent_and_index_mut(&mut write_val, &dead.path_segments) {
                    let children = parent.get_mut("children").and_then(|c| c.as_array_mut()).unwrap();
                    let node = children.remove(idx);

                    if let Some(iso_node) = get_root_node_mut(&mut write_val, ISOLATE_FOLDER_NAME) {
                        if let Some(iso_children) = iso_node.get_mut("children").and_then(|c| c.as_array_mut()) {
                            iso_children.push(node);
                            moved_count += 1;
                        }
                    }
                }
            }
        }

        write_profile_bookmarks(prof, &write_val)?;
        let target_dest_name = if to_trash { TRASH_FOLDER_NAME } else { ISOLATE_FOLDER_NAME };
        println!("Successfully moved {} dead bookmark(s) to '{}'.", moved_count, target_dest_name);
        log_msg(&format!("[{}] Link check: moved {} dead items to '{}'", prof.name, moved_count, target_dest_name));
    }

    Ok(())
}

// ==============================================================================
// ダンプ出力機能 (Emacs / 外部ツール横断検索連携)
// ==============================================================================

fn dump_node_tsv<W: Write>(
    writer: &mut W,
    prof_name: &str,
    current_path: &str,
    node: &Value,
) -> Result<()> {
    let n_type = node.get("type").and_then(|v| v.as_str()).unwrap_or("");
    let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("");

    // ゴミ箱やリンク切れフォルダは除外
    if n_type == "folder" && (name == TRASH_FOLDER_NAME || name == ISOLATE_FOLDER_NAME || name.eq_ignore_ascii_case("trash")) {
        return Ok(());
    }

    if n_type == "url" {
        if let Some(url) = node.get("url").and_then(|u| u.as_str()) {
            let u_trim = url.trim();
            if !u_trim.is_empty() {
                let clean_name = name.replace(['\t', '\r', '\n'], " ");
                let clean_url = u_trim.replace(['\t', '\r', '\n'], "");
                let clean_path = current_path.replace(['\t', '\r', '\n'], " ");
                let _ = writeln!(writer, "{}\t{}\t{}\t{}", clean_name, clean_url, prof_name, clean_path);
            }
        }
    } else if n_type == "folder" {
        let next_path = if current_path.is_empty() {
            name.to_string()
        } else {
            format!("{}/{}", current_path, name)
        };
        if let Some(children) = node.get("children").and_then(|c| c.as_array()) {
            for child in children {
                dump_node_tsv(writer, prof_name, &next_path, child)?;
            }
        }
    }

    Ok(())
}

/// dump サブコマンド (全ブラウザまたは指定ブラウザのブックマークをTSV出力)
fn cmd_dump(config: &Config, target_prof: Option<&str>) -> Result<()> {
    let profiles: Vec<&Profile> = if let Some(p_name) = target_prof {
        let p = find_profile(config, p_name)
            .ok_or_else(|| anyhow!("Browser profile '{}' not found", p_name))?;
        vec![p]
    } else {
        config.profiles.iter().collect()
    };

    let mut out = BufWriter::new(std::io::stdout());

    for prof in profiles {
        let val = match read_profile_bookmarks(prof) {
            Ok(v) => v,
            Err(_) => continue,
        };

        if let Some(roots) = val.get("roots") {
            if let Some(bar) = roots.get("bookmark_bar") {
                dump_node_tsv(&mut out, &prof.name, "", bar)?;
            }
            if let Some(other) = roots.get("other") {
                dump_node_tsv(&mut out, &prof.name, "", other)?;
            }
            if let Some(menu) = roots.get("menu") {
                dump_node_tsv(&mut out, &prof.name, "", menu)?;
            }
            if let Some(hist) = roots.get("history") {
                dump_node_tsv(&mut out, &prof.name, "", hist)?;
            }
        }
    }

    let _ = out.flush();
    Ok(())
}

fn print_help() {
    println!("auxbookmark v0.2.0 - PPx aux: path Web Bookmark bridge");
    println!("A lightweight CLI bridge between Paper Plane xUI (PPx) aux: path and Chromium & Firefox Web Bookmarks.");
    println!();
    println!("Copyright (c) 2026 auxbookmark contributors");
    println!();
    println!("Usage:");
    println!("  auxbookmark <command> [arguments...] [-debug]");
    println!();
    println!("Commands:");
    println!("  list    <virtual_path> [output_file]");
    println!("          Enumerate bookmarks and folders in PPx ListFile format.");
    println!("  get     <virtual_path> <dest_file>");
    println!("          Generate an .url InternetShortcut file for browser launching.");
    println!("  copy    <src_path> <dest_path>");
    println!("          Copy bookmark or folder across folders or browsers.");
    println!("  move    <src_path> <dest_path>");
    println!("          Move bookmark or folder across folders or browsers.");
    println!("  rename  <src_path> <dest_path>");
    println!("          Rename bookmark or folder.");
    println!("  delete  <virtual_path> [items...]");
    println!("          Move bookmark(s) to Trash, or permanently delete if already in Trash.");
    println!("  makedir <parent_path> [folder_name]");
    println!("          Create a new bookmark folder.");
    println!("  deldir  <virtual_path> [folder_name]");
    println!("          Delete a bookmark folder (moves to Trash).");
    println!("  export  <profile_or_path> [output_file] [--all | --include-trash]");
    println!("          Export bookmarks to Netscape Bookmark HTML format.");
    println!("          Default: Excludes Trash folder for a clean export.");
    println!("          --all / --include-trash: Includes Trash folder.");
    println!("  check   <profile_or_path> [--trash | --isolate] [--timeout <secs>] [--threads <N>]");
    println!("          Check dead bookmarks concurrently with SSL-insecure fallback.");
    println!("          Default: Report only (dry-run).");
    println!("          --trash:   Move dead bookmarks to Trash.");
    println!("          --isolate: Move dead bookmarks to 'リンク切れ' folder.");
    println!("          --timeout: Connection timeout in seconds (default: 5).");
    println!("          --threads: Concurrency level (default: 8).");
    println!("  dump    [profile_name]");
    println!("          Dump all bookmarks as TSV (title\\turl\\tbrowser\\tpath) for Emacs/CLI.");
    println!();
    println!("Options:");
    println!("  -d, -debug, --debug   Enable debug logging (stderr + auxbookmark.log).");
    println!("  -h, -help, --help     Show this help message.");
    println!();
    println!("Supported Browsers:");
    println!("  Brave, Google Chrome, Microsoft Edge, Firefox, and Firefox-compatible browsers (configured in auxbookmark.ini).");
}

// ==============================================================================
// エントリポイント
// ==============================================================================

fn main() {
    let raw_args: Vec<String> = env::args().collect();
    let mut args = Vec::new();
    for arg in &raw_args {
        if is_opt(arg, &["debug", "d"]) {
            set_debug_mode(true);
        } else {
            args.push(arg.clone());
        }
    }

    log_msg(&format!("START: {:?}", args));

    if args.len() < 2 {
        print_help();
        return;
    }

    let cmd = args[1].to_lowercase();
    if matches!(cmd.as_str(), "help" | "--help" | "-help" | "-h" | "/h" | "/help" | "/?" | "-?") {
        print_help();
        return;
    }

    let config = match load_config() {
        Ok(c) => c,
        Err(e) => {
            log_msg(&format!("Configuration Error: {:#}", e));
            std::process::exit(1);
        }
    };

    let res = match cmd.as_str() {
        "list" => {
            let virtual_path = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let output_file = args.get(3).map(|s| s.as_str());
            let res = cmd_list(&config, virtual_path, output_file);
            if res.is_err() {
                if let Some(out_path) = output_file {
                    if let Ok(mut f) = File::create(out_path) {
                        let _ = write!(f, ";ListFile\r\n");
                    }
                }
            }
            res
        }
        "get" => {
            if args.len() < 4 {
                Err(anyhow!("Usage: auxbookmark get <virtual_path> <dest_local_file>"))
            } else {
                cmd_get(&config, &args[2], &args[3])
            }
        }
        "makedir" => {
            let parent_path = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let folder_name = args.get(3).map(|s| s.as_str());
            cmd_makedir(&config, parent_path, folder_name)
        }
        "deldir" => {
            let virtual_path = args.get(2).map(|s| s.as_str()).unwrap_or("");
            let folder_name = args.get(3).map(|s| s.as_str());
            cmd_deldir(&config, virtual_path, folder_name)
        }
        "delete" => {
            if args.len() < 3 {
                Err(anyhow!("Usage: auxbookmark delete <parent_path> [item1 item2...]"))
            } else {
                let parent_path = &args[2];
                let items = &args[3..];
                cmd_delete(&config, parent_path, items)
            }
        }
        "move" => {
            if args.len() < 4 {
                Err(anyhow!("Usage: auxbookmark move <src_path> <dest_path>"))
            } else {
                cmd_move(&config, &args[2], &args[3])
            }
        }
        "copy" => {
            if args.len() < 4 {
                Err(anyhow!("Usage: auxbookmark copy <src_path> <dest_path>"))
            } else {
                cmd_copy(&config, &args[2], &args[3])
            }
        }
        "rename" => {
            if args.len() < 4 {
                Err(anyhow!("Usage: auxbookmark rename <src_path> <dest_path>"))
            } else {
                cmd_rename(&config, &args[2], &args[3])
            }
        }
        "export" => {
            let mut target = "";
            let mut out_file = None;
            let mut include_trash = false;

            for arg in &args[2..] {
                if is_opt(arg, &["all"]) || is_opt(arg, &["include-trash"]) {
                    include_trash = true;
                } else if target.is_empty() {
                    target = arg.as_str();
                } else if out_file.is_none() {
                    out_file = Some(arg.as_str());
                }
            }

            cmd_export(&config, target, out_file, include_trash)
        }
        "check" => {
            let mut target = "";
            let mut to_trash = false;
            let mut to_isolate = false;
            let mut timeout_secs = 5u64;
            let mut threads = 8usize;

            let mut i = 2;
            while i < args.len() {
                let arg = args[i].as_str();
                if is_opt(arg, &["trash"]) {
                    to_trash = true;
                } else if is_opt(arg, &["isolate"]) {
                    to_isolate = true;
                } else if is_opt(arg, &["timeout"]) && i + 1 < args.len() {
                    i += 1;
                    if let Ok(t) = args[i].parse::<u64>() {
                        timeout_secs = t.max(1);
                    }
                } else if let Some(val) = strip_opt_val(arg, &["timeout"]) {
                    if let Ok(t) = val.parse::<u64>() {
                        timeout_secs = t.max(1);
                    }
                } else if is_opt(arg, &["threads"]) && i + 1 < args.len() {
                    i += 1;
                    if let Ok(th) = args[i].parse::<usize>() {
                        threads = th.max(1);
                    }
                } else if let Some(val) = strip_opt_val(arg, &["threads"]) {
                    if let Ok(th) = val.parse::<usize>() {
                        threads = th.max(1);
                    }
                } else if target.is_empty() {
                    target = arg;
                }
                i += 1;
            }

            cmd_check(&config, target, to_trash, to_isolate, timeout_secs, threads)
        }
        "dump" => {
            let target = args.get(2).map(|s| s.as_str());
            cmd_dump(&config, target)
        }
        _ => Err(anyhow!("Unknown command: {}", cmd)),
    };

    if let Err(e) = res {
        log_msg(&format!("ERROR: {:#}", e));
        std::process::exit(1);
    } else {
        log_msg("SUCCESS");
    }
}

