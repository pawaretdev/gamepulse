<p align="center"><img src="assets/icon-256.png" width="128" alt="Gamepulse icon"></p>

<h1 align="center">Gamepulse</h1>

<p align="center"><b>English</b> · <a href="README.th.md">ภาษาไทย</a></p>

A small Windows tray app that watches your game clients and alerts your phone the moment one disconnects, so you can leave characters logged in overnight and still sleep.

- Supported game: **Ragnarok Origin Classic** (`rooc.exe`). Watch several games at once from the library
- Alerts through **ntfy**, a **Discord** webhook or a **Telegram** bot, and repeats until the client is back
- Never reads game memory, clicks anything or touches your account
- Keeps watching from the system tray when you close the window
- Pin a game window on top, or keep it in a floating picture-in-picture view while you work
- Optional **MVP tracker**: reads the game's own MVP announcements from its window and warns you on Discord or with a Windows notification before each MVP respawns

<p align="center">
  <img src="docs/library.png" width="49%" alt="Gamepulse game library with alerts on and both clients online">
  <img src="docs/game.png" width="49%" alt="Game page showing each client's connection and session time">
</p>

## Getting started

1. Download `gamepulse-vX.Y.Z-windows-x64.zip` from [Releases](https://github.com/pawaretdev/gamepulse/releases/latest) and unzip it into a folder you will keep. There is no installer.
2. Run **`gamepulse.exe`**. If Windows shows "Windows protected your PC" (the file isn't code-signed), click **More info › Run anyway**.
3. Follow the 4-step setup: pick an alert channel, send a test to your phone, make your phone wake you up, and choose whether to start with Windows.
4. Before you sleep, run **Settings › Test your setup › Run drill** once. It proves the alerts wake you, not just that they arrive.
5. Leave it running. Closing the window hides it to the tray.

Also set Windows **Power & sleep** to **Never**, or the PC sleeps and the game and monitor stop together.

## Documentation

- [User guide](docs/guide.md): the main screen, settings, MVP tracker, covers, updating, uninstalling, command line
- [Phone setup](docs/phone-setup.md): make Android or iPhone actually wake you up
- [How it works](docs/how-it-works.md): how disconnects are detected, when you get alerted, limitations
- [DEVELOPMENT.md](DEVELOPMENT.md): building from source and publishing releases

## License

[MIT](LICENSE) © 2026 pawaretdev. Bundled fonts ([Inter](assets/fonts/Inter-OFL.txt), [JetBrains Mono](assets/fonts/JetBrainsMono-OFL.txt), [Fredoka](assets/fonts/Fredoka-OFL.txt)) are under the [SIL Open Font License 1.1](https://openfontlicense.org); the release zip includes their licenses in `fonts/`.

Ragnarok Origin is a trademark of its respective owners; this project is not affiliated with or endorsed by them.
