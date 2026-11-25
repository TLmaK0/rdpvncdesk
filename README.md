# rdpvncdesk

Cross-platform **RDP client** for **Windows, Linux and macOS**, built from a single Rust codebase.

The RDP protocol is handled by [IronRDP](https://github.com/Devolutions/IronRDP) (pure Rust), the window by
[winit](https://github.com/rust-windowing/winit) and drawing by [softbuffer](https://github.com/rust-windowing/softbuffer).
No platform-specific code paths in this repo: the same source compiles on all three systems.

> Status: early. Connect, view the remote desktop, keyboard and mouse work. VNC support is planned (hence the name).

## Usage

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
| `--width` / `--height` | | Remote desktop size (default 1280x800) |

The window can be resized freely; the remote image is scaled to fit. Logging: `RDPVNCDESK_LOG=debug`.

## Build

Requires Rust 1.94+.

```sh
cargo build --release
```

Linux also needs: `libxkbcommon-dev libwayland-dev libx11-dev` (Debian/Ubuntu names).

Prebuilt binaries for every OS are produced by GitHub Actions on each push (artifacts) and attached to releases for `v*` tags.

## Security note

Like IronRDP's own viewer, the server's TLS certificate is **not validated** yet. Only connect to servers on networks you trust.

## Roadmap

- [ ] Server certificate validation / trust-on-first-use
- [ ] Clipboard
- [ ] Dynamic resolution on window resize
- [ ] Connection dialog (GUI) instead of CLI-only
- [ ] VNC protocol

## License

MIT OR Apache-2.0. `src/keymap.rs` is adapted from IronRDP (© Devolutions Inc., MIT OR Apache-2.0).
