//! Window, rendering and input handling (same code on Windows, Linux and macOS).

use std::num::NonZeroU32;
use std::sync::Arc;

use ironrdp::client::rdp::{RdpInputEvent, RdpOutputEvent};
use ironrdp::connector::ConnectorErrorKind;
use ironrdp::pdu::nego::FailureCode;
use tokio::sync::mpsc::UnboundedSender;

type RdpInputSender = UnboundedSender<RdpInputEvent>;
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, OwnedDisplayHandle};
use winit::keyboard::{KeyCode, PhysicalKey};
use winit::window::{Window, WindowId};

use crate::keymap::{is_modifier, map_key_code};

type WindowSurface = Surface<OwnedDisplayHandle, Arc<Window>>;

pub struct App {
    context: Context<OwnedDisplayHandle>,
    window: Option<(Arc<Window>, WindowSurface)>,
    title: String,
    initial_size: (u16, u16),
    input: RdpInputSender,
    db: Database,
    /// Last full frame from the server, 0RGB u32 pixels.
    frame: Vec<u32>,
    frame_size: (u16, u16),
    close_requested: bool,
    exit_code: i32,
    /// Ctrl and Alt currently held, to detect the Ctrl+Alt+Del shortcuts.
    ctrl_alt: bool,
    /// Local key currently being sent as Delete, so its release is sent as Delete too.
    remapped_to_delete: Option<KeyCode>,
    /// Physical Ctrl held.
    ctrl_down: bool,
    /// macOS Command key handling, see [`App::command_key`].
    cmd: CmdState,
}

/// On macOS, Cmd+C/V/X/A/Z/Y/S/F are sent as Ctrl+key, like Mac users expect; any other key
/// with Cmd is sent with the Windows key. The Windows key press is delayed until we know which,
/// because a lone Win press-release would open the remote Start menu.
#[derive(Default)]
struct CmdState {
    held: bool,
    /// Win key already pressed on the server.
    logo_sent: bool,
    /// Some key was pressed while Cmd was held.
    used: bool,
    /// Key currently being sent as Ctrl+key.
    shortcut_key: Option<KeyCode>,
}

const CTRL: Scancode = Scancode::from_u8(false, 0x1D);
const LOGO: Scancode = Scancode::from_u8(true, 0x5B);
const MAC: bool = cfg!(target_os = "macos");

fn is_ctrl_shortcut(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::KeyC
            | KeyCode::KeyV
            | KeyCode::KeyX
            | KeyCode::KeyA
            | KeyCode::KeyZ
            | KeyCode::KeyY
            | KeyCode::KeyS
            | KeyCode::KeyF
    )
}

impl App {
    pub fn new(
        event_loop: &EventLoop<RdpOutputEvent>,
        input: RdpInputSender,
        title: String,
        initial_size: (u16, u16),
    ) -> anyhow::Result<Self> {
        let context = Context::new(event_loop.owned_display_handle())
            .map_err(|e| anyhow::anyhow!("softbuffer context: {e}"))?;
        Ok(Self {
            context,
            window: None,
            title,
            initial_size,
            input,
            db: Database::new(),
            frame: Vec::new(),
            frame_size: (0, 0),
            close_requested: false,
            exit_code: 0,
            ctrl_alt: false,
            remapped_to_delete: None,
            ctrl_down: false,
            cmd: CmdState::default(),
        })
    }

    pub fn exit_code(&self) -> i32 {
        self.exit_code
    }

    fn send(&mut self, ops: impl IntoIterator<Item = Operation>) {
        let events = self.db.apply(ops);
        if !events.is_empty() {
            let _ = self.input.send(RdpInputEvent::FastPath(events));
        }
    }

    /// Paints the last frame scaled (nearest neighbour) to the window size.
    fn draw(&mut self) {
        let Some((window, surface)) = self.window.as_mut() else {
            return;
        };
        let size = window.inner_size();
        let (Some(w), Some(h)) = (NonZeroU32::new(size.width), NonZeroU32::new(size.height)) else {
            return;
        };
        if surface.resize(w, h).is_err() {
            return;
        }
        let Ok(mut buf) = surface.buffer_mut() else {
            return;
        };
        let (fw, fh) = (
            usize::from(self.frame_size.0),
            usize::from(self.frame_size.1),
        );
        let (ww, wh) = (w.get() as usize, h.get() as usize);
        if fw == 0 || fh == 0 || self.frame.len() < fw * fh {
            buf.fill(0x0020_2020);
        } else if fw == ww && fh == wh {
            buf.copy_from_slice(&self.frame[..fw * fh]);
        } else {
            for y in 0..wh {
                let src_row = (y * fh / wh) * fw;
                let dst_row = y * ww;
                for x in 0..ww {
                    buf[dst_row + x] = self.frame[src_row + x * fw / ww];
                }
            }
        }
        let _ = buf.present();
    }

    /// Maps a window position to remote desktop coordinates.
    fn to_remote(&self, pos: PhysicalPosition<f64>) -> Option<MousePosition> {
        let (window, _) = self.window.as_ref()?;
        let size = window.inner_size();
        if size.width == 0 || size.height == 0 || self.frame_size.0 == 0 {
            return None;
        }
        let scale = |v: f64, win: u32, remote: u16| -> u16 {
            let r = v * f64::from(remote) / f64::from(win);
            r.clamp(0.0, f64::from(remote.saturating_sub(1))) as u16
        };
        Some(MousePosition {
            x: scale(pos.x, size.width, self.frame_size.0),
            y: scale(pos.y, size.height, self.frame_size.1),
        })
    }

    /// Ctrl+Alt+Del can't be typed into the window: Windows keeps it for itself and Mac keyboards
    /// have no Del key. Like mstsc, Ctrl+Alt+End sends it; on macOS Ctrl+Option+Delete does too.
    /// Returns the Delete scancode when `code` must be sent as Delete.
    fn secure_attention_key(&mut self, code: KeyCode, state: ElementState) -> Option<Scancode> {
        const DELETE: Scancode = Scancode::from_u8(true, 0x53);
        match state {
            ElementState::Pressed => {
                let shortcut = code == KeyCode::End
                    || (cfg!(target_os = "macos") && code == KeyCode::Backspace);
                if shortcut && self.ctrl_alt {
                    self.remapped_to_delete = Some(code);
                    return Some(DELETE);
                }
                // Key repeat of the remapped key while still held.
                (self.remapped_to_delete == Some(code)).then_some(DELETE)
            }
            ElementState::Released if self.remapped_to_delete == Some(code) => {
                self.remapped_to_delete = None;
                Some(DELETE)
            }
            ElementState::Released => None,
        }
    }

    /// macOS Command key: returns true when the key event was fully handled here.
    fn command_key(&mut self, code: KeyCode, state: ElementState) -> bool {
        let Some((scancode, _)) = map_key_code(code) else {
            return false;
        };
        match state {
            // Key repeat of a Ctrl+key shortcut.
            ElementState::Pressed if self.cmd.shortcut_key == Some(code) => {
                self.send([Operation::KeyPressed(scancode)]);
                true
            }
            ElementState::Pressed if self.cmd.held => {
                self.cmd.used = true;
                if is_ctrl_shortcut(code) {
                    let mut ops = Vec::new();
                    if self.cmd.logo_sent {
                        ops.push(Operation::KeyReleased(LOGO));
                        self.cmd.logo_sent = false;
                    }
                    ops.push(Operation::KeyPressed(CTRL));
                    ops.push(Operation::KeyPressed(scancode));
                    self.send(ops);
                    self.cmd.shortcut_key = Some(code);
                    true
                } else {
                    if !self.cmd.logo_sent {
                        self.send([Operation::KeyPressed(LOGO)]);
                        self.cmd.logo_sent = true;
                    }
                    false
                }
            }
            ElementState::Released if self.cmd.shortcut_key == Some(code) => {
                self.cmd.shortcut_key = None;
                let mut ops = vec![Operation::KeyReleased(scancode)];
                if !self.ctrl_down {
                    ops.push(Operation::KeyReleased(CTRL));
                }
                self.send(ops);
                true
            }
            _ => false,
        }
    }

    fn on_command_modifier(&mut self, held: bool) {
        if held == self.cmd.held {
            return;
        }
        if held {
            self.cmd = CmdState {
                held: true,
                ..CmdState::default()
            };
        } else {
            self.cmd.held = false;
            if self.cmd.logo_sent {
                self.send([Operation::KeyReleased(LOGO)]);
            } else if !self.cmd.used {
                // Cmd alone: Windows key tap (Start menu).
                self.send([Operation::KeyPressed(LOGO)]);
                self.send([Operation::KeyReleased(LOGO)]);
            }
            self.cmd.logo_sent = false;
        }
    }

    fn on_key(&mut self, event: &KeyEvent) {
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        // Modifiers are reported through ModifiersChanged.
        if is_modifier(code) {
            return;
        }
        if MAC && self.command_key(code, event.state) {
            return;
        }
        if let Some(scancode) = self.secure_attention_key(code, event.state) {
            match event.state {
                ElementState::Pressed => self.send([Operation::KeyPressed(scancode)]),
                ElementState::Released => self.send([Operation::KeyReleased(scancode)]),
            }
            return;
        }
        let Some((scancode, release_only)) = map_key_code(code) else {
            tracing::debug!(?code, "unmapped key");
            return;
        };
        match event.state {
            ElementState::Pressed => self.send([Operation::KeyPressed(scancode)]),
            ElementState::Released if release_only => self.send([
                Operation::KeyPressed(scancode),
                Operation::KeyReleased(scancode),
            ]),
            ElementState::Released => self.send([Operation::KeyReleased(scancode)]),
        }
    }
}

impl ApplicationHandler<RdpOutputEvent> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title(&self.title)
            .with_inner_size(LogicalSize::new(self.initial_size.0, self.initial_size.1));
        match event_loop.create_window(attrs) {
            Ok(window) => {
                let window = Arc::new(window);
                match Surface::new(&self.context, Arc::clone(&window)) {
                    Ok(surface) => self.window = Some((window, surface)),
                    Err(e) => {
                        eprintln!("Unable to create drawing surface: {e}");
                        self.exit_code = 1;
                        event_loop.exit();
                    }
                }
            }
            Err(e) => {
                eprintln!("Unable to create window: {e}");
                self.exit_code = 1;
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => {
                if self.close_requested {
                    // Second close: don't wait for the server.
                    event_loop.exit();
                } else {
                    self.close_requested = true;
                    let _ = self.input.send(RdpInputEvent::Close);
                }
            }
            WindowEvent::RedrawRequested => self.draw(),
            WindowEvent::Resized(_) => {
                if let Some((w, _)) = &self.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Focused(false) => {
                // Avoid keys stuck "down" on the server when the window loses focus.
                self.remapped_to_delete = None;
                self.cmd = CmdState::default();
                let events = self.db.release_all();
                if !events.is_empty() {
                    let _ = self.input.send(RdpInputEvent::FastPath(events));
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_key(&event),
            WindowEvent::ModifiersChanged(mods) => {
                const SHIFT: Scancode = Scancode::from_u8(false, 0x2A);
                const ALT: Scancode = Scancode::from_u8(false, 0x38);
                let s = mods.state();
                self.ctrl_alt = s.control_key() && s.alt_key();
                self.ctrl_down = s.control_key();
                // Keep Ctrl down on the server while a Cmd+key shortcut is still held.
                let ctrl = s.control_key() || self.cmd.shortcut_key.is_some();
                let mut mods_down = vec![(s.shift_key(), SHIFT), (ctrl, CTRL), (s.alt_key(), ALT)];
                if MAC {
                    self.on_command_modifier(s.super_key());
                } else {
                    mods_down.push((s.super_key(), LOGO));
                }
                let ops = mods_down.into_iter().map(|(down, sc)| {
                    if down {
                        Operation::KeyPressed(sc)
                    } else {
                        Operation::KeyReleased(sc)
                    }
                });
                self.send(ops);
            }
            WindowEvent::CursorMoved { position, .. } => {
                if let Some(pos) = self.to_remote(position) {
                    self.send([Operation::MouseMove(pos)]);
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Left,
                    winit::event::MouseButton::Right => MouseButton::Right,
                    winit::event::MouseButton::Middle => MouseButton::Middle,
                    winit::event::MouseButton::Back => MouseButton::X1,
                    winit::event::MouseButton::Forward => MouseButton::X2,
                    winit::event::MouseButton::Other(_) => return,
                };
                self.send([match state {
                    ElementState::Pressed => Operation::MouseButtonPressed(button),
                    ElementState::Released => Operation::MouseButtonReleased(button),
                }]);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (dx, dy) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(x) * 120.0, f64::from(y) * 120.0)
                    }
                    MouseScrollDelta::PixelDelta(p) => (p.x, p.y),
                };
                let ops = [(false, dx), (true, dy)]
                    .into_iter()
                    .filter(|(_, d)| d.abs() > 0.001)
                    .map(|(is_vertical, d)| {
                        Operation::WheelRotations(WheelRotations {
                            is_vertical,
                            rotation_units: d.clamp(f64::from(i16::MIN), f64::from(i16::MAX))
                                as i16,
                        })
                    });
                self.send(ops);
            }
            _ => {}
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: RdpOutputEvent) {
        match event {
            RdpOutputEvent::Image {
                buffer,
                width,
                height,
            } => {
                self.frame = buffer;
                self.frame_size = (width.get(), height.get());
                save_frame_for_debugging(&self.frame, self.frame_size);
                if let Some((w, _)) = &self.window {
                    w.request_redraw();
                }
            }
            RdpOutputEvent::PointerHidden => {
                if let Some((w, _)) = &self.window {
                    w.set_cursor_visible(false);
                }
            }
            RdpOutputEvent::PointerDefault | RdpOutputEvent::PointerBitmap(_) => {
                if let Some((w, _)) = &self.window {
                    w.set_cursor_visible(true);
                }
            }
            RdpOutputEvent::ConnectionFailure(error) => {
                tracing::error!(phase = %crate::phase::last_state(), "Connection failed: {}", error.report());
                eprintln!("Connection failed: {}", error.report());
                // The launcher shows the last stderr line, so the hint goes last.
                if let Some(hint) = failure_hint(error.kind()) {
                    tracing::error!("{hint}");
                    eprintln!("{hint}");
                }
                self.exit_code = 1;
                event_loop.exit();
            }
            RdpOutputEvent::Terminated(result) => {
                match result {
                    Ok(reason) => println!("Session ended: {reason}"),
                    Err(error) if self.close_requested => {
                        tracing::debug!(%error, "session closed by user");
                    }
                    Err(error) => {
                        tracing::error!("Session error: {}", error.report());
                        eprintln!("Session error: {}", error.report());
                        self.exit_code = 1;
                    }
                }
                event_loop.exit();
            }
            RdpOutputEvent::PointerPosition { .. } => {}
        }
    }
}

/// Plain-language explanation for the connection failures users can fix themselves.
fn failure_hint(kind: &ConnectorErrorKind) -> Option<&'static str> {
    match kind {
        ConnectorErrorKind::Negotiation(failure)
            if failure.code() == FailureCode::HYBRID_REQUIRED_BY_SERVER =>
        {
            Some("This server requires NLA: turn on \"Use NLA\" and enter user and password")
        }
        ConnectorErrorKind::AccessDenied => {
            Some("Access denied: this user is not allowed to connect to this server")
        }
        // Servers that reject the credentials usually just close the connection during NLA
        // (Windows sometimes sends an error code first, which surfaces as a CredSSP error).
        _ if crate::phase::failed_during_authentication() => Some(
            "Sign-in failed: the server rejected the user name or password. \
             Check user, password and domain (leave domain empty for local or Linux accounts)",
        ),
        _ => None,
    }
}

/// RDPVNCDESK_SESSION_SCREENSHOT=<file.tga>: keeps the latest remote frame in that file, to check
/// rendering without screen-recording permission.
fn save_frame_for_debugging(frame: &[u32], (width, height): (u16, u16)) {
    let Some(path) = std::env::var_os("RDPVNCDESK_SESSION_SCREENSHOT") else { return };
    let mut out = vec![0u8, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0];
    out.extend_from_slice(&width.to_le_bytes());
    out.extend_from_slice(&height.to_le_bytes());
    out.extend_from_slice(&[24, 0x20]);
    for px in frame {
        let [_, r, g, b] = px.to_be_bytes();
        out.extend_from_slice(&[b, g, r]);
    }
    let _ = std::fs::write(path, out);
}
