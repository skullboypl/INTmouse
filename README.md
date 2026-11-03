# INTmouse

Free, open-source mouse and keyboard sharing for Windows PCs on a local network.
Move the cursor past the edge of the screen and it continues on the next computer:
one mouse, one keyboard, no account, no license key, no ads. GPL-2.0-or-later.
A [ShareMouse](https://www.sharemouse.com/) alternative: UDP for input, TCP for the handshake,
clipboard and files.

Site: <https://intmouse.vxh.pl> · Download: [GitHub Releases](https://github.com/skullboypl/INTmouse/releases)

*Po polsku: darmowe i otwarte (GPL) udostępnianie myszy i klawiatury między komputerami z Windows
w sieci lokalnej. Kursor przechodzi za krawędź ekranu na drugi komputer, bez konta i bez licencji.
Interfejs programu jest po polsku; reszta tej strony jest po angielsku.*

## Status

**0.1 beta, Windows 10/11 only.** The unit tests pass, and the program is being tried on two
computers on a LAN. Expect rough edges. Report bugs in
[Issues](https://github.com/skullboypl/INTmouse/issues).

## What it does

- **Cursor across the edge.** Monitor layout with the other computer's monitors pulled in
  automatically. Edges glue together when you drag a computer close. Optional monitor wrap
  works across computers too.
- **All input goes over.** Movement, all buttons, wheel and tilt, and every key and shortcut:
  Ctrl+C, Alt+Tab, Win, held keys repeating. While you steer the other computer, nothing reaches
  this one.
- **Clipboard.** Plain and formatted text, pictures, and files copied in Explorer (Ctrl+C here,
  Ctrl+V there). Files travel with a progress notice.
- **Files.** Drag a file across the screen edge and let go on the other computer, or send files
  from the Exchange tab.
- **Switch locks.** Fullscreen, maximized window, corners, a held key, a program list, a second
  tap on the edge, "only a fast hit switches", and "never while a game holds the cursor"
  (e.g. a crosshair in CS2).
- **Taking control back.** Moving a computer's own mouse while it is being steered gives control
  back to it at once.
- **Network.** A shared password (computers with the same password find each other, no IPs to
  type), an allowed-IP list, adapter choice, IPv4 and IPv6, TCP fallback when UDP is blocked.
- **Everyday.** Start with Windows (tray), run as administrator, panic key, dim inactive screens,
  tray icon with state (green: on, red: off), automatic updates.

Not yet: Linux, macOS, sending whole folders.

## How to use

1. Download `INTmouse.exe` from [Releases](https://github.com/skullboypl/INTmouse/releases) and run it
   on both computers (same version). Keep it in one place: the built-in updater replaces it there.
2. **Firewall:** on the first start a bar asks once to allow INTmouse on the local network
   (TCP 6555 and UDP 1046, local subnet only). The rules cover the ports, not one exe file, so a
   new version does not ask again. You can add them later under Network → "Windows Firewall".
3. Network tab: turn on "Require password" and type the **same password on both computers**.
   They find each other and connect (top bar: "● connected: …"). Prefer to choose by hand? Add the
   other computer's IP under Computers (a non-empty list means only those addresses).
4. Screen layout: the other computer's monitors appear by themselves. Drag the whole computer where
   it stands on your desk; a close edge glues. Save.
5. Turn sharing on (tray icon, or `Ctrl+Shift+A`) and move the cursor past the edge. To come back,
   cross the edge you came in by (or use the monitor wrap).

Shortcuts: `Ctrl+Shift+A` sharing on/off, `Ctrl+Shift+Q` jump to the next local monitor, hold `Esc`
for the panic key (sharing off, mouse returned).

**Emergency quit:** `Ctrl+Alt+Shift+Q` closes the program immediately, in any state.
Without the program: `Ctrl+Shift+Esc` (Task Manager), or `Win+R` and `taskkill /F /IM INTmouse*`.

## Building

Requires Rust (toolchain `stable-x86_64-pc-windows-msvc`) and the Windows SDK (`rc.exe` embeds the icon).

```bat
cargo build -p inputshare --release   :: target\release\INTmouse.exe
cargo test --workspace
build.cmd                              :: same build, copies the file to dist\
dev.cmd                                :: runs the UI with a console
```

The program captures the mouse and keyboard. Do not run it from scripts, or on a machine nobody is
sitting at. The unit tests do not touch the mouse, the keyboard or the clipboard.

## Layout of the repo

| Directory | Contents |
|---|---|
| `crates/protocol` | packet format (UDP: input, TCP: handshake, clipboard, files) |
| `crates/core` | config, monitor layout, switching rules |
| `crates/app` | the program: network, input capture and injection, GUI (egui), tray |
| `site/` | the site <https://intmouse.vxh.pl> (Next.js, static, no database and no login) |

A packet-format change has to bump `PROTOCOL_VERSION` in `crates/protocol`.

## Start with Windows, and as administrator

Security tab → Startup:

- **Start with Windows**: after login the program starts hidden in the tray (`--tray`).
  A normal user gets a value under `HKCU\...\Run`. An administrator gets a Task Scheduler task at the
  highest privileges (a registry run entry will not start an elevated program without a prompt at every login).
- **Run as administrator**: lets you steer windows of programs that were started as administrator
  (Windows blocks sending input into a higher-privileged window). After you change the setting, the program
  saves the config and **starts again** in the new mode (turning it on asks for consent, UAC; if you refuse,
  the program keeps running unchanged). Dragging files from Explorer onto the window does not work in
  administrator mode.

## Updates

A few seconds after start, and then every 12 hours, the program checks for a newer version (turn this
off on the Updates tab). It only downloads <https://intmouse.vxh.pl/version.json>, where the site mirrors
the latest GitHub release. When a newer version exists, a bar appears at the top of the window:
**Update / Later**. Update downloads the file, checks its SHA-256, replaces the exe in place and starts
the program again. The previous version stays as `INTmouse.exe.old` so you can put it back.

To publish: tag the version from `Cargo.toml` (for example `v0.1.0-beta.2`), create a GitHub release
and attach the signed `INTmouse.exe` and a `.zip`. The site refreshes `version.json` at most once an
hour. A new version must have a higher number, because the program compares them
(`0.1.0-beta.2` is newer than `0.1.0-beta.1`, and `0.1.0` is newer than both).

## When the computers do not connect

The top bar always shows either "● connected: …" or "○ not connected". The **Connected** tab shows who
is connected, who is visible on the network (with a Connect now button), automatic connect options, and
the **connection log**. When a connection fails, the reason is in red: wrong password, or "Require
password" set on only one computer, a different protocol version, a missing address on the other
computer's list, or the firewall.

The **Logs** tab shows the full program log (the last 3000 lines) with a filter, search and copy.
Verbose mode adds debug lines and writes `inputshare.log` in the config folder (`%APPDATA%\INTmouse`).

Automatic connect works only with "Require password" on and an empty Computers list. The password
crosses the network as an unsalted SHA-256 hash with no encryption, so use the program on a network
you trust.

## Contributing

Pull requests and issues are welcome. Before a PR, run `cargo test --workspace` and
`cargo check --workspace --all-targets` (no warnings). In `site/`: `npm run lint`, `npm run typecheck`, `npm test`.

## License

[GNU GPL 2.0 or later](LICENSE) © 2026 Artur Spychalski. INTmouse is free software: free to use, for
anyone, for any purpose. You can change it and pass it on under the GPL (changed versions must stay
open source too).
