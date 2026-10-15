//! GUI + monitoring loop
//!
//! `Engine` (check/alert/heartbeat) runs on its own worker thread, not tied to egui's `update()`,
//! because once the window is hidden to the tray Windows sends no WM_PAINT to the invisible window,
//! so eframe never calls `update()` — logic living there would stop all monitoring while hidden.
//! For the same reason the tray menu is handled in the tray's own event handler, calling Win32 directly.
use crate::{
    boss,
    notify::{self, Alert, Kind},
    windows::boss_watch,
    Config, GameProfile, Identity, Snapshot, GAMES,
};
use eframe::egui::{
    self, Align, Color32, CornerRadius, FontFamily, FontId, Frame, Layout, Margin, RichText,
    Stroke, TextStyle, Ui,
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    fs,
    io::Write,
    net::{SocketAddr, TcpStream},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver},
        Arc, Mutex, MutexGuard,
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tray_icon::{
    menu::{Menu, MenuEvent, MenuItem},
    Icon, MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent,
};
use windows::core::w;

mod hub;
mod onboarding;

/// One theme's palette; values match the preview page in the "ROOC Monitor Themes" artifact
/// green/red/amber = status (ok/dropped/warning) and must keep that meaning in every theme; blue = accent
struct Palette {
    name: &'static str,
    dark: bool,
    bg: Color32,
    panel: Color32,
    card: Color32,
    card_hover: Color32,
    border: Color32,
    text: Color32,
    muted: Color32,
    green: Color32,
    on_green: Color32,
    red: Color32,
    amber: Color32,
    blue: Color32,
    grey: Color32,
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

#[rustfmt::skip]
static THEMES: [Palette; 6] = [
    Palette { name: "Midnight", dark: true, bg: rgb(0x0f1115), panel: rgb(0x15181e), card: rgb(0x1c2028), card_hover: rgb(0x262b35), border: rgb(0x2a2f3a), text: rgb(0xe6e8ee), muted: rgb(0x8b93a7), green: rgb(0x3fd68c), on_green: rgb(0x0f1115), red: rgb(0xff6b6b), amber: rgb(0xffc65c), blue: rgb(0x7aa2f7), grey: rgb(0x6b7280) },
    Palette { name: "Night Watch", dark: true, bg: rgb(0x000000), panel: rgb(0x0a0a0b), card: rgb(0x121315), card_hover: rgb(0x1a1c1f), border: rgb(0x1f2125), text: rgb(0xc9ccd1), muted: rgb(0x6f757e), green: rgb(0x2fbf71), on_green: rgb(0x000000), red: rgb(0xe5484d), amber: rgb(0xd9a441), blue: rgb(0x5b8def), grey: rgb(0x55595f) },
    Palette { name: "Poring", dark: true, bg: rgb(0x1a1420), panel: rgb(0x221a29), card: rgb(0x2b2133), card_hover: rgb(0x35293f), border: rgb(0x3e3048), text: rgb(0xf3e8f0), muted: rgb(0xa896ad), green: rgb(0x5fd4a0), on_green: rgb(0x1a1420), red: rgb(0xff6b8a), amber: rgb(0xffc670), blue: rgb(0xff9ec4), grey: rgb(0x7d6d82) },
    Palette { name: "Frost", dark: true, bg: rgb(0x2e3440), panel: rgb(0x343b48), card: rgb(0x3b4252), card_hover: rgb(0x434c5e), border: rgb(0x4c566a), text: rgb(0xeceff4), muted: rgb(0xa3adbf), green: rgb(0xa3be8c), on_green: rgb(0x2e3440), red: rgb(0xe07a82), amber: rgb(0xebcb8b), blue: rgb(0x88c0d0), grey: rgb(0x7b8598) },
    Palette { name: "Mocha", dark: true, bg: rgb(0x1e1e2e), panel: rgb(0x181825), card: rgb(0x2a2b3d), card_hover: rgb(0x313244), border: rgb(0x45475a), text: rgb(0xcdd6f4), muted: rgb(0xa6adc8), green: rgb(0xa6e3a1), on_green: rgb(0x1e1e2e), red: rgb(0xf38ba8), amber: rgb(0xf9e2af), blue: rgb(0x89b4fa), grey: rgb(0x7f849c) },
    Palette { name: "Daylight", dark: false, bg: rgb(0xf4f6f9), panel: rgb(0xffffff), card: rgb(0xffffff), card_hover: rgb(0xe9edf3), border: rgb(0xdde2ea), text: rgb(0x1c2230), muted: rgb(0x667085), green: rgb(0x12a150), on_green: rgb(0xffffff), red: rgb(0xd93f3f), amber: rgb(0xb7791f), blue: rgb(0x2f6fde), grey: rgb(0x8a93a3) },
];

/// Theme currently shown; changed only from the UI thread via `set_theme`
static CURRENT_THEME: AtomicUsize = AtomicUsize::new(0);

fn p() -> &'static Palette {
    &THEMES[CURRENT_THEME.load(Ordering::Relaxed).min(THEMES.len() - 1)]
}

fn theme_index(name: &str) -> usize {
    THEMES.iter().position(|t| t.name == name).unwrap_or(0)
}

const MAX_EVENTS: usize = 300;
/// FILETIME of 1970-01-01 (100 ns intervals since 1601)
const UNIX_EPOCH_FILETIME: i64 = 116_444_736_000_000_000;
const SHOW_EVENT_NAME: windows::core::PCWSTR = w!("Local\\GamepulseShow");
/// MVP timers, next to config.json, so a restart keeps counting down
const BOSSES_FILE: &str = "bosses.json";

fn root_path(name: &str) -> PathBuf {
    if let Ok(executable) = std::env::current_exe() {
        for directory in executable.ancestors().skip(1) {
            if directory.join("config.json").exists() {
                return directory.join(name);
            }
        }
        if let Some(directory) = executable.parent() {
            return directory.join(name);
        }
    }
    std::env::current_dir().unwrap_or_default().join(name)
}

fn load_config() -> Config {
    fs::read_to_string(root_path("config.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_config(config: &Config) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(config).map_err(|e| e.to_string())?;
    let path = root_path("config.json");
    fs::write(path, bytes).map_err(|e| e.to_string())
}

fn lock(engine: &Mutex<Engine>) -> MutexGuard<'_, Engine> {
    engine
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Local time "YYYY-MM-DD HH:MM:SS"
fn local_stamp() -> String {
    let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        t.wYear, t.wMonth, t.wDay, t.wHour, t.wMinute, t.wSecond
    )
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Local "HH:MM" of a unix time, using the current offset between local time and UTC
fn clock(unix: u64) -> String {
    use windows::Win32::System::SystemInformation::{GetLocalTime, GetSystemTime};
    let (local, utc) = unsafe { (GetLocalTime(), GetSystemTime()) };
    let minutes =
        |t: windows::Win32::Foundation::SYSTEMTIME| i64::from(t.wHour) * 60 + i64::from(t.wMinute);
    // Local and UTC may be on different days (or months: 1st vs 31st)
    let days = match i32::from(local.wDay) - i32::from(utc.wDay) {
        0 => 0,
        1 | i32::MIN..=-2 => 1,
        _ => -1,
    };
    let offset = minutes(local) - minutes(utc) + days * 24 * 60;
    let day_minute = (unix as i64 / 60 + offset).rem_euclid(24 * 60);
    format!("{:02}:{:02}", day_minute / 60, day_minute % 60)
}

fn now_filetime() -> i64 {
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    UNIX_EPOCH_FILETIME + (unix.as_nanos() / 100) as i64
}

fn format_duration(seconds: u64) -> String {
    let (h, m, s) = (seconds / 3600, seconds / 60 % 60, seconds % 60);
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Level {
    Info,
    Good,
    Warn,
    Bad,
}

impl Level {
    fn color(self) -> Color32 {
        match self {
            Self::Info => p().muted,
            Self::Good => p().green,
            Self::Warn => p().amber,
            Self::Bad => p().red,
        }
    }
    fn of_kind(kind: Kind) -> Self {
        match kind {
            Kind::Down => Self::Bad,
            Kind::Up => Self::Good,
            Kind::Warn => Self::Warn,
            Kind::Info => Self::Info,
        }
    }
    /// Result of notify::send as text, e.g. "discord: delivered", "ntfy: HTTP 429"
    fn of_result(text: &str) -> Self {
        if text.ends_with(": delivered") {
            Self::Good
        } else if text.contains("captured") || text.ends_with(": played") || text.contains("saved")
        {
            Self::Info
        } else {
            Self::Warn
        }
    }
}

struct Event {
    seq: u64,
    stamp: String,
    text: String,
    level: Level,
}

struct ClientState {
    game: &'static GameProfile,
    misses: u32,
    alerted: bool,
    repeats: u32,
    last_alert: Option<Instant>,
    session: Option<i64>,
    down_since: Option<Instant>,
}

#[derive(Default)]
struct AlertState {
    active: bool,
    repeats: u32,
    last_alert: Option<Instant>,
}

/// A finished "Pin on top" request: client, requested state, outcome
type PinResult = (Identity, bool, Result<(), String>);

/// All monitoring state; `tick()` is called by the worker thread every 250 ms whether the window is open or hidden
struct Engine {
    config: Config,
    snapshot: Option<Snapshot>,
    receiver: Option<Receiver<(Snapshot, bool)>>,
    watching: bool,
    last_check: Option<Instant>,
    last_summary: String,
    events: VecDeque<Event>,
    clients: HashMap<Identity, ClientState>,
    /// "Game closed" per game (key = `GameProfile::id`); one game closing doesn't affect the others
    no_process: HashMap<&'static str, AlertState>,
    last_heartbeat: Instant,
    jobs: Vec<Receiver<Vec<String>>>,
    pins: Vec<Receiver<PinResult>>,
    /// Open picture-in-picture windows (HWND as usize), one per client at most
    pips: HashMap<Identity, usize>,
    internet_up: bool,
    internet_misses: u32,
    internet_alert: AlertState,
    drill: Option<Drill>,
    next_seq: u64,
    /// MVP tracker: the game it runs for (first in `GAMES` with an MVP list), its timers,
    /// and the capture/OCR thread while it is on
    boss_game: Option<&'static GameProfile>,
    bosses: boss::Tracker,
    boss_watch: Option<boss_watch::Watcher>,
    /// None = reading the game window; Some = why not (or "Starting...")
    boss_problem: Option<String>,
    boss_channel_warned: bool,
    /// Open MVP route window (HWND as usize) and what it shows
    route: Option<(usize, crate::windows::route::Shared)>,
}

/// Disconnect drill: fires the same burst as a real drop DRILL_ROUNDS times, RepeatAlertMin apart.
/// The Test alert button only proves "delivered"; the drill proves it actually wakes you at 3 a.m.
struct Drill {
    sent: u32,
    next_at: Instant,
    config: Config,
    cancel: Arc<AtomicBool>,
    game: &'static GameProfile,
    pid: u32,
}

const DRILL_ROUNDS: u32 = 5;

impl Engine {
    fn new() -> Self {
        let config = load_config();
        let watching = config.auto_start && !notify::channels(&config).is_empty();
        let mut engine = Self {
            config,
            snapshot: None,
            receiver: None,
            watching,
            last_check: None,
            last_summary: String::new(),
            events: VecDeque::new(),
            clients: HashMap::new(),
            no_process: HashMap::new(),
            last_heartbeat: Instant::now(),
            jobs: Vec::new(),
            pins: Vec::new(),
            pips: HashMap::new(),
            internet_up: true,
            internet_misses: 0,
            internet_alert: AlertState::default(),
            drill: None,
            next_seq: 0,
            boss_game: GAMES.iter().find(|g| !g.mvps.is_empty()),
            bosses: boss::Tracker::new(&[], Default::default()),
            boss_watch: None,
            boss_problem: None,
            boss_channel_warned: false,
            route: None,
        };
        engine.push(Level::Info, "Application started".into());
        if let Some(game) = engine.boss_game {
            let timers = fs::read_to_string(root_path(BOSSES_FILE))
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
            engine.bosses = boss::Tracker::new(game.mvps, timers);
            if engine.config.boss_tracker {
                engine.start_boss_watch();
            }
        }
        if engine.config.auto_start && !engine.watching {
            engine.push(
                Level::Warn,
                "Not watching: configure at least one notification channel".into(),
            );
        }
        if engine.config.watched().is_empty() {
            engine.push(
                Level::Warn,
                "No game has alerts on: turn on a game card in the library".into(),
            );
        }
        engine.request_snapshot();
        engine
    }

    /// Write every line to the log immediately, not relying on UI queue indexes, which drop the oldest entries when too long
    fn write_log(stamp: &str, text: &str) {
        if let Ok(mut file) = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(root_path("gamepulse.log"))
        {
            let _ = writeln!(file, "{stamp}  {text}");
        }
    }

    fn push(&mut self, level: Level, text: String) {
        let stamp = local_stamp();
        Self::write_log(&stamp, &text);
        self.next_seq += 1;
        self.events.push_back(Event {
            seq: self.next_seq,
            stamp,
            text,
            level,
        });
        while self.events.len() > MAX_EVENTS {
            self.events.pop_front();
        }
    }

    fn request_snapshot(&mut self) {
        if self.receiver.is_some() {
            return;
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let address: SocketAddr = "1.1.1.1:443".parse().expect("static socket address");
            let internet = TcpStream::connect_timeout(&address, Duration::from_secs(3)).is_ok();
            // Always check every game (same cost), so cards show real status even for games with alerts off
            let _ = sender.send((crate::windows::snapshot(GAMES), internet));
        });
        self.receiver = Some(receiver);
    }

    /// Turn a game's alerts on/off from the switch on its card
    fn set_watched(&mut self, game: &'static GameProfile, on: bool) {
        self.config.set_watched(game.id, on);
        if let Err(error) = save_config(&self.config) {
            self.push(Level::Warn, format!("Cannot save settings: {error}"));
        }
        self.prune_unwatched();
        if on {
            self.push(Level::Good, format!("Alerts on for {}", game.name));
        } else {
            self.push(Level::Warn, format!("Alerts off for {}", game.name));
        }
    }

    /// Drop state of games whose alerts were turned off; turning them back on starts grace counting again
    fn prune_unwatched(&mut self) {
        let config = &self.config;
        self.clients
            .retain(|_, state| config.watches(state.game.id));
        self.no_process.retain(|id, _| config.watches(id));
    }

    /// Check summary text (logged every round, shown in Events when it changes)
    fn check_summary(&self, snapshot: &Snapshot) -> (Level, String) {
        if !snapshot.errors.is_empty() {
            return (
                Level::Warn,
                format!("Check incomplete: {}", snapshot.errors.join("; ")),
            );
        }
        let mut level = Level::Info;
        let mut parts = Vec::new();
        for game in GAMES {
            let watched = self.config.watches(game.id);
            let (online, total) = snapshot.counts(game.id);
            if !watched && total == 0 {
                continue;
            }
            if watched && (total == 0 || online < total) {
                level = Level::Warn;
            }
            parts.push(if total == 0 {
                format!("{} not running", game.name)
            } else {
                format!("{} {online}/{total} online", game.name)
            });
        }
        if parts.is_empty() {
            (Level::Info, "Checked: no game running".into())
        } else {
            (level, format!("Checked: {}", parts.join(", ")))
        }
    }

    fn tick(&mut self) {
        self.step_drill();
        self.tick_bosses();
        self.update_route();
        for index in (0..self.jobs.len()).rev() {
            if let Ok(results) = self.jobs[index].try_recv() {
                for result in results {
                    self.push(Level::of_result(&result), result);
                }
                self.jobs.remove(index);
            }
        }
        for index in (0..self.pins.len()).rev() {
            if let Ok((identity, on, result)) = self.pins[index].try_recv() {
                self.pins.remove(index);
                self.finish_pin(identity, on, result);
            }
        }
        // Closed from the PiP itself (menu, double-click) or because the game window went away
        self.pips
            .retain(|_, hwnd| crate::windows::pip::is_open(*hwnd));
        let received = self.receiver.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some((snapshot, internet)) = received {
            self.internet_up = internet;
            let (level, summary) = self.check_summary(&snapshot);
            // Log every round (proves monitoring is still running), but Events only shows changes, otherwise it fills up every 30 s
            if summary == self.last_summary {
                Self::write_log(&local_stamp(), &summary);
            } else {
                self.last_summary = summary.clone();
                self.push(level, summary);
            }
            if self.watching && snapshot.errors.is_empty() {
                self.process_alerts(&snapshot, internet);
            }
            self.snapshot = Some(snapshot);
            self.last_check = Some(Instant::now());
            self.receiver = None;
        }
        if self.watching
            && self.receiver.is_none()
            && self.last_check.is_none_or(|last| {
                last.elapsed() >= Duration::from_secs(self.config.interval_sec.max(1))
            })
        {
            self.request_snapshot();
        }
    }

    fn start(&mut self) -> bool {
        if notify::channels(&self.config).is_empty() {
            self.push(
                Level::Warn,
                "Cannot start: configure at least one notification channel".into(),
            );
            return false;
        }
        self.watching = true;
        self.clients.clear();
        self.no_process.clear();
        self.internet_alert = AlertState::default();
        self.internet_misses = 0;
        self.last_heartbeat = Instant::now();
        self.last_check = None;
        self.request_snapshot();
        self.push(Level::Good, "Started watching".into());
        true
    }

    fn stop(&mut self) {
        self.watching = false;
        self.push(Level::Warn, "Stopped watching".into());
    }

    fn apply_config(&mut self, config: Config) -> Result<(), String> {
        save_config(&config)?;
        self.config = config;
        self.prune_unwatched();
        self.push(Level::Good, "Settings saved".into());
        Ok(())
    }

    fn spawn_job(&mut self, job: impl FnOnce() -> Vec<String> + Send + 'static) {
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let _ = sender.send(job());
        });
        self.jobs.push(receiver);
    }

    /// Pin a client's game window on top of other windows (or unpin it). Runs on its own
    /// thread: SetWindowPos waits on the game's message loop, which may be hung
    fn set_pinned(&mut self, identity: Identity, on: bool) {
        let Some(client) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.clients.iter().find(|c| c.identity == identity))
        else {
            return;
        };
        let hwnds: Vec<usize> = client.windows.iter().map(|w| w.hwnd).collect();
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let result = crate::windows::set_topmost(identity.pid, &hwnds, on);
            let _ = sender.send((identity, on, result));
        });
        self.pins.push(receiver);
    }

    /// Open or close a picture-in-picture view of a client's game window
    fn set_pip(&mut self, identity: Identity, on: bool) {
        if !on {
            if let Some(hwnd) = self.pips.remove(&identity) {
                crate::windows::pip::close(hwnd);
            }
            return;
        }
        let Some(client) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.clients.iter().find(|c| c.identity == identity))
        else {
            return;
        };
        let name = crate::find_game(client.game).map_or("Game", |g| g.name);
        let Some(window) = client
            .windows
            .iter()
            .find(|w| !w.minimized)
            .or(client.windows.first())
        else {
            return;
        };
        let title = format!("{name} (PID {})", identity.pid);
        match crate::windows::pip::open(identity.pid, window.hwnd, &title, self.pips.len()) {
            Ok(hwnd) => {
                self.pips.insert(identity, hwnd);
                self.push(Level::Info, format!("Picture-in-picture: {title}"));
            }
            Err(error) => self.push(
                Level::Warn,
                format!("Cannot open picture-in-picture for {title}: {error}"),
            ),
        }
    }

    /// Show the new pin state right away instead of waiting for the next check
    /// (which doesn't come at all while paused)
    fn finish_pin(&mut self, identity: Identity, on: bool, result: Result<(), String>) {
        let client = self
            .snapshot
            .as_mut()
            .and_then(|s| s.clients.iter_mut().find(|c| c.identity == identity));
        let name = client
            .as_ref()
            .and_then(|c| crate::find_game(c.game))
            .map_or("Game", |g| g.name);
        match result {
            Ok(()) => {
                if let Some(client) = client {
                    for window in &mut client.windows {
                        window.topmost = on;
                    }
                }
                let verb = if on { "Pinned on top" } else { "Unpinned" };
                self.push(
                    Level::Info,
                    format!("{verb}: {name} (PID {})", identity.pid),
                );
            }
            Err(error) => {
                let verb = if on { "pin" } else { "unpin" };
                self.push(
                    Level::Warn,
                    format!("Cannot {verb} {name} (PID {}): {error}", identity.pid),
                );
            }
        }
    }

    // ------------------------------------------------ MVP tracker

    fn start_boss_watch(&mut self) {
        if let Some(game) = self.boss_game {
            self.boss_watch = Some(boss_watch::start(game));
            self.boss_problem = Some("Starting...".into());
        }
    }

    /// Turn the MVP tracker on/off (saved). Off drops the capture thread, so it costs no CPU
    fn set_boss_tracker(&mut self, on: bool) {
        self.config.boss_tracker = on;
        if let Err(error) = save_config(&self.config) {
            self.push(Level::Warn, format!("Cannot save settings: {error}"));
        }
        if on {
            self.start_boss_watch();
            self.push(Level::Good, "MVP tracker on".into());
            if notify::boss_channels(&self.config).is_empty() {
                self.push(
                    Level::Warn,
                    "MVP alerts have nowhere to go: turn on Discord or Windows notifications in Settings › MVP tracker".into(),
                );
            }
        } else {
            self.boss_watch = None;
            self.boss_problem = None;
            self.push(Level::Info, "MVP tracker off".into());
        }
    }

    fn set_boss_tracked(&mut self, name: &str, on: bool) {
        self.config.set_boss_tracked(name, on);
        if let Err(error) = save_config(&self.config) {
            self.push(Level::Warn, format!("Cannot save settings: {error}"));
        }
    }

    /// Forget every MVP timer (e.g. after Gamepulse was closed for a while): all back to "not seen"
    fn clear_boss_timers(&mut self) {
        self.bosses.timers.clear();
        self.save_bosses();
        self.push(
            Level::Info,
            "MVP timers cleared: every MVP is back to not seen yet".into(),
        );
    }

    fn set_all_bosses_tracked(&mut self, on: bool) {
        self.config.tracked_bosses = if on {
            self.bosses
                .mvps()
                .iter()
                .map(|m| m.name.to_string())
                .collect()
        } else {
            Vec::new()
        };
        if let Err(error) = save_config(&self.config) {
            self.push(Level::Warn, format!("Cannot save settings: {error}"));
        }
    }

    fn save_bosses(&mut self) {
        let result = serde_json::to_vec_pretty(&self.bosses.timers)
            .map_err(|e| e.to_string())
            .and_then(|bytes| fs::write(root_path(BOSSES_FILE), bytes).map_err(|e| e.to_string()));
        if let Err(error) = result {
            self.push(Level::Warn, format!("Cannot save MVP timers: {error}"));
        }
    }

    /// Readings from the capture thread → timers → notices; also the time-based notices
    fn tick_bosses(&mut self) {
        let (Some(game), Some(watch)) = (self.boss_game, &self.boss_watch) else {
            return;
        };
        let readings: Vec<_> = watch.readings.try_iter().collect();
        let now = unix_now();
        let lead = self.config.boss_lead_min * 60;
        let before = self.bosses.timers.clone();
        let mut notices = Vec::new();
        for reading in readings {
            match reading {
                boss_watch::Reading::Problem(problem) => {
                    if problem != self.boss_problem {
                        match &problem {
                            None => self
                                .push(Level::Good, "MVP tracker: reading the game window".into()),
                            Some(problem) => self.push(
                                Level::Warn,
                                format!("MVP tracker cannot read the game: {problem}"),
                            ),
                        }
                    }
                    self.boss_problem = problem;
                }
                boss_watch::Reading::Dimmed(gain) => self.push(
                    Level::Info,
                    match gain {
                        Some(gain) => format!(
                            "MVP tracker: the game is in power-saving mode; brightening it ×{gain:.1} to read"
                        ),
                        None => "MVP tracker: the game left power-saving mode".into(),
                    },
                ),
                boss_watch::Reading::Text { full, green } => {
                    // Every reading goes to the log file only, for checking OCR later
                    Self::write_log(&local_stamp(), &format!("MVP bar: {full} | green: {green}"));
                    if let Some((mvp, sighting)) = boss::parse(game.mvps, &full, &green) {
                        notices.extend(self.bosses.observe(mvp, sighting, now, lead));
                    }
                }
            }
        }
        notices.extend(self.bosses.tick(now, lead));
        for notice in notices {
            self.boss_notice(game, notice, now);
        }
        if self.bosses.timers != before {
            self.save_bosses();
        }
    }

    /// Log every notice; alert only for tracked MVPs and the alert kinds turned on in Settings
    fn boss_notice(&mut self, game: &'static GameProfile, notice: boss::Notice, now: u64) {
        let map_of = |name: &str| {
            game.mvps
                .iter()
                .find(|m| m.name == name)
                .map_or("", |m| m.map)
        };
        let config = &self.config;
        let (name, wanted, title, message, kind) = match notice {
            boss::Notice::Died { mvp, spawn_at } => (
                mvp,
                config.boss_alert_death,
                format!("{mvp} died"),
                format!("Respawns around {} · {}", clock(spawn_at), map_of(mvp)),
                Kind::Info,
            ),
            boss::Notice::Soon { mvp, spawn_at } => (
                mvp,
                config.boss_lead_min > 0,
                format!(
                    "{mvp} spawns in {} min",
                    spawn_at.saturating_sub(now).div_ceil(60)
                ),
                format!("{} · around {}", map_of(mvp), clock(spawn_at)),
                Kind::Warn,
            ),
            boss::Notice::Spawned { mvp, announced } => (
                mvp,
                config.boss_alert_spawn,
                if announced {
                    format!("{mvp} has spawned")
                } else {
                    format!("{mvp} should be up now")
                },
                if announced {
                    map_of(mvp).to_string()
                } else {
                    format!(
                        "{} · predicted; the game's announcement was not seen",
                        map_of(mvp)
                    )
                },
                Kind::Up,
            ),
            boss::Notice::LowHp { mvp } => (
                mvp,
                config.boss_alert_low_hp,
                format!("{mvp} is at 30% HP"),
                map_of(mvp).to_string(),
                Kind::Warn,
            ),
            boss::Notice::PresumedDead { mvp, spawn_at } => (
                mvp,
                config.boss_alert_death,
                format!("{mvp} probably died"),
                format!(
                    "Its death announcement was not seen 5 min after 30% HP · respawns around {} (estimated) · {}",
                    clock(spawn_at),
                    map_of(mvp)
                ),
                Kind::Info,
            ),
        };
        self.push(Level::of_kind(kind), format!("MVP: {title} · {message}"));
        if wanted && self.config.tracks_boss(name) {
            let alert = Alert {
                title: titled(game, &title),
                message,
                kind,
                urgent: false,
                fields: vec![],
                desktop_bmp: None,
            };
            self.send_boss_alert(self.config.clone(), alert);
        }
    }

    fn send_boss_alert(&mut self, config: Config, alert: Alert) {
        if notify::boss_channels(&config).is_empty() {
            if !self.boss_channel_warned {
                self.boss_channel_warned = true;
                self.push(
                    Level::Warn,
                    "MVP alert not sent: turn on Discord or Windows notifications in Settings › MVP tracker".into(),
                );
            }
            return;
        }
        self.spawn_job(move || notify::send_boss(config, alert));
    }

    /// Settings button: uses the unsaved draft, like the other test buttons
    fn test_boss_alert(&mut self, config: Config) {
        let channels = notify::boss_channels(&config);
        if !channels.is_empty() {
            self.push(
                Level::Info,
                format!("Sending a test MVP alert via {}", channels.join(", ")),
            );
        }
        let game = self.boss_game.unwrap_or(&GAMES[0]);
        self.boss_channel_warned = false;
        self.send_boss_alert(
            config,
            Alert {
                title: titled(game, "Test MVP alert"),
                message: "MVP spawn alerts will look like this.".into(),
                kind: Kind::Info,
                urgent: false,
                fields: vec![],
                desktop_bmp: None,
            },
        );
    }

    /// Header pill of the MVP tracker
    fn boss_status(&self) -> (String, Color32) {
        match (&self.boss_watch, &self.boss_problem) {
            (None, _) => ("OFF".into(), p().grey),
            (Some(_), None) => ("READING".into(), p().green),
            (Some(_), Some(problem)) if problem == "Starting..." => ("STARTING".into(), p().blue),
            (Some(_), Some(_)) => ("CAN'T READ".into(), p().amber),
        }
    }

    /// Status text and color of one MVP. Colors: green = up, amber = up at 30% HP,
    /// blue = spawns within the warning time, red = dead (counting down), grey = unknown
    fn boss_state(&self, mvp: &boss::Mvp, now: u64) -> (String, Color32) {
        let soon = (self.config.boss_lead_min * 60).max(boss::WARNING_SECS);
        match self.bosses.status(mvp) {
            boss::Status::Waiting { spawn_at } if spawn_at > now => (
                format!(
                    "Spawns {} · in {}",
                    clock(spawn_at),
                    format_duration(spawn_at - now)
                ),
                if spawn_at - now <= soon {
                    p().blue
                } else {
                    p().red
                },
            ),
            // Only while the tracker is off: the predicted time passed without a reading
            boss::Status::Waiting { spawn_at } => {
                (format!("Due since {}", clock(spawn_at)), p().green)
            }
            boss::Status::Up {
                since,
                low_hp_since: Some(low),
            } => (
                format!(
                    "30% HP for {} · up since {}",
                    format_duration(now.saturating_sub(low)),
                    clock(since)
                ),
                p().amber,
            ),
            boss::Status::Up { since, .. } => (format!("Up since {}", clock(since)), p().green),
            boss::Status::Unknown => ("Not seen yet".into(), p().muted),
        }
    }

    /// One row per MVP, in the game's list order
    fn boss_rows(&self) -> Vec<BossRow> {
        let now = unix_now();
        self.bosses
            .mvps()
            .iter()
            .map(|mvp| {
                let (status, color) = self.boss_state(mvp, now);
                BossRow {
                    name: mvp.name,
                    map: mvp.map,
                    tracked: self.config.tracks_boss(mvp.name),
                    status,
                    color,
                }
            })
            .collect()
    }

    /// Where to go next: MVPs at 30% HP first (most recent first: they die within minutes, so
    /// the freshest one is the likeliest still alive), then the ones that are up (longest up
    /// first), then the rest by spawn time. MVPs never seen since the tracker started are left out
    fn route_lines(&self) -> Vec<crate::windows::route::RouteLine> {
        let now = unix_now();
        let mut lines: Vec<((u8, u64), crate::windows::route::RouteLine)> = self
            .bosses
            .mvps()
            .iter()
            .filter(|mvp| self.config.boss_route_all || self.config.tracks_boss(mvp.name))
            .filter_map(|mvp| {
                let (order, when) = match self.bosses.status(mvp) {
                    boss::Status::Unknown => return None,
                    boss::Status::Up {
                        low_hp_since: Some(low),
                        ..
                    } => (
                        (0, u64::MAX - low),
                        format!("30% HP · {}", format_duration(now.saturating_sub(low))),
                    ),
                    boss::Status::Up { since, .. } => (
                        (1, since),
                        format!("UP · {}", format_duration(now.saturating_sub(since))),
                    ),
                    boss::Status::Waiting { spawn_at } if spawn_at <= now => {
                        ((1, spawn_at), "due".to_string())
                    }
                    boss::Status::Waiting { spawn_at } => (
                        (2, spawn_at),
                        format!(
                            "{} · in {}",
                            clock(spawn_at),
                            format_duration(spawn_at - now)
                        ),
                    ),
                };
                let (_, color) = self.boss_state(mvp, now);
                Some((
                    order,
                    crate::windows::route::RouteLine {
                        name: mvp.name.to_string(),
                        map: mvp.map.to_string(),
                        when,
                        color: hex(color),
                    },
                ))
            })
            .collect();
        lines.sort_by_key(|(order, _)| *order);
        lines.into_iter().map(|(_, line)| line).collect()
    }

    fn route_open(&self) -> bool {
        self.route.is_some()
    }

    /// Open or close the floating MVP route window
    fn toggle_route(&mut self) {
        if let Some((hwnd, _)) = self.route.take() {
            crate::windows::route::close(hwnd);
            return;
        }
        let shared = Arc::new(Mutex::new(crate::windows::route::RouteView {
            show_all: self.config.boss_route_all,
            ..Default::default()
        }));
        self.route = Some((0, shared.clone()));
        self.update_route();
        match crate::windows::route::open(shared.clone()) {
            Ok(hwnd) => self.route = Some((hwnd, shared)),
            Err(error) => {
                self.route = None;
                self.push(
                    Level::Warn,
                    format!("Cannot open the MVP route window: {error}"),
                );
            }
        }
    }

    /// Keep the route window's lines current; save its "all / ticked" choice
    fn update_route(&mut self) {
        let Some((hwnd, shared)) = self.route.clone() else {
            return;
        };
        if hwnd != 0 && !crate::windows::route::is_open(hwnd) {
            self.route = None;
            return;
        }
        let show_all = shared
            .lock()
            .map_or(self.config.boss_route_all, |v| v.show_all);
        if show_all != self.config.boss_route_all {
            self.config.boss_route_all = show_all;
            if let Err(error) = save_config(&self.config) {
                self.push(Level::Warn, format!("Cannot save settings: {error}"));
            }
        }
        let lines = self.route_lines();
        let empty = if self.config.boss_route_all {
            "No MVP seen yet. Keep the MVP tracker on."
        } else {
            "No ticked MVP seen yet. Keep the MVP tracker on."
        };
        if let Ok(mut view) = shared.lock() {
            view.lines = lines;
            view.empty = empty.into();
            view.colors = crate::windows::route::RouteColors {
                background: hex(p().panel),
                border: hex(p().border),
                text: hex(p().text),
                muted: hex(p().muted),
            };
        };
    }
    fn send(&mut self, alert: Alert) {
        let config = self.config.clone();
        self.send_with(config, alert, None);
    }

    /// Test buttons in Settings pass the unsaved config, so new values can be tried before saving
    fn send_with(&mut self, config: Config, alert: Alert, cancel: Option<Arc<AtomicBool>>) {
        let configured = notify::channels(&config);
        if configured.is_empty() {
            self.push(
                Level::Warn,
                "Alert not sent: no notification channel configured".into(),
            );
            return;
        }
        self.push(
            Level::of_kind(alert.kind),
            format!("Sending '{}' via {}", alert.title, configured.join(", ")),
        );
        self.spawn_job(move || notify::send_cancellable(config, alert, cancel.as_deref()));
    }

    fn start_drill(&mut self, config: Config) {
        if notify::channels(&config).is_empty() {
            self.push(
                Level::Warn,
                "Drill not started: no notification channel configured".into(),
            );
            return;
        }
        let game = config.watched().first().copied().unwrap_or(&GAMES[0]);
        let pid = self
            .snapshot
            .as_ref()
            .and_then(|s| s.clients_of(game.id).next())
            .map_or(0, |c| c.identity.pid);
        let minutes = config.repeat_alert_min.max(1);
        self.push(
            Level::Warn,
            format!("Drill started: {DRILL_ROUNDS} rounds, one every {minutes} min"),
        );
        self.drill = Some(Drill {
            sent: 0,
            next_at: Instant::now(),
            config,
            cancel: Arc::new(AtomicBool::new(false)),
            game,
            pid,
        });
        self.step_drill();
    }

    fn stop_drill(&mut self) {
        if let Some(drill) = self.drill.take() {
            drill.cancel.store(true, Ordering::Relaxed);
            self.push(Level::Warn, "Drill stopped".into());
        }
    }

    fn step_drill(&mut self) {
        let Some(drill) = &mut self.drill else {
            return;
        };
        if Instant::now() < drill.next_at {
            return;
        }
        drill.sent += 1;
        let (round, config, cancel) = (drill.sent, drill.config.clone(), drill.cancel.clone());
        let minutes = config.repeat_alert_min.max(1);
        drill.next_at = Instant::now() + Duration::from_secs(minutes * 60);
        // Simulated drop time = grace period + repeat rounds so far; looks like the real alert, only marked DRILL
        let fake_down = config.interval_sec * u64::from(config.grace_checks.max(1))
            + (u64::from(round) - 1) * minutes * 60;
        let alert = Alert {
            title: format!("DRILL - {}", titled(drill.game, "Client disconnected")),
            message: "This is a practice alert. Nothing is actually wrong.".into(),
            kind: Kind::Warn,
            urgent: true,
            fields: vec![
                ("Client".into(), format!("PID {}", drill.pid)),
                ("Down for".into(), format_duration(fake_down)),
                ("Round".into(), format!("{round} of {DRILL_ROUNDS}")),
            ],
            desktop_bmp: None,
        };
        self.send_with(config, alert, Some(cancel));
        if round >= DRILL_ROUNDS {
            // No cancel here: the rest of the final round's burst must be sent in full
            self.drill = None;
            self.push(
                Level::Warn,
                format!("Drill complete ({DRILL_ROUNDS} rounds). If that did not wake you, fix the phone side."),
            );
        }
    }

    fn drill_status(&self) -> Option<String> {
        self.drill.as_ref().map(|drill| {
            let left = drill
                .next_at
                .saturating_duration_since(Instant::now())
                .as_secs();
            format!(
                "Stop drill ({}/{DRILL_ROUNDS} · next in {})",
                drill.sent,
                format_duration(left)
            )
        })
    }

    fn due(config: &Config, state: &mut AlertState) -> bool {
        if !state.active {
            state.active = true;
            state.last_alert = Some(Instant::now());
            return true;
        }
        if config.repeat_alert_min > 0
            && state.repeats < config.max_repeats
            && state
                .last_alert
                .is_some_and(|at| at.elapsed() >= Duration::from_secs(config.repeat_alert_min * 60))
        {
            state.repeats += 1;
            state.last_alert = Some(Instant::now());
            return true;
        }
        false
    }

    fn process_alerts(&mut self, snapshot: &Snapshot, internet: bool) {
        if !internet {
            self.internet_misses += 1;
            if self.internet_misses >= self.config.grace_checks.max(1)
                && Self::due(&self.config, &mut self.internet_alert)
            {
                self.send(Alert {
                    title: "Internet is down".into(),
                    message:
                        "This PC cannot reach the internet, so the game has probably dropped too."
                            .into(),
                    kind: Kind::Down,
                    urgent: true,
                    fields: vec![],
                    desktop_bmp: None,
                });
            }
            return;
        }
        if self.internet_alert.active {
            self.internet_alert = AlertState::default();
            self.send(Alert {
                title: "Internet is back".into(),
                message: "The connection recovered.".into(),
                kind: Kind::Up,
                urgent: false,
                fields: vec![],
                desktop_bmp: None,
            });
        }
        self.internet_misses = 0;
        let mut alerts = Vec::new();
        for game in self.config.watched() {
            let running = snapshot.clients_of(game.id).next().is_some();
            let state = self.no_process.entry(game.id).or_default();
            if !running {
                if Self::due(&self.config, state) {
                    alerts.push(Alert {
                        title: titled(game, "Game closed"),
                        message: format!(
                            "{} is not running — the game exited or crashed.",
                            game.name
                        ),
                        kind: Kind::Down,
                        urgent: true,
                        fields: vec![],
                        desktop_bmp: None,
                    });
                }
            } else if state.active {
                *state = AlertState::default();
                alerts.push(Alert {
                    title: titled(game, "Game reopened"),
                    message: format!("{} is running again.", game.name),
                    kind: Kind::Up,
                    urgent: false,
                    fields: vec![],
                    desktop_bmp: None,
                });
            }
        }

        let alive: HashSet<_> = snapshot
            .clients
            .iter()
            .map(|client| client.identity)
            .collect();
        let vanished: Vec<_> = self
            .clients
            .iter()
            .filter(|(id, _)| !alive.contains(id))
            .map(|(id, state)| (*id, state.game))
            .collect();
        for (identity, game) in vanished {
            self.clients.remove(&identity);
            // Whole game closed: the "Game closed" alert above already covers it, no per-client alerts
            if snapshot.clients_of(game.id).next().is_none() {
                continue;
            }
            alerts.push(Alert {
                title: titled(game, "Client closed"),
                message: format!(
                    "{} client PID {} is no longer running.",
                    game.name, identity.pid
                ),
                kind: Kind::Down,
                urgent: true,
                fields: vec![("Client".into(), format!("PID {}", identity.pid))],
                desktop_bmp: None,
            });
        }

        for client in &snapshot.clients {
            let Some(game) = crate::find_game(client.game).filter(|g| self.config.watches(g.id))
            else {
                continue;
            };
            let online = !client.connections.is_empty();
            let session = client.connections.iter().map(|c| c.created_filetime).min();
            let state = self.clients.entry(client.identity).or_insert(ClientState {
                game,
                misses: 0,
                alerted: false,
                repeats: 0,
                last_alert: None,
                session,
                down_since: None,
            });
            if online {
                if state.session.is_some() && session > state.session {
                    alerts.push(Alert {
                        title: titled(game, "Reconnected on its own"),
                        message: "The client established a new game session without help.".into(),
                        kind: Kind::Warn,
                        urgent: false,
                        fields: vec![("Client".into(), format!("PID {}", client.identity.pid))],
                        desktop_bmp: None,
                    });
                }
                state.session = session;
                if state.alerted {
                    alerts.push(Alert {
                        title: titled(game, "Back online"),
                        message: "The client reconnected to the game server.".into(),
                        kind: Kind::Up,
                        urgent: false,
                        fields: vec![("Client".into(), format!("PID {}", client.identity.pid))],
                        desktop_bmp: None,
                    });
                }
                state.misses = 0;
                state.alerted = false;
                state.repeats = 0;
                state.last_alert = None;
                state.down_since = None;
            } else {
                state.misses += 1;
                let down_since = *state.down_since.get_or_insert_with(Instant::now);
                let due = !state.alerted
                    || (self.config.repeat_alert_min > 0
                        && state.repeats < self.config.max_repeats
                        && state.last_alert.is_some_and(|at| {
                            at.elapsed() >= Duration::from_secs(self.config.repeat_alert_min * 60)
                        }));
                if state.misses >= self.config.grace_checks.max(1) && due {
                    if state.alerted {
                        state.repeats += 1;
                    }
                    state.alerted = true;
                    state.last_alert = Some(Instant::now());
                    alerts.push(Alert {
                        title: titled(game, "Client disconnected"),
                        message: "No connection to the game server. You are logged out.".into(),
                        kind: Kind::Down,
                        urgent: true,
                        fields: vec![
                            ("Client".into(), format!("PID {}", client.identity.pid)),
                            (
                                "Down for".into(),
                                format!("{}s", down_since.elapsed().as_secs()),
                            ),
                            (
                                "Reminder".into(),
                                if state.repeats == 0 {
                                    "first alert".into()
                                } else {
                                    format!("#{}", state.repeats)
                                },
                            ),
                        ],
                        desktop_bmp: None,
                    });
                }
            }
        }
        for alert in alerts {
            self.send(alert);
        }

        if self.config.heartbeat_hours > 0.0
            && self.last_heartbeat.elapsed().as_secs_f64() >= self.config.heartbeat_hours * 3600.0
        {
            self.last_heartbeat = Instant::now();
            let fields = heartbeat_fields(&self.config, Some(snapshot));
            self.send_heartbeat(self.config.clone(), fields, false);
        }
    }

    fn send_heartbeat(&mut self, config: Config, fields: Vec<(String, String)>, test: bool) {
        if test {
            let note = if !config.attach_desktop_screenshot {
                "Test heartbeat: screenshot option is off, sending text only"
            } else if config.webhook_url.trim().is_empty() {
                "Test heartbeat: screenshot needs a Discord webhook, sending text only"
            } else {
                "Test heartbeat: capturing desktop for Discord"
            };
            self.push(Level::Info, note.into());
        }
        // Title must be "Still watching": notify::send uses it to recognise the heartbeat and attach the screenshot
        self.send_with(
            config,
            Alert {
                title: "Still watching".into(),
                message: if test {
                    "Test heartbeat — this is what a routine check-in looks like.".into()
                } else {
                    "Routine check-in — the monitor is alive.".into()
                },
                kind: Kind::Info,
                urgent: false,
                fields,
                desktop_bmp: None,
            },
            None,
        );
    }

    fn test_alert(&mut self, config: Config) {
        self.send_with(
            config,
            Alert {
                title: "Test alert".into(),
                message: "If you can see this on your phone, delivery works.".into(),
                kind: Kind::Info,
                urgent: true,
                fields: vec![],
                desktop_bmp: None,
            },
            None,
        );
    }

    fn capture(&mut self) {
        let snapshot = self.snapshot.clone();
        self.push(Level::Info, "Capturing desktop...".into());
        self.spawn_job(move || {
            let directory = root_path("screenshots");
            let snapshot = snapshot.unwrap_or_else(|| crate::windows::snapshot(GAMES));
            let code = crate::windows::capture::run(&snapshot, Some(&directory));
            vec![if code == std::process::ExitCode::SUCCESS {
                format!("Desktop capture saved to {}", directory.display())
            } else {
                "Desktop capture failed".into()
            }]
        });
    }

    fn overall(&self) -> (String, Color32) {
        if !self.watching {
            return ("PAUSED".into(), p().grey);
        }
        if !self.internet_up {
            return ("INTERNET DOWN".into(), p().red);
        }
        let Some(snapshot) = &self.snapshot else {
            return ("CHECKING".into(), p().blue);
        };
        if !snapshot.errors.is_empty() {
            return ("CHECK INCOMPLETE".into(), p().amber);
        }
        let watched = self.config.watched();
        if watched.is_empty() {
            return ("NO GAME WATCHED".into(), p().amber);
        }
        let (mut disconnected, mut not_running) = (0, 0);
        for game in &watched {
            let (online, total) = snapshot.counts(game.id);
            disconnected += total - online;
            not_running += usize::from(total == 0);
        }
        if disconnected > 0 {
            (format!("{disconnected} DISCONNECTED"), p().red)
        } else if not_running == 1 && watched.len() == 1 {
            ("GAME NOT RUNNING".into(), p().red)
        } else if not_running > 0 {
            (format!("{not_running} GAMES NOT RUNNING"), p().red)
        } else {
            ("ALL ONLINE".into(), p().green)
        }
    }

    /// Status shown on the game card: always the real state; whether it alerts depends on the card's switch
    fn game_status(&self, game: &GameProfile) -> (String, Color32) {
        let Some(snapshot) = &self.snapshot else {
            return ("CHECKING".into(), p().blue);
        };
        if !snapshot.errors.is_empty() {
            return ("CHECK INCOMPLETE".into(), p().amber);
        }
        let alerting = self.watching && self.config.watches(game.id);
        let cards = self.cards(game);
        let down = cards.iter().filter(|c| c.color == p().red).count();
        let confirming = cards.iter().any(|c| c.color == p().amber);
        if cards.is_empty() {
            let color = if alerting { p().red } else { p().grey };
            ("NOT RUNNING".into(), color)
        } else if down > 0 {
            (format!("{down} DISCONNECTED"), p().red)
        } else if confirming {
            ("CONFIRMING".into(), p().amber)
        } else {
            let (online, total) = snapshot.counts(game.id);
            (format!("{online}/{total} ONLINE"), p().green)
        }
    }

    fn check_status(&self) -> String {
        if self.receiver.is_some() {
            return "Checking...".into();
        }
        let Some(last) = self.last_check else {
            return String::new();
        };
        let ago = format!("Checked {} ago", format_duration(last.elapsed().as_secs()));
        if !self.watching {
            return ago;
        }
        let next = self
            .config
            .interval_sec
            .max(1)
            .saturating_sub(last.elapsed().as_secs());
        format!("{ago} · next in {}", format_duration(next))
    }

    fn cards(&self, game: &GameProfile) -> Vec<CardInfo> {
        let Some(snapshot) = &self.snapshot else {
            return vec![];
        };
        let grace = self.config.grace_checks.max(1);
        let now = now_filetime();
        snapshot
            .clients_of(game.id)
            .enumerate()
            .map(|(index, client)| {
                let state = self.clients.get(&client.identity);
                let online = !client.connections.is_empty();
                let (status, color) = if online {
                    ("ONLINE".to_string(), p().green)
                } else {
                    match state {
                        Some(s) if s.misses < grace => {
                            (format!("CONFIRMING {}/{grace}", s.misses), p().amber)
                        }
                        _ => ("DISCONNECTED".to_string(), p().red),
                    }
                };
                let detail = if online {
                    let started = client.connections.iter().map(|c| c.created_filetime).min();
                    started
                        .map(|ft| {
                            format!(
                                "Session {}",
                                format_duration(((now - ft).max(0) / 10_000_000) as u64)
                            )
                        })
                        .unwrap_or_default()
                } else {
                    state
                        .and_then(|s| s.down_since)
                        .map(|at| format!("Down for {}", format_duration(at.elapsed().as_secs())))
                        .unwrap_or_else(|| "No game socket".into())
                };
                let window = if client.windows.is_empty() {
                    "no window"
                } else if client.windows.iter().all(|w| w.minimized) {
                    "minimized"
                } else {
                    "window visible"
                };
                let pinned =
                    (!client.windows.is_empty()).then(|| client.windows.iter().any(|w| w.topmost));
                CardInfo {
                    index: index + 1,
                    identity: client.identity,
                    pinned,
                    pip: self.pips.contains_key(&client.identity),
                    pid: client.identity.pid,
                    status,
                    color,
                    server: client.connections.first().map(|c| c.remote.clone()),
                    extra_sockets: client.connections.len().saturating_sub(1),
                    detail,
                    window,
                }
            })
            .collect()
    }

    fn footer(&self) -> (String, Color32) {
        let channels = notify::channels(&self.config);
        if channels.is_empty() {
            return (
                "No alert channel configured — open Settings".into(),
                p().red,
            );
        }
        let mut text = format!("Alerts via {}", channels.join(" · "));
        if self.config.heartbeat_hours > 0.0 {
            let every = self.config.heartbeat_hours * 3600.0;
            text += &format!("   |   Heartbeat every {}", format_duration(every as u64));
            if self.watching {
                let left = (every - self.last_heartbeat.elapsed().as_secs_f64()).max(0.0);
                text += &format!(" (next in {})", format_duration(left as u64));
            }
            if self.config.attach_desktop_screenshot {
                text += " + screenshot";
            }
        }
        (text, p().muted)
    }
}

struct BossRow {
    name: &'static str,
    map: &'static str,
    tracked: bool,
    status: String,
    color: Color32,
}

/// 0xRRGGBB for the Win32 route window
fn hex(color: Color32) -> u32 {
    (u32::from(color.r()) << 16) | (u32::from(color.g()) << 8) | u32::from(color.b())
}

struct CardInfo {
    index: usize,
    identity: Identity,
    /// Whether the game window is always on top; None = no window to pin
    pinned: Option<bool>,
    /// Picture-in-picture window open for this client
    pip: bool,
    pid: u32,
    status: String,
    color: Color32,
    server: Option<String>,
    extra_sockets: usize,
    detail: String,
    window: &'static str,
}

/// Game name prefixed to the title, so on waking you know at once which game it is.
/// Uses ":" rather than "—" because ntfy's Title header strips non-ASCII characters
fn titled(game: &GameProfile, title: &str) -> String {
    format!("{}: {title}", game.name)
}

/// Heartbeat reports counts per game with alerts on
fn heartbeat_fields(config: &Config, snapshot: Option<&Snapshot>) -> Vec<(String, String)> {
    let watched = config.watched();
    if watched.is_empty() {
        return vec![("Games".into(), "none watched".into())];
    }
    watched
        .iter()
        .map(|game| {
            let (online, total) = snapshot.map_or((0, 0), |s| s.counts(game.id));
            (game.name.to_string(), format!("{online} of {total} online"))
        })
        .collect()
}

// ---------------------------------------------------------------- window / tray

thread_local! {
    // The tray icon is created and dropped on the main thread only
    static TRAY: RefCell<Option<TrayIcon>> = const { RefCell::new(None) };
}

/// Win32 HWND of the main window, stored as isize so it can cross threads
#[derive(Clone, Copy)]
struct Hwnd(isize);

impl Hwnd {
    fn show(self, context: &egui::Context) {
        use windows::Win32::{
            Foundation::HWND,
            UI::WindowsAndMessaging::{
                IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW,
            },
        };
        if self.0 != 0 {
            let hwnd = HWND(self.0 as *mut _);
            unsafe {
                let command = if IsIconic(hwnd).as_bool() {
                    SW_RESTORE
                } else {
                    SW_SHOW
                };
                let _ = ShowWindow(hwnd, command);
                let _ = SetForegroundWindow(hwnd);
            }
        }
        // Tell egui/winit the window is visible again, so the next hide really hides it
        context.send_viewport_cmd(egui::ViewportCommand::Visible(true));
        context.send_viewport_cmd(egui::ViewportCommand::Focus);
        context.request_repaint();
    }
}

fn exit_app(engine: &Mutex<Engine>) -> ! {
    lock(engine).push(Level::Info, "Exited".into());
    TRAY.with(|tray| drop(tray.borrow_mut().take()));
    std::process::exit(0);
}

/// 256×256 app icon (transparent corners), generated by assets/make_icons.py
const ICON_PNG: &[u8] = include_bytes!("../assets/icon-256.png");

fn icon_rgba(size: u32) -> Option<Vec<u8>> {
    let image = image::load_from_memory_with_format(ICON_PNG, image::ImageFormat::Png).ok()?;
    Some(
        image
            .resize_exact(size, size, image::imageops::FilterType::Lanczos3)
            .to_rgba8()
            .into_raw(),
    )
}

/// Prefer the icon in the exe's resources (Windows picks the sharpest small size from the .ico itself)
fn tray_icon() -> Option<Icon> {
    Icon::from_resource(1, Some((32, 32)))
        .ok()
        .or_else(|| Icon::from_rgba(icon_rgba(32)?, 32, 32).ok())
}

fn install_tray(engine: Arc<Mutex<Engine>>, hwnd: Hwnd, context: egui::Context) {
    let menu = Menu::new();
    let show = MenuItem::new("Show Gamepulse", true, None);
    let exit = MenuItem::new("Exit", true, None);
    let _ = menu.append_items(&[&show, &exit]);
    let (show_id, exit_id) = (show.id().clone(), exit.id().clone());

    let tray = tray_icon().and_then(|icon| {
        TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("Gamepulse")
            .with_icon(icon)
            .build()
            .ok()
    });
    TRAY.with(|slot| *slot.borrow_mut() = tray);

    let menu_context = context.clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == show_id {
            hwnd.show(&menu_context);
        } else if event.id == exit_id {
            exit_app(&engine);
        }
    }));
    TrayIconEvent::set_event_handler(Some(move |event: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = event
        {
            hwnd.show(&context);
        }
    }));
}

/// Launching again → the new instance signals a named event so the running one shows its window, then exits
fn listen_for_second_instance(hwnd: Hwnd, context: egui::Context) {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject, INFINITE};
    thread::spawn(move || unsafe {
        let Ok(event) = CreateEventW(None, false, false, SHOW_EVENT_NAME) else {
            return;
        };
        while WaitForSingleObject(event, INFINITE) == WAIT_OBJECT_0 {
            hwnd.show(&context);
        }
    });
}

pub fn show_existing_instance() {
    use windows::Win32::{
        Foundation::CloseHandle,
        System::Threading::{OpenEventW, SetEvent, EVENT_MODIFY_STATE},
        UI::WindowsAndMessaging::{AllowSetForegroundWindow, ASFW_ANY},
    };
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
        if let Ok(event) = OpenEventW(EVENT_MODIFY_STATE, false, SHOW_EVENT_NAME) {
            let _ = SetEvent(event);
            let _ = CloseHandle(event);
        }
    }
}

// ---------------------------------------------------------------- UI

fn darken(color: Color32, factor: f32) -> Color32 {
    let scale = |c: u8| (f32::from(c) * factor) as u8;
    Color32::from_rgb(scale(color.r()), scale(color.g()), scale(color.b()))
}

fn apply_theme(context: &egui::Context) {
    // Force dark/light per theme, otherwise egui follows Windows and the other mode's style stays default
    context.set_theme(if p().dark {
        egui::ThemePreference::Dark
    } else {
        egui::ThemePreference::Light
    });
    let mut visuals = if p().dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    visuals.panel_fill = p().bg;
    visuals.window_fill = p().panel;
    visuals.extreme_bg_color = if p().dark {
        darken(p().bg, 0.7)
    } else {
        p().bg
    };
    visuals.faint_bg_color = p().card;
    visuals.window_stroke = Stroke::new(1.0_f32, p().border);
    visuals.window_corner_radius = CornerRadius::same(12);
    visuals.override_text_color = Some(p().text);
    visuals.hyperlink_color = p().blue;
    visuals.selection.bg_fill = tint(p().blue, 90);
    visuals.selection.stroke = Stroke::new(1.0_f32, p().blue);
    let widgets = &mut visuals.widgets;
    for style in [
        &mut widgets.noninteractive,
        &mut widgets.inactive,
        &mut widgets.hovered,
        &mut widgets.active,
        &mut widgets.open,
    ] {
        style.corner_radius = CornerRadius::same(8);
    }
    widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p().border);
    widgets.inactive.bg_fill = p().card;
    widgets.inactive.weak_bg_fill = p().card;
    widgets.inactive.bg_stroke = Stroke::new(1.0_f32, p().border);
    widgets.hovered.bg_fill = p().card_hover;
    widgets.hovered.weak_bg_fill = p().card_hover;
    widgets.hovered.bg_stroke = Stroke::new(1.0_f32, tint(p().muted, 120));
    widgets.active.weak_bg_fill = p().border;
    context.set_visuals(visuals);
    context.style_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.window_margin = Margin::same(16);
        for (text_style, size) in [
            (TextStyle::Body, 14.0),
            (TextStyle::Button, 14.0),
            (TextStyle::Small, 12.0),
        ] {
            style
                .text_styles
                .insert(text_style, FontId::proportional(size));
        }
        style.text_styles.insert(TextStyle::Heading, semibold(20.0));
        style
            .text_styles
            .insert(TextStyle::Monospace, FontId::monospace(13.0));
    });
}

/// Font family for Inter SemiBold, used via `semibold()` / `.semibold()`
const SEMIBOLD: &str = "semibold";
/// Rounded font family (Fredoka Medium), used via `rounded()`, e.g. the placeholder cover letter
const ROUNDED: &str = "rounded";

/// Fonts embedded in the exe (OFL, licenses in assets/fonts): Inter instead of SF Pro, JetBrains Mono instead of SF Mono.
/// egui's default fonts are appended as fallback for glyphs Inter lacks
fn install_fonts(context: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    for (name, bytes) in [
        (
            "inter",
            &include_bytes!("../assets/fonts/Inter-Regular.ttf")[..],
        ),
        (
            "inter-semibold",
            include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
        ),
        (
            "jetbrains-mono",
            include_bytes!("../assets/fonts/JetBrainsMono-Regular.ttf"),
        ),
        (
            "fredoka-medium",
            include_bytes!("../assets/fonts/Fredoka-Medium.ttf"),
        ),
    ] {
        fonts.font_data.insert(
            name.to_owned(),
            Arc::new(egui::FontData::from_static(bytes)),
        );
    }
    let families = &mut fonts.families;
    let proportional = families.entry(FontFamily::Proportional).or_default();
    proportional.insert(0, "inter".to_owned());
    let mut semibold = vec!["inter-semibold".to_owned()];
    semibold.extend(proportional.iter().skip(1).cloned());
    // The bundled Fredoka only covers Latin; other characters fall back to Inter
    let mut rounded = vec!["fredoka-medium".to_owned()];
    rounded.extend(proportional.iter().cloned());
    families.insert(FontFamily::Name(SEMIBOLD.into()), semibold);
    families.insert(FontFamily::Name(ROUNDED.into()), rounded);
    families
        .entry(FontFamily::Monospace)
        .or_default()
        .insert(0, "jetbrains-mono".to_owned());
    context.set_fonts(fonts);
}

fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(SEMIBOLD.into()))
}

fn rounded(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(ROUNDED.into()))
}

/// egui's `.strong()` only changes the color; this makes text really bold with Inter SemiBold
trait Semibold {
    fn semibold(self) -> RichText;
}

impl Semibold for RichText {
    fn semibold(self) -> RichText {
        self.strong().family(FontFamily::Name(SEMIBOLD.into()))
    }
}

fn tint(color: Color32, alpha: u8) -> Color32 {
    Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha)
}

fn dot(ui: &mut Ui, color: Color32, radius: f32) {
    let (rect, _) =
        ui.allocate_exact_size(egui::vec2(radius * 2.0, radius * 2.0), egui::Sense::hover());
    ui.painter().circle_filled(rect.center(), radius, color);
}

fn pill(ui: &mut Ui, text: &str, color: Color32) {
    Frame::new()
        .fill(tint(color, 34))
        .stroke(Stroke::new(1.0_f32, tint(color, 90)))
        .corner_radius(CornerRadius::same(99))
        .inner_margin(Margin::symmetric(10, 3))
        .show(ui, |ui| {
            // horizontal inherits direction from the parent: in right_to_left, add in reverse so the dot comes before the text
            let right_to_left = ui.layout().prefer_right_to_left();
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let label = RichText::new(text).color(color).semibold().size(12.0);
                if right_to_left {
                    ui.label(label);
                    dot(ui, color, 3.5);
                } else {
                    dot(ui, color, 3.5);
                    ui.label(label);
                }
            });
        });
}

fn card_frame() -> Frame {
    Frame::new()
        .fill(p().card)
        .stroke(Stroke::new(1.0_f32, p().border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(Margin::same(14))
}

/// Square icon toggle for the client card, with the icon painted by hand because the font has
/// no 📌 or PiP glyph. On = filled accent background, so the state shows without hovering
fn icon_toggle(
    ui: &mut Ui,
    on: bool,
    hover: &str,
    icon: impl FnOnce(&egui::Painter, egui::Pos2, Color32),
) -> bool {
    let mut button = egui::Button::new("")
        .min_size(egui::vec2(28.0, 22.0))
        .corner_radius(CornerRadius::same(6));
    if on {
        button = button
            .fill(tint(p().blue, 40))
            .stroke(Stroke::new(1.0_f32, tint(p().blue, 120)));
    }
    let response = ui.add(button).on_hover_text(hover);
    let color = if on || response.hovered() {
        p().blue
    } else {
        p().muted
    };
    icon(ui.painter(), response.rect.center(), color);
    response.clicked()
}

fn pin_button(ui: &mut Ui, pinned: bool) -> bool {
    let hover = if pinned {
        "Unpin: let other windows cover this game window again"
    } else {
        "Pin on top: keep this game window above other windows, so you can work in another app and still see the game"
    };
    icon_toggle(ui, pinned, hover, |painter, c, color| {
        let bar = |left: f32, top: f32, right: f32, bottom: f32| {
            egui::Rect::from_min_max(c + egui::vec2(left, top), c + egui::vec2(right, bottom))
        };
        // cap, neck, base plate, then the needle
        painter.rect_filled(bar(-3.5, -7.0, 3.5, -5.0), CornerRadius::same(1), color);
        painter.rect_filled(bar(-2.0, -5.0, 2.0, -1.0), CornerRadius::ZERO, color);
        painter.rect_filled(bar(-5.0, -1.0, 5.0, 1.0), CornerRadius::same(1), color);
        painter.line_segment(
            [c + egui::vec2(0.0, 1.0), c + egui::vec2(0.0, 7.0)],
            Stroke::new(1.4_f32, color),
        );
    })
}

fn pip_button(ui: &mut Ui, open: bool) -> bool {
    let hover = if open {
        "Close picture-in-picture"
    } else {
        "Picture-in-picture: a small live view of this game that floats above other windows. \
         Drag to move, drag the edges to resize, double-click to go back to the game. \
         Don't minimize the game (Windows stops drawing it); leave it behind your other apps."
    };
    icon_toggle(ui, open, hover, |painter, c, color| {
        // screen outline with a small filled window in the bottom-right corner
        painter.rect_stroke(
            egui::Rect::from_center_size(c, egui::vec2(15.0, 11.0)),
            CornerRadius::same(2),
            Stroke::new(1.3_f32, color),
            egui::StrokeKind::Middle,
        );
        painter.rect_filled(
            egui::Rect::from_min_max(c + egui::vec2(0.5, 0.5), c + egui::vec2(5.5, 3.5)),
            CornerRadius::same(1),
            color,
        );
    })
}

/// What the user clicked on a client card
enum CardAction {
    Pin(bool),
    Pip(bool),
}

fn client_card(ui: &mut Ui, card: &CardInfo) -> Option<CardAction> {
    let mut action = None;
    let response = card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!("Client {}", card.index))
                    .semibold()
                    .size(16.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                pill(ui, &card.status, card.color);
            });
        });
        ui.horizontal(|ui| {
            let mut info = format!("PID {} · {}", card.pid, card.window);
            if card.pinned == Some(true) {
                info += " · pinned on top";
            }
            ui.label(RichText::new(info).color(p().muted).small());
            if let Some(pinned) = card.pinned {
                // right_to_left: the first button added ends up rightmost
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if pin_button(ui, pinned) {
                        action = Some(CardAction::Pin(!pinned));
                    }
                    if pip_button(ui, card.pip) {
                        action = Some(CardAction::Pip(!card.pip));
                    }
                });
            }
        });
        ui.add_space(2.0);
        match &card.server {
            Some(server) => {
                let mut text = server.clone();
                if card.extra_sockets > 0 {
                    text += &format!("  (+{})", card.extra_sockets);
                }
                ui.label(RichText::new(text).monospace());
            }
            None => {
                ui.label(RichText::new("—").monospace().color(p().muted));
            }
        }
        ui.label(
            RichText::new(&card.detail).color(if card.color == p().green {
                p().muted
            } else {
                card.color
            }),
        );
    });
    // Colored bar on the left shows status at a glance without reading
    let rect = response.response.rect;
    ui.painter().rect_filled(
        egui::Rect::from_min_size(rect.min, egui::vec2(4.0, rect.height())),
        CornerRadius {
            nw: 10,
            sw: 10,
            ne: 0,
            se: 0,
        },
        card.color,
    );
    action
}

fn empty_state(ui: &mut Ui, title: &str, body: &str, color: Color32) {
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.vertical_centered(|ui| {
            ui.add_space(18.0);
            ui.label(RichText::new(title).size(17.0).semibold().color(color));
            ui.label(RichText::new(body).color(p().muted));
            ui.add_space(18.0);
        });
    });
}

fn section(ui: &mut Ui, title: &str, add: impl FnOnce(&mut Ui)) {
    ui.add_space(6.0);
    ui.label(RichText::new(title).semibold().color(p().blue));
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        add(ui);
    });
}

fn hint(ui: &mut Ui, text: &str, color: Color32) {
    ui.label(RichText::new(text).small().color(color));
}

fn row(ui: &mut Ui, label: &str, add: impl FnOnce(&mut Ui)) {
    ui.label(RichText::new(label).color(p().muted));
    add(ui);
    ui.end_row();
}

fn text_edit<'t>(value: &'t mut String, secret: bool, placeholder: &str) -> egui::TextEdit<'t> {
    egui::TextEdit::singleline(value)
        .password(secret)
        .hint_text(placeholder)
        .margin(Margin::symmetric(10, 7))
}

fn text_field(ui: &mut Ui, value: &mut String, secret: bool, placeholder: &str) {
    ui.add(text_edit(value, secret, placeholder).desired_width(250.0));
}

/// Label above, field spanning the card: URLs/tokens are too long for a 2-column Grid
fn wide_field(ui: &mut Ui, label: &str, value: &mut String, secret: bool, placeholder: &str) {
    ui.label(RichText::new(label).color(p().muted));
    ui.add(text_edit(value, secret, placeholder).desired_width(f32::INFINITY));
    ui.add_space(4.0);
}

// ---------------------------------------------------------------- Open at sign-in
// Replaces the old PowerShell version's Task Scheduler + Start-Watch.cmd: uses HKCU Run, no admin rights needed

const RUN_KEY: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
const RUN_VALUE: windows::core::PCWSTR = w!("Gamepulse");
/// Name before the rename to Gamepulse; points at rooc-monitor.exe, which no longer exists
const LEGACY_RUN_VALUE: windows::core::PCWSTR = w!("ROOC Monitor");

fn launch_at_login() -> bool {
    run_value_exists(RUN_VALUE)
}

fn run_value_exists(value: windows::core::PCWSTR) -> bool {
    use windows::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ},
    };
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            RUN_KEY,
            value,
            RRF_RT_REG_SZ,
            None,
            None,
            None,
        )
    };
    result == ERROR_SUCCESS
}

fn set_launch_at_login(enabled: bool) -> Result<(), String> {
    use windows::Win32::{
        Foundation::ERROR_SUCCESS,
        System::Registry::{RegSetKeyValueW, HKEY_CURRENT_USER, REG_SZ},
    };
    let result = if enabled {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command: Vec<u16> = format!("\"{}\"", exe.display())
            .encode_utf16()
            .chain([0])
            .collect();
        unsafe {
            RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                RUN_VALUE,
                REG_SZ.0,
                Some(command.as_ptr().cast()),
                (command.len() * 2) as u32,
            )
        }
    } else {
        return delete_run_value(RUN_VALUE);
    };
    if result == ERROR_SUCCESS {
        Ok(())
    } else {
        Err(format!("registry error {}", result.0))
    }
}

fn delete_run_value(value: windows::core::PCWSTR) -> Result<(), String> {
    use windows::Win32::{
        Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS},
        System::Registry::{RegDeleteKeyValueW, HKEY_CURRENT_USER},
    };
    match unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, value) } {
        ERROR_SUCCESS | ERROR_FILE_NOT_FOUND => Ok(()),
        other => Err(format!("registry error {}", other.0)),
    }
}

/// Users who had ticked "Open ROOC Monitor at sign-in": move it to the new value pointing at this exe
fn migrate_legacy_launch(engine: &mut Engine) {
    if !run_value_exists(LEGACY_RUN_VALUE) {
        return;
    }
    let result = set_launch_at_login(true).and_then(|()| delete_run_value(LEGACY_RUN_VALUE));
    match result {
        Ok(()) => engine.push(
            Level::Info,
            "Moved the old ROOC Monitor sign-in launch to Gamepulse".into(),
        ),
        Err(error) => engine.push(
            Level::Warn,
            format!("Cannot move the old ROOC Monitor sign-in launch: {error}"),
        ),
    }
}

/// Values being edited in Settings; no effect until Save (except the theme, which previews immediately)
struct SettingsDraft {
    config: Config,
    launch_at_login: bool,
}

fn tools_section(ui: &mut Ui, draft: &Config, engine: &mut Engine) {
    section(ui, "Test your setup", |ui| {
        hint(
            ui,
            "Uses the values on this screen, even before you press Save.",
            p().muted,
        );
        ui.horizontal_wrapped(|ui| {
            if ui
                .button("🔔  Test alert")
                .on_hover_text("One urgent alert burst to every channel: proves delivery works")
                .clicked()
            {
                engine.test_alert(draft.clone());
            }
            match engine.drill_status() {
                Some(status) => {
                    let stop =
                        egui::Button::new(RichText::new(format!("⏹  {status}")).color(p().amber));
                    if ui.add(stop).clicked() {
                        engine.stop_drill();
                    }
                }
                None => {
                    if ui
                        .button("🚨  Run drill")
                        .on_hover_text(format!(
                            "Sends {DRILL_ROUNDS} practice disconnect alerts, one every {} min, \
                             exactly like a real drop: proves it actually wakes you up",
                            draft.repeat_alert_min.max(1)
                        ))
                        .clicked()
                    {
                        engine.start_drill(draft.clone());
                    }
                }
            }
            if ui
                .button("💓  Test heartbeat")
                .on_hover_text("Sends a heartbeat now, with a screenshot if that option is on")
                .clicked()
            {
                let fields = heartbeat_fields(draft, engine.snapshot.as_ref());
                engine.send_heartbeat(draft.clone(), fields, true);
            }
            if ui
                .button("📷  Capture desktop")
                .on_hover_text(
                    "Saves a full-desktop BMP into the screenshots folder. Nothing is sent.",
                )
                .clicked()
            {
                engine.capture();
            }
        });
    });
}

/// Which MVP alerts to send and where; turning the tracker on/off and choosing MVPs is on the game page
fn boss_settings(ui: &mut Ui, draft: &mut Config, engine: &mut Engine) {
    section(ui, "MVP tracker", |ui| {
        egui::Grid::new("mvp")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                row(ui, "Warn before a spawn", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut draft.boss_lead_min)
                            .range(0..=55)
                            .suffix(" min"),
                    );
                });
            });
        if draft.boss_lead_min == 0 {
            hint(ui, "No warning before a spawn.", p().muted);
        }
        ui.checkbox(&mut draft.boss_alert_spawn, "Alert when an MVP spawns");
        ui.checkbox(&mut draft.boss_alert_death, "Alert when an MVP dies");
        ui.checkbox(
            &mut draft.boss_alert_low_hp,
            "Alert when an MVP drops to 30% HP",
        );
        ui.add_space(4.0);
        ui.label(RichText::new("Send MVP alerts to").color(p().muted));
        ui.checkbox(&mut draft.boss_discord, "Discord webhook");
        if draft.boss_discord && draft.webhook_url.trim().is_empty() {
            hint(ui, "Needs the Discord webhook above.", p().amber);
        }
        ui.checkbox(&mut draft.boss_toast, "Windows notification");
        if ui
            .button("🔔  Test MVP alert")
            .on_hover_text("Sends one MVP-style alert to the places ticked above")
            .clicked()
        {
            engine.test_boss_alert(draft.clone());
        }
        hint(
            ui,
            "Turn the tracker on and pick MVPs on the game's page. It reads the game's \
             announcements from the game window once a second: keep the game open (covered is \
             fine, minimized is not). If the game runs as administrator, Gamepulse must too.",
            p().muted,
        );
    });
}

fn settings_form(ui: &mut Ui, draft: &mut SettingsDraft, engine: &mut Engine) {
    section(ui, "Appearance", |ui| {
        egui::Grid::new("appearance")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                row(ui, "Theme", |ui| {
                    egui::ComboBox::from_id_salt("theme")
                        .selected_text(draft.config.theme.as_str())
                        .width(180.0)
                        .show_ui(ui, |ui| {
                            for theme in &THEMES {
                                ui.selectable_value(
                                    &mut draft.config.theme,
                                    theme.name.to_string(),
                                    theme.name,
                                );
                            }
                        });
                });
            });
        hint(
            ui,
            "Game covers: right-click a game card › Choose cover image.",
            p().muted,
        );
    });
    let draft_config = &mut draft.config;
    section(ui, "Notifications", |ui| {
        ui.spacing_mut().item_spacing.y = 4.0;
        wide_field(
            ui,
            "ntfy topic",
            &mut draft_config.ntfy_topic,
            false,
            "secret-topic",
        );
        wide_field(
            ui,
            "ntfy server",
            &mut draft_config.ntfy_server,
            false,
            "https://ntfy.sh",
        );
        wide_field(
            ui,
            "Discord webhook",
            &mut draft_config.webhook_url,
            true,
            "https://discord.com/api/webhooks/...",
        );
        wide_field(
            ui,
            "Telegram bot token",
            &mut draft_config.telegram_token,
            true,
            "",
        );
        wide_field(
            ui,
            "Telegram chat ID",
            &mut draft_config.telegram_chat_id,
            false,
            "",
        );
        ui.spacing_mut().item_spacing.y = 8.0;
        ui.checkbox(
            &mut draft_config.discord_here,
            "Mention @here on Discord for real disconnects",
        );
        ui.checkbox(
            &mut draft_config.sound,
            "Also play an alarm sound on this PC",
        );
        if notify::channels(draft_config).is_empty() {
            hint(
                ui,
                "Configure at least one channel — watching will not start without one.",
                p().red,
            );
        }
    });
    tools_section(ui, draft_config, engine);
    section(ui, "Detection", |ui| {
        egui::Grid::new("detect")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                row(ui, "Check every", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut draft_config.interval_sec)
                            .range(1..=3600)
                            .suffix(" s"),
                    );
                });
                row(ui, "Grace checks", |ui| {
                    ui.add(egui::DragValue::new(&mut draft_config.grace_checks).range(1..=20));
                });
            });
        hint(
            ui,
            &format!(
                "A client must stay disconnected ~{} before the first alert.",
                format_duration(
                    draft_config.interval_sec.max(1) * u64::from(draft_config.grace_checks.max(1))
                )
            ),
            p().muted,
        );
    });
    section(ui, "Wake-up alerts", |ui| {
        egui::Grid::new("alerts")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                row(ui, "Repeat every", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut draft_config.repeat_alert_min)
                            .range(0..=1440)
                            .suffix(" min"),
                    );
                });
                row(ui, "Maximum repeats", |ui| {
                    ui.add(egui::DragValue::new(&mut draft_config.max_repeats).range(0..=100));
                });
                row(ui, "Notifications per alert", |ui| {
                    ui.add(egui::DragValue::new(&mut draft_config.burst_count).range(1..=20));
                });
                row(ui, "Gap between them", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut draft_config.burst_gap_sec)
                            .range(1..=300)
                            .suffix(" s"),
                    );
                });
            });
        if draft_config.repeat_alert_min == 0 {
            hint(
                ui,
                "Repeat is off — you get a single alert per disconnect.",
                p().amber,
            );
        }
        if draft_config.burst_count <= 1 {
            hint(
                ui,
                "1 notification = your phone vibrates only once per alert.",
                p().amber,
            );
        } else if draft_config.burst_gap_sec < 3 {
            hint(ui, "Gaps under 3 s may be merged by Android.", p().amber);
        }
    });
    section(ui, "Heartbeat", |ui| {
        egui::Grid::new("heartbeat")
            .num_columns(2)
            .spacing([16.0, 10.0])
            .show(ui, |ui| {
                row(ui, "Send every", |ui| {
                    ui.add(
                        egui::DragValue::new(&mut draft_config.heartbeat_hours)
                            .range(0.0..=168.0)
                            .speed(0.25)
                            .suffix(" h"),
                    );
                });
            });
        ui.checkbox(
            &mut draft_config.attach_desktop_screenshot,
            "Attach a desktop screenshot (Discord only)",
        );
        if draft_config.attach_desktop_screenshot {
            hint(
                ui,
                "Captures every monitor — anything open on screen is sent to Discord.",
                p().amber,
            );
            if draft_config.webhook_url.trim().is_empty() {
                hint(ui, "Needs a Discord webhook to be sent.", p().red);
            }
        }
        if draft_config.heartbeat_hours <= 0.0 {
            hint(ui, "Heartbeat is off.", p().muted);
        }
    });
    if engine.boss_game.is_some() {
        boss_settings(ui, &mut draft.config, engine);
    }
    section(ui, "App", |ui| {
        ui.checkbox(
            &mut draft.launch_at_login,
            "Open Gamepulse when I sign in to Windows",
        );
        ui.checkbox(
            &mut draft.config.auto_start,
            "Start watching when the app opens",
        );
        if draft.launch_at_login && !draft.config.auto_start {
            hint(
                ui,
                "The app will open at sign-in but will not watch until you press Start.",
                p().amber,
            );
        }
        hint(
            ui,
            "Set Windows Power & sleep to Never, or the PC sleeps and the game and monitor stop together.",
            p().muted,
        );
    });
}

struct App {
    engine: Arc<Mutex<Engine>>,
    settings: Option<SettingsDraft>,
    onboarding: Option<onboarding::Onboarding>,
    guide_open: bool,
    /// None = Library page, Some = that game's detail page
    open_game: Option<&'static GameProfile>,
    filter: hub::Filter,
    covers: hub::Covers,
    mvp_art: hub::MvpArt,
    /// Cover picker dialog currently open (runs on a separate thread), one at a time
    cover_job: Option<hub::CoverJob>,
}

fn open_settings(engine: &Engine) -> SettingsDraft {
    SettingsDraft {
        config: engine.config.clone(),
        launch_at_login: launch_at_login(),
    }
}

impl App {
    fn new(creation: &eframe::CreationContext<'_>) -> Self {
        let engine = Arc::new(Mutex::new(Engine::new()));
        if let Err(error) = crate::windows::toast::register() {
            lock(&engine).push(
                Level::Warn,
                format!("Windows notifications may not show: {error}"),
            );
        }
        CURRENT_THEME.store(theme_index(&lock(&engine).config.theme), Ordering::Relaxed);
        install_fonts(&creation.egui_ctx);
        apply_theme(&creation.egui_ctx);
        let worker = engine.clone();
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(250));
            lock(&worker).tick();
        });

        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        let hwnd = match creation.window_handle().map(|handle| handle.as_raw()) {
            Ok(RawWindowHandle::Win32(handle)) => Hwnd(handle.hwnd.get()),
            _ => Hwnd(0),
        };
        install_tray(engine.clone(), hwnd, creation.egui_ctx.clone());
        listen_for_second_instance(hwnd, creation.egui_ctx.clone());
        // First run: with no alert channel yet, show onboarding; existing users who already set one up skip it silently
        let covers = hub::load_covers(&creation.egui_ctx, &mut lock(&engine));
        let mvp_art = hub::load_mvp_art(&creation.egui_ctx, &mut lock(&engine));
        let onboarding = {
            let mut engine = lock(&engine);
            migrate_legacy_launch(&mut engine);
            if engine.config.onboarded {
                None
            } else if notify::channels(&engine.config).is_empty() {
                Some(onboarding::Onboarding::new(&engine.config))
            } else {
                onboarding::mark_onboarded(&mut engine);
                None
            }
        };
        Self {
            engine,
            settings: None,
            onboarding,
            guide_open: false,
            open_game: None,
            filter: hub::Filter::All,
            covers,
            mvp_art,
            cover_job: None,
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, context: &egui::Context, _frame: &mut eframe::Frame) {
        let engine = self.engine.clone();
        let mut engine = lock(&engine);
        if context.input(|input| input.viewport().close_requested()) {
            context.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            context.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            engine.push(
                Level::Info,
                "Window hidden to system tray — still watching".into(),
            );
        }
        context.request_repaint_after(Duration::from_millis(500));

        let finished = self.cover_job.as_ref().and_then(|job| job.try_recv().ok());
        if let Some((game, result)) = finished {
            self.cover_job = None;
            match result
                .and_then(|changed| changed.then(|| hub::load_cover(context, game)).transpose())
            {
                Ok(Some(Some(cover))) => {
                    self.covers.insert(game.id, cover);
                    engine.push(Level::Good, format!("Cover updated for {}", game.name));
                }
                Ok(_) => {}
                Err(error) => engine.push(Level::Warn, format!("Cannot use that image: {error}")),
            }
        }

        // Settings previews the theme immediately; Cancel reverts to the saved one
        let wanted = theme_index(
            self.settings
                .as_ref()
                .map_or(&engine.config.theme, |draft| &draft.config.theme),
        );
        if wanted != CURRENT_THEME.load(Ordering::Relaxed) {
            CURRENT_THEME.store(wanted, Ordering::Relaxed);
            apply_theme(context);
        }

        egui::TopBottomPanel::top("header")
            .frame(
                Frame::new()
                    .fill(p().panel)
                    .inner_margin(Margin::symmetric(16, 12))
                    .stroke(Stroke::new(1.0_f32, p().border)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Gamepulse").size(20.0).semibold());
                    ui.add_space(6.0);
                    let (status, color) = engine.overall();
                    pill(ui, &status, color);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let (label, fill, text) = if engine.watching {
                            ("⏸  Pause watching", p().card, p().text)
                        } else {
                            ("▶  Start watching", p().green, p().on_green)
                        };
                        let button = egui::Button::new(RichText::new(label).semibold().color(text))
                            .fill(fill)
                            .min_size(egui::vec2(160.0, 34.0));
                        if ui.add(button).clicked() {
                            if engine.watching {
                                engine.stop();
                            } else if !engine.start() {
                                self.settings = Some(open_settings(&engine));
                            }
                        }
                        if engine.internet_up {
                            pill(ui, "Internet", p().green);
                        } else {
                            pill(ui, "Internet down", p().red);
                        }
                    });
                });
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("🔄  Check now").clicked() {
                        engine.request_snapshot();
                    }
                    if ui
                        .button("⚙  Settings")
                        .on_hover_text("Notifications, tests and drill, appearance")
                        .clicked()
                    {
                        self.settings = Some(open_settings(&engine));
                    }
                    if ui
                        .button("❔  Guide")
                        .on_hover_text("How to read the screen, set up your phone, troubleshoot")
                        .clicked()
                    {
                        self.guide_open = true;
                    }
                    // The drill keeps running after Settings closes: it must stay visible and stoppable from the main page
                    if let Some(status) = engine.drill_status() {
                        let stop = egui::Button::new(
                            RichText::new(format!("⏹  {status}")).color(p().amber),
                        );
                        if ui.add(stop).clicked() {
                            engine.stop_drill();
                        }
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            RichText::new(engine.check_status())
                                .color(p().muted)
                                .small(),
                        );
                    });
                });
            });

        egui::TopBottomPanel::bottom("footer")
            .frame(
                Frame::new()
                    .fill(p().panel)
                    .inner_margin(Margin::symmetric(16, 6))
                    .stroke(Stroke::new(1.0_f32, p().border)),
            )
            .show(context, |ui| {
                let (text, color) = engine.footer();
                ui.label(RichText::new(text).small().color(color));
            });

        egui::TopBottomPanel::bottom("events")
            .resizable(true)
            .default_height(150.0)
            .min_height(100.0)
            .frame(
                Frame::new()
                    .fill(p().panel)
                    .inner_margin(Margin::symmetric(16, 10))
                    .stroke(Stroke::new(1.0_f32, p().border)),
            )
            .show(context, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Events").semibold());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Clear").clicked() {
                            engine.events.clear();
                        }
                    });
                });
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 4.0;
                        for event in &engine.events {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(
                                    RichText::new(event.stamp.get(11..).unwrap_or(&event.stamp))
                                        .monospace()
                                        .color(p().muted),
                                );
                                dot(ui, event.level.color(), 3.0);
                                ui.label(RichText::new(&event.text).color(
                                    if event.level == Level::Info {
                                        p().text
                                    } else {
                                        event.level.color()
                                    },
                                ));
                            });
                        }
                    });
            });

        let action = egui::CentralPanel::default()
            .frame(Frame::new().fill(p().bg).inner_margin(Margin::same(16)))
            .show(context, |ui| match self.open_game {
                None => hub::library(ui, &mut engine, &self.covers, &mut self.filter),
                Some(game) => hub::game_page(
                    ui,
                    &mut engine,
                    game,
                    self.covers.get(game.id),
                    &self.mvp_art,
                ),
            })
            .inner;
        match action {
            Some(hub::Action::Open(game)) => self.open_game = Some(game),
            Some(hub::Action::Back) => self.open_game = None,
            Some(hub::Action::ChooseCover(game)) => {
                if self.cover_job.is_none() {
                    self.cover_job = Some(hub::choose_cover(game));
                }
            }
            Some(hub::Action::RemoveCover(game)) => match hub::remove_cover_files(game) {
                Ok(()) => {
                    self.covers.remove(game.id);
                    engine.push(Level::Info, format!("Cover removed for {}", game.name));
                }
                Err(error) => engine.push(Level::Warn, format!("Cannot remove cover: {error}")),
            },
            None => {}
        }

        if self.guide_open && onboarding::show_guide(context, &mut self.guide_open) {
            self.guide_open = false;
            self.settings = None;
            self.onboarding = Some(onboarding::Onboarding::new(&engine.config));
        }

        if let Some(mut setup) = self.onboarding.take() {
            if let onboarding::Outcome::Open = onboarding::show(context, &mut setup, &mut engine) {
                self.onboarding = Some(setup);
            }
            return;
        }

        if let Some(mut draft) = self.settings.take() {
            let mut keep_open = true;
            let modal = egui::Modal::new(egui::Id::new("settings")).show(context, |ui| {
                ui.set_width(470.0);
                ui.label(RichText::new("Settings").size(18.0).semibold());
                egui::ScrollArea::vertical()
                    .max_height((context.screen_rect().height() - 150.0).max(200.0))
                    .show(ui, |ui| settings_form(ui, &mut draft, &mut engine));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let save =
                        egui::Button::new(RichText::new("Save").semibold().color(p().on_green))
                            .fill(p().green)
                            .min_size(egui::vec2(96.0, 32.0));
                    if ui.add(save).clicked() {
                        if draft.launch_at_login != launch_at_login() {
                            match set_launch_at_login(draft.launch_at_login) {
                                Ok(()) => engine.push(
                                    Level::Good,
                                    if draft.launch_at_login {
                                        "Gamepulse will open when you sign in to Windows"
                                    } else {
                                        "Gamepulse will no longer open at sign-in"
                                    }
                                    .into(),
                                ),
                                Err(error) => engine.push(
                                    Level::Bad,
                                    format!("Cannot change sign-in launch: {error}"),
                                ),
                            }
                        }
                        match engine.apply_config(draft.config.clone()) {
                            Ok(()) => keep_open = false,
                            Err(error) => {
                                engine.push(Level::Bad, format!("Cannot save settings: {error}"))
                            }
                        }
                    }
                    if ui
                        .add(egui::Button::new("Cancel").min_size(egui::vec2(96.0, 32.0)))
                        .clicked()
                    {
                        keep_open = false;
                    }
                });
            });
            if modal.should_close() {
                keep_open = false;
            }
            if keep_open {
                self.settings = Some(draft);
            }
        }
    }
}

pub fn run() -> Result<(), String> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Gamepulse")
        .with_inner_size([900.0, 780.0])
        .with_min_inner_size([560.0, 460.0]);
    if let Some(rgba) = icon_rgba(64) {
        viewport = viewport.with_icon(egui::IconData {
            rgba,
            width: 64,
            height: 64,
        });
    }
    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Gamepulse",
        options,
        Box::new(|creation| Ok(Box::new(App::new(creation)))),
    )
    .map_err(|e| e.to_string())
}
