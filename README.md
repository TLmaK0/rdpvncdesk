# rdpvncdesk

Cross-platform **RDP client** for **Windows, Linux and macOS**, built from a single Rust codebase.

The RDP protocol is handled by [IronRDP](https://github.com/Devolutions/IronRDP) (pure Rust), the window by
[winit](https://github.com/rust-windowing/winit) and drawing by [softbuffer](https://github.com/rust-windowing/softbuffer).
No platform-specific code paths in this repo: the same source compiles on all three systems.

> Status: early. Connect, view the remote desktop, keyboard and mouse work. VNC support is planned (hence the name).

## Connection manager

Run `rdpvncdesk` with no arguments to open the connection manager:

- Saved hosts list (double-click to connect), with name, host[:port], user, domain and resolution.
- Passwords are stored in the **OS keychain** (macOS Keychain, Windows Credential Manager, Linux Secret Service such as GNOME Keyring or KWallet), never in the config file.
  On Linux without a Secret Service running, uncheck "Remember password" and type it each time.
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

A link connects straight away **only** to a host already saved with a password in the keychain.
Any other link just fills in the connection form, so a link from a web page can never make the client
send stored credentials to a host you did not choose. Passwords inside URLs are ignored.

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
| `--width` / `--height` | | Remote desktop size (default 1280x800) |

The window can be resized freely; the remote image is scaled to fit. Logging: `RDPVNCDESK_LOG=debug`.

## Build

Requires Rust 1.94+.

```sh
cargo build --release
```

Linux also needs: `libxkbcommon-dev libwayland-dev libx11-dev libgl1-mesa-dev libegl1-mesa-dev` (Debian/Ubuntu names).

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
