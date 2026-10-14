//! First-run setup (onboarding) and the in-app Guide
//!
//! The most common mistake is a phone that "receives" but doesn't "wake you", so a test send is required before finishing,
//! and the test step includes phone-side tips for both Android and iOS
use super::*;
use std::hash::{BuildHasher, Hasher};

#[derive(Clone, Copy, PartialEq)]
enum Channel {
    Ntfy,
    Discord,
    Telegram,
}

pub(super) struct Onboarding {
    step: usize,
    draft: Config,
    channel: Channel,
    launch_at_login: bool,
    /// seq of the latest event when Send was pressed: only results after it are shown
    test_sent_at: Option<u64>,
    no_buzz: bool,
}

pub(super) enum Outcome {
    Open,
    Finished,
    Skipped,
}

const STEPS: [&str; 4] = ["Welcome", "Alerts", "Test", "Done"];

/// ntfy.sh topics are public and must be hard to guess: 64 bits from RandomState (seeded randomly by the OS)
fn random_topic() -> String {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos(),
    );
    format!("gamepulse-{:016x}", hasher.finish())
}

impl Onboarding {
    pub(super) fn new(config: &Config) -> Self {
        let channel = if !config.webhook_url.trim().is_empty() {
            Channel::Discord
        } else if !config.telegram_token.trim().is_empty() {
            Channel::Telegram
        } else {
            Channel::Ntfy
        };
        Self {
            step: 0,
            draft: config.clone(),
            channel,
            launch_at_login: true,
            test_sent_at: None,
            no_buzz: false,
        }
    }
}

fn bullet(ui: &mut Ui, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("•").color(p().blue));
        ui.label(text);
    });
}

fn numbered(ui: &mut Ui, number: usize, text: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(
            RichText::new(format!("{number}."))
                .color(p().blue)
                .semibold(),
        );
        ui.label(text);
    });
}

fn progress(ui: &mut Ui, step: usize) {
    ui.horizontal(|ui| {
        for (index, name) in STEPS.iter().enumerate() {
            let color = if index <= step { p().blue } else { p().muted };
            dot(ui, color, 4.0);
            let text = RichText::new(*name).small().color(color);
            ui.label(if index == step { text.semibold() } else { text });
            if index + 1 < STEPS.len() {
                ui.add_space(6.0);
            }
        }
    });
}

fn primary(ui: &mut Ui, text: &str, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new(text).semibold().color(p().on_green))
        .fill(p().green)
        .min_size(egui::vec2(120.0, 34.0));
    ui.add_enabled(enabled, button).clicked()
}

pub(super) fn android_tips(ui: &mut Ui) {
    numbered(
        ui,
        1,
        "ntfy › tap your topic › Notification settings › turn on Dedicated channel.",
    );
    numbered(
        ui,
        2,
        "Open that channel: Importance = Urgent, vibration on, pick a loud sound.",
    );
    numbered(
        ui,
        3,
        "Settings › Sound & vibration › Do Not Disturb › Apps › allow ntfy (and Discord).",
    );
    numbered(
        ui,
        4,
        "Settings › Apps › ntfy › Battery › Unrestricted, or Android delays alerts at night.",
    );
    hint(
        ui,
        "Steps 3 and 4 matter most. Without them the alert arrives silently.",
        p().amber,
    );
}

pub(super) fn ios_tips(ui: &mut Ui) {
    numbered(ui, 1, "Settings › Notifications › ntfy (or Discord): allow notifications and sounds, banner style Persistent.");
    numbered(ui, 2, "Settings › Focus › Sleep › Apps: allow ntfy and Discord. Otherwise Sleep Focus silences everything.");
    numbered(
        ui,
        3,
        "Turn on Time Sensitive Notifications if the app offers it.",
    );
    numbered(
        ui,
        4,
        "Check the Silent switch and turn off Low Power Mode at night.",
    );
    hint(
        ui,
        "iOS vibrates once per notification, so raise \"Notifications per alert\" in Settings to buzz more.",
        p().amber,
    );
}

fn step_welcome(ui: &mut Ui) {
    let names: Vec<_> = GAMES.iter().map(|g| g.name).collect();
    ui.label(RichText::new("Welcome to Gamepulse").size(20.0).semibold());
    ui.label(
        "Gamepulse watches your game clients and alerts your phone when one disconnects, \
         so you can sleep while your characters stay logged in.",
    );
    hint(ui, &format!("Supported: {}.", names.join(", ")), p().muted);
    ui.add_space(6.0);
    bullet(
        ui,
        "Every 30 seconds it checks that each game client still holds its game-server connection.",
    );
    bullet(
        ui,
        "It never reads game memory, clicks anything, or touches your account.",
    );
    bullet(
        ui,
        "Closing the window keeps it watching from the system tray.",
    );
    ui.add_space(6.0);
    hint(
        ui,
        "Setup takes about two minutes. You need your phone.",
        p().muted,
    );
}

fn step_alerts(ui: &mut Ui, ob: &mut Onboarding) {
    ui.label(
        RichText::new("Where should alerts go?")
            .size(20.0)
            .semibold(),
    );
    ui.label("Pick at least one. You can add more later in Settings.");
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        for (channel, name, set) in [
            (
                Channel::Ntfy,
                "ntfy app",
                !ob.draft.ntfy_topic.trim().is_empty(),
            ),
            (
                Channel::Discord,
                "Discord",
                !ob.draft.webhook_url.trim().is_empty(),
            ),
            (
                Channel::Telegram,
                "Telegram",
                !ob.draft.telegram_token.trim().is_empty()
                    && !ob.draft.telegram_chat_id.trim().is_empty(),
            ),
        ] {
            let label = if set {
                format!("✔ {name}")
            } else {
                name.to_string()
            };
            ui.selectable_value(&mut ob.channel, channel, label);
        }
    });
    card_frame().show(ui, |ui| {
        ui.set_width(ui.available_width());
        match ob.channel {
            Channel::Ntfy => {
                hint(ui, "Recommended: free, no account, works on Android and iOS.", p().green);
                numbered(ui, 1, "Install the ntfy app from the Play Store or App Store.");
                numbered(ui, 2, "Create a private topic name here, then tap + in the app and subscribe to exactly the same name.");
                ui.horizontal(|ui| {
                    text_field(ui, &mut ob.draft.ntfy_topic, false, "your-private-topic");
                    if ui.button("Generate").clicked() {
                        ob.draft.ntfy_topic = random_topic();
                    }
                    if ui
                        .add_enabled(!ob.draft.ntfy_topic.trim().is_empty(), egui::Button::new("Copy"))
                        .clicked()
                    {
                        ui.ctx().copy_text(ob.draft.ntfy_topic.trim().to_string());
                    }
                });
                hint(
                    ui,
                    "ntfy.sh is public: anyone who knows the topic can read your alerts. Keep it secret.",
                    p().amber,
                );
            }
            Channel::Discord => {
                numbered(ui, 1, "In your Discord server: Server Settings › Integrations › Webhooks › New Webhook.");
                numbered(ui, 2, "Choose the channel for alerts, then Copy Webhook URL and paste it below.");
                text_field(
                    ui,
                    &mut ob.draft.webhook_url,
                    true,
                    "https://discord.com/api/webhooks/...",
                );
                ui.checkbox(&mut ob.draft.discord_here, "Mention @here on real disconnects");
                hint(
                    ui,
                    "Make sure Discord notifications for that channel are on in the phone app.",
                    p().muted,
                );
            }
            Channel::Telegram => {
                numbered(ui, 1, "Message @BotFather, send /newbot and paste the bot token below.");
                numbered(ui, 2, "Send any message to your new bot.");
                numbered(ui, 3, "Open api.telegram.org/bot<token>/getUpdates in a browser and copy chat.id.");
                egui::Grid::new("onboard-telegram")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        row(ui, "Bot token", |ui| {
                            text_field(ui, &mut ob.draft.telegram_token, true, "")
                        });
                        row(ui, "Chat ID", |ui| {
                            text_field(ui, &mut ob.draft.telegram_chat_id, false, "")
                        });
                    });
                hint(ui, "Telegram alerts cannot bypass silent mode, so ntfy wakes you more reliably.", p().muted);
            }
        }
    });
}

fn step_test(ui: &mut Ui, ob: &mut Onboarding, engine: &mut Engine) {
    ui.label(
        RichText::new("Make sure it reaches your phone")
            .size(20.0)
            .semibold(),
    );
    ui.label(format!(
        "This sends one urgent alert ({} notifications, {} s apart) to every channel you set up.",
        ob.draft.burst_count.max(1),
        ob.draft.burst_gap_sec.max(1)
    ));
    ui.add_space(4.0);
    let label = if ob.test_sent_at.is_some() {
        "🔔  Send again"
    } else {
        "🔔  Send test alert"
    };
    if ui
        .add(egui::Button::new(label).min_size(egui::vec2(170.0, 34.0)))
        .clicked()
    {
        ob.test_sent_at = Some(engine.next_seq);
        ob.no_buzz = false;
        engine.test_alert(ob.draft.clone());
    }
    if let Some(since) = ob.test_sent_at {
        // Real send results from notify (delivered / HTTP xxx / timeout) arrive via Events
        let results: Vec<_> = engine
            .events
            .iter()
            .filter(|event| event.seq > since && event.text.contains(": "))
            .collect();
        card_frame().show(ui, |ui| {
            ui.set_width(ui.available_width());
            if results.is_empty() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Sending...").color(p().muted));
                });
            }
            for event in results {
                ui.horizontal_wrapped(|ui| {
                    dot(ui, event.level.color(), 3.0);
                    ui.label(RichText::new(&event.text).color(event.level.color()));
                });
            }
        });
        ui.add_space(4.0);
        ui.label(RichText::new("Did your phone buzz?").semibold());
        ui.horizontal(|ui| {
            if primary(ui, "Yes, it did", true) {
                ob.step = 3;
            }
            if ui
                .add(egui::Button::new("No").min_size(egui::vec2(80.0, 34.0)))
                .clicked()
            {
                ob.no_buzz = true;
            }
        });
        if ob.no_buzz {
            hint(
                ui,
                "If a line above says \"delivered\", the alert reached the service and your phone settings are blocking it. \
                 Otherwise go Back and check the channel details.",
                p().amber,
            );
        }
    }
    ui.add_space(4.0);
    egui::CollapsingHeader::new("Android: make it wake you up")
        .default_open(ob.no_buzz)
        .show(ui, android_tips);
    egui::CollapsingHeader::new("iPhone: make it wake you up")
        .default_open(ob.no_buzz)
        .show(ui, ios_tips);
}

fn step_done(ui: &mut Ui, ob: &mut Onboarding) {
    ui.label(RichText::new("You're all set").size(20.0).semibold());
    ui.checkbox(
        &mut ob.launch_at_login,
        "Open Gamepulse when I sign in to Windows",
    );
    ui.checkbox(&mut ob.draft.auto_start, "Start watching automatically");
    ui.add_space(6.0);
    bullet(ui, "Close the window to keep watching from the tray. Right-click the tray icon › Exit to quit.");
    bullet(ui, "Before you sleep, run a drill (Settings › Test your setup) to prove the alerts wake you up.");
    bullet(ui, "Set Windows Power & sleep to Never. A sleeping PC stops the game and the monitor together.");
    bullet(ui, "Open Guide from the main screen any time.");
}

/// Returns an Outcome so App decides whether to close the modal; saving/starting monitoring happens here
pub(super) fn show(context: &egui::Context, ob: &mut Onboarding, engine: &mut Engine) -> Outcome {
    let mut outcome = Outcome::Open;
    egui::Modal::new(egui::Id::new("onboarding")).show(context, |ui| {
        ui.set_width(540.0);
        progress(ui, ob.step);
        ui.add_space(8.0);
        egui::ScrollArea::vertical()
            .max_height((context.screen_rect().height() - 170.0).max(220.0))
            .show(ui, |ui| match ob.step {
                0 => step_welcome(ui),
                1 => step_alerts(ui, ob),
                2 => step_test(ui, ob, engine),
                _ => step_done(ui, ob),
            });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ob.step > 0 && ui.button("Back").clicked() {
                ob.step -= 1;
            }
            if ui
                .button(RichText::new("Skip setup").color(p().muted))
                .on_hover_text("Set up later from Settings")
                .clicked()
            {
                outcome = Outcome::Skipped;
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| match ob.step {
                0 => {
                    if primary(ui, "Get started", true) {
                        ob.step = 1;
                    }
                }
                1 => {
                    let ready = !notify::channels(&ob.draft).is_empty();
                    if primary(ui, "Next", ready) {
                        ob.step = 2;
                    }
                    if !ready {
                        ui.label(
                            RichText::new("Fill in one channel to continue")
                                .small()
                                .color(p().muted),
                        );
                    }
                }
                2 => {
                    if ui.button("I'll test later").clicked() {
                        ob.step = 3;
                    }
                }
                _ => {
                    if primary(ui, "Finish", true) {
                        outcome = Outcome::Finished;
                    }
                }
            });
        });
    });
    match outcome {
        Outcome::Finished => {
            ob.draft.onboarded = true;
            if let Err(error) = engine.apply_config(ob.draft.clone()) {
                engine.push(Level::Bad, format!("Cannot save settings: {error}"));
                return Outcome::Open;
            }
            if ob.launch_at_login != launch_at_login() {
                if let Err(error) = set_launch_at_login(ob.launch_at_login) {
                    engine.push(Level::Bad, format!("Cannot change sign-in launch: {error}"));
                }
            }
            if ob.draft.auto_start && !engine.watching {
                engine.start();
            }
        }
        Outcome::Skipped => mark_onboarded(engine),
        Outcome::Open => {}
    }
    outcome
}

/// Don't show onboarding again, but don't save any other values left half-filled
pub(super) fn mark_onboarded(engine: &mut Engine) {
    engine.config.onboarded = true;
    if let Err(error) = save_config(&engine.config) {
        engine.push(Level::Warn, format!("Cannot save settings: {error}"));
    }
}

fn guide_section(ui: &mut Ui, title: &str, open: bool, add: impl FnOnce(&mut Ui)) {
    egui::CollapsingHeader::new(RichText::new(title).semibold())
        .default_open(open)
        .show(ui, add);
}

fn legend(ui: &mut Ui, text: &str, color: Color32, meaning: &str) {
    ui.horizontal_wrapped(|ui| {
        pill(ui, text, color);
        ui.label(meaning);
    });
}

/// Guide window (not modal, can stay open beside the main window); returns true if "Run setup again" was clicked
pub(super) fn show_guide(context: &egui::Context, open: &mut bool) -> bool {
    let mut rerun = false;
    egui::Window::new("Guide")
        .open(open)
        .default_size([560.0, 520.0])
        .collapsible(false)
        .show(context, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                guide_section(ui, "The library", true, |ui| {
                    bullet(ui, "One card per supported game. The card shows its live status even when alerts are off.");
                    bullet(ui, "The Alerts switch on a card turns alerts for that game on or off. Turn on several cards to watch several games at once.");
                    bullet(ui, "Click a card to see its clients. The Library button goes back.");
                    bullet(ui, "Pause watching in the header pauses every game at once.");
                    bullet(ui, "Covers: right-click a card (or the picture icon on it) › Choose cover image. Remove cover goes back to the plain placeholder.");
                });
                guide_section(ui, "Reading the status", false, |ui| {
                    legend(ui, "ALL ONLINE", p().green, "Every client holds its game-server connection.");
                    legend(ui, "CONFIRMING 1/3", p().amber, "A connection vanished. Waiting for the grace checks before alerting, to ignore map changes.");
                    legend(ui, "DISCONNECTED", p().red, "Confirmed drop. Alerts repeat until it comes back.");
                    legend(ui, "GAME NOT RUNNING", p().red, "A game with alerts on is not running: it closed or crashed.");
                    legend(ui, "NOT RUNNING", p().grey, "On a card with alerts off: the game is closed, and that's fine.");
                    legend(ui, "INTERNET DOWN", p().red, "This PC cannot reach the internet.");
                    legend(ui, "PAUSED", p().grey, "Not watching. Press Start watching.");
                    hint(ui, "Each card shows the server address and how long the session has lasted, or how long it has been down.", p().muted);
                    hint(ui, "The pin icon keeps that game window above your other apps (windowed or borderless mode only).", p().muted);
                    hint(ui, "The picture-in-picture icon opens a small live view that floats above everything. Don't minimize the game; leave it behind your other apps.", p().muted);
                });
                guide_section(ui, "When you get alerted", false, |ui| {
                    bullet(ui, "Disconnected, game closed, client closed, internet down: urgent alert, repeated every few minutes.");
                    bullet(ui, "Back online, internet back, game reopened: one quiet message.");
                    bullet(ui, "Reconnected on its own: quiet message so you see it when you wake up.");
                    bullet(ui, "Heartbeat: a quiet \"Still watching\" every few hours. Silence all night then means the PC or app died.");
                    hint(ui, "Default timing: 3 checks × 30 s = about 90 s of real disconnection before the first alert.", p().muted);
                });
                guide_section(ui, "Settings explained", false, |ui| {
                    bullet(ui, "Notifications: where alerts go. Test your setup: send a test, run a drill, test the heartbeat.");
                    bullet(ui, "Detection: how often to check and how many misses count as a real drop.");
                    bullet(ui, "Wake-up alerts: repeat interval and how many notifications per alert. Phones vibrate once per notification, so 4 = four buzzes.");
                    bullet(ui, "Heartbeat: optional desktop screenshot to Discord. It shows every monitor, so anything on screen is sent.");
                    bullet(ui, "App: open at Windows sign-in and start watching automatically. Appearance: color theme and game covers.");
                });
                guide_section(ui, "Android: make it wake you up", false, android_tips);
                guide_section(ui, "iPhone: make it wake you up", false, ios_tips);
                guide_section(ui, "Troubleshooting", false, |ui| {
                    bullet(ui, "Nothing arrives: open Events. \"delivered\" means the service accepted it; HTTP 4xx means a wrong topic, webhook or token.");
                    bullet(ui, "Arrives but no sound: phone settings. See the Android and iPhone sections.");
                    bullet(ui, "Monitor stopped overnight: set Windows sleep to Never; turn on the heartbeat to notice.");
                    bullet(ui, "False alarms on map changes: raise Grace checks in Settings › Detection.");
                    bullet(ui, "Can't update the exe: the app is still in the tray. Right-click the tray icon › Exit first.");
                });
                ui.add_space(8.0);
                if ui.button("Run setup again").clicked() {
                    rerun = true;
                }
            });
        });
    rerun
}
