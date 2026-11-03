//! The Shrine of the God of Small Code.
//!
//! A tiny always-there desktop shrine. Offer it small files (drag, Ctrl+V,
//! Send To) and it consumes them in an eternal flame; the files are moved to an
//! `ashes` folder, never destroyed. Pray to it and it answers uselessly.
//!
//! Raw Win32 via windows-sys: one thread, a layered window, ~15 fps idle,
//! zero work between timer ticks.
#![cfg_attr(windows, windows_subsystem = "windows")]

mod gfx;
mod oracle;

use std::cell::RefCell;
use std::ffi::c_void;
use std::fs;
use std::mem::{size_of, zeroed};
use std::path::{Path, PathBuf};
use std::ptr::{null, null_mut};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows_sys::w;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::DataExchange::*;
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::*;
use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};
use windows_sys::Win32::System::Threading::*;
use windows_sys::Win32::UI::Controls::{EM_SCROLLCARET, EM_SETLIMITTEXT, EM_SETSEL};
use windows_sys::Win32::UI::HiDpi::*;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::*;
use windows_sys::Win32::UI::Shell::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use gfx::Gfx;

// ------------------------------------------------------------------ consts

const T_FRAME: usize = 1;
const T_SPEECH: usize = 2;

const IDM_PRAY: usize = 1001;
const IDM_PASTE: usize = 1002;
const IDM_GATHER: usize = 1003;
const IDM_SCATTER: usize = 1004;
const IDM_TOPMOST: usize = 1005;
const IDM_CORNER: usize = 1006;
const IDM_QUIT: usize = 1007;

const ID_PRAY_BTN: i32 = 1;
const ID_EDIT_IN: i32 = 101;
const ID_EDIT_LOG: i32 = 102;

const COPYDATA_MAGIC: usize = 0x474F_5343; // "GOSC"
const MAX_OFFERING: u64 = 1 << 20; // 1 MiB: the god scorns the large
const MAX_FILES_PER_OFFERING: usize = 64;
const FRAME_MS_IDLE: u32 = 66; // ~15 fps
const FRAME_MS_BUSY: u32 = 33; // ~30 fps while something burns

const fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}
const COL_BG: u32 = rgb(28, 22, 18);
const COL_TEXT: u32 = rgb(255, 205, 130);

// ------------------------------------------------------------------- state

struct Drag {
    start: POINT,
    origin: POINT,
    moved: bool,
}

struct Config {
    x: Option<i32>,
    y: Option<i32>,
    topmost: bool,
}

struct Ui {
    hwnd: HWND,
    dpi: u32,
    cfg: Config,
    ashes: PathBuf,
    data_dir: PathBuf,
    font: HFONT,
    brush: HBRUSH,
    prayer: HWND,
    prayer_in: HWND,
    prayer_log: HWND,
    speech: HWND,
    transcript: String,
    drag: Option<Drag>,
}

/// A 32-bit top-down DIB section the renderer draws straight into.
struct Surface {
    dc: HDC,
    bmp: HBITMAP,
    old: HGDIOBJ,
    bits: *mut u32,
    w: usize,
    h: usize,
}

impl Drop for Surface {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.dc, self.old);
            DeleteObject(self.bmp);
            DeleteDC(self.dc);
        }
    }
}

struct Anim {
    hwnd: HWND,
    gfx: Gfx,
    surf: Surface,
    last: Instant,
    fast: bool,
}

impl Anim {
    fn present(&mut self, pos: Option<(i32, i32)>) {
        let buf = unsafe { std::slice::from_raw_parts_mut(self.surf.bits, self.surf.w * self.surf.h) };
        self.gfx.render(buf);
        let size = SIZE { cx: self.surf.w as i32, cy: self.surf.h as i32 };
        let src = POINT { x: 0, y: 0 };
        let dst = pos.map(|(x, y)| POINT { x, y });
        let blend = BLENDFUNCTION {
            BlendOp: AC_SRC_OVER as u8,
            BlendFlags: 0,
            SourceConstantAlpha: 255,
            AlphaFormat: AC_SRC_ALPHA as u8,
        };
        unsafe {
            UpdateLayeredWindow(
                self.hwnd,
                null_mut(),
                dst.as_ref().map_or(null(), |p| p as *const POINT),
                &size,
                self.surf.dc,
                &src,
                0,
                &blend,
                ULW_ALPHA,
            );
        }
    }

    fn set_fast(&mut self, fast: bool) {
        if fast != self.fast {
            self.fast = fast;
            unsafe {
                SetTimer(self.hwnd, T_FRAME, if fast { FRAME_MS_BUSY } else { FRAME_MS_IDLE }, None);
            }
        }
    }
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
    static ANIM: RefCell<Option<Anim>> = const { RefCell::new(None) };
    static SPEECH_TEXT: RefCell<Vec<u16>> = const { RefCell::new(Vec::new()) };
}

/// Short-lived access to the UI state. Never hold across a Win32 call that
/// can dispatch messages; if it is busy we just skip (returns None).
fn ui<R>(f: impl FnOnce(&mut Ui) -> R) -> Option<R> {
    UI.with(|c| c.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

fn anim<R>(f: impl FnOnce(&mut Anim) -> R) -> Option<R> {
    ANIM.with(|c| c.try_borrow_mut().ok().and_then(|mut b| b.as_mut().map(f)))
}

// ----------------------------------------------------------------- helpers

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(windows)]
fn wide_to_path(w: &[u16]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    PathBuf::from(std::ffi::OsString::from_wide(w))
}
#[cfg(not(windows))]
fn wide_to_path(w: &[u16]) -> PathBuf {
    PathBuf::from(String::from_utf16_lossy(w))
}

#[cfg(windows)]
fn path_to_wide(p: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    p.as_os_str().encode_wide().collect()
}
#[cfg(not(windows))]
fn path_to_wide(p: &Path) -> Vec<u16> {
    p.to_string_lossy().encode_utf16().collect()
}

fn time_seed() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(1)
}

fn unix_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn human(n: u64) -> String {
    if n < 1024 {
        format!("{n} B")
    } else if n < 1024 * 1024 {
        format!("{:.1} KiB", n as f64 / 1024.0)
    } else {
        format!("{:.1} MiB", n as f64 / 1048576.0)
    }
}

fn data_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("GodOfSmallCode")
}

fn fatal(msg: &str) -> ! {
    unsafe {
        MessageBoxW(null_mut(), wide(msg).as_ptr(), w!("God of Small Code"), MB_OK | MB_ICONERROR);
    }
    std::process::exit(1);
}

// ------------------------------------------------------------------ config

impl Config {
    fn load(dir: &Path) -> Config {
        let mut c = Config { x: None, y: None, topmost: true };
        if let Ok(text) = fs::read_to_string(dir.join("shrine.ini")) {
            for line in text.lines() {
                if let Some((k, v)) = line.split_once('=') {
                    match k.trim() {
                        "x" => c.x = v.trim().parse().ok(),
                        "y" => c.y = v.trim().parse().ok(),
                        "topmost" => c.topmost = v.trim() != "0",
                        _ => {}
                    }
                }
            }
        }
        c
    }

    fn save(&self, dir: &Path) {
        let mut s = String::new();
        if let (Some(x), Some(y)) = (self.x, self.y) {
            s.push_str(&format!("x={x}\ny={y}\n"));
        }
        s.push_str(&format!("topmost={}\n", if self.topmost { 1 } else { 0 }));
        let _ = fs::create_dir_all(dir);
        let _ = fs::write(dir.join("shrine.ini"), s);
    }
}

unsafe fn save_config(hwnd: HWND) {
    let mut rc: RECT = zeroed();
    GetWindowRect(hwnd, &mut rc);
    ui(|u| {
        u.cfg.x = Some(rc.left);
        u.cfg.y = Some(rc.top);
        u.cfg.save(&u.data_dir);
    });
}

// ---------------------------------------------------------------- geometry

unsafe fn default_pos(w: i32, h: i32) -> (i32, i32) {
    let mut rc = RECT { left: 0, top: 0, right: 1280, bottom: 720 };
    SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut rc as *mut RECT as *mut c_void, 0);
    let margin = w / 8;
    (rc.right - w - margin, rc.bottom - h - margin)
}

unsafe fn on_screen(x: i32, y: i32, w: i32, h: i32) -> bool {
    let vx = GetSystemMetrics(SM_XVIRTUALSCREEN);
    let vy = GetSystemMetrics(SM_YVIRTUALSCREEN);
    let vw = GetSystemMetrics(SM_CXVIRTUALSCREEN);
    let vh = GetSystemMetrics(SM_CYVIRTUALSCREEN);
    let (cx, cy) = (x + w / 2, y + h / 2);
    cx >= vx && cx < vx + vw && cy >= vy && cy < vy + vh
}

unsafe fn make_surface(w: usize, h: usize) -> Option<Surface> {
    let mut bmi: BITMAPINFO = zeroed();
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w as i32;
    bmi.bmiHeader.biHeight = -(h as i32); // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;
    let screen = GetDC(null_mut());
    let dc = CreateCompatibleDC(screen);
    let mut bits: *mut c_void = null_mut();
    let bmp = CreateDIBSection(screen, &bmi, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
    ReleaseDC(null_mut(), screen);
    if dc.is_null() || bmp.is_null() || bits.is_null() {
        if !bmp.is_null() {
            DeleteObject(bmp);
        }
        if !dc.is_null() {
            DeleteDC(dc);
        }
        return None;
    }
    let old = SelectObject(dc, bmp);
    Some(Surface { dc, bmp, old, bits: bits as *mut u32, w, h })
}

unsafe fn make_font(dpi: u32) -> HFONT {
    let height = -((10 * dpi as i32 + 36) / 72); // 10 pt
    CreateFontW(
        height,
        0,
        0,
        0,
        400,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        (DEFAULT_PITCH | FF_DONTCARE) as u32,
        w!("Segoe UI"),
    )
}

// ---------------------------------------------------------- offerings

#[derive(Clone, Copy, Debug)]
enum Refusal {
    Folder,
    NotPlain,
    TooLarge,
    Clings,
    Vanished,
    NoAshes,
    AlreadyAshes,
    TooMany,
}

impl Refusal {
    fn text(self) -> &'static str {
        match self {
            Refusal::Folder => "folders are not offerings; the god eats files",
            Refusal::NotPlain => "only plain files may be offered",
            Refusal::TooLarge => "the god scorns the large (limit 1 MiB)",
            Refusal::Clings => "it clings to its owner (in use, locked or read-only?)",
            Refusal::Vanished => "it was gone before the flame arrived",
            Refusal::NoAshes => "the ashes could not be gathered (disk or permissions?)",
            Refusal::AlreadyAshes => "it is already ashes",
            Refusal::TooMany => "too many at once; the god has only one flame",
        }
    }
}

#[derive(Default)]
struct Outcome {
    accepted: Vec<(String, u64)>,
    refused: Vec<(String, Refusal)>,
}

fn display_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// Pick a name in `dir` that does not exist yet: `a.txt`, `a (2).txt`, ...
fn unique_dest(dir: &Path, name: &std::ffi::OsStr) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "offering".into());
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    for n in 2..10_000 {
        let cand = dir.join(format!("{stem} ({n}){ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    dir.join(format!("{stem}-{}{ext}", unix_secs()))
}

/// Move one file into the ashes. Never deletes anything unless the copy landed.
fn consume_file(src: &Path, ashes: &Path) -> Result<u64, Refusal> {
    let meta = fs::symlink_metadata(src).map_err(|_| Refusal::Vanished)?;
    if meta.is_dir() {
        return Err(Refusal::Folder);
    }
    if !meta.file_type().is_file() {
        return Err(Refusal::NotPlain);
    }
    let len = meta.len();
    if len > MAX_OFFERING {
        return Err(Refusal::TooLarge);
    }
    if src.starts_with(ashes) {
        return Err(Refusal::AlreadyAshes);
    }
    let name = src.file_name().ok_or(Refusal::NotPlain)?;
    fs::create_dir_all(ashes).map_err(|_| Refusal::NoAshes)?;
    let dest = unique_dest(ashes, name);
    if fs::rename(src, &dest).is_ok() {
        return Ok(len);
    }
    // different volume (or similar): copy, then remove the original
    fs::copy(src, &dest).map_err(|_| Refusal::Clings)?;
    if fs::remove_file(src).is_err() {
        let _ = fs::remove_file(&dest); // undo: the original stays, nothing duplicated
        return Err(Refusal::Clings);
    }
    Ok(len)
}

fn offer_paths(paths: Vec<PathBuf>) {
    let Some(ashes) = ui(|u| u.ashes.clone()) else { return };
    let mut out = Outcome::default();
    for (i, p) in paths.iter().enumerate() {
        let name = display_name(p);
        if i >= MAX_FILES_PER_OFFERING {
            out.refused.push((name, Refusal::TooMany));
            continue;
        }
        match consume_file(p, &ashes) {
            Ok(n) => out.accepted.push((name, n)),
            Err(r) => out.refused.push((name, r)),
        }
    }
    announce(out);
}

fn offer_text(text: String) {
    let Some(ashes) = ui(|u| u.ashes.clone()) else { return };
    let mut out = Outcome::default();
    let name = "pasted text".to_string();
    if text.trim().is_empty() {
        say("The clipboard holds only whitespace. The god respects the void, but cannot burn it.");
        return;
    }
    if text.len() as u64 > MAX_OFFERING {
        out.refused.push((name, Refusal::TooLarge));
    } else if fs::create_dir_all(&ashes).is_err() {
        out.refused.push((name, Refusal::NoAshes));
    } else {
        let dest = unique_dest(&ashes, std::ffi::OsStr::new(&format!("pasted-{}.txt", unix_secs())));
        match fs::write(&dest, text.as_bytes()) {
            Ok(()) => out.accepted.push((name, text.len() as u64)),
            Err(_) => out.refused.push((name, Refusal::NoAshes)),
        }
    }
    announce(out);
}

fn quip(name: &str) -> &'static str {
    let ext = Path::new(name).extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "py" => "Snakes are acceptable fuel.",
        "sh" | "bash" | "zsh" => "Shell scripts burn with a satisfying crackle.",
        "ps1" | "psm1" => "PowerShell: verbose, yet flammable.",
        "bat" | "cmd" => "Ancient batch. The flame is nostalgic.",
        "json" => "Braces burn brightest.",
        "md" => "Prose! The god is moved.",
        "txt" => "Plain and honest fuel.",
        "rs" => "Rust burns slowly, and without leaks.",
        "js" | "ts" => "Brackets, semicolons and regret. Delicious.",
        "yml" | "yaml" | "toml" | "ini" | "cfg" => "Indentation is sacred. The flame respects it.",
        "c" | "cpp" | "h" | "hpp" => "Pointers. The flame handles them with care.",
        "sql" => "Queries rise as smoke and return no rows.",
        "html" | "css" => "Markup crackles prettily.",
        _ => "The flame accepts this.",
    }
}

fn announce(out: Outcome) {
    let n = out.accepted.len();
    let m = out.refused.len();
    if n > 0 {
        anim(|a| a.gfx.offer(n));
    } else if m > 0 {
        anim(|a| a.gfx.recoil());
    }
    let total: u64 = out.accepted.iter().map(|(_, s)| *s).sum();
    let msg = match (n, m) {
        (0, 0) => return,
        (1, 0) => format!("Consumed: {} ({}).\n{}", out.accepted[0].0, human(total), quip(&out.accepted[0].0)),
        (_, 0) => format!("Consumed {n} offerings ({}).\nThe flame is pleased.", human(total)),
        (0, _) => format!("Refused: {}\nBecause {}.", out.refused[0].0, out.refused[0].1.text()),
        _ => format!(
            "Consumed {n} ({}). Refused {m}.\nFirst refusal: {}, because {}.",
            human(total),
            out.refused[0].0,
            out.refused[0].1.text()
        ),
    };
    say(&msg);
    anim(|a| a.set_fast(true));
}

// ---------------------------------------------------------- input sources

unsafe fn drop_paths(h: HDROP) -> Vec<PathBuf> {
    let n = DragQueryFileW(h, 0xFFFF_FFFF, null_mut(), 0);
    let mut v = Vec::new();
    for i in 0..n.min(512) {
        let len = DragQueryFileW(h, i, null_mut(), 0) as usize;
        if len == 0 {
            continue;
        }
        let mut buf = vec![0u16; len + 1];
        let got = DragQueryFileW(h, i, buf.as_mut_ptr(), buf.len() as u32) as usize;
        v.push(wide_to_path(&buf[..got.min(len)]));
    }
    v
}

unsafe fn read_global_text(h: HGLOBAL) -> Option<String> {
    let size = GlobalSize(h);
    let p = GlobalLock(h) as *const u16;
    if p.is_null() {
        return None;
    }
    let all = std::slice::from_raw_parts(p, size / 2);
    let len = all.iter().position(|&c| c == 0).unwrap_or(all.len());
    let s = String::from_utf16_lossy(&all[..len.min(MAX_OFFERING as usize + 1)]);
    GlobalUnlock(h);
    Some(s)
}

unsafe fn paste(hwnd: HWND) {
    let mut paths: Vec<PathBuf> = Vec::new();
    let mut text: Option<String> = None;
    if OpenClipboard(hwnd) != 0 {
        let h = GetClipboardData(CF_HDROP as u32);
        if !h.is_null() {
            paths = drop_paths(h as HDROP);
        } else {
            let t = GetClipboardData(CF_UNICODETEXT as u32);
            if !t.is_null() {
                text = read_global_text(t as HGLOBAL);
            }
        }
        CloseClipboard();
    }
    if !paths.is_empty() {
        offer_paths(paths);
    } else if let Some(t) = text {
        offer_text(t);
    } else {
        say("The clipboard holds nothing the god can eat.");
        anim(|a| a.gfx.recoil());
        anim(|a| a.set_fast(true));
    }
}

unsafe fn on_copydata(lp: LPARAM) {
    let cds = &*(lp as *const COPYDATASTRUCT);
    if cds.dwData != COPYDATA_MAGIC {
        return;
    }
    let n = (cds.cbData as usize / 2).min(1 << 20);
    let paths: Vec<PathBuf> = if n == 0 || cds.lpData.is_null() {
        Vec::new()
    } else {
        std::slice::from_raw_parts(cds.lpData as *const u16, n)
            .split(|&c| c == 10)
            .filter(|p| !p.is_empty())
            .map(wide_to_path)
            .collect()
    };
    if paths.is_empty() {
        anim(|a| a.gfx.flare());
        anim(|a| a.set_fast(true));
        say("Thou summonest the God of Small Code. He is already here. He has always been here.");
    } else {
        offer_paths(paths);
    }
}

// ------------------------------------------------------------- speech bubble

fn say(text: &str) {
    SPEECH_TEXT.with(|t| *t.borrow_mut() = text.encode_utf16().collect());
    unsafe { show_speech() }
}

unsafe fn measure(hwnd: HWND, font: HFONT, text: &[u16], max_w: i32) -> (i32, i32) {
    let dc = GetDC(hwnd);
    let old = SelectObject(dc, font);
    let mut rc = RECT { left: 0, top: 0, right: max_w, bottom: 0 };
    DrawTextW(dc, text.as_ptr(), text.len() as i32, &mut rc, DT_CALCRECT | DT_WORDBREAK | DT_NOPREFIX);
    SelectObject(dc, old);
    ReleaseDC(hwnd, dc);
    (rc.right - rc.left, rc.bottom - rc.top)
}

unsafe fn show_speech() {
    let Some((owner, dpi, font, mut speech)) = ui(|u| (u.hwnd, u.dpi, u.font, u.speech)) else { return };
    if speech.is_null() {
        speech = CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_NOACTIVATE,
            w!("GodOfSmallCodeSpeech"),
            null(),
            WS_POPUP | WS_BORDER,
            0,
            0,
            10,
            10,
            owner,
            null_mut(),
            GetModuleHandleW(null()),
            null(),
        );
        if speech.is_null() {
            return;
        }
        ui(|u| u.speech = speech);
    }
    let sf = dpi as f32 / 96.0;
    let pad = (10.0 * sf) as i32;
    let (tw, th, chars) = SPEECH_TEXT.with(|t| {
        let t = t.borrow();
        let (w, h) = measure(speech, font, &t, (300.0 * sf) as i32);
        (w, h, t.len())
    });
    let (bw, bh) = (tw + 2 * pad + 4, th + 2 * pad + 4);

    let mut sr: RECT = zeroed();
    GetWindowRect(owner, &mut sr);
    let mon = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(mon, &mut mi);
    let work = mi.rcWork;
    let gap = (6.0 * sf) as i32;
    let mut x = sr.left - bw - gap;
    if x < work.left {
        x = sr.right + gap;
    }
    let mut y = sr.top + (100.0 * sf) as i32 - bh / 2;
    x = x.clamp(work.left, (work.right - bw).max(work.left));
    y = y.clamp(work.top, (work.bottom - bh).max(work.top));

    SetWindowPos(speech, HWND_TOPMOST, x, y, bw, bh, SWP_NOACTIVATE | SWP_SHOWWINDOW);
    InvalidateRect(speech, null(), 1);
    let ms = (3500 + 40 * chars as u32).min(9000);
    SetTimer(owner, T_SPEECH, ms, None);
}

unsafe fn hide_speech() {
    if let Some((owner, speech)) = ui(|u| (u.hwnd, u.speech)) {
        if !speech.is_null() {
            ShowWindow(speech, SW_HIDE);
        }
        KillTimer(owner, T_SPEECH);
    }
}

unsafe extern "system" fn speech_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            let mut ps: PAINTSTRUCT = zeroed();
            let dc = BeginPaint(hwnd, &mut ps);
            let mut rc: RECT = zeroed();
            GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(COL_BG);
            FillRect(dc, &rc, brush);
            DeleteObject(brush);
            let (font, dpi) = ui(|u| (u.font, u.dpi)).unwrap_or((null_mut(), 96));
            let old = SelectObject(dc, font);
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, COL_TEXT);
            let pad = (10.0 * dpi as f32 / 96.0) as i32;
            rc.left += pad;
            rc.right -= pad;
            rc.top += pad;
            rc.bottom -= pad;
            SPEECH_TEXT.with(|t| {
                let t = t.borrow();
                DrawTextW(dc, t.as_ptr(), t.len() as i32, &mut rc, DT_WORDBREAK | DT_NOPREFIX);
            });
            SelectObject(dc, old);
            EndPaint(hwnd, &ps);
            0
        }
        WM_ERASEBKGND => 1,
        WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
        WM_LBUTTONDOWN => {
            hide_speech();
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ------------------------------------------------------------ prayer window

unsafe fn open_prayer() {
    let Some((owner, dpi, font, existing, input)) = ui(|u| (u.hwnd, u.dpi, u.font, u.prayer, u.prayer_in)) else {
        return;
    };
    if !existing.is_null() && IsWindow(existing) != 0 {
        ShowWindow(existing, SW_SHOW);
        SetForegroundWindow(existing);
        SetFocus(input);
        return;
    }
    let sf = dpi as f32 / 96.0;
    let px = |n: i32| (n as f32 * sf).round() as i32;
    let (cw, ch) = (px(470), px(340));
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
    let mut rc = RECT { left: 0, top: 0, right: cw, bottom: ch };
    AdjustWindowRectExForDpi(&mut rc, style, 0, 0, dpi);
    let (ww, wh) = (rc.right - rc.left, rc.bottom - rc.top);

    let mon = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
    let mut mi: MONITORINFO = zeroed();
    mi.cbSize = size_of::<MONITORINFO>() as u32;
    GetMonitorInfoW(mon, &mut mi);
    let work = mi.rcWork;
    let x = work.left + ((work.right - work.left) - ww) / 2;
    let y = work.top + ((work.bottom - work.top) - wh) / 3;

    let hinst = GetModuleHandleW(null());
    let win = CreateWindowExW(
        0,
        w!("GodOfSmallCodePrayer"),
        w!("Pray to the God of Small Code"),
        style,
        x,
        y,
        ww,
        wh,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if win.is_null() {
        return;
    }
    let child = |ex: u32, class: *const u16, text: *const u16, style: u32, x: i32, y: i32, w: i32, h: i32, id: i32| {
        CreateWindowExW(ex, class, text, style, x, y, w, h, win, id as usize as HMENU, hinst, null())
    };
    let label = child(
        0,
        w!("STATIC"),
        w!("Speak thy prayer. The flame shall answer."),
        WS_CHILD | WS_VISIBLE,
        px(14),
        px(12),
        cw - px(28),
        px(20),
        0,
    );
    let input = child(
        WS_EX_CLIENTEDGE,
        w!("EDIT"),
        null(),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | ES_AUTOHSCROLL as u32,
        px(14),
        px(36),
        cw - px(28) - px(86),
        px(26),
        ID_EDIT_IN,
    );
    let button = child(
        0,
        w!("BUTTON"),
        w!("Pray"),
        WS_CHILD | WS_VISIBLE | WS_TABSTOP | BS_PUSHBUTTON as u32,
        cw - px(14) - px(76),
        px(35),
        px(76),
        px(28),
        ID_PRAY_BTN,
    );
    let log = child(
        WS_EX_CLIENTEDGE,
        w!("EDIT"),
        null(),
        WS_CHILD | WS_VISIBLE | WS_VSCROLL | (ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL) as u32,
        px(14),
        px(76),
        cw - px(28),
        ch - px(76) - px(14),
        ID_EDIT_LOG,
    );
    for c in [label, input, button, log] {
        SendMessageW(c, WM_SETFONT, font as WPARAM, 1);
    }
    SendMessageW(input, EM_SETLIMITTEXT, 200, 0);

    let transcript = ui(|u| {
        u.prayer = win;
        u.prayer_in = input;
        u.prayer_log = log;
        u.transcript.clone()
    })
    .unwrap_or_default();
    SetWindowTextW(log, wide(&transcript).as_ptr());
    let n = transcript.encode_utf16().count();
    SendMessageW(log, EM_SETSEL, n, n as isize);
    SendMessageW(log, EM_SCROLLCARET, 0, 0);

    ShowWindow(win, SW_SHOW);
    SetForegroundWindow(win);
    SetFocus(input);
}

unsafe fn pray() {
    let Some((input, log)) = ui(|u| (u.prayer_in, u.prayer_log)) else { return };
    if input.is_null() || log.is_null() {
        return;
    }
    let len = GetWindowTextLengthW(input).max(0) as usize;
    let mut buf = vec![0u16; len + 1];
    let got = GetWindowTextW(input, buf.as_mut_ptr(), buf.len() as i32).max(0) as usize;
    let prayer = String::from_utf16_lossy(&buf[..got.min(len)]);

    let reply = oracle::answer(&prayer, time_seed());
    let shown = if prayer.trim().is_empty() { "(silence)".to_string() } else { prayer.trim().to_string() };
    let transcript = ui(|u| {
        u.transcript.push_str(&format!("You:  {shown}\r\nThe God:  {reply}\r\n\r\n"));
        if u.transcript.len() > 6000 {
            let mut cut = u.transcript.len() - 5000;
            while !u.transcript.is_char_boundary(cut) {
                cut += 1;
            }
            u.transcript.drain(..cut);
        }
        u.transcript.clone()
    })
    .unwrap_or_default();

    SetWindowTextW(log, wide(&transcript).as_ptr());
    let n = transcript.encode_utf16().count();
    SendMessageW(log, EM_SETSEL, n, n as isize);
    SendMessageW(log, EM_SCROLLCARET, 0, 0);
    SetWindowTextW(input, w!(""));
    SetFocus(input);

    anim(|a| a.gfx.holy());
    anim(|a| a.set_fast(true));
}

unsafe extern "system" fn prayer_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            if (wp & 0xffff) as i32 == ID_PRAY_BTN {
                pray();
            }
            0
        }
        WM_SETFOCUS => {
            if let Some(input) = ui(|u| u.prayer_in) {
                if !input.is_null() {
                    SetFocus(input);
                }
            }
            0
        }
        WM_CTLCOLOREDIT | WM_CTLCOLORSTATIC => match ui(|u| u.brush) {
            Some(brush) => {
                SetTextColor(wp as HDC, COL_TEXT);
                SetBkColor(wp as HDC, COL_BG);
                brush as LRESULT
            }
            None => DefWindowProcW(hwnd, msg, wp, lp),
        },
        WM_CLOSE => {
            DestroyWindow(hwnd);
            0
        }
        WM_DESTROY => {
            ui(|u| {
                u.prayer = null_mut();
                u.prayer_in = null_mut();
                u.prayer_log = null_mut();
            });
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// -------------------------------------------------------------- shrine window

unsafe fn begin_drag(hwnd: HWND) {
    let mut cur: POINT = zeroed();
    GetCursorPos(&mut cur);
    let mut rc: RECT = zeroed();
    GetWindowRect(hwnd, &mut rc);
    ui(|u| u.drag = Some(Drag { start: cur, origin: POINT { x: rc.left, y: rc.top }, moved: false }));
    SetCapture(hwnd);
}

unsafe fn drag_move(hwnd: HWND) {
    let Some(Some((sx, sy, ox, oy, moved))) =
        ui(|u| u.drag.as_ref().map(|d| (d.start.x, d.start.y, d.origin.x, d.origin.y, d.moved)))
    else {
        return;
    };
    if GetKeyState(VK_LBUTTON as i32) >= 0 {
        // button already up (capture was stolen, e.g. by a menu): forget the drag
        ui(|u| u.drag = None);
        return;
    }
    let mut cur: POINT = zeroed();
    GetCursorPos(&mut cur);
    let (dx, dy) = (cur.x - sx, cur.y - sy);
    if !moved && dx.abs().max(dy.abs()) < GetSystemMetrics(SM_CXDRAG).max(4) {
        return; // a click, not a drag
    }
    ui(|u| {
        if let Some(d) = u.drag.as_mut() {
            d.moved = true;
        }
    });
    SetWindowPos(hwnd, null_mut(), ox + dx, oy + dy, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
}

unsafe fn end_drag(hwnd: HWND) {
    ReleaseCapture();
    let was = ui(|u| u.drag.take()).flatten();
    if was.is_some_and(|d| d.moved) {
        save_config(hwnd);
    }
}

unsafe fn context_menu(hwnd: HWND) {
    let topmost = ui(|u| u.cfg.topmost).unwrap_or(true);
    let menu = CreatePopupMenu();
    AppendMenuW(menu, MF_STRING, IDM_PRAY, w!("Pray...   (double-click)"));
    AppendMenuW(menu, MF_STRING, IDM_PASTE, w!("Offer the clipboard   (Ctrl+V)"));
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, IDM_GATHER, w!("Gather the ashes (open folder)"));
    AppendMenuW(menu, MF_STRING, IDM_SCATTER, w!("Scatter the ashes (empty folder)"));
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(
        menu,
        MF_STRING | if topmost { MF_CHECKED } else { 0 },
        IDM_TOPMOST,
        w!("Keep above other windows"),
    );
    AppendMenuW(menu, MF_STRING, IDM_CORNER, w!("Return to the corner"));
    AppendMenuW(menu, MF_SEPARATOR, 0, null());
    AppendMenuW(menu, MF_STRING, IDM_QUIT, w!("Banish the shrine (quit)"));

    let mut pt: POINT = zeroed();
    GetCursorPos(&mut pt);
    SetForegroundWindow(hwnd);
    let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, hwnd, null());
    DestroyMenu(menu);

    match cmd as usize {
        IDM_PRAY => open_prayer(),
        IDM_PASTE => paste(hwnd),
        IDM_GATHER => gather_ashes(hwnd),
        IDM_SCATTER => scatter_ashes(hwnd),
        IDM_TOPMOST => {
            let now = ui(|u| {
                u.cfg.topmost = !u.cfg.topmost;
                u.cfg.topmost
            })
            .unwrap_or(true);
            SetWindowPos(
                hwnd,
                if now { HWND_TOPMOST } else { HWND_NOTOPMOST },
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            save_config(hwnd);
        }
        IDM_CORNER => {
            if let Some((w, h)) = anim(|a| (a.surf.w as i32, a.surf.h as i32)) {
                let (x, y) = default_pos(w, h);
                SetWindowPos(hwnd, null_mut(), x, y, 0, 0, SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE);
                save_config(hwnd);
            }
        }
        IDM_QUIT => {
            DestroyWindow(hwnd);
        }
        _ => {}
    }
}

unsafe fn gather_ashes(hwnd: HWND) {
    if let Some(ashes) = ui(|u| u.ashes.clone()) {
        let _ = fs::create_dir_all(&ashes);
        ShellExecuteW(hwnd, w!("open"), wide(&ashes.to_string_lossy()).as_ptr(), null(), null(), SW_SHOWNORMAL);
    }
}

unsafe fn scatter_ashes(hwnd: HWND) {
    let Some(ashes) = ui(|u| u.ashes.clone()) else { return };
    let r = MessageBoxW(
        hwnd,
        w!("Scatter the ashes?\n\nThis permanently deletes every file in the ashes folder."),
        w!("Scatter the ashes"),
        MB_YESNO | MB_ICONWARNING | MB_DEFBUTTON2,
    );
    if r != IDYES {
        return;
    }
    let mut gone = 0usize;
    if let Ok(rd) = fs::read_dir(&ashes) {
        for e in rd.flatten() {
            // plain files only; never recurse, never follow links
            if e.file_type().is_ok_and(|t| t.is_file()) && fs::remove_file(e.path()).is_ok() {
                gone += 1;
            }
        }
    }
    say(&format!("The ashes are scattered ({gone} file{}).\nThe flame forgets, as flames do.", if gone == 1 { "" } else { "s" }));
}

unsafe fn on_key(hwnd: HWND, vk: usize) {
    let ctrl = GetKeyState(VK_CONTROL as i32) < 0;
    let vk = vk as u16;
    if vk == VK_V && ctrl {
        paste(hwnd);
    } else if vk == VK_P || vk == VK_RETURN {
        open_prayer();
    } else if vk == VK_ESCAPE {
        hide_speech();
    }
}

unsafe fn on_frame(hwnd: HWND) {
    anim(|a| {
        let now = Instant::now();
        let dt = now.duration_since(a.last).as_secs_f32();
        a.last = now;
        a.gfx.step(dt);
        a.present(None);
        let busy = a.gfx.busy();
        a.set_fast(busy);
    });
    let _ = hwnd;
}

/// Rebuild everything that depends on the DPI (monitor change, or startup).
unsafe fn rebuild(dpi: u32, pos: Option<(i32, i32)>) {
    let gfx = Gfx::new(dpi as f32 / 96.0, time_seed());
    let (w, h) = gfx.size();
    let Some(surf) = make_surface(w, h) else { return };
    if let Some((speech, old_font)) = ui(|u| {
        let s = u.speech;
        u.speech = null_mut();
        u.dpi = dpi;
        let f = u.font;
        u.font = make_font(dpi);
        (s, f)
    }) {
        if !speech.is_null() {
            DestroyWindow(speech);
        }
        if !old_font.is_null() {
            DeleteObject(old_font);
        }
    }
    anim(|a| {
        a.gfx = gfx;
        a.surf = surf; // the old surface is released here
        a.present(pos);
    });
}

unsafe extern "system" fn shrine_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_TIMER => {
            match wp {
                T_FRAME => on_frame(hwnd),
                T_SPEECH => hide_speech(),
                _ => {}
            }
            0
        }
        WM_LBUTTONDOWN => {
            begin_drag(hwnd);
            0
        }
        WM_MOUSEMOVE => {
            drag_move(hwnd);
            0
        }
        WM_LBUTTONUP => {
            end_drag(hwnd);
            0
        }
        WM_LBUTTONDBLCLK => {
            open_prayer();
            0
        }
        WM_RBUTTONUP => {
            context_menu(hwnd);
            0
        }
        WM_KEYDOWN => {
            if lp & (1 << 30) == 0 {
                on_key(hwnd, wp); // ignore auto-repeat: one Ctrl+V, one offering
            }
            0
        }
        WM_CAPTURECHANGED => {
            ui(|u| u.drag = None);
            0
        }
        WM_DROPFILES => {
            let h = wp as HDROP;
            let paths = drop_paths(h);
            DragFinish(h);
            offer_paths(paths);
            0
        }
        WM_COPYDATA => {
            on_copydata(lp);
            1
        }
        WM_DPICHANGED => {
            let dpi = (wp & 0xffff) as u32;
            let rc = &*(lp as *const RECT);
            rebuild(dpi, Some((rc.left, rc.top)));
            0
        }
        WM_DESTROY => {
            save_config(hwnd);
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ------------------------------------------------------------------ start-up

unsafe fn register_classes(hinst: HINSTANCE, brush: HBRUSH) {
    let arrow = LoadCursorW(null_mut(), IDC_ARROW);
    let mk = |style: u32, proc_: WNDPROC, name: *const u16, bg: HBRUSH| WNDCLASSEXW {
        cbSize: size_of::<WNDCLASSEXW>() as u32,
        style,
        lpfnWndProc: proc_,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: hinst,
        hIcon: null_mut(),
        hCursor: arrow,
        hbrBackground: bg,
        lpszMenuName: null(),
        lpszClassName: name,
        hIconSm: null_mut(),
    };
    let classes = [
        mk(CS_DBLCLKS, Some(shrine_proc), w!("GodOfSmallCodeShrine"), null_mut()),
        mk(0, Some(speech_proc), w!("GodOfSmallCodeSpeech"), null_mut()),
        mk(0, Some(prayer_proc), w!("GodOfSmallCodePrayer"), brush),
    ];
    for c in &classes {
        if RegisterClassExW(c) == 0 {
            fatal("The shrine could not register its window classes.");
        }
    }
}

/// Second launch: hand the files (or just a summons) to the running shrine.
unsafe fn forward_to_running(args: &[PathBuf]) {
    for _ in 0..30 {
        let hwnd = FindWindowW(w!("GodOfSmallCodeShrine"), null());
        if !hwnd.is_null() {
            let mut payload: Vec<u16> = Vec::new();
            for (i, p) in args.iter().enumerate() {
                if i > 0 {
                    payload.push(10);
                }
                payload.extend(path_to_wide(p));
            }
            let cds = COPYDATASTRUCT {
                dwData: COPYDATA_MAGIC,
                cbData: (payload.len() * 2) as u32,
                lpData: if payload.is_empty() { null_mut() } else { payload.as_mut_ptr() as *mut c_void },
            };
            SendMessageW(hwnd, WM_COPYDATA, 0, &cds as *const COPYDATASTRUCT as LPARAM);
            return;
        }
        Sleep(100);
    }
}

unsafe fn run() {
    let args: Vec<PathBuf> = std::env::args_os()
        .skip(1)
        .map(|a| std::path::absolute(&a).unwrap_or_else(|_| PathBuf::from(a)))
        .collect();

    // one shrine per login session; later launches just deliver offerings
    let _mutex = CreateMutexW(null(), 0, w!("Local\\GodOfSmallCodeShrine"));
    if GetLastError() == ERROR_ALREADY_EXISTS {
        forward_to_running(&args);
        return;
    }

    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    SetPriorityClass(GetCurrentProcess(), BELOW_NORMAL_PRIORITY_CLASS);

    let dir = data_dir();
    let ashes = dir.join("ashes");
    let _ = fs::create_dir_all(&ashes);
    let cfg = Config::load(&dir);

    let hinst = GetModuleHandleW(null());
    let brush = CreateSolidBrush(COL_BG);
    register_classes(hinst, brush);

    let dpi = GetDpiForSystem().max(96);
    let gfx = Gfx::new(dpi as f32 / 96.0, time_seed());
    let (w, h) = gfx.size();
    let Some(surf) = make_surface(w, h) else { fatal("The shrine could not conjure its surface.") };
    let (x, y) = match (cfg.x, cfg.y) {
        (Some(x), Some(y)) if on_screen(x, y, w as i32, h as i32) => (x, y),
        _ => default_pos(w as i32, h as i32),
    };

    let ex = WS_EX_LAYERED | WS_EX_TOOLWINDOW | WS_EX_ACCEPTFILES | if cfg.topmost { WS_EX_TOPMOST } else { 0 };
    let hwnd = CreateWindowExW(
        ex,
        w!("GodOfSmallCodeShrine"),
        w!("The God of Small Code"),
        WS_POPUP,
        x,
        y,
        w as i32,
        h as i32,
        null_mut(),
        null_mut(),
        hinst,
        null(),
    );
    if hwnd.is_null() {
        fatal("The shrine could not be raised.");
    }

    UI.with(|c| {
        *c.borrow_mut() = Some(Ui {
            hwnd,
            dpi,
            cfg,
            ashes,
            data_dir: dir,
            font: make_font(dpi),
            brush,
            prayer: null_mut(),
            prayer_in: null_mut(),
            prayer_log: null_mut(),
            speech: null_mut(),
            transcript: String::new(),
            drag: None,
        });
    });
    ANIM.with(|c| {
        *c.borrow_mut() = Some(Anim { hwnd, gfx, surf, last: Instant::now(), fast: false });
    });

    // the monitor the window landed on may not use the system DPI
    let real = GetDpiForWindow(hwnd);
    if real != 0 && real != dpi {
        rebuild(real, Some((x, y)));
    } else {
        anim(|a| a.present(Some((x, y))));
    }
    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    SetTimer(hwnd, T_FRAME, FRAME_MS_IDLE, None);

    if !args.is_empty() {
        offer_paths(args);
    }

    let mut msg: MSG = zeroed();
    while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
        // Enter prays, Esc closes: handled here so the prayer window needs no dialog machinery
        if msg.message == WM_KEYDOWN && (msg.wParam == VK_RETURN as usize || msg.wParam == VK_ESCAPE as usize) {
            if let Some((prayer, input)) = ui(|u| (u.prayer, u.prayer_in)) {
                if !prayer.is_null() && (msg.hwnd == prayer || GetParent(msg.hwnd) == prayer) {
                    if msg.wParam == VK_RETURN as usize {
                        if msg.hwnd == input {
                            PostMessageW(prayer, WM_COMMAND, ID_PRAY_BTN as usize, 0);
                        }
                    } else {
                        PostMessageW(prayer, WM_CLOSE, 0, 0);
                    }
                    continue;
                }
            }
        }
        TranslateMessage(&msg);
        DispatchMessageW(&msg);
    }
}

fn main() {
    // no console in release: leave a note if the god ever stumbles
    std::panic::set_hook(Box::new(|info| {
        let dir = data_dir();
        let _ = fs::create_dir_all(&dir);
        let _ = fs::write(dir.join("crash.log"), format!("{info}\n"));
    }));
    unsafe { run() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("gosc-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn consumes_a_file_into_the_ashes() {
        let d = scratch("ok");
        let ashes = d.join("ashes");
        let f = d.join("deploy.sh");
        fs::write(&f, "echo hi\n").unwrap();
        assert_eq!(consume_file(&f, &ashes).ok(), Some(8));
        assert!(!f.exists(), "the offering leaves its old home");
        assert_eq!(fs::read_to_string(ashes.join("deploy.sh")).unwrap(), "echo hi\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn same_name_twice_never_overwrites() {
        let d = scratch("dup");
        let ashes = d.join("ashes");
        for body in ["one", "two", "three"] {
            let f = d.join("a.txt");
            fs::write(&f, body).unwrap();
            consume_file(&f, &ashes).unwrap();
        }
        assert_eq!(fs::read_to_string(ashes.join("a.txt")).unwrap(), "one");
        assert_eq!(fs::read_to_string(ashes.join("a (2).txt")).unwrap(), "two");
        assert_eq!(fs::read_to_string(ashes.join("a (3).txt")).unwrap(), "three");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn refusals_leave_the_original_alone() {
        let d = scratch("refuse");
        let ashes = d.join("ashes");
        fs::create_dir_all(&ashes).unwrap();

        let big = d.join("big.bin");
        fs::write(&big, vec![0u8; MAX_OFFERING as usize + 1]).unwrap();
        assert!(matches!(consume_file(&big, &ashes), Err(Refusal::TooLarge)));
        assert!(big.exists());

        let dir = d.join("folder");
        fs::create_dir_all(&dir).unwrap();
        assert!(matches!(consume_file(&dir, &ashes), Err(Refusal::Folder)));
        assert!(dir.exists());

        assert!(matches!(consume_file(&d.join("nope.txt"), &ashes), Err(Refusal::Vanished)));

        let ash = ashes.join("already.txt");
        fs::write(&ash, "x").unwrap();
        assert!(matches!(consume_file(&ash, &ashes), Err(Refusal::AlreadyAshes)));
        assert!(ash.exists());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn exactly_one_mib_is_accepted() {
        let d = scratch("edge");
        let ashes = d.join("ashes");
        let f = d.join("edge.txt");
        fs::write(&f, vec![b'x'; MAX_OFFERING as usize]).unwrap();
        assert!(consume_file(&f, &ashes).is_ok());
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn config_round_trips_and_survives_garbage() {
        let d = scratch("cfg");
        let c = Config { x: Some(-1200), y: Some(40), topmost: false };
        c.save(&d);
        let r = Config::load(&d);
        assert_eq!((r.x, r.y, r.topmost), (Some(-1200), Some(40), false));
        fs::write(d.join("shrine.ini"), "x=banana\ny\n\u{0}\ntopmost=1\n").unwrap();
        let r = Config::load(&d);
        assert_eq!((r.x, r.y, r.topmost), (None, None, true));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn sizes_and_quips() {
        assert_eq!(human(212), "212 B");
        assert_eq!(human(1536), "1.5 KiB");
        assert_eq!(quip("x.PS1"), "PowerShell: verbose, yet flammable.");
        assert_eq!(quip("Makefile"), "The flame accepts this.");
    }
}
