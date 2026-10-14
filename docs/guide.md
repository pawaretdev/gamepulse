# User guide

**English** · [ภาษาไทย](guide.th.md) · [Back to README](../README.md)

## The main screen

- The **library** shows one card per supported game with its live status on the cover (`2/2 ONLINE`, `1 DISCONNECTED`, `NOT RUNNING`). A card with a disconnect you are watching gets a red border.
- The **Alerts on / off** switch on each card picks which games alert you. Turn on several to watch several games at once. The filter chips show All, only games with alerts on, or only running games.
- Click a card to open that game: a header with the cover, status and alert switch, then one card per client. **Library** goes back.
- The header shows the overall status (ALL ONLINE / 1 DISCONNECTED / GAME NOT RUNNING / PAUSED), internet status and the Start/Pause button, plus when the last check ran and when the next one is due.
- One card per client: ONLINE or disconnected, PID, server address, and how long the session has lasted or how long it has been down.
  While a connection is missing but the grace checks haven't finished, the card shows `CONFIRMING 1/3` in amber.
- The **pin** icon on a client card keeps that game window above every other window, so you can work in a maximized app and still see the game. The icon turns blue while pinned; click it again to unpin. Make the game window smaller and put it in a corner first. It works in windowed or borderless mode, not exclusive fullscreen. The pin lasts until the game window closes. If the game runs as administrator, GamePulse has to run as administrator too.
- The **picture-in-picture** icon next to it opens a small live view of that game that floats above every window, like a video player's mini player. Drag it to move it, drag its edges to resize it (the game's shape is kept), double-click it to go back to the game, or right-click it for a menu. **Don't minimize the game**: Windows stops drawing minimized windows, so the view would go blank. Leave the game behind your other apps instead; covered windows keep drawing. Windows draws the view itself on the GPU, so it costs almost no CPU, and GamePulse never reads or saves the picture.
- **Events** lists drops and alert deliveries. A check result only appears when it changes; the log file records every check.
- The footer shows your alert channels and when the next heartbeat is due.
- Closing the window hides it to the tray. Left-click the tray icon to reopen it; right-click › **Exit** to quit.
- Launching the exe again while it's running brings the existing window back. Only one copy runs at a time.

The **Guide** button on the main screen explains every status, when you get alerted, each setting, phone setup and troubleshooting. It also has **Run setup again**.

## Settings

| Section | What's in it |
|---|---|
| Appearance | Theme: Midnight (default), Night Watch, Poring, Frost, Mocha, Daylight. Previews live; Cancel reverts. |
| Notifications | ntfy topic/server, Discord webhook, Telegram bot, `@here`, alarm sound on this PC |
| Test your setup | **Test alert**, **Run drill**, **Test heartbeat**, **Capture desktop**. They use the values on screen, even before you press Save. |
| Detection | Check interval, grace checks |
| Wake-up alerts | Repeat interval, maximum repeats, notifications per alert, gap between them |
| Heartbeat | Interval, optional desktop screenshot to Discord |
| App | Open at Windows sign-in, start watching when the app opens |

- **Test alert** sends one alert burst to every channel. It proves delivery works.
- **Run drill** sends the same alerts as a real drop, 5 rounds spaced by `Repeat every`, marked `DRILL`. It proves the alerts wake you up. A drill keeps running after you close Settings and can be stopped from **Stop drill** on the main screen.
- **Test heartbeat** sends a heartbeat now. With the screenshot option on, you can confirm the image reaches Discord.
- **Capture desktop** saves a BMP to the `screenshots` folder and sends nothing.

### Using ntfy

Install the **ntfy** app from the Play Store or App Store, tap **+** and subscribe to the same topic you entered in GamePulse. The setup screen has **Generate** (a hard-to-guess name) and **Copy** buttons.

Keep the topic private. ntfy.sh is a public server and anyone who knows the topic can read your alerts. Alerts contain only a PID and times, no account data. To keep them fully private, self-host ntfy and enter it as `ntfy server`.

## Game covers

Covers are not bundled (the art belongs to the publishers). Every game gets a blank placeholder cover. To add your own, right-click a game card (or click the picture icon that appears on the cover) › **Choose cover image** and pick a PNG, JPG or WebP; portrait 3:4 looks best. The app keeps a copy in the `covers` folder next to it. **Remove cover** goes back to the placeholder.

## Where your settings live

Settings are stored in `config.json` next to the exe, and the log is `gamepulse.log` in the same folder.
`config.json` contains your webhook and tokens. Never share it or attach it when asking for help.
(When you build from this repository, the app uses `config.json` at the repository root instead. That file is gitignored.)

## Open automatically at Windows sign-in

Settings › App › tick **Open GamePulse when I sign in to Windows**, then Save.
This writes to `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`, so it needs no admin rights and no Task Scheduler.
Also tick **Start watching when the app opens**, or the app will open but not watch.
If you move the exe or update into a different folder, untick and tick it again to save the new path.
Upgrading from ROOC Monitor: if you had the old sign-in launch on, GamePulse moves it to itself the first time it opens.

More important than the app: set Windows **Power & sleep** to **Never**. Otherwise the PC sleeps and the game and monitor stop together, which may be why you were dropping overnight in the first place.

## Updating

1. Right-click the tray icon › **Exit**. Clicking X only hides the window and the exe stays locked.
2. Unzip the new version over the old `.exe` files in the same folder. `config.json` is left alone, so your settings stay.

## Uninstalling

There is no uninstaller. The app only writes to its own folder and one sign-in value in the registry.

1. If **Open GamePulse when I sign in to Windows** is ticked, untick it and Save first. Otherwise Windows keeps trying to open a missing file at every sign-in.
2. Right-click the tray icon › **Exit**.
3. Delete the folder. That removes the exe, `config.json` (your webhook and tokens), `gamepulse.log` and any `screenshots`.

## Command line

`gamepulse-cli.exe` (in the same zip) is separate from the GUI. It checks once or tests screen capture; it never watches or sends alerts:

```powershell
.\gamepulse-cli.exe --once      # JSON snapshot; exit 0 all online, 1 some dropped, 2 no supported game running, 3 incomplete read
.\gamepulse-cli.exe --capture   # capture the desktop and report metadata without writing a file
.\gamepulse-cli.exe --help
```

Building from source, tests and how to publish a release are in [DEVELOPMENT.md](../DEVELOPMENT.md).
