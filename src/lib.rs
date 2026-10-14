use serde::{Deserialize, Serialize};
use std::net::IpAddr;

pub mod boss;

#[cfg(windows)]
pub mod notify;

pub mod preview;

#[cfg(windows)]
pub mod windows;

#[cfg(windows)]
pub mod gui;

/// FILETIME ticks (100 ns since 1601-01-01 UTC), preserved without rounding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct Identity {
    pub pid: u32,
    pub started_filetime: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Connection {
    pub local: String,
    pub remote: String,
    pub created_filetime: i64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Client {
    /// `GameProfile::id` of the game this process belongs to
    pub game: &'static str,
    pub identity: Identity,
    pub connections: Vec<Connection>,
    pub windows: Vec<Window>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Window {
    pub hwnd: usize,
    pub minimized: bool,
    /// Always on top (`WS_EX_TOPMOST`), set by "Pin on top" or by the game itself
    pub topmost: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Snapshot {
    pub schema_version: u32,
    pub sampled_unix_ms: u128,
    pub clients: Vec<Client>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default, rename_all = "PascalCase")]
pub struct Config {
    pub ntfy_topic: String,
    pub ntfy_server: String,
    pub webhook_url: String,
    pub telegram_token: String,
    pub telegram_chat_id: String,
    pub interval_sec: u64,
    pub grace_checks: u32,
    pub repeat_alert_min: u64,
    pub max_repeats: u32,
    pub heartbeat_hours: f64,
    pub burst_count: u32,
    pub burst_gap_sec: u64,
    pub discord_here: bool,
    pub sound: bool,
    pub auto_start: bool,
    pub attach_desktop_screenshot: bool,
    pub theme: String,
    /// `GameProfile::id`s of watched games (only these alert); unknown ids are ignored
    pub watched_games: Vec<String>,
    /// Whether first-run setup is done (old configs without this key = false)
    pub onboarded: bool,
    /// MVP tracker running (reads the game window every second; off = no capture, no CPU)
    pub boss_tracker: bool,
    /// `boss::Mvp::name`s that alert; unknown names are ignored
    pub tracked_bosses: Vec<String>,
    /// Minutes before a predicted spawn to warn; 0 = off
    pub boss_lead_min: u64,
    pub boss_alert_spawn: bool,
    pub boss_alert_death: bool,
    pub boss_alert_low_hp: bool,
    /// MVP alerts go to the Discord webhook and/or a Windows notification, not to ntfy/Telegram
    pub boss_discord: bool,
    pub boss_toast: bool,
    /// MVP route window lists every MVP instead of only the ticked ones
    pub boss_route_all: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            ntfy_topic: String::new(),
            ntfy_server: "https://ntfy.sh".into(),
            webhook_url: String::new(),
            telegram_token: String::new(),
            telegram_chat_id: String::new(),
            interval_sec: 30,
            grace_checks: 3,
            repeat_alert_min: 3,
            max_repeats: 20,
            heartbeat_hours: 4.0,
            burst_count: 4,
            burst_gap_sec: 4,
            discord_here: true,
            sound: false,
            auto_start: true,
            attach_desktop_screenshot: false,
            theme: "Midnight".into(),
            watched_games: vec![GAMES[0].id.into()],
            onboarded: false,
            boss_tracker: false,
            tracked_bosses: GAMES
                .iter()
                .flat_map(|g| g.mvps)
                .map(|m| m.name.to_string())
                .collect(),
            boss_lead_min: 10,
            boss_alert_spawn: true,
            boss_alert_death: false,
            boss_alert_low_hp: false,
            boss_discord: true,
            boss_toast: true,
            boss_route_all: false,
        }
    }
}

impl Config {
    pub fn watches(&self, id: &str) -> bool {
        self.watched_games.iter().any(|watched| watched == id)
    }

    /// Watched games, in `GAMES` order
    pub fn watched(&self) -> Vec<&'static GameProfile> {
        GAMES.iter().filter(|g| self.watches(g.id)).collect()
    }

    pub fn tracks_boss(&self, name: &str) -> bool {
        self.tracked_bosses.iter().any(|tracked| tracked == name)
    }

    pub fn set_boss_tracked(&mut self, name: &str, on: bool) {
        self.tracked_bosses.retain(|tracked| tracked != name);
        if on {
            self.tracked_bosses.push(name.to_string());
        }
    }

    /// Turn watching a game on/off, keeping `GAMES` order and dropping unknown ids
    pub fn set_watched(&mut self, id: &str, on: bool) {
        self.watched_games = GAMES
            .iter()
            .filter(|g| if g.id == id { on } else { self.watches(g.id) })
            .map(|g| g.id.to_string())
            .collect();
    }
}

impl Snapshot {
    pub fn clients_of<'a>(&'a self, game: &'a str) -> impl Iterator<Item = &'a Client> + 'a {
        self.clients.iter().filter(move |c| c.game == game)
    }

    /// (with a game socket, total) for that game
    pub fn counts(&self, game: &str) -> (usize, usize) {
        let (mut online, mut total) = (0, 0);
        for client in self.clients_of(game) {
            total += 1;
            online += usize::from(!client.connections.is_empty());
        }
        (online, total)
    }

    /// Incomplete observations must never become an online / no-process verdict.
    pub fn exit_code(&self) -> u8 {
        if !self.errors.is_empty() {
            3
        } else if self.clients.is_empty() {
            2
        } else if self.clients.iter().any(|c| c.connections.is_empty()) {
            1
        } else {
            0
        }
    }
}

/// Supported games, pure data: adding a game = adding a row here, no separate logic.
/// Test with the real game first that the "Established TCP not on an ignored_ports port" rule holds
#[derive(Debug)]
pub struct GameProfile {
    /// Stored in config.json; never change it after a release
    pub id: &'static str,
    /// Full name shown in the UI and alert text
    pub name: &'static str,
    /// Process file name (case-insensitive)
    pub exe: &'static str,
    /// Remote ports not counted as game connections (web/launcher/patch)
    pub ignored_ports: &'static [u16],
    /// Field MVPs for the MVP tracker; empty = the game has no tracker
    pub mvps: &'static [boss::Mvp],
}

pub const GAMES: &[GameProfile] = &[GameProfile {
    id: "rooc",
    name: "Ragnarok Origin Classic",
    exe: "rooc.exe",
    ignored_ports: &[80, 443, 8080],
    mvps: boss::ROOC_MVPS,
}];

pub fn find_game(id: &str) -> Option<&'static GameProfile> {
    GAMES.iter().find(|g| g.id == id)
}

/// Match the legacy address filter, with explicit unspecified/mapped IPv6 handling.
pub fn is_game_connection(
    established: bool,
    address: IpAddr,
    port: u16,
    ignored_ports: &[u16],
) -> bool {
    let address = match address {
        IpAddr::V6(ip) => ip.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(address),
        _ => address,
    };
    established
        && !address.is_loopback()
        && !address.is_unspecified()
        && !ignored_ports.contains(&port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filters_only_established_non_web_non_loopback_connections() {
        let ignored = find_game("rooc").unwrap().ignored_ports;
        for address in [
            "127.0.0.1",
            "127.2.3.4",
            "0.0.0.0",
            "::1",
            "::",
            "::ffff:127.0.0.1",
            "::ffff:0.0.0.0",
        ] {
            assert!(!is_game_connection(
                true,
                address.parse().unwrap(),
                10011,
                ignored
            ));
        }
        for address in [
            "192.0.2.12",
            "2001:db8::12",
            "::ffff:192.0.2.12",
            "10.0.0.2",
        ] {
            let ip = address.parse().unwrap();
            assert!(is_game_connection(true, ip, 10011, ignored));
            assert!(!is_game_connection(false, ip, 10011, ignored));
            for &port in ignored {
                assert!(!is_game_connection(true, ip, port, ignored));
            }
        }
    }

    #[test]
    fn game_profiles_are_unique_and_watch_list_ignores_unknown_ids() {
        for (index, profile) in GAMES.iter().enumerate() {
            assert!(!profile.name.is_empty() && profile.exe.ends_with(".exe"));
            assert!(GAMES[index + 1..]
                .iter()
                .all(|other| other.id != profile.id));
        }
        assert_eq!(find_game("rooc").unwrap().name, "Ragnarok Origin Classic");
        assert!(find_game("removed-game").is_none());
        // Old configs without the key = watch Ragnarok Origin Classic as before
        let old: Config = serde_json::from_str(r#"{"NtfyTopic":"t"}"#).unwrap();
        assert_eq!(old.watched_games, [GAMES[0].id]);
        let mut config: Config =
            serde_json::from_str(r#"{"WatchedGames":["removed-game","rooc"]}"#).unwrap();
        assert_eq!(config.watched().len(), 1);
        config.set_watched("rooc", false);
        assert!(config.watched_games.is_empty() && !config.watches("rooc"));
        config.set_watched("rooc", true);
        assert_eq!(config.watched_games, ["rooc"]);
    }

    #[test]
    fn old_configs_get_the_mvp_tracker_off_with_every_mvp_tracked() {
        let mut config: Config = serde_json::from_str(r#"{"NtfyTopic":"t"}"#).unwrap();
        assert!(!config.boss_tracker);
        assert_eq!(config.tracked_bosses.len(), boss::ROOC_MVPS.len());
        assert!(config.tracks_boss("Maya"));
        config.set_boss_tracked("Maya", false);
        assert!(!config.tracks_boss("Maya"));
        config.set_boss_tracked("Maya", true);
        config.set_boss_tracked("Maya", true);
        assert_eq!(
            config
                .tracked_bosses
                .iter()
                .filter(|n| *n == "Maya")
                .count(),
            1
        );
    }

    #[test]
    fn once_verdict_includes_all_clients_and_preserves_unknown() {
        let mut s = Snapshot {
            schema_version: 2,
            sampled_unix_ms: 0,
            clients: vec![],
            errors: vec![],
        };
        assert_eq!(s.exit_code(), 2);
        s.clients.push(Client {
            game: "rooc",
            identity: Identity {
                pid: 10,
                started_filetime: 1,
            },
            connections: vec![],
            windows: vec![],
        });
        assert_eq!(s.exit_code(), 1);
        s.clients[0].connections.push(Connection {
            local: "local".into(),
            remote: "remote".into(),
            created_filetime: 1,
        });
        assert_eq!(s.exit_code(), 0);
        s.clients.push(Client {
            game: "rooc",
            identity: Identity {
                pid: 20,
                started_filetime: 2,
            },
            connections: vec![],
            windows: vec![],
        });
        assert_eq!(s.exit_code(), 1);
        s.errors.push("TCP query failed".into());
        assert_eq!(s.exit_code(), 3);
        s.clients.clear();
        assert_eq!(s.exit_code(), 3);
    }
}
