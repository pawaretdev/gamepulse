# Agent guidelines for this repository

## Project goal

Gamepulse (formerly ROOC Monitor) is a Windows-only tray app that watches game clients, because a game can drop in the middle of the night while the player is asleep. The core job is to detect disconnects and alert the player's phone reliably (ntfy / Discord webhook / Telegram), and to leave enough evidence to review afterwards. Detection and alert correctness come before cosmetic features.

Supported games are data in `GAMES` (`src/lib.rs`). Currently only Ragnarok Origin Classic (`rooc.exe`) is listed. Never hard-code game names, exe names, server IPs or ports outside `GAMES`. Profile `id`s are persisted (in config and cover filenames), so never rename them.

This is a monitor, not a bot. Never add auto-login, character control, reading or writing game memory, or anything that touches anti-cheat, unless the user explicitly changes the scope.

## Layout and runtime

A single Rust crate (`gamepulse`) at the repository root. It builds two executables:

- `gamepulse.exe`: egui/eframe GUI plus system tray. It has no console window and rejects command-line arguments.
- `gamepulse-cli.exe`: diagnostic probe (`--once` / `--capture` / `--capture-preview DIR`) that checks every game in `GAMES`. It must stay a console app so stdout and exit codes work.

The old PowerShell/WPF implementation (and the `rust/` subfolder) was removed on 2026-09-30. Don't reintroduce either.

Main files in `src/`:

- `lib.rs`: platform-neutral types (`Snapshot`, `Client`, `Identity`, `Config`, `GameProfile`/`GAMES`) and socket filtering.
- `windows/mod.rs`: one Tool Help process pass plus one `GetExtendedTcpTable` read (IPv4 and IPv6) per check, covering all games.
- `windows/capture.rs`, `preview.rs`: GDI capture of the whole virtual desktop (all monitors in one image, never a specific window).
- `notify.rs`: ntfy / Discord / Telegram senders (blocking `reqwest`) and the local alarm sound.
- `gui.rs`: window, tray, themes, fonts, Settings, and `Engine`, which owns the alert state machine and the 5-round drill. `Engine` runs on its own worker thread, ticked every 250 ms.
- `gui/hub.rs`: game library (cards with an Alerts on/off switch) and the per-game page.
- `gui/onboarding.rs`: first-run setup and the Guide window.
- `main.rs`: GUI entry point (single-instance mutex). `cli.rs`: CLI entry point.
- `build.rs`: generates the Windows resource script (icon plus VERSIONINFO taken from the `Cargo.toml` version).

Never move monitoring logic into eframe's `update()`. eframe stops calling `update()` while the window is hidden to the tray, which once silently stopped all monitoring. Tray events are handled in their own event handlers for the same reason.

`DEVELOPMENT.md` is the developer guide. Its "Open gates" section lists what is still unverified. User docs are the short `README.md` / `README.th.md` plus the guides in `docs/` (`guide`, `phone-setup`, `how-it-works`, each with a `.th.md` twin). Keep English and Thai in sync, and put details in `docs/`, not the README.

## Detection rules to preserve

- A client is online only if it has an `Established` TCP connection that is not loopback or unspecified and is not to one of the game's `ignored_ports` (ROOC: `80`, `443`, `8080`).
- Never treat a running process or a window title as proof of being online. When the game drops, the process and window title stay the same; only the socket disappears.
- A socket shows a connection, not that the character is playing normally. Don't claim more than the data shows.
- Client identity is PID plus process creation time, re-checked after reading the TCP table to catch PID reuse. Track state per client, so one client recovering never clears another client's state.
- A new socket for the same client means it dropped and reconnected by itself. Report it as information; don't wake the player.
- Check the internet separately (TCP connect to `1.1.1.1:443`), because sockets can linger after the connection is lost. When offline, send a separate alert and don't trust the remaining sockets.
- `Snapshot::exit_code()`: any error returns `3`. Never report an error as online or as "no process".

## Alerts and robustness

- Keep the state machine: `GraceChecks` consecutive misses trigger the first alert, then repeats every `RepeatAlertMin` up to `MaxRepeats`, and everything resets on recovery. Each urgent alert is a burst of `BurstCount` notifications `BurstGapSec` apart. Every loop that sends must have a limit.
- The heartbeat title must be exactly "Still watching" (it is the only alert that can carry the optional screenshot). Other alert titles are prefixed with the game name, using an ASCII `:` because ntfy strips non-ASCII titles.
- Real disconnects must stay distinct from recoveries, heartbeats and informational messages, both in urgency and in Discord `@here` (sent only when `DiscordHere` is on).
- Every game is always observed so its card shows real status, but only games in `WatchedGames` send alerts.
- Monitoring must refuse to start when no notification channel is configured.
- Network work needs timeouts and runs on spawned threads that report back over channels, so the UI never freezes. One failing channel must not block the others. Never claim an alert reached the phone just because a request was started.

## UI, local data and privacy

- Code comments, UI text, alert messages and `DEVELOPMENT.md` are all in English. Only `README.th.md` and `docs/*.th.md` are in Thai.
- Closing the window only hides it to the tray. The app exits from tray → Exit.
- All colors go through the theme palette (`p()`). Status colors (green, red, amber) must keep their meaning in every theme.
- Fonts are embedded from `assets/fonts/` (Inter, JetBrains Mono, Fredoka) with their OFL license files alongside. egui has no font weights, so use `semibold()` / `RichText::semibold()` for bold text; `.strong()` only changes the color.
- New config keys need a default in `Config::default()` (`#[serde(default)]`) so old config files keep loading. Don't overwrite the user's local config to test something.
- Never commit `config.json`, `*.log`, `screenshots/`, `evidence/` or `covers/` (all gitignored). Never put a real ntfy topic, webhook URL or Telegram token in code, docs or output.
- Screenshots cover every monitor and can contain private data. Capture stays off by default and is never uploaded or saved without the user opting in.
- Never bundle game cover art (it belongs to the publishers). Covers are user-supplied files in `covers/`.

## Verifying changes

The same commands as CI (run from the repository root):

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
.\target\release\gamepulse-cli.exe --once   # JSON snapshot; exit 0/1/2/3
```

A running `gamepulse.exe` locks the release exe, so the release build fails with "Access is denied" until the app is exited from the tray.

On macOS/Linux, `cargo test --locked` still works (Windows code is `#[cfg(windows)]`-gated), and `cargo check --locked --target x86_64-pc-windows-msvc --all-targets` type-checks the Windows code.

- For docs-only changes, checking them against the code and running `git diff --check` is enough.
- Don't close the game or cut the network to test a disconnect without the user's agreement. Hosted CI cannot verify `rooc.exe` detection or screen capture.
- Test buttons and the drill send real notifications. Use only channels the user has approved, and wait for the user to confirm the phone received them before saying alerts work.

If you have no Windows machine, game or phone to test with, say so and list what was not tested. Never conclude that runtime behaviour works just because the code reads correctly or builds.

## Releases

Pushing a `v*` tag runs `.github/workflows/release.yml`. The tag must match the `Cargo.toml` version. It builds on a clean runner and publishes a zip containing both exes, both READMEs, `LICENSE` and the font licenses. See "Publishing a release" in `DEVELOPMENT.md`. Don't build locally and upload by hand.

## How to hand work back

Change only what was asked and keep the user's existing work. Update the English and Thai user docs when usage or user-facing settings change. Summarise what changed, what was checked, and what is still limited. If asked to commit, use Conventional Commits (`docs:`, `fix:`, `feat:`), stage only files from this task, and never list an AI agent as author or add a `Co-Authored-By` line.
