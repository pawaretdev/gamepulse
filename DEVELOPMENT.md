# GamePulse: development

The GamePulse app itself (formerly ROOC Monitor). End-user instructions are in the [README](README.md) ([Thai](README.th.md)); this file is for people building or changing the code.
The old PowerShell/WPF version has been deleted (2026-09-30; see commit `a288aa8` for history). Outstanding verification work is listed at the end of this file.

Two executables are built:

| File | Purpose |
|---|---|
| `gamepulse.exe` | GUI + system tray; windows subsystem, so no console; takes no arguments |
| `gamepulse-cli.exe` | `--once` / `--capture` / `--capture-preview DIR`; must be a console app so it can print results and return exit codes; checks every game in `GAMES` |

The GUI reads/saves `config.json` at the repo root (found by walking up from the exe's folder), sends via ntfy/Discord/Telegram,
alerts with burst/repeat/recovery, runs a 5-round drill, sends heartbeats (optionally with a WebP screenshot), probes the internet, can open at Windows sign-in (HKCU Run),
has 6 themes, and hides to the system tray while monitoring keeps running.

## Supported games

Games are pure data in `GAMES` (`src/lib.rs`): `id` (stored in config key `WatchedGames` and used as the cover filename; never change it after a release),
`name` (full name shown in the UI and alert text), `exe` and `ignored_ports`. Currently only Ragnarok Origin Classic (`rooc`, `rooc.exe`, 80/443/8080).

- `windows::snapshot(GAMES)` enumerates processes and reads the TCP table once, then splits by exe; every `Client` carries its `game` (JSON `schema_version` 2).
  Cost barely depends on the number of games, and every game is always observed, so cards show real status even for games with alerts off.
- Only games in `WatchedGames` alert (the switch on each card → `Engine::set_watched`); turning alerts off drops that game's state (`prune_unwatched`).
  "Game closed" is per game (`no_process` is a map). Every alert title starts with the game name (`titled`), except the heartbeat, which must stay "Still watching".
- The library and per-game pages live in `src/gui/hub.rs`. Covers are loaded once at startup from `covers/<id>.png|jpg|jpeg|webp` next to `config.json`.
  They are never bundled in the exe because the art belongs to the publishers; `covers/` is gitignored. No file = a placeholder with the first letter of the game's name.

Adding a game = adding a row to `GAMES`, but first confirm with the real game that "an Established TCP connection that is not loopback and not in `ignored_ports`"
actually disappears when it disconnects (games that use UDP, connect directly on 443, or have a launcher/anti-cheat holding a connection need the rule extended first).

## Done

- Enumerate processes by `GameProfile::exe` via Tool Help and re-check the executable name through the process handle
- Identity uses PID + process creation FILETIME, re-checked after reading TCP to catch PID reuse / processes vanishing mid-scan
- `GetExtendedTcpTable` with `OWNER_MODULE_ALL` for both IPv4/IPv6; keeps every game connection with its creation timestamp instead of arbitrarily picking the first socket as the session
- Filters Established, loopback, unspecified addresses and the game's `ignored_ports` (ROOC: 80/443/8080); adds filtering of IPv4-mapped loopback and `::`, which the old regex didn't cover
- Reports each PID's visible top-level windows (including minimized ones) in the snapshot, for inspection only
- Captures the whole virtual desktop, all monitors, as a single BMP via GDI without targeting a window/monitor and without restoring/focusing any window
- Bounds image allocation and reports unavailable when the desktop can't be read

- GUI: monitoring/alerting lives in `Engine`, which runs on its own worker thread and is not tied to egui's `update()` (which stops while the window is hidden)
- Tray menu, reopening the window, and Exit go through the tray event handlers + Win32 directly
- Icons: `assets/icon.ico` is embedded in the exe via `build.rs` (`embed-resource`, needs `rc.exe` from the Windows SDK) and `assets/icon-256.png` is used for the window.
  Both are generated from the source art with `python assets/make_icons.py <art.png>` (needs Pillow); the script cuts black corners to transparent.
  The source art is not kept in the repo. To change the icon, run the script on the new art and rebuild. `icon-256.png` is also the README logo.

Heartbeat screenshots sent to Discord are converted to lossless WebP first (1920×1080 ≈ 200 KiB, ~30 ms); over 8 MiB they fall back to JPEG.

`--capture` captures and reports metadata without writing a file; `--capture-preview DIR` saves a BMP to the explicitly given directory. Nothing is uploaded.
The image covers the virtual desktop and may contain data from every program/monitor; treat it as sensitive.
Open the preview file and inspect the actual image before passing the capture gate (there is no preview window in the app yet).

## Build and check

Use Rust stable and MSVC C++ build tools + Windows SDK on Windows 11 x64:

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
.\target\release\gamepulse-cli.exe --once
$LASTEXITCODE
.\target\release\gamepulse-cli.exe --capture
$LASTEXITCODE
# Optionally save an image to inspect by hand: stored only in the given directory
.\target\release\gamepulse-cli.exe --capture-preview .\evidence\normal
$LASTEXITCODE
```

`--once` exit codes: `0` every client has a socket, `1` some client is missing its socket, `2` no client found,
`3` incomplete data / error. Never interpret code 3 as online or game closed.
It does not check the internet or confirm alerts.
JSON stores FILETIMEs as integers in 100 ns units since 1601 UTC (beware of JavaScript Number losing precision).
If a TCP timestamp is zero/negative, an error is reported and the snapshot is not considered complete.

`--capture` exit codes: `0` desktop captured, `1` capture unavailable, `3` argument/serialization error.
`captured_unix_ms` is when the capture finished, and bounds may have negative `left`/`top` when a monitor sits left of or above the primary one.
`--capture` stores nothing on disk; `--capture-preview` saves a BMP with the capture time in the file name,
never overwrites an existing file, and caps the pixel buffer at 256 MiB.
Image files may contain data from every program and monitor; keep them only for testing and delete them when done. Images are never sent or cached across sessions.

On macOS/Linux, unit/CLI tests run, and the observe/capture commands exit with code 3:

```sh
cargo test --locked
rustup target add x86_64-pc-windows-msvc
cargo check --locked --target x86_64-pc-windows-msvc --all-targets
```

The cross-target check does not link an executable and is not a Windows runtime test.
The CI workflow (`.github/workflows/ci.yml`) runs build/tests on Windows but has no game/GPU target to validate capture.

`.cargo/config.toml` links the C runtime statically (`+crt-static`), so the exe runs on machines without the Visual C++ Redistributable.

## Publishing a release

`.github/workflows/release.yml` runs when a tag starting with `v` is pushed: it runs tests, builds on a GitHub runner (a clean machine with no `config.json`),
then creates a GitHub Release with `gamepulse-vX.Y.Z-windows-x64.zip` (containing `gamepulse.exe`, `gamepulse-cli.exe`, `README.md`, `README.th.md`, `LICENSE`, the user docs in `docs/`, the README logo and the bundled font licenses in `fonts/`) and a `.sha256` file.

1. Bump `version` in `Cargo.toml`, then run `cargo build` once so `Cargo.lock` updates
2. Commit and push to `main`
3. Create a tag matching the version and push it (the workflow fails if the tag and `Cargo.toml` don't match):
   ```powershell
   git tag v0.1.0
   git push origin v0.1.0
   ```
4. Watch progress in the **Actions** tab; when done it's under **Releases**. Release notes can be edited on the web page.

If a release breaks: delete both the release and the tag with `gh release delete v0.1.0 --cleanup-tag`, then push the tag again.
Don't build locally and upload the zip by hand. If you really must, check that no `config.json`, logs or capture images are included.

## Comparing with `Get-NetTCPConnection` on the game machine

Use a normal session first. If the API can't be read, record the error and try elevated, without concluding the game is closed.
Run the probe and the following command close together while the game is stable (each scan is not an atomic snapshot):

```powershell
Get-Process -Name rooc -ErrorAction SilentlyContinue | ForEach-Object {
    $client = $_
    [pscustomobject]@{
        Pid = $client.Id
        StartedFiletime = $client.StartTime.ToUniversalTime().ToFileTimeUtc()
    }
    Get-NetTCPConnection -OwningProcess $client.Id -State Established -ErrorAction Stop |
        Where-Object {
            $_.RemoteAddress -notmatch '^(127\.|::1$|0\.0\.0\.0)' -and
            $_.RemotePort -notin @(80,443,8080)
        } |
        Select-Object OwningProcess, LocalAddress, LocalPort, RemoteAddress, RemotePort,
            @{Name='CreatedFiletime'; Expression={$_.CreationTime.ToUniversalTime().ToFileTimeUtc()}}
}
```

Compare PID/start time, both endpoints and TCP creation timestamps as complete sets, not by row order.
Record differences caused by the IPv6 filtering above separately from API mismatches, and confirm a real IPv6 connection exists before concluding IPv6 passes.
Test single/multiple clients, clients opening/closing on their own, and user-controlled session changes.
Don't close the game or cut the network to test without prior agreement.

Keep results locally in `evidence/` (ignored) along with OS build, GPU/driver, time, client count,
window mode and error codes. Never attach config/credentials. Use this table as the acceptance record:

| Case | TCP vs `Get-NetTCPConnection` | Desktop capture | Real image / correct bounds |
|---|---|---|---|
| Normal / occluded | Not tested | Not tested | Not tested |
| Minimized / fullscreen | Not tested | Not tested | Not tested |
| Multiple monitors / mixed DPI | Not tested | Not tested | Not tested |
| Multiple clients / newly opened / closed during capture | Not tested | Not tested | Not tested |
| Lock screen / display off / sleep-resume | Not tested | Not tested | Not tested |

## Open gates

The grace/repeat/burst/internet logic, alerts, Settings and tray have all moved to Rust, and the PowerShell version was deleted per the user's decision.
But phase 1 acceptance in the table above is not done: the API/session timestamps have not been fully compared against `Get-NetTCPConnection`,
and capture has not been checked with multiple monitors / mixed DPI / lock screen / sleep-resume.
There are no fake-clock regression tests for the state machine yet (phase 2); for now it is verified by real use on the game machine.
"Pin on top" (`windows::set_topmost`) has not been tried on `rooc.exe` yet. Still to check: that the game doesn't reset its own z-order, the elevated-game error, and behaviour after a resolution change that recreates the window.
Picture-in-picture (`windows::pip`, DWM thumbnail) has not been tried on `rooc.exe` either. Still to check: that the game keeps rendering while covered and unfocused, mixed-DPI monitors, and dragging a `WS_EX_NOACTIVATE` window.

API sources: [GetExtendedTcpTable](https://learn.microsoft.com/en-us/windows/win32/api/iphlpapi/nf-iphlpapi-getextendedtcptable),
[IPv6 ownership/timestamp](https://learn.microsoft.com/en-us/windows/win32/api/tcpmib/ns-tcpmib-mib_tcp6row_owner_module),
[Capturing an Image](https://learn.microsoft.com/en-us/windows/win32/gdi/capturing-an-image)
