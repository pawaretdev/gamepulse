//! gamepulse-cli.exe — console probe. The GUI is a separate exe (main.rs, gamepulse.exe)
//! Checks every game in `GAMES` at once; the exit code covers all clients.
//! Separate because the GUI must use the windows subsystem (no console popping up), which can't print stdout or return exit codes to a console.
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args == ["--help"] || args == ["-h"] {
        println!("Gamepulse API probe (Windows 11 x64). The GUI is gamepulse.exe.\nChecks every supported game: {}.\n\n  --once       Print process/TCP/window snapshot as JSON\n  --capture    Capture the complete virtual desktop and print metadata\n  --capture-preview DIR  Also save one full-desktop BMP in DIR\n  --mvp-probe SECONDS    Read the game's message bar for that long and print each MVP announcement as JSON\n\nNo monitoring, internet check, configuration import or notifications.\nExit: 0 sockets present / capture succeeded, 1 missing socket / capture unavailable,\n      2 no clients, 3 observation or argument error. Socket presence is not proof of gameplay.", gamepulse::GAMES.iter().map(|g| format!("{} ({})", g.name, g.exe)).collect::<Vec<_>>().join(", "));
        return ExitCode::SUCCESS;
    }
    let preview = args.len() == 2 && args[0] == "--capture-preview" && !args[1].is_empty();
    let mvp_seconds = (args.len() == 2 && args[0] == "--mvp-probe")
        .then(|| {
            args[1]
                .parse::<u64>()
                .ok()
                .filter(|s| (1..=3600).contains(s))
        })
        .flatten();
    if args != ["--once"] && args != ["--capture"] && !preview && mvp_seconds.is_none() {
        eprintln!("Use --once, --capture, --capture-preview DIR, --mvp-probe SECONDS (1-3600) or --help. No action performed.");
        return ExitCode::from(3);
    }
    #[cfg(windows)]
    {
        if let Some(seconds) = mvp_seconds {
            return mvp_probe(seconds);
        }
        let snapshot = gamepulse::windows::snapshot(gamepulse::GAMES);
        if args == ["--capture"] || preview {
            let directory = if preview {
                Some(std::path::Path::new(&args[1]))
            } else {
                None
            };
            return gamepulse::windows::capture::run(&snapshot, directory);
        }
        match serde_json::to_string_pretty(&snapshot) {
            Ok(json) => println!("{json}"),
            Err(error) => {
                eprintln!("Cannot serialize snapshot: {error}");
                return ExitCode::from(3);
            }
        }
        ExitCode::from(snapshot.exit_code())
    }
    #[cfg(not(windows))]
    {
        eprintln!("This probe requires Windows 11 x64. No observation performed.");
        ExitCode::from(3)
    }
}

/// Prints one JSON line per reading: what OCR saw on the message bar and how the tracker reads it.
/// Exit 0 if the bar was read at least once, 1 if the game window could not be read
#[cfg(windows)]
fn mvp_probe(seconds: u64) -> ExitCode {
    use gamepulse::windows::boss_watch::{self, Reading};
    use std::time::{Duration, Instant};
    let Some(game) = gamepulse::GAMES.iter().find(|g| !g.mvps.is_empty()) else {
        eprintln!("No supported game has an MVP list.");
        return ExitCode::from(3);
    };
    let watcher = boss_watch::start(game);
    let deadline = Instant::now() + Duration::from_secs(seconds);
    let mut read_ok = false;
    while let Some(left) = deadline.checked_duration_since(Instant::now()) {
        let line = match watcher.readings.recv_timeout(left) {
            Ok(Reading::Problem(None)) => {
                read_ok = true;
                serde_json::json!({"reading": "game window"})
            }
            Ok(Reading::Problem(Some(problem))) => serde_json::json!({"problem": problem}),
            Ok(Reading::Dimmed(gain)) => serde_json::json!({"power_saving_gain": gain}),
            Ok(Reading::Panel {
                names,
                times,
                check,
                red,
            }) => {
                let rows: Vec<String> =
                    gamepulse::boss::read_panel(game.mvps, &names, &times, &check, &red)
                        .into_iter()
                        .map(|(mvp, row)| format!("{} {row:?}", mvp.name))
                        .collect();
                serde_json::json!({"mvp_panel": rows, "names": names, "times": times, "check": check, "red": red})
            }
            Ok(Reading::Text { full, green }) => {
                let parsed = gamepulse::boss::parse(game.mvps, &full, &green)
                    .map(|(mvp, sighting)| format!("{} {sighting:?}", mvp.name));
                serde_json::json!({"text": full, "green": green, "mvp": parsed})
            }
            Err(_) => break,
        };
        println!("{line}");
    }
    ExitCode::from(if read_ok { 0 } else { 1 })
}
