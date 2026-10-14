# How Gamepulse works

**English** · [ภาษาไทย](how-it-works.th.md) · [Back to README](../README.md)

## How it detects a disconnect

While you're in game, `rooc.exe` keeps a TCP connection open to the game's gate server, one per client:

```
<game-server-ip>:10011   <- client 1
<game-server-ip>:10012   <- client 2
```

When you drop, that socket disappears immediately. Other connections (443 to the CDN, localhost between game threads) stay, the process keeps running and the window title doesn't change.
So **whether that socket exists is the most reliable online signal**, and reading it needs no access to game memory and nothing that could bother anti-cheat.

The app counts sockets that are Established, not localhost and not on port 80/443/8080. No IP or port is hard-coded, so it keeps working if the game moves servers. Each client is identified by PID plus process start time, so a reused PID is never mistaken for the same client.

Monitoring runs on a background thread separate from the window, so it keeps checking and alerting while hidden in the tray.

## When you get alerted

| Event | Meaning | Wakes you? |
|---|---|---|
| No game socket for N checks in a row | Dropped and not back | Yes, repeated every 3 min |
| No `rooc.exe` process | Game closed or crashed | Yes, repeated |
| One client disappears | That client was closed | Yes |
| No internet from this PC | Internet down (remaining sockets can't be trusted) | Yes, repeated |
| Game socket has a new creation time | Dropped and reconnected on its own | No, just shows when you wake up |
| Back online / internet back | All good | No |

**Grace checks** delay the alert so map changes and character switches don't trigger false alarms. The default is 3 checks × 30 seconds, so a real drop must last about 90 seconds before you're woken.

**Repeat** (`Repeat every`) is what actually wakes you: alerts keep firing until the client is back or `Maximum repeats` is reached. That works better than relying on any one app's vibration pattern.

**Heartbeat** (every 4 hours by default) sends "Still watching", so a silent night means everything was fine and not that the monitor died at 2 a.m.
With **Attach a desktop screenshot** on, each heartbeat to Discord includes a WebP image of all monitors. It shows everything on screen, so it's off by default.

## Why does my phone only buzz once?

A phone vibrates once per notification. How long it vibrates is decided by the OS notification channel, and the sender can't change it, not even with `Priority: urgent`.

The fix is to **send several in a row**, set in Settings › Wake-up alerts:

| Setting | Meaning |
|---|---|
| Notifications per alert (`BurstCount`) | 1 = one buzz, 4 = four buzzes. Default 4. |
| Gap between them (`BurstGapSec`) | Default 4. Under 3 seconds, Android may merge the later ones. |

So each drop sends `BurstCount` notifications, and the whole burst repeats every `Repeat every` minutes until the client is back or `Maximum repeats` is reached.

**With `BurstCount` set to 1 this doesn't work at all.** Settings shows an amber warning when it's 1.

## Which channels support urgent alerts

| Channel | Priority support |
|---|---|
| ntfy | Sends `Priority: urgent` (level 5). **Only has an effect on Android.** |
| Discord webhook | No priority, but real drops include `@here` |
| Telegram bot | Sound on/off only, no urgency level |

Discord only adds `@here` to urgent alerts. Recoveries and heartbeats don't ping.
Turn it off in Settings › Notifications (`"DiscordHere": false` in `config.json`).

### What Discord messages look like

Alerts are embeds, and the color bar tells you the event type at a glance:

| Color | Event |
|---|---|
| 🔴 Red | Disconnected / game closed / internet down |
| 🟢 Green | Back online / internet back |
| 🟡 Yellow | Reconnected on its own / drill |
| 🔵 Blue | Heartbeat and test messages |

Fields show the PID, how long it has been down and which repeat this is. The footer shows the notification's place in the burst (for example `alert 2 of 4`), so you can tell whether the burst finished.

## Limitations

It can only tell whether **the socket still exists**, not whether your character is still moving.
It can't tell "kicked out" from "stuck at the login screen"; both show as disconnected, which for this purpose is the same thing.

Comparing the API results against `Get-NetTCPConnection` on a real machine, and checking capture in every situation (multiple monitors, lock screen, sleep), isn't finished yet. The open items are listed in [DEVELOPMENT.md](../DEVELOPMENT.md#open-gates).
