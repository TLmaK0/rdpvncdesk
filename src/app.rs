//! Window, rendering and input handling (same code on Windows, Linux and macOS).

use std::num::NonZeroU32;
use std::sync::Arc;

use ironrdp::client::rdp::{RdpInputEvent, RdpOutputEvent};
use tokio::sync::mpsc::UnboundedSender;

type RdpInputSender = UnboundedSender<RdpInputEvent>;
use ironrdp::input::{Database, MouseButton, MousePosition, Operation, Scancode, WheelRotations};
use softbuffer::{Context, Surface};
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalPosition};
use winit::event::{ElementState, KeyEvent, MouseScrollDelta, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop, OwnedDisplayHandle};
use winit::keyboard::PhysicalKey;
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

    fn on_key(&mut self, event: &KeyEvent) {
        let PhysicalKey::Code(code) = event.physical_key else {
            return;
        };
        // Modifiers are reported through ModifiersChanged.
        if is_modifier(code) {
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
                let events = self.db.release_all();
                if !events.is_empty() {
                    let _ = self.input.send(RdpInputEvent::FastPath(events));
                }
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_key(&event),
            WindowEvent::ModifiersChanged(mods) => {
                const SHIFT: Scancode = Scancode::from_u8(false, 0x2A);
                const CTRL: Scancode = Scancode::from_u8(false, 0x1D);
                const ALT: Scancode = Scancode::from_u8(false, 0x38);
                const LOGO: Scancode = Scancode::from_u8(true, 0x5B);
                let s = mods.state();
                let ops = [
                    (s.shift_key(), SHIFT),
                    (s.control_key(), CTRL),
                    (s.alt_key(), ALT),
                    (s.super_key(), LOGO),
                ]
                .map(|(down, sc)| {
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
                eprintln!("Connection failed: {}", error.report());
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
