//! Connection manager window: list, edit and launch saved hosts.
//!
//! Each connection runs in its own child process (this same executable in session mode).
//! That keeps sessions independent: several can be open at once, and a crash in one
//! does not take down the launcher.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock, mpsc};

use eframe::egui;

use crate::deeplink::{self, LinkTarget};
use crate::hosts::{self, Host};

/// Result of a finished session, reported back to the launcher.
struct SessionEnded {
    label: String,
    success: bool,
    stderr: String,
}

/// Links received from the OS (macOS Apple Events), waiting for the next frame.
static INCOMING_LINKS: Mutex<Vec<String>> = Mutex::new(Vec::new());
/// Set once the window exists, so a link can wake the UI up.
static REPAINT_CTX: OnceLock<egui::Context> = OnceLock::new();

#[cfg(target_os = "macos")]
fn queue_link(url: String) {
    if let Ok(mut q) = INCOMING_LINKS.lock() {
        q.push(url);
    }
    if let Some(ctx) = REPAINT_CTX.get() {
        ctx.request_repaint();
    }
}

pub fn run(initial_link: Option<LinkTarget>) -> anyhow::Result<()> {
    // Must be installed before the event loop starts: when a link launches the app, macOS
    // delivers it right after launch, before the window (and our App) exist.
    #[cfg(target_os = "macos")]
    deeplink::install_url_handler(queue_link);

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("rdpvncdesk")
            .with_inner_size([720.0, 440.0])
            .with_min_inner_size([520.0, 320.0]),
        ..Default::default()
    };
    eframe::run_native(
        "rdpvncdesk",
        options,
        Box::new(move |cc| {
            let _ = REPAINT_CTX.set(cc.egui_ctx.clone());
            // AppKit replaces the GURL handler when it finishes launching; take it back now
            // so links also reach an already running launcher.
            #[cfg(target_os = "macos")]
            deeplink::install_url_handler(queue_link);
            let mut launcher = Launcher::new();
            launcher.pending_link = initial_link;
            Ok(Box::new(launcher))
        }),
    )
    .map_err(|e| anyhow::anyhow!("launcher window: {e}"))
}

struct Launcher {
    /// Credentials dialog shown at connect time (issue #5). `Some(true)` on the frame it opens,
    /// to focus the password field once.
    ask_password: Option<bool>,
    hosts: Vec<Host>,
    selected: Option<usize>,
    /// Form being edited (copy of the selected host, or a new one).
    form: Host,
    /// Password typed in the form. Empty means "use the one saved in the keychain": the keychain
    /// is only read when connecting, so opening the launcher never triggers an OS prompt.
    password: String,
    status: Status,
    tx: mpsc::Sender<SessionEnded>,
    rx: mpsc::Receiver<SessionEnded>,
    running: usize,
    pending_link: Option<LinkTarget>,
}

enum Status {
    None,
    Info(String),
    Error(String),
    /// A session that failed: the last line is shown, the whole log can be copied.
    SessionError { summary: String, log: String },
}

/// Executable used for sessions. On macOS, inside the app bundle, that's the helper app
/// (Contents/Helpers/rdpvncdesk Session.app, issue #3): a session started from the main
/// executable makes macOS think the app is already open, so the manager can't be reopened.
fn session_exe() -> std::io::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    if cfg!(target_os = "macos")
        && let Some(contents) = exe.parent().and_then(|macos| macos.parent())
    {
        let helper = contents.join("Helpers/rdpvncdesk Session.app/Contents/MacOS/rdpvncdesk");
        if helper.is_file() {
            return Ok(helper);
        }
    }
    Ok(exe)
}

/// Removes ANSI colour codes (the session's log output is coloured).
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            // ESC [ ... final byte in 0x40..=0x7E
            if chars.next() == Some('[') {
                for c in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c) {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl Launcher {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel();
        let (hosts, status) = match hosts::load() {
            Ok(h) => (h, Status::None),
            Err(e) => (
                Vec::new(),
                Status::Error(format!("Could not load saved hosts: {e:#}")),
            ),
        };
        let mut me = Self {
            ask_password: None,
            hosts,
            selected: None,
            form: Host::default(),
            password: String::new(),
            status,
            tx,
            rx,
            running: 0,
            pending_link: None,
        };
        if !me.hosts.is_empty() {
            me.select(0);
        }
        me
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.form = self.hosts[index].clone();
        self.password.clear();
    }

    /// Opens an rdp:// link.
    ///
    /// A saved host is connected to straight away, without asking for anything. An unknown host
    /// only pre-fills the form (with NLA off): a link (e.g. from a web page) must never make us
    /// send stored credentials to, or open a sign-in screen from, a host the user did not choose.
    fn handle_link(&mut self, ctx: &egui::Context, link: LinkTarget) {
        ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        let wanted = deeplink::normalize_address(&link.address);
        let found = self.hosts.iter().position(|h| {
            deeplink::normalize_address(&h.address) == wanted
                && (link.username.is_empty() || h.username.eq_ignore_ascii_case(&link.username))
        });
        if let Some(i) = found {
            self.select(i);
            // Links never stop to ask for credentials: NLA with the saved password if there is
            // one, otherwise without NLA for this session only (the host's setting is kept).
            // Read the keychain once and hand the password over (issue #2: it was read
            // here and again in launch(), so macOS asked twice).
            let saved = if self.form.nla { self.form.load_password() } else { None };
            self.launch(ctx, saved.is_some(), saved);
            return;
        }
        self.new_host();
        self.form.address = link.address;
        self.form.username = link.username;
        self.form.domain = link.domain;
        if let Some(w) = link.width {
            self.form.width = w;
        }
        if let Some(h) = link.height {
            self.form.height = h;
        }
        // No NLA needed for links, but an unknown host is never connected to automatically:
        // a web page could otherwise open a fake Windows sign-in screen to phish the password.
        self.form.nla = false;
        self.status = Status::Info(
            "New host from link: check the address and press Connect to sign in on its screen"
                .into(),
        );
    }

    fn poll_links(&mut self, ctx: &egui::Context) {
        if let Some(link) = self.pending_link.take() {
            self.handle_link(ctx, link);
        }
        let urls = INCOMING_LINKS
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default();
        for url in urls {
            tracing::info!(%url, "received link");
            match deeplink::parse(&url) {
                Some(link) => self.handle_link(ctx, link),
                None => self.status = Status::Error(format!("Unsupported link: {url}")),
            }
        }
    }

    fn new_host(&mut self) {
        self.selected = None;
        self.form = Host::default();
        self.password.clear();
    }

    fn validate(&self) -> Result<(), String> {
        if self.form.address.trim().is_empty() {
            return Err("Host is required".into());
        }
        if self.form.nla && self.form.username.trim().is_empty() {
            return Err("User is required".into());
        }
        Ok(())
    }

    fn save(&mut self) -> bool {
        if let Err(e) = self.validate() {
            self.status = Status::Error(e);
            return false;
        }
        self.form.address = self.form.address.trim().to_owned();
        self.form.username = self.form.username.trim().to_owned();

        // If address/user changed, the old keychain entry would be orphaned.
        if let Some(i) = self.selected {
            let old = &self.hosts[i];
            if old.address != self.form.address || old.username != self.form.username {
                // The keychain entry is keyed by user@address: carry the password over.
                if self.password.is_empty() && self.form.remember_password {
                    self.password = old.load_password().unwrap_or_default();
                }
                old.delete_password();
            }
            self.hosts[i] = self.form.clone();
        } else {
            self.hosts.push(self.form.clone());
            self.selected = Some(self.hosts.len() - 1);
        }

        if let Err(e) = hosts::save(&self.hosts) {
            self.status = Status::Error(format!("Could not save hosts: {e:#}"));
            return false;
        }

        if !self.form.nla {
            // Leave the keychain alone: the password is not used without NLA, and keeping it
            // means switching NLA back on doesn't require typing it again.
        } else if !self.form.remember_password {
            self.form.delete_password();
        } else if !self.password.is_empty()
            && let Err(e) = self.form.store_password(&self.password)
        {
            self.status = Status::Error(format!("Saved host, but not the password: {e:#}"));
            return true;
        }
        // Remember checked and nothing typed: keep whatever the keychain already has.
        self.status = Status::Info(format!("Saved \"{}\"", self.form.label()));
        true
    }

    fn delete(&mut self) {
        let Some(i) = self.selected else { return };
        let host = self.hosts.remove(i);
        host.delete_password();
        if let Err(e) = hosts::save(&self.hosts) {
            self.status = Status::Error(format!("Could not save hosts: {e:#}"));
        } else {
            self.status = Status::Info(format!("Deleted \"{}\"", host.label()));
        }
        if self.hosts.is_empty() {
            self.new_host();
        } else {
            self.select(i.min(self.hosts.len() - 1));
        }
    }

    fn connect(&mut self, ctx: &egui::Context) {
        let nla = self.form.nla;
        self.launch(ctx, nla, None);
    }

    /// Saves the form and starts a session, using NLA or not for this session only.
    /// `saved_password`: already read from the keychain by the caller, so it isn't read twice.
    fn launch(&mut self, ctx: &egui::Context, nla: bool, saved_password: Option<String>) {
        if let Err(e) = self.validate() {
            self.status = Status::Error(e);
            return;
        }
        if !self.save() {
            return;
        }
        // Read the keychain only now, at connect time.
        let password = if !nla {
            String::new()
        } else if let Some(saved) = saved_password {
            saved
        } else if !self.password.is_empty() {
            self.password.clone()
        } else if let Some(saved) = self.form.load_password() {
            saved
        } else {
            // Like mstsc: ask for the password now instead of failing (issue #5).
            self.ask_password = Some(true);
            self.status = Status::None;
            return;
        };

        let exe = match session_exe() {
            Ok(p) => p,
            Err(e) => {
                self.status = Status::Error(format!("Cannot locate own executable: {e}"));
                return;
            }
        };
        let mut host = self.form.clone();
        host.nla = nla;
        let mut cmd = Command::new(exe);
        cmd.arg(&host.address)
            .args(["--width", &host.width.to_string()])
            .args(["--height", &host.height.to_string()])
            // Make sure values from the launcher's environment don't override the form.
            .env_remove("RDPVNCDESK_PASSWORD")
            .env_remove("RDPVNCDESK_USER")
            .env_remove("RDPVNCDESK_DOMAIN")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
        if !host.username.is_empty() {
            cmd.args(["--username", &host.username]);
        }
        if host.nla {
            cmd.arg("--password-stdin");
        } else {
            cmd.arg("--no-nla");
        }
        if !host.domain.trim().is_empty() {
            cmd.args(["--domain", host.domain.trim()]);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                self.status = Status::Error(format!("Could not start session: {e}"));
                return;
            }
        };
        // Password goes through a pipe: never on the command line or in the environment.
        if let Some(mut stdin) = child.stdin.take()
            && host.nla
        {
            let _ = writeln!(stdin, "{password}");
        }

        self.running += 1;
        self.status = Status::Info(format!("Connecting to {}…", host.label()));
        let tx = self.tx.clone();
        let ctx = ctx.clone();
        let label = host.label().to_owned();
        std::thread::spawn(move || {
            let mut stderr = String::new();
            if let Some(mut err) = child.stderr.take() {
                let _ = err.read_to_string(&mut stderr);
            }
            let success = child.wait().map(|s| s.success()).unwrap_or(false);
            let _ = tx.send(SessionEnded {
                label,
                success,
                stderr,
            });
            ctx.request_repaint();
        });
    }

    fn poll_sessions(&mut self) {
        while let Ok(ended) = self.rx.try_recv() {
            self.running = self.running.saturating_sub(1);
            self.status = if ended.success {
                Status::Info(format!("Session with {} closed", ended.label))
            } else {
                let log = strip_ansi(&ended.stderr).trim().to_owned();
                let detail = log
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("unknown error");
                Status::SessionError {
                    summary: format!("{}: {detail}", ended.label),
                    log,
                }
            };
        }
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_sessions();
        let ctx = ui.ctx().clone();
        self.poll_links(&ctx);
        // Return does what the Connect button does (also from inside a text field).
        let enter_pressed = ctx.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.is_none());

        egui::Panel::left("hosts")
            .resizable(true)
            .default_size(200.0)
            .show(ui, |ui| {
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading("Hosts");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("+ New").clicked() {
                            self.new_host();
                        }
                    });
                });
                ui.separator();
                let mut clicked = None;
                let mut connect = None;
                egui::ScrollArea::vertical().show(ui, |ui| {
                    if self.hosts.is_empty() {
                        ui.weak("No saved hosts yet.");
                    }
                    for (i, host) in self.hosts.iter().enumerate() {
                        let r = ui.selectable_label(self.selected == Some(i), host.label());
                        if r.clicked() {
                            clicked = Some(i);
                        }
                        if r.double_clicked() {
                            connect = Some(i);
                        }
                    }
                });
                if let Some(i) = clicked {
                    self.select(i);
                }
                if let Some(i) = connect {
                    self.select(i);
                    self.connect(&ctx);
                }
            });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(6.0);
            ui.heading(if self.selected.is_some() {
                "Edit connection"
            } else {
                "New connection"
            });
            ui.add_space(8.0);

            egui::Grid::new("form")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut self.form.name).hint_text("Office PC"));
                    ui.end_row();

                    ui.label("Host");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.address)
                            .hint_text("server or server:3389"),
                    );
                    ui.end_row();

                    ui.label("User");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.form.username)
                            .hint_text(if self.form.nla { "" } else { "optional" }),
                    );
                    ui.end_row();

                    ui.label("Domain");
                    ui.add(egui::TextEdit::singleline(&mut self.form.domain).hint_text("optional"));
                    ui.end_row();

                    ui.label("");
                    let toggled = ui
                        .checkbox(&mut self.form.nla, "Use NLA (send credentials before connecting)")
                        .on_hover_text(
                            "Turn off to sign in on the server's own Windows screen instead. \
                             Only works if the server allows it, and is less safe: the password \
                             is typed into the remote screen.",
                        )
                        .changed();
                    if toggled {
                        self.password.clear();
                    }
                    ui.end_row();

                    let nla = self.form.nla;
                    ui.label("Password");
                    ui.add_enabled(
                        nla,
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .hint_text(if !nla {
                                "typed on the server's screen"
                            } else if self.selected.is_some() {
                                "empty = use saved password"
                            } else {
                                ""
                            }),
                    );
                    ui.end_row();

                    ui.label("");
                    ui.add_enabled(
                        nla,
                        egui::Checkbox::new(
                            &mut self.form.remember_password,
                            "Remember password in system keychain",
                        ),
                    );
                    ui.end_row();

                    ui.label("Resolution");
                    ui.horizontal(|ui| {
                        ui.add(egui::DragValue::new(&mut self.form.width).range(640..=7680));
                        ui.label("×");
                        ui.add(egui::DragValue::new(&mut self.form.height).range(480..=4320));
                    });
                    ui.end_row();
                });

            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("Connect").on_hover_text("Return").clicked()
                    || (enter_pressed && self.ask_password.is_none())
                {
                    self.connect(&ctx);
                }
                if ui.button("Save").clicked() {
                    self.save();
                }
                if self.selected.is_some() && ui.button("Delete").clicked() {
                    self.delete();
                }
            });

            ui.add_space(12.0);
            match &self.status {
                Status::None => {}
                Status::Info(msg) => {
                    ui.label(msg);
                }
                Status::Error(msg) => {
                    let msg = msg.clone();
                    error_with_copy(ui, &msg, &msg);
                }
                Status::SessionError { summary, log } => {
                    let (summary, log) = (summary.clone(), log.clone());
                    error_with_copy(ui, &summary, if log.is_empty() { &summary } else { &log });
                }
            }
            if self.running > 0 {
                ui.weak(format!("Open sessions: {}", self.running));
            }
        });

        if self.ask_password.is_some() {
            self.password_dialog(&ctx, enter_pressed);
        }
    }
}

impl Launcher {
    /// Asks for the password at connect time when NLA is on and none is saved (issue #5).
    fn password_dialog(&mut self, ctx: &egui::Context, enter_pressed: bool) {
        let focus_password = self.ask_password == Some(true);
        self.ask_password = Some(false);
        let mut connect = enter_pressed;
        let mut cancel = false;
        let modal = egui::Modal::new(egui::Id::unique("password-dialog")).show(ctx, |ui| {
            ui.set_width(320.0);
            ui.heading(format!("Sign in to {}", self.form.label()));
            ui.add_space(8.0);
            egui::Grid::new("password-dialog-grid")
                .num_columns(2)
                .spacing([12.0, 8.0])
                .show(ui, |ui| {
                    ui.label("User");
                    ui.text_edit_singleline(&mut self.form.username);
                    ui.end_row();
                    ui.label("Domain");
                    ui.add(egui::TextEdit::singleline(&mut self.form.domain).hint_text("optional"));
                    ui.end_row();
                    ui.label("Password");
                    let field = ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                    if focus_password {
                        field.request_focus();
                    }
                    ui.end_row();
                });
            ui.checkbox(
                &mut self.form.remember_password,
                "Remember password in system keychain",
            );
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                connect |= ui.button("Connect").clicked();
                cancel |= ui.button("Cancel").clicked();
            });
        });
        if cancel || modal.should_close() {
            self.ask_password = None;
            self.password.clear();
        } else if connect && !self.password.is_empty() {
            self.ask_password = None;
            self.connect(ctx);
        }
    }
}

/// Error text with a button that copies `copy` (e.g. the full session log) to the clipboard.
fn error_with_copy(ui: &mut egui::Ui, shown: &str, copy: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.colored_label(ui.visuals().error_fg_color, shown);
        let copied_id = ui.scope_id().with("error-copied");
        let copied = ui.data(|d| d.get_temp::<String>(copied_id)).as_deref() == Some(copy);
        if ui
            .small_button(if copied { "Copied" } else { "Copy" })
            .on_hover_text("Copy the full error to the clipboard")
            .clicked()
        {
            ui.ctx().copy_text(copy.to_owned());
            ui.data_mut(|d| d.insert_temp(copied_id, copy.to_owned()));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::strip_ansi;

    #[test]
    fn strips_colour_codes() {
        assert_eq!(
            strip_ansi("\u{1b}[2m2026\u{1b}[0m \u{1b}[31mERROR\u{1b}[0m failed"),
            "2026 ERROR failed"
        );
    }
}
