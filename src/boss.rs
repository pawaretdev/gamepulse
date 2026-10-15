//! MVP spawn tracker: reads the game's own server-wide announcements (OCR of the message bar
//! at the bottom of the game window) and keeps a respawn timer per MVP.
//!
//! Platform-neutral: the Windows side (`windows::boss_watch`) only turns pixels into text.
//! Every MVP respawns `RESPAWN_SECS` after it dies; the game announces:
//! - death: "Congratulations <team> for defeating MVP <boss> ... MVP Team", once per rewarded team (~15)
//! - 5 minutes before a spawn: "<boss> will appear at <map> in 5 minutes"
//! - the spawn: "<boss> has appeared at <map>"
//! - 30% HP: "<boss> ... Berserk ... HP 30%!"
//!
//! The Thai parts of those messages come out of OCR as noise, so only the English boss/map names
//! and a few markers are used, and names are snapped to `Mvp` entries with an edit-distance match.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// An MVP and the map it spawns on, as the game spells them
#[derive(Debug)]
pub struct Mvp {
    /// Stored in config.json (`TrackedBosses`) and bosses.json; never rename after a release
    pub name: &'static str,
    pub map: &'static str,
}

/// Every MVP respawns one hour after it dies
pub const RESPAWN_SECS: u64 = 3600;
/// The game's own spawn warning comes this long before the spawn
pub const WARNING_SECS: u64 = 300;
/// One death produces ~15 team announcements; one spawn a few lines. Ignore repeats this close together
const REPEAT_SECS: u64 = 600;
/// A predicted spawn found more than this late (e.g. the app was closed) is marked up without an alert
const STALE_SECS: u64 = 300;
/// Nothing can be up this soon after it died; such sightings are misread fragments of other lines
const DEAD_FOR_SURE_SECS: u64 = RESPAWN_SECS - 15 * 60;
/// Of 267 logged kills, 98% died within 170 s of the 30% HP announcement (median 59 s).
/// With no death seen this long after 30%, the death announcement was missed
const LOW_HP_GIVE_UP_SECS: u64 = 300;
/// A missed death is placed this long after the 30% announcement (the median)
const LOW_HP_TO_DEATH_SECS: u64 = 60;

/// Ragnarok Origin Classic field MVPs (from the rooc-kit research data; Dark Priest is a Mini),
/// in the order the MVP list shows them
pub const ROOC_MVPS: &[Mvp] = &[
    Mvp {
        name: "Angeling",
        map: "Poring Island",
    },
    Mvp {
        name: "Golden Thief Bug",
        map: "Culvert 2F",
    },
    Mvp {
        name: "Deviling",
        map: "Poring Island",
    },
    Mvp {
        name: "Orc Lord",
        map: "Orc Village",
    },
    Mvp {
        name: "Maya",
        map: "Ant Hell",
    },
    Mvp {
        name: "Drake",
        map: "Shipwreck Labyrinth",
    },
    Mvp {
        name: "Orc Hero",
        map: "Orc Village",
    },
    Mvp {
        name: "Goblin Leader",
        map: "Goblin Forest",
    },
    Mvp {
        name: "Eddga",
        map: "Deep Payon Forest",
    },
    Mvp {
        name: "Mistress",
        map: "Garden",
    },
    Mvp {
        name: "Osiris",
        map: "Pyramid 3F",
    },
    Mvp {
        name: "Phreeoni",
        map: "Southern Sograt",
    },
    Mvp {
        name: "Moonlight Flower",
        map: "Payon Cave 3F",
    },
    Mvp {
        name: "Dracula",
        map: "Geffen Underground 2F",
    },
    Mvp {
        name: "Doppelganger",
        map: "Geffen Underground 2F",
    },
    Mvp {
        name: "Pharaoh",
        map: "Sphinx Crypt 2F",
    },
    Mvp {
        name: "Baphomet",
        map: "Labyrinth Forest 1F",
    },
    Mvp {
        name: "Dark Lord",
        map: "Catacomb",
    },
    Mvp {
        name: "Owl Baron",
        map: "Glast Heim Castle 2F",
    },
    Mvp {
        name: "Chimera",
        map: "Glast Heim",
    },
    Mvp {
        name: "Bloody Knight",
        map: "Glast Heim Order 2F",
    },
];

/// What one announcement says about an MVP
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sighting {
    Died,
    SpawnSoon,
    Spawned,
    LowHp,
}

/// Lowercase letters and digits only: OCR spacing and punctuation are unreliable
fn letters(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

fn edit_distance(a: &[u8], b: &[u8]) -> usize {
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    let mut current = vec![0; b.len() + 1];
    for (i, &ca) in a.iter().enumerate() {
        current[0] = i + 1;
        for (j, &cb) in b.iter().enumerate() {
            current[j + 1] = (previous[j] + usize::from(ca != cb))
                .min(previous[j + 1] + 1)
                .min(current[j] + 1);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[b.len()]
}

/// Best similarity (0..1) of `needle` against any substring of `haystack` of about its length.
/// OCR drops a letter now and then, so windows one shorter count too, except for short names:
/// there one missing letter is too much ("maa" in noise must not be Maya)
fn best_match(haystack: &[u8], needle: &[u8]) -> f64 {
    if needle.is_empty() || haystack.is_empty() {
        return 0.0;
    }
    let shortest = if needle.len() <= 5 {
        needle.len()
    } else {
        needle.len() - 1
    };
    let mut best = 0.0_f64;
    for length in shortest..=needle.len() + 2 {
        if length > haystack.len() {
            break;
        }
        for start in 0..=haystack.len() - length {
            let distance = edit_distance(&haystack[start..start + length], needle);
            best = best.max(1.0 - distance as f64 / needle.len().max(length) as f64);
        }
    }
    best
}

/// The MVP whose name appears in `text` and how well (about one wrong letter in five allowed)
fn scored_mvp(mvps: &'static [Mvp], text: &str) -> Option<(&'static Mvp, f64)> {
    let haystack = letters(text);
    let mut best: Option<(&'static Mvp, f64)> = None;
    for mvp in mvps {
        let needle = letters(mvp.name);
        let score = best_match(haystack.as_bytes(), needle.as_bytes());
        let needed = if needle.len() <= 5 { 0.75 } else { 0.7 };
        // Ties keep the earlier entry; a full "Dark Lord" beats "rk Lord" in "Orc Lord"
        if score >= needed && best.is_none_or(|(_, s)| score > s + 1e-9) {
            best = Some((mvp, score));
        }
    }
    best
}

fn find_mvp(mvps: &'static [Mvp], text: &str) -> Option<&'static Mvp> {
    scored_mvp(mvps, text).map(|(mvp, _)| mvp)
}

/// MVPs living on the map named in `text` (several MVPs share a map, e.g. Orc Village)
fn on_named_map(mvps: &'static [Mvp], text: &str) -> Vec<&'static Mvp> {
    let haystack = letters(text);
    let mut best: Option<(&'static str, f64)> = None;
    for mvp in mvps {
        let score = best_match(haystack.as_bytes(), letters(mvp.map).as_bytes());
        // "Glast Heim" is part of "Glast Heim Castle 2F": the map covering the most letters wins
        let covered = score * mvp.map.len() as f64;
        if score >= 0.8 && best.is_none_or(|(_, c)| covered > c + 1e-9) {
            best = Some((mvp.map, covered));
        }
    }
    best.map_or_else(Vec::new, |(map, _)| {
        mvps.iter().filter(|m| m.map == map).collect()
    })
}

/// The map a line consisting of just a map name names, as in the game's Event › MVP list
/// ("Poring Island", "Culvert 2F"); longer lines don't count
pub fn map_named(mvps: &'static [Mvp], line: &str) -> Option<&'static str> {
    let haystack = letters(line);
    let mut best: Option<(&'static str, f64)> = None;
    for mvp in mvps {
        let needle = letters(mvp.map);
        if haystack.len() > needle.len() + 3 {
            continue;
        }
        let score = best_match(haystack.as_bytes(), needle.as_bytes());
        if score >= 0.8 && best.is_none_or(|(_, s)| score > s + 1e-9) {
            best = Some((mvp.map, score));
        }
    }
    best.map(|(map, _)| map)
}

/// What a row of the game's Event › MVP list says
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelRow {
    /// The red countdown, in seconds
    RespawnsIn(u64),
    /// No countdown: the row says "refreshed" (in Thai, which OCR can't read), i.e. it is up
    Up,
}

/// Distance between rows of the Event › MVP list, in percent of the list's height
const PANEL_ROW_PITCH: f32 = 18.75;

/// "47:21" → 2841 s (one hour at most). The game always prints two-digit minutes ("03:12"), so
/// "0:27" is a misread "40:27" with its first digit lost
fn countdown(text: &str) -> Option<u64> {
    let (minutes, seconds) = text.trim().split_once(':')?;
    let digits = |s: &str| s.len() == 2 && s.bytes().all(|b| b.is_ascii_digit());
    if !digits(minutes) || !digits(seconds) {
        return None;
    }
    let (minutes, seconds) = (minutes.parse::<u64>().ok()?, seconds.parse::<u64>().ok()?);
    let total = minutes * 60 + seconds;
    (seconds < 60 && (minutes < 60 || total == RESPAWN_SECS)).then_some(total)
}

/// The rows of the Event › MVP list seen in one frame. `names` and `times` are OCR lines with
/// their top in percent of the list height, from the whole list and from the countdown column
/// (read twice at different scales: `times` and `check`, which must agree, since one reading
/// alone turns 27 into 21 now and then); `red` are the tops of red text in that column. The list shows only map names, in `mvps`
/// order, so the visible maps are lined up with that order (Poring Island is Angeling or
/// Deviling depending on its neighbours); nothing is returned unless exactly one position fits.
/// A row is up only when it has red text that OCR reads nothing from (the Thai "refreshed");
/// rows with a misread countdown or nothing red yet (the window still opening) are left out
pub fn read_panel(
    mvps: &'static [Mvp],
    names: &[(f32, String)],
    times: &[(f32, String)],
    check: &[(f32, String)],
    red: &[f32],
) -> Vec<(&'static Mvp, PanelRow)> {
    let maps: Vec<(f32, &str)> = names
        .iter()
        .filter_map(|(top, text)| map_named(mvps, text).map(|map| (*top, map)))
        .collect();
    let Some(&(first, _)) = maps.first() else {
        return vec![];
    };
    let rows: Vec<(usize, f32, &str)> = maps
        .iter()
        .map(|&(top, map)| (((top - first) / PANEL_ROW_PITCH).round() as usize, top, map))
        .collect();
    let fits = |offset: usize| {
        rows.iter()
            .all(|&(row, _, map)| mvps.get(offset + row).is_some_and(|m| m.map == map))
    };
    let offsets: Vec<usize> = (0..mvps.len()).filter(|&o| fits(o)).collect();
    let [offset] = offsets[..] else {
        return vec![];
    };
    rows.iter()
        .filter_map(|&(row, top, _)| {
            let in_row = |t: f32| t >= top - 2.0 && t < top + PANEL_ROW_PITCH * 0.8;
            let texts = |lines: &'_ [(f32, String)]| -> Vec<String> {
                lines
                    .iter()
                    .filter(|(t, _)| in_row(*t))
                    .map(|(_, text)| text.clone())
                    .collect()
            };
            let (first, second) = (texts(times), texts(check));
            let read = |texts: &[String]| texts.iter().find_map(|text| countdown(text));
            let state = match (read(&first), read(&second)) {
                (Some(a), Some(b)) if a == b => PanelRow::RespawnsIn(a),
                // OCR returns nothing at all for the Thai "refreshed"; any text is a misread
                // countdown ("36:oa", "oa•m6" for 03:46)
                (None, None)
                    if first.is_empty() && second.is_empty() && red.iter().any(|&t| in_row(t)) =>
                {
                    PanelRow::Up
                }
                _ => return None,
            };
            Some((&mvps[offset + row], state))
        })
        .collect()
}

/// Which MVP a spawn or HP line is about. The bar scrolls, so the name can be cut off
/// ("'wer Berserk ... Payon Cave 3F" is Moonlight Flower): a doubtful name has to agree with
/// the map, and a map that only one MVP lives on is enough on its own
fn identify(mvps: &'static [Mvp], text: &str) -> Option<&'static Mvp> {
    let name = scored_mvp(mvps, text);
    let on_map = on_named_map(mvps, text);
    match name {
        Some((mvp, score)) if score >= 0.9 => Some(mvp),
        Some((mvp, _)) if on_map.iter().any(|m| std::ptr::eq(*m, mvp)) => Some(mvp),
        _ if on_map.len() == 1 => Some(on_map[0]),
        Some((mvp, score)) if on_map.is_empty() && score >= 0.8 => Some(mvp),
        _ => None,
    }
}

/// Spawn notices always name the map; requiring it drops fragments of other messages
/// ("Drake MW" from a death announcement, "Dracula canyon!" from an invasion)
fn names_map(mvp: &Mvp, text: &str) -> bool {
    best_match(letters(text).as_bytes(), letters(mvp.map).as_bytes()) >= 0.7
}

fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric() && c != '%')
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

/// "MVP" often comes out of OCR as "WP", "NP", "MW" or "EVP", and "Team" as "ream", "loam", "Icam", "learn"
fn is_mvp_word(word: &str) -> bool {
    matches!(
        word,
        "mvp" | "wp" | "np" | "vvp" | "mp" | "mvf" | "mw" | "evp"
    )
}

fn is_team_word(word: &str) -> bool {
    word.len() >= 4
        && word.len() <= 5
        && (word.ends_with("am") || word == "learn")
        && !word.starts_with("ad")
}

/// Whether the bar mentions "MVP" at all; the watcher then also reads its green text
/// (the boss name in death announcements)
pub fn mentions_mvp(text: &str) -> bool {
    words(text).any(|w| is_mvp_word(&w))
}

/// Lines that are not MVP field announcements: Epoch Branch invasions, Lucky Flip, cards
fn is_noise(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "epoch", "branch", "ranch", "nvad", "channel", "lucky", "flip", "card",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

/// The boss named right after an "MVP" ("... defeating MVP Pharaoh and ..."). Other uses of the
/// word, like "MVP Radiant Pass" item announcements, are not followed by a boss name
fn boss_after_mvp(mvps: &'static [Mvp], words: &[String]) -> Option<&'static Mvp> {
    words
        .iter()
        .enumerate()
        .filter(|(_, w)| is_mvp_word(w))
        .find_map(|(i, _)| {
            let next = &words[i + 1..words.len().min(i + 3)];
            next.first()
                .filter(|w| !is_mvp_word(w) && !is_team_word(w))
                .and_then(|_| find_mvp(mvps, &next.join(" ")))
        })
}

/// Interpret one OCR'd message bar. `green` is the OCR of only the green text, which is the
/// boss name in death announcements (team names are white, so they can't be mistaken for a boss)
pub fn parse(mvps: &'static [Mvp], full: &str, green: &str) -> Option<(&'static Mvp, Sighting)> {
    let all: Vec<String> = words(full).collect();
    let mvp_words = all.iter().filter(|w| is_mvp_word(w)).count();
    let team = all
        .iter()
        .skip_while(|w| !is_mvp_word(w))
        .any(|w| is_team_word(w));
    let after = boss_after_mvp(mvps, &all);
    // Only death announcements color the boss name green, so "MVP" plus a green boss name is a
    // death even when OCR splits the white copy of the name ("WP Ploody", "MVP P)aody Knight")
    let green_boss = (mvp_words >= 1).then(|| find_mvp(mvps, green)).flatten();
    // The bar scrolls: one frame may show "MVP <boss>" without "... MVP Team", or the reverse
    if mvp_words >= 2 || (mvp_words == 1 && team) || after.is_some() || green_boss.is_some() {
        let mvp = green_boss.or_else(|| find_mvp(mvps, green)).or(after)?;
        return Some((mvp, Sighting::Died));
    }
    if is_noise(full) {
        return None;
    }
    // "Berserk" is often misread ("Perwk", "geserk"), "30%" less so
    if full.to_ascii_lowercase().contains("erserk") || full.contains("30%") {
        return identify(mvps, full).map(|mvp| (mvp, Sighting::LowHp));
    }
    // "HP 50%", "HP 90%" and other health lines are not spawn notices
    if all.iter().any(|w| w == "hp" || w.contains('%')) {
        return None;
    }
    let mvp = identify(mvps, full).filter(|mvp| names_map(mvp, full))?;
    // "<boss> <map> 5": the Thai "in 5 minutes" leaves a lone 5 (or S) after the names
    let soon = all.iter().any(|w| w == "5" || w == "s");
    Some((
        mvp,
        if soon {
            Sighting::SpawnSoon
        } else {
            Sighting::Spawned
        },
    ))
}
/// Persisted per MVP in bosses.json (unix seconds)
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "PascalCase")]
pub struct Timer {
    pub died_at: Option<u64>,
    /// Expected spawn; None once it is up or when nothing is known
    pub spawn_at: Option<u64>,
    pub up_since: Option<u64>,
    pub low_hp_at: Option<u64>,
    /// `died_at` was guessed from a 30% HP announcement; a real death announcement replaces it
    pub died_estimated: bool,
    /// Alerts already sent for the current `spawn_at`
    pub lead_sent: bool,
    pub spawn_sent: bool,
    /// `up_since` is when the game's MVP list first showed it up, not when it spawned
    pub up_since_unknown: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unknown,
    Waiting {
        spawn_at: u64,
    },
    /// `low_hp_since`: when the game announced it at 30% HP, if it did since it came up.
    /// `since_known` false: `since` is when the MVP list first showed it up; it spawned earlier
    Up {
        since: u64,
        low_hp_since: Option<u64>,
        since_known: bool,
    },
}

/// Something worth telling the user; the Engine decides which ones become alerts
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Notice {
    Died {
        mvp: &'static str,
        spawn_at: u64,
    },
    /// `lead_secs` before the spawn (sooner if the timer only started with the 5-minute warning)
    Soon {
        mvp: &'static str,
        spawn_at: u64,
    },
    /// `announced` = the game said so; otherwise it is the predicted time
    Spawned {
        mvp: &'static str,
        announced: bool,
    },
    LowHp {
        mvp: &'static str,
    },
    /// No death announcement seen `LOW_HP_GIVE_UP_SECS` after 30% HP: assumed dead, timer estimated
    PresumedDead {
        mvp: &'static str,
        spawn_at: u64,
    },
    /// The game's MVP list put the timer right: `spawn_at` None = it is up. `was` is the spawn
    /// time the tracker had (None: unknown or thought up). Only sent for real changes
    Listed {
        mvp: &'static str,
        spawn_at: Option<u64>,
        was: Option<u64>,
    },
}

/// One reading of the MVP list, kept until the next one confirms it
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listed {
    SpawnsAt(u64),
    Up,
}

/// Two MVP list readings this far apart or more don't confirm each other
const LIST_CONFIRM_SECS: u64 = 10;
/// Countdown readings this close agree (frames are not taken on the second)
const LIST_SLACK_SECS: u64 = 3;

/// Timers of one game's MVPs; `timers` is what gets saved (bosses.json)
#[derive(Debug, Clone)]
pub struct Tracker {
    mvps: &'static [Mvp],
    pub timers: BTreeMap<String, Timer>,
    /// Last MVP list reading per MVP and when it was seen (not saved)
    listed: BTreeMap<&'static str, (Listed, u64)>,
}

impl Tracker {
    /// Timers for names no longer in `mvps` are dropped
    pub fn new(mvps: &'static [Mvp], mut timers: BTreeMap<String, Timer>) -> Self {
        timers.retain(|name, _| mvps.iter().any(|m| m.name == name));
        Self {
            mvps,
            timers,
            listed: BTreeMap::new(),
        }
    }

    /// One row of the game's Event › MVP list, read at `now`. The list's countdown is the
    /// server's own clock, so it overrides whatever the announcements gave (including estimates),
    /// once two readings in a row agree: OCR still misreads a frame now and then. Sends no alert
    pub fn list(&mut self, mvp: &'static Mvp, row: PanelRow, now: u64) -> Option<Notice> {
        let seen = match row {
            PanelRow::RespawnsIn(secs) => Listed::SpawnsAt(now + secs),
            PanelRow::Up => Listed::Up,
        };
        let previous = self.listed.insert(mvp.name, (seen, now));
        let confirmed = previous.is_some_and(|(before, at)| {
            now.saturating_sub(at) < LIST_CONFIRM_SECS
                && match (before, seen) {
                    (Listed::SpawnsAt(a), Listed::SpawnsAt(b)) => a.abs_diff(b) <= LIST_SLACK_SECS,
                    (a, b) => a == b,
                }
        });
        if !confirmed {
            return None;
        }
        let timer = self.timers.entry(mvp.name.to_string()).or_default();
        let was = timer.spawn_at;
        match seen {
            Listed::SpawnsAt(at) => {
                if was.is_some_and(|was| was.abs_diff(at) <= LIST_SLACK_SECS)
                    && !timer.died_estimated
                {
                    return None;
                }
                // Already warned about (nearly) this spawn: don't warn again
                let lead_sent = timer.lead_sent && was.is_some_and(|was| was.abs_diff(at) <= 120);
                let estimated = timer.died_estimated;
                *timer = Timer {
                    died_at: Some(at.saturating_sub(RESPAWN_SECS)),
                    spawn_at: Some(at),
                    lead_sent,
                    ..Timer::default()
                };
                // Moves of a few seconds are not worth a line in the log, unless an estimate
                // just became the game's own time
                let moved = was.is_none_or(|was| was.abs_diff(at) > 60);
                (moved || estimated).then_some(Notice::Listed {
                    mvp: mvp.name,
                    spawn_at: Some(at),
                    was,
                })
            }
            Listed::Up => {
                if timer.up_since.is_some() {
                    return None;
                }
                *timer = Timer {
                    up_since: Some(now),
                    up_since_unknown: true,
                    // Its spawn is over: no "should be up now" for an old prediction
                    spawn_sent: true,
                    ..Timer::default()
                };
                Some(Notice::Listed {
                    mvp: mvp.name,
                    spawn_at: None,
                    was,
                })
            }
        }
    }

    pub fn mvps(&self) -> &'static [Mvp] {
        self.mvps
    }

    pub fn status(&self, mvp: &Mvp) -> Status {
        match self.timers.get(mvp.name) {
            Some(Timer {
                up_since: Some(since),
                low_hp_at,
                up_since_unknown,
                ..
            }) => Status::Up {
                since: *since,
                low_hp_since: low_hp_at.filter(|at| at >= since),
                since_known: !up_since_unknown,
            },
            Some(Timer {
                spawn_at: Some(at), ..
            }) => Status::Waiting { spawn_at: *at },
            _ => Status::Unknown,
        }
    }

    /// Apply one announcement seen at `now`, then whatever became due because of it
    pub fn observe(
        &mut self,
        mvp: &'static Mvp,
        sighting: Sighting,
        now: u64,
        lead_secs: u64,
    ) -> Vec<Notice> {
        let timer = self.timers.entry(mvp.name.to_string()).or_default();
        let recent = |at: Option<u64>| at.is_some_and(|at| now.saturating_sub(at) < REPEAT_SECS);
        let just_died = timer
            .died_at
            .is_some_and(|at| now.saturating_sub(at) < DEAD_FOR_SURE_SECS);
        let mut notices = Vec::new();
        match sighting {
            Sighting::SpawnSoon | Sighting::Spawned | Sighting::LowHp
                if just_died && !timer.died_estimated => {}
            Sighting::Died => {
                if !recent(timer.died_at) || timer.died_estimated {
                    *timer = Timer {
                        died_at: Some(now),
                        spawn_at: Some(now + RESPAWN_SECS),
                        ..Timer::default()
                    };
                    notices.push(Notice::Died {
                        mvp: mvp.name,
                        spawn_at: now + RESPAWN_SECS,
                    });
                }
            }
            Sighting::SpawnSoon => {
                let expected = now + WARNING_SECS;
                match timer.spawn_at {
                    // Already counting down to about then: just line up with the game's clock
                    Some(at) if at.abs_diff(expected) <= 180 => timer.spawn_at = Some(expected),
                    _ => {
                        // Its death was missed (or it was thought to be up): start from the warning
                        *timer = Timer {
                            spawn_at: Some(expected),
                            ..Timer::default()
                        };
                    }
                }
            }
            Sighting::Spawned => {
                if !recent(timer.up_since) {
                    let alert = !timer.spawn_sent;
                    timer.up_since = Some(now);
                    timer.spawn_at = None;
                    timer.spawn_sent = true;
                    if alert {
                        notices.push(Notice::Spawned {
                            mvp: mvp.name,
                            announced: true,
                        });
                    }
                }
            }
            Sighting::LowHp => {
                if timer.up_since.is_none() {
                    timer.up_since = Some(now);
                    timer.spawn_at = None;
                    timer.spawn_sent = true;
                }
                if !recent(timer.low_hp_at) {
                    timer.low_hp_at = Some(now);
                    notices.push(Notice::LowHp { mvp: mvp.name });
                }
            }
        }
        notices.extend(self.tick(now, lead_secs));
        notices
    }

    /// Time-based notices: the lead warning and predicted spawns. `lead_secs` 0 = no lead warning
    pub fn tick(&mut self, now: u64, lead_secs: u64) -> Vec<Notice> {
        let mut notices = Vec::new();
        for (name, timer) in self.timers.iter_mut() {
            let Some(name) = self.mvps.iter().find(|m| m.name == name).map(|m| m.name) else {
                continue;
            };
            if let (Some(since), Some(low)) = (timer.up_since, timer.low_hp_at) {
                if low >= since && now >= low + LOW_HP_GIVE_UP_SECS {
                    let died = low + LOW_HP_TO_DEATH_SECS;
                    *timer = Timer {
                        died_at: Some(died),
                        spawn_at: Some(died + RESPAWN_SECS),
                        died_estimated: true,
                        ..Timer::default()
                    };
                    // Found long after the fact (the app was closed): just fix the timer quietly
                    if now - low - LOW_HP_GIVE_UP_SECS < STALE_SECS {
                        notices.push(Notice::PresumedDead {
                            mvp: name,
                            spawn_at: died + RESPAWN_SECS,
                        });
                    }
                }
            }
            let Some(spawn_at) = timer.spawn_at else {
                continue;
            };
            if now >= spawn_at {
                timer.up_since = Some(spawn_at);
                timer.spawn_at = None;
                if !timer.spawn_sent {
                    timer.spawn_sent = true;
                    if now - spawn_at < STALE_SECS {
                        notices.push(Notice::Spawned {
                            mvp: name,
                            announced: false,
                        });
                    }
                }
            } else if lead_secs > 0 && !timer.lead_sent && now + lead_secs >= spawn_at {
                timer.lead_sent = true;
                notices.push(Notice::Soon {
                    mvp: name,
                    spawn_at,
                });
            }
        }
        notices
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mvp(name: &str) -> &'static Mvp {
        ROOC_MVPS.iter().find(|m| m.name == name).unwrap()
    }

    #[test]
    fn mvp_names_are_unique() {
        for (i, a) in ROOC_MVPS.iter().enumerate() {
            assert!(ROOC_MVPS[i + 1..].iter().all(|b| b.name != a.name));
            assert!(!a.map.is_empty());
        }
        assert!(ROOC_MVPS.iter().all(|m| m.name != "Dark Priest"));
        assert_eq!(ROOC_MVPS.len(), 21);
        assert_eq!(
            (ROOC_MVPS[0].name, ROOC_MVPS[20].name),
            ("Angeling", "Bloody Knight")
        );
    }

    /// Real OCR output captured from the game's message bar on 2026-10-14
    #[test]
    fn parses_real_ocr_lines() {
        // (OCR of the whole bar, OCR of its green text, expected reading)
        type Case = (&'static str, &'static str, Option<(&'static str, Sighting)>);
        let cases: &[Case] = &[
            ("Notezell MVP Orc Lordgaddtmeia WP Team", "Orc Lord", Some(("Orc Lord", Sighting::Died))),
            ("ö%ämoOnlyYouKN WP Owl Baron MVP ream", "", Some(("Owl Baron", Sighting::Died))),
            ("o-ntqsvtotl$lu MVP Dracolaua:tailßl$ia MVP Team", "", Some(("Dracula", Sighting::Died))),
            ("IhcXop MVP Orc MVP Team", "Orc Hero", Some(("Orc Hero", Sighting::Died))),
            ("Asaa MVP Dark Lurdua:tailßl$ia MVP Team", "Dark Lord", Some(("Dark Lord", Sighting::Died))),
            ("MVP f lower MVP Team", "Moonlight Flower", Some(("Moonlight Flower", Sighting::Died))),
            // Unreadable boss name: a duplicate announcement, nothing to learn
            ("Egon MVP MVP Team", "Map", None),
            ("Dracula Geffen Underground 2F S", "", Some(("Dracula", Sighting::SpawnSoon))),
            ("Orc Lord Orc Village 5", "", Some(("Orc Lord", Sighting::SpawnSoon))),
            ("Orc Hero Orc Village 5", "", Some(("Orc Hero", Sighting::SpawnSoon))),
            ("Drake Shipwrock labyrinth 5", "", Some(("Drake", Sighting::SpawnSoon))),
            ("Maya Ant Hcll", "", Some(("Maya", Sighting::Spawned))),
            ("Moonlight Flowor Payon Cavo 3F", "", Some(("Moonlight Flower", Sighting::Spawned))),
            ("Orc Hero tallnnof Orc Village", "", Some(("Orc Hero", Sighting::Spawned))),
            ("Mistress Garden", "", Some(("Mistress", Sighting::Spawned))),
            ("[ätJ'aant%7DdI: Dark Lord Berserk tuneqatnu! Ifisnsv HP åD 30%!", "", Some(("Dark Lord", Sighting::LowHp))),
            ("Maya Verserk tgøuqmm! HP äo 30%!", "", Some(("Maya", Sighting::LowHp))),
            ("(ätJLaanY4fiDdI: Moonl(ght Flower taäuml*'äuarmqneqin Berserk tuneqatnu! Ifisno,c HP åD 30%!", "", Some(("Moonlight Flower", Sighting::LowHp))),
            ("Mistress Ifisnsu HP åa GO;'.! ; Ltmesno: Garden", "", None),
            ("Dracula HP 5ö ; i.Jcffcr. lundcrgruond 2f", "", None),
            ("Owl Paron I.Asncg HP 00%! ; Glast \"ciru Castle 2f", "", None),
            ("NodChp Epoch Branch fiqnlmading Dark Lord öönsnt% Glast Heim Order IF! Channel IGIast Heim Order IF(19d. SO)I I", "", None),
            ("Volz art} aestr 7ranch Furtous Tmadtng fddgaöönsn Western Morroc! Channel IWcstcrn Morroc(116. 12)1 1", "", None),
            ("DckJå99 Lucky Flip IS 2 6nmtJ!", "", None),
            ("Poring Island 5", "", None),
            ("Deviling Ifisnsu HP 5a GO • Poring Island", "", None),
            ("(J%äauJ IheKop MVP Osirisuaöitn•gåii WP Team", "Osiris", Some(("Osiris", Sighting::Died))),
            ("TomatoManz MVP Radiant Pass Ollntcl Earrings 121 {Giftable))!!", "", None),
            ("Hccdong Lucky Flip IS 2 6nsno!", "", None),
            // The bar scrolls: names cut off, "MVP Team" not in the frame (2026-10-14 22:31-22:34)
            ("'wer Berserk HP 303. ! fåöbmaamfi* Papn Cave 3F!", "", Some(("Moonlight Flower", Sighting::LowHp))),
            ("Moonlight Flower Berserk HP SOX!", "", Some(("Moonlight Flower", Sighting::LowHp))),
            ("ö%åmt} Reversi MVP Pharaoh'.äEMM1a&", "", Some(("Pharaoh", Sighting::Died))),
            ("CYå9 MVP OrakeuaEMtmc&", "", Some(("Drake", Sighting::Died))),
            ("MVP Drakegaddtmc&", "", Some(("Drake", Sighting::Died))),
            ("öuåmt} 6Kuruu MVP", "", None),
            ("MVP", "", None),
            // Map cut off or too garbled: skipped, the next frames of the same line are clearer
            ("Maya Ant S", "", None),
            ("Maya Ant Holl 5", "", Some(("Maya", Sighting::SpawnSoon))),
            ("Owl Paron Glast Hoim Castlo 2F un", "", Some(("Owl Baron", Sighting::Spawned))),
            ("Owl Paron ö:11nntlG Glast Heim Castle 2F 5", "", Some(("Owl Baron", Sighting::SpawnSoon))),
            ("ght Berserk åD 30%! Glast Heim Order 2F!", "", Some(("Bloody Knight", Sighting::LowHp))),
            ("Perserk tuøuaat1*J! HP iL«äötvnvi 30%! Glast Heim Castle 2f!", "", Some(("Owl Baron", Sighting::LowHp))),
            ("Chimora Glast Hoim", "", Some(("Chimera", Sighting::Spawned))),
            ("Chimera Perwk åö 30%!", "", Some(("Chimera", Sighting::LowHp))),
            ("Mistress 30%!", "", Some(("Mistress", Sighting::LowHp))),
            // The white name split by OCR; the green copy names the boss
            ("Ouåeu DojaCat WP Ploody", "Ploody Knight", Some(("Bloody Knight", Sighting::Died))),
            ("$uånocdPlayz MVP P)aody Knight", "P)oody Knight", Some(("Bloody Knight", Sighting::Died))),
            ("Eddga HPtuecgtgk 90%! atmno: Deep Payon Forest", "", None),
            ("Dracula canyon! Canyon(168. 1", "", None),
            ("68k\"CyberTum Drake MW", "", None),
            ("PabyPK EVP Dra kegatt*mch", "", Some(("Drake", Sighting::Died))),
            ("Southern Sograt tg 5", "", Some(("Phreeoni", Sighting::SpawnSoon))),
            ("Golden Thiof Pug Culvert 25 un", "", Some(("Golden Thief Bug", Sighting::Spawned))),
            ("Poss P)ocks Island Mööt.JN%in!! '•«alinuqofio Blocks IslendJ", "", None),
            ("Poss Bbcks Blocks IslendJ", "", None),
        ];
        for (full, green, expected) in cases {
            let got = parse(ROOC_MVPS, full, green).map(|(m, s)| (m.name, s));
            assert_eq!(got, *expected, "{full:?} / {green:?}");
        }
    }

    #[test]
    fn map_lines_of_the_mvp_list() {
        let map = |line| map_named(ROOC_MVPS, line);
        assert_eq!(map("Poring Island"), Some("Poring Island"));
        assert_eq!(map("Culvert 2F"), Some("Culvert 2F"));
        assert_eq!(map("Glast Heim Castle 2F"), Some("Glast Heim Castle 2F"));
        assert_eq!(map("Glast Heim"), Some("Glast Heim"));
        assert_eq!(map("Ant HelI"), Some("Ant Hell"));
        assert_eq!(map("47:21"), None);
        assert_eq!(map("Lv50"), None);
        // Bar announcements are longer than a map name
        assert_eq!(map("Owl Paron Glast Hoim Castle 2F"), None);
    }

    /// OCR of the Event › MVP window on 2026-10-16 (1094×585 client)
    fn panel_lines(lines: &[(f32, &str)]) -> Vec<(f32, String)> {
        lines.iter().map(|&(t, s)| (t, s.to_string())).collect()
    }

    #[test]
    fn mvp_list_rows_become_countdowns() {
        let names = panel_lines(&[
            (1.0, "Poring Island"),
            (8.0, "O d7:21"),
            (10.0, "lv50"),
            (20.0, "Culvert 2F"),
            (29.0, "Lv52"),
            (39.0, "Poring Island"),
            (46.0, "0 0:53"),
            (48.0, "v5d"),
            (57.0, "Orc Village"),
            (65.0, "0 5617"),
            (76.0, "Ant Hell"),
            (84.0, "JeryQocket"),
            (86.0, "Lv58"),
        ]);
        let times = panel_lines(&[
            (8.0, "47:21"),
            (27.0, "54:51"),
            (45.0, "49:53"),
            (64.0, "56:17"),
        ]);
        // Red text in every row: the four countdowns and Maya's Thai "refreshed"
        let red = [8.0, 27.0, 45.0, 64.0, 89.0];
        let rows: Vec<_> = read_panel(ROOC_MVPS, &names, &times, &times, &red)
            .into_iter()
            .map(|(mvp, row)| (mvp.name, row))
            .collect();
        assert_eq!(
            rows,
            [
                ("Angeling", PanelRow::RespawnsIn(47 * 60 + 21)),
                ("Golden Thief Bug", PanelRow::RespawnsIn(54 * 60 + 51)),
                ("Deviling", PanelRow::RespawnsIn(49 * 60 + 53)),
                ("Orc Lord", PanelRow::RespawnsIn(56 * 60 + 17)),
                ("Maya", PanelRow::Up),
            ]
        );
    }

    /// Misreads seen live on 2026-10-16: none of them may become a time or "up"
    #[test]
    fn doubtful_mvp_list_rows_are_left_out() {
        let names = panel_lines(&[(1.0, "Poring Island"), (20.0, "Culvert 2F")]);
        let rows = |times: &[(f32, &str)], check: &[(f32, &str)], red: &[f32]| {
            read_panel(
                ROOC_MVPS,
                &names,
                &panel_lines(times),
                &panel_lines(check),
                red,
            )
            .into_iter()
            .map(|(mvp, row)| (mvp.name, row))
            .collect::<Vec<_>>()
        };
        // The two readings disagree (27 read as 21 once): only the row they agree on
        assert_eq!(
            rows(
                &[(8.0, "21:20"), (27.0, "54:51")],
                &[(8.0, "27:20"), (27.0, "54:51")],
                &[8.0, 27.0]
            ),
            [("Golden Thief Bug", PanelRow::RespawnsIn(54 * 60 + 51))]
        );
        // A countdown OCR can't make sense of is not "refreshed", even in one reading only
        assert!(rows(&[(8.0, "36:oa")], &[(8.0, "36:oa")], &[8.0]).is_empty());
        assert!(rows(&[], &[(8.0, "oa•m6")], &[8.0]).is_empty());
        // Red text that OCR reads nothing from is the Thai "refreshed"
        assert_eq!(rows(&[], &[], &[8.0]), [("Angeling", PanelRow::Up)]);
        // Nothing red yet: the window is still opening, not "refreshed"
        assert!(rows(&[], &[], &[]).is_empty());
    }

    #[test]
    fn scrolled_mvp_list_lines_up_by_neighbours() {
        // Scrolled by two rows: the first Poring Island is now Deviling
        let names = panel_lines(&[(5.0, "Poring Island"), (24.0, "Orc Village")]);
        let rows = read_panel(ROOC_MVPS, &names, &[], &[], &[12.0, 31.0]);
        assert_eq!(rows[0].0.name, "Deviling");
        assert_eq!(rows[1].0.name, "Orc Lord");
        // One Poring Island alone could be either: nothing
        let alone = panel_lines(&[(5.0, "Poring Island")]);
        assert!(read_panel(ROOC_MVPS, &alone, &[], &[], &[12.0]).is_empty());
    }

    #[test]
    fn countdowns_parse_strictly() {
        assert_eq!(countdown("47:21"), Some(2841));
        assert_eq!(countdown("03:12"), Some(192));
        assert_eq!(countdown("60:00"), Some(3600));
        // "0:27" was a "40:27" whose 4 was lost
        for bad in [
            "0:27", "47:2T", "5a•.5T", "61:00", "4721", "47:2", "36:oa", "47:61",
        ] {
            assert_eq!(countdown(bad), None, "{bad}");
        }
    }

    #[test]
    fn short_names_need_a_close_match() {
        assert!(find_mvp(ROOC_MVPS, "Map").is_none());
        assert!(find_mvp(ROOC_MVPS, "fåöbmaamfi").is_none());
        assert_eq!(find_mvp(ROOC_MVPS, "Mayo").map(|m| m.name), Some("Maya"));
        assert_eq!(
            find_mvp(ROOC_MVPS, "Pharauhua").map(|m| m.name),
            Some("Pharaoh")
        );
    }

    #[test]
    fn death_starts_a_one_hour_timer_and_repeats_are_ignored() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let maya = mvp("Maya");
        let died = tracker.observe(maya, Sighting::Died, 1000, 600);
        assert_eq!(
            died,
            [Notice::Died {
                mvp: "Maya",
                spawn_at: 4600
            }]
        );
        // The other ~14 team announcements
        assert!(tracker.observe(maya, Sighting::Died, 1030, 600).is_empty());
        assert_eq!(tracker.status(maya), Status::Waiting { spawn_at: 4600 });
        assert!(tracker.tick(3999, 600).is_empty());
        assert_eq!(
            tracker.tick(4000, 600),
            [Notice::Soon {
                mvp: "Maya",
                spawn_at: 4600
            }]
        );
        assert!(tracker.tick(4100, 600).is_empty());
        // The game's own 5-minute warning lines up the clock without a second alert
        assert!(tracker
            .observe(maya, Sighting::SpawnSoon, 4290, 600)
            .is_empty());
        assert_eq!(tracker.status(maya), Status::Waiting { spawn_at: 4590 });
        // Announced spawn: one alert, and the predicted spawn doesn't add another
        assert_eq!(
            tracker.observe(maya, Sighting::Spawned, 4592, 600),
            [Notice::Spawned {
                mvp: "Maya",
                announced: true
            }]
        );
        assert!(tracker.tick(4700, 600).is_empty());
        assert_eq!(
            tracker.status(maya),
            Status::Up {
                since: 4592,
                low_hp_since: None,
                since_known: true
            }
        );
    }

    #[test]
    fn nothing_is_up_soon_after_it_died() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let drake = mvp("Drake");
        tracker.observe(drake, Sighting::Died, 1000, 600);
        for sighting in [Sighting::Spawned, Sighting::SpawnSoon, Sighting::LowHp] {
            assert!(tracker.observe(drake, sighting, 1010, 600).is_empty());
        }
        assert_eq!(tracker.status(drake), Status::Waiting { spawn_at: 4600 });
    }

    #[test]
    fn predicted_spawn_alerts_when_the_announcement_is_missed() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let drake = mvp("Drake");
        tracker.observe(drake, Sighting::Died, 0, 0);
        assert!(tracker.tick(3599, 0).is_empty());
        assert_eq!(
            tracker.tick(3601, 0),
            [Notice::Spawned {
                mvp: "Drake",
                announced: false
            }]
        );
        assert_eq!(
            tracker.status(drake),
            Status::Up {
                since: 3600,
                low_hp_since: None,
                since_known: true
            }
        );
    }

    #[test]
    fn warning_without_known_death_starts_the_timer() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let osiris = mvp("Osiris");
        // Lead (10 min) already passed: warn right away
        assert_eq!(
            tracker.observe(osiris, Sighting::SpawnSoon, 100, 600),
            [Notice::Soon {
                mvp: "Osiris",
                spawn_at: 400
            }]
        );
        assert!(tracker
            .observe(osiris, Sighting::SpawnSoon, 110, 600)
            .is_empty());
    }

    #[test]
    fn stale_timers_after_a_restart_do_not_alert() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let dracula = mvp("Dracula");
        tracker.observe(dracula, Sighting::Died, 0, 600);
        assert!(tracker.tick(3600 * 3, 600).is_empty());
        assert_eq!(
            tracker.status(dracula),
            Status::Up {
                since: 3600,
                low_hp_since: None,
                since_known: true
            }
        );
    }

    #[test]
    fn missed_death_after_low_hp_is_estimated_and_a_late_announcement_wins() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let baphomet = mvp("Baphomet");
        tracker.observe(baphomet, Sighting::LowHp, 1000, 600);
        assert!(tracker.tick(1299, 600).is_empty());
        assert_eq!(
            tracker.tick(1300, 600),
            [Notice::PresumedDead {
                mvp: "Baphomet",
                spawn_at: 1060 + RESPAWN_SECS
            }]
        );
        assert_eq!(
            tracker.status(baphomet),
            Status::Waiting {
                spawn_at: 1060 + RESPAWN_SECS
            }
        );
        assert!(tracker.tick(1400, 600).is_empty());
        // The real announcement replaces the estimate, and its repeats are ignored as usual
        assert_eq!(
            tracker.observe(baphomet, Sighting::Died, 1420, 600),
            [Notice::Died {
                mvp: "Baphomet",
                spawn_at: 1420 + RESPAWN_SECS
            }]
        );
        assert!(tracker
            .observe(baphomet, Sighting::Died, 1425, 600)
            .is_empty());
    }

    #[test]
    fn mvp_list_sets_the_timer_once_two_readings_agree() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let eddga = mvp("Eddga");
        // Estimated from a missed death: respawn 4300
        tracker.observe(eddga, Sighting::LowHp, 1000, 600);
        tracker.tick(1300, 0);
        assert_eq!(tracker.status(eddga), Status::Waiting { spawn_at: 4660 });
        // One reading is not enough; the next, 3 s later and 1 s off, confirms it
        assert_eq!(tracker.list(eddga, PanelRow::RespawnsIn(3400), 1500), None);
        assert_eq!(tracker.status(eddga), Status::Waiting { spawn_at: 4660 });
        assert_eq!(
            tracker.list(eddga, PanelRow::RespawnsIn(3397), 1503),
            Some(Notice::Listed {
                mvp: "Eddga",
                spawn_at: Some(4900),
                was: Some(4660)
            })
        );
        assert_eq!(tracker.status(eddga), Status::Waiting { spawn_at: 4900 });
        // Later readings of the same countdown change nothing
        assert_eq!(tracker.list(eddga, PanelRow::RespawnsIn(3395), 1505), None);
        // A death announcement right after is the same death, not a new one
        assert!(tracker.observe(eddga, Sighting::Died, 1510, 600).is_empty());
    }

    #[test]
    fn mvp_list_confirming_an_estimate_is_reported_even_when_close() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let eddga = mvp("Eddga");
        tracker.observe(eddga, Sighting::LowHp, 1000, 600);
        tracker.tick(1300, 0);
        // The estimate (4660) was 14 s off
        tracker.list(eddga, PanelRow::RespawnsIn(2846), 1800);
        assert_eq!(
            tracker.list(eddga, PanelRow::RespawnsIn(2843), 1803),
            Some(Notice::Listed {
                mvp: "Eddga",
                spawn_at: Some(4646),
                was: Some(4660)
            })
        );
        // Now it is the game's time: the same reading again says nothing
        tracker.list(eddga, PanelRow::RespawnsIn(2840), 1806);
        assert_eq!(tracker.list(eddga, PanelRow::RespawnsIn(2837), 1809), None);
    }

    #[test]
    fn mvp_list_readings_that_disagree_change_nothing() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let knight = mvp("Bloody Knight");
        tracker.observe(knight, Sighting::Died, 0, 600);
        // 27:20 read as 21:20 every other frame
        for (now, secs) in [(1000, 1640), (1003, 1277), (1006, 1634), (1009, 1271)] {
            assert_eq!(tracker.list(knight, PanelRow::RespawnsIn(secs), now), None);
        }
        assert_eq!(tracker.status(knight), Status::Waiting { spawn_at: 3600 });
        // Two readings too far apart don't confirm each other either
        assert_eq!(tracker.list(knight, PanelRow::RespawnsIn(1000), 2000), None);
        assert_eq!(tracker.list(knight, PanelRow::RespawnsIn(980), 2020), None);
    }

    #[test]
    fn mvp_list_up_without_a_known_spawn() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let pharaoh = mvp("Pharaoh");
        tracker.list(pharaoh, PanelRow::Up, 500);
        assert_eq!(
            tracker.list(pharaoh, PanelRow::Up, 503),
            Some(Notice::Listed {
                mvp: "Pharaoh",
                spawn_at: None,
                was: None
            })
        );
        assert_eq!(
            tracker.status(pharaoh),
            Status::Up {
                since: 503,
                low_hp_since: None,
                since_known: false
            }
        );
        assert_eq!(tracker.list(pharaoh, PanelRow::Up, 506), None);
        // Up as predicted: the list agrees and keeps the known spawn time
        let drake = mvp("Drake");
        tracker.observe(drake, Sighting::Died, 0, 0);
        tracker.tick(3700, 0);
        tracker.list(drake, PanelRow::Up, 3800);
        assert_eq!(tracker.list(drake, PanelRow::Up, 3803), None);
        assert_eq!(
            tracker.status(drake),
            Status::Up {
                since: 3600,
                low_hp_since: None,
                since_known: true
            }
        );
    }

    #[test]
    fn old_low_hp_found_after_a_restart_is_fixed_quietly() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let maya = mvp("Maya");
        tracker.observe(maya, Sighting::LowHp, 1000, 600);
        assert!(tracker.tick(1000 + 3 * 3600, 600).is_empty());
    }

    #[test]
    fn low_hp_means_it_is_up_and_alerts_once() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        let lord = mvp("Dark Lord");
        assert_eq!(
            tracker.observe(lord, Sighting::LowHp, 50, 600),
            [Notice::LowHp { mvp: "Dark Lord" }]
        );
        assert!(tracker.observe(lord, Sighting::LowHp, 52, 600).is_empty());
        assert_eq!(
            tracker.status(lord),
            Status::Up {
                since: 50,
                low_hp_since: Some(50),
                since_known: true
            }
        );
    }

    #[test]
    fn tracker_round_trips_through_json() {
        let mut tracker = Tracker::new(ROOC_MVPS, BTreeMap::new());
        tracker.observe(mvp("Maya"), Sighting::Died, 10, 600);
        tracker
            .timers
            .insert("Removed Boss".into(), Timer::default());
        let json = serde_json::to_string(&tracker.timers).unwrap();
        assert!(json.contains("\"Maya\"") && json.contains("\"SpawnAt\":3610"));
        let loaded = Tracker::new(ROOC_MVPS, serde_json::from_str(&json).unwrap());
        assert_eq!(loaded.timers.len(), 1);
        assert_eq!(
            loaded.status(mvp("Maya")),
            Status::Waiting { spawn_at: 3610 }
        );
    }
}
