# rdpvncdesk

Cross-platform **RDP client** for **Windows, Linux and macOS**, built from a single Rust codebase.

The RDP protocol is handled by [IronRDP](https://github.com/Devolutions/IronRDP) (pure Rust), the window by
[winit](https://github.com/rust-windowing/winit) and drawing by [softbuffer](https://github.com/rust-windowing/softbuffer).
No platform-specific code paths in this repo: the same source compiles on all three systems.

**[Download the latest release](https://github.com/TLmaK0/rdpvncdesk/releases/latest)** (Windows, Linux, macOS Apple Silicon and Intel).

> Status: early. Connect, view the remote desktop, keyboard and mouse work. VNC support is planned (hence the name).

## Connection manager

Run `rdpvncdesk` with no arguments to open the connection manager:

- Saved hosts list (double-click to connect), with name, host[:port], user, domain and resolution.
- Passwords are stored in the **OS keychain** (macOS Keychain, Windows Credential Manager, Linux Secret Service such as GNOME Keyring or KWallet), never in the config file.
  On Linux without a Secret Service running, uncheck "Remember password" and type it each time.
- **Use NLA** (on by default): uncheck it to skip user/password and sign in on the server's own Windows screen.
  Only works if the server doesn't require NLA, and it's less safe: the password is typed into a remote
  screen over TLS whose certificate isn't checked. The keychain password is never used for such hosts.
- Each connection opens in its own window and process, so several sessions can run at once.

Hosts are saved in `hosts.toml` in the platform config directory:

| OS | Path |
|---|---|
| Linux | `~/.config/rdpvncdesk/hosts.toml` |
| macOS | `~/Library/Application Support/rdpvncdesk/hosts.toml` |
| Windows | `%APPDATA%\rdpvncdesk\config\hosts.toml` |

## rdp:// links

rdpvncdesk can open `rdp://` links, both the simple form and Microsoft's:

```
rdp://server.example.com
rdp://alice@server.example.com:3390
rdp://full%20address=s:server.example.com:3389&username=s:CORP%5Calice
```

Links never ask for credentials. A link to a **saved host** connects straight away: with NLA and the
keychain password if there is one, otherwise without NLA (you sign in on the server's screen).
A link to any **other host** only fills in the connection form (NLA off) and waits for you to press
Connect, so a web page can neither get your stored credentials sent to a host you did not choose nor
pop up a look-alike Windows sign-in screen. Passwords inside URLs are ignored.

Register it as the handler (this replaces any previous rdp:// handler, e.g. Microsoft Remote Desktop / Windows App):

| OS | How |
|---|---|
| Windows | `rdpvncdesk --register-url-handler` (writes `HKCU\Software\Classes\rdp`, no admin needed) |
| Linux | `rdpvncdesk --register-url-handler` (creates `~/.local/share/applications/rdpvncdesk.desktop` and runs `xdg-mime`) |
| macOS | `packaging/macos/make-app.sh`, copy `target/release/rdpvncdesk.app` to `/Applications`, open it once, then run `/Applications/rdpvncdesk.app/Contents/MacOS/rdpvncdesk --register-url-handler` |

On macOS links only work through the `.app` bundle (macOS delivers them as Apple Events, not arguments).
CI builds the bundle for both Mac architectures.

## Command line

```sh
rdpvncdesk server.example.com -u alice            # password prompted
rdpvncdesk 10.0.0.5:3390 -u alice -d CORP --width 1920 --height 1080
```

| Option | Env var | Description |
|---|---|---|
| `<DESTINATION>` | | `host` or `host:port` (default port 3389) |
| `-u, --username` | `RDPVNCDESK_USER` | User name (prompted if missing) |
| `-d, --domain` | `RDPVNCDESK_DOMAIN` | Windows domain |
| `-p, --password` | `RDPVNCDESK_PASSWORD` | Password (prompted if missing; prefer the prompt) |
| `--password-stdin` | | Read the password from the first line of stdin (for scripts) |
| `--no-nla` | | Connect without NLA: no credentials sent, the server shows its own sign-in screen (if it allows it) |
| `--width` / `--height` | | Remote desktop size (default 1280x800) |

**Ctrl+Alt+Del:** press **Ctrl+Alt+End** (like mstsc); on macOS **Ctrl+Option+Delete** also works.

**macOS:** **Cmd+C/V/X/A/Z/Y/S/F** are sent as Ctrl+key; Cmd with any other key, or alone, acts as the Windows key.
Clipboard sharing between the local and remote machine is not implemented yet.

The window can be resized freely; the remote image is scaled to fit.

Each session writes its log to `last-session-<host>.log` (overwritten by the next session to that host; a
second session to the same host while the first is open uses `last-session-<host>-2.log`):
`~/Library/Logs/rdpvncdesk/` on macOS, `<local data dir>/rdpvncdesk/logs/` on Windows and Linux. Logging: `RDPVNCDESK_LOG=debug`.

## Build

Requires Rust 1.94+.

```sh
cargo build --release
```

Linux also needs: `libxkbcommon-dev libwayland-dev libx11-dev libgl1-mesa-dev libegl1-mesa-dev libssl-dev pkg-config` (Debian/Ubuntu names).

Prebuilt binaries for every OS are produced by GitHub Actions on each push (artifacts) and attached to releases for `v*` tags.

## Security note

Like IronRDP's own viewer, the server's TLS certificate is **not validated** yet. Only connect to servers on networks you trust.

## Roadmap

- [ ] Server certificate validation / trust-on-first-use
- [ ] Clipboard
- [ ] Dynamic resolution on window resize
- [x] Connection manager with saved hosts and keychain passwords
- [x] rdp:// deep links
- [ ] Windows: hide the console window when launched by double-click
- [ ] Windows/Linux: reuse the running launcher for new links (today each link opens a new one)
- [ ] VNC protocol

## License

MIT OR Apache-2.0. `src/keymap.rs` is adapted from IronRDP (© Devolutions Inc., MIT OR Apache-2.0).
