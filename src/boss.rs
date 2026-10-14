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
    // The bar scrolls: one frame may show "MVP <boss>" without "... MVP Team", or the reverse
    if mvp_words >= 2 || (mvp_words == 1 && team) || after.is_some() {
        let mvp = find_mvp(mvps, green).or(after)?;
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
    /// Alerts already sent for the current `spawn_at`
    pub lead_sent: bool,
    pub spawn_sent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Unknown,
    Waiting {
        spawn_at: u64,
    },
    /// `low_hp`: the game announced it at 30% HP since it came up
    Up {
        since: u64,
        low_hp: bool,
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
}

/// Timers of one game's MVPs; `timers` is what gets saved (bosses.json)
#[derive(Debug, Clone)]
pub struct Tracker {
    mvps: &'static [Mvp],
    pub timers: BTreeMap<String, Timer>,
}

impl Tracker {
    /// Timers for names no longer in `mvps` are dropped
    pub fn new(mvps: &'static [Mvp], mut timers: BTreeMap<String, Timer>) -> Self {
        timers.retain(|name, _| mvps.iter().any(|m| m.name == name));
        Self { mvps, timers }
    }

    pub fn mvps(&self) -> &'static [Mvp] {
        self.mvps
    }

    pub fn status(&self, mvp: &Mvp) -> Status {
        match self.timers.get(mvp.name) {
            Some(Timer {
                up_since: Some(since),
                low_hp_at,
                ..
            }) => Status::Up {
                since: *since,
                low_hp: low_hp_at.is_some_and(|at| at >= *since),
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
            Sighting::SpawnSoon | Sighting::Spawned | Sighting::LowHp if just_died => {}
            Sighting::Died => {
                if !recent(timer.died_at) {
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
            let Some(spawn_at) = timer.spawn_at else {
                continue;
            };
            let Some(name) = self.mvps.iter().find(|m| m.name == name).map(|m| m.name) else {
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
                low_hp: false
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
                low_hp: false
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
                low_hp: false
            }
        );
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
                low_hp: true
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
