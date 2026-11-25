//! Connection manager window: list, edit and launch saved hosts.
//!
//! Each connection runs in its own child process (this same executable in session mode).
//! That keeps sessions independent: several can be open at once, and a crash in one
//! does not take down the launcher.

use std::io::{Read as _, Write as _};
use std::process::{Command, Stdio};
use std::sync::mpsc;

use eframe::egui;

use crate::hosts::{self, Host};

/// Result of a finished session, reported back to the launcher.
struct SessionEnded {
    label: String,
    success: bool,
    stderr: String,
}

pub fn run() -> anyhow::Result<()> {
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
        Box::new(|_cc| Ok(Box::new(Launcher::new()))),
    )
    .map_err(|e| anyhow::anyhow!("launcher window: {e}"))
}

struct Launcher {
    hosts: Vec<Host>,
    selected: Option<usize>,
    /// Form being edited (copy of the selected host, or a new one).
    form: Host,
    password: String,
    remember_password: bool,
    status: Status,
    tx: mpsc::Sender<SessionEnded>,
    rx: mpsc::Receiver<SessionEnded>,
    running: usize,
}

enum Status {
    None,
    Info(String),
    Error(String),
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
            hosts,
            selected: None,
            form: Host::default(),
            password: String::new(),
            remember_password: true,
            status,
            tx,
            rx,
            running: 0,
        };
        if !me.hosts.is_empty() {
            me.select(0);
        }
        me
    }

    fn select(&mut self, index: usize) {
        self.selected = Some(index);
        self.form = self.hosts[index].clone();
        self.password = self.form.load_password().unwrap_or_default();
        self.remember_password = true;
    }

    fn new_host(&mut self) {
        self.selected = None;
        self.form = Host::default();
        self.password.clear();
        self.remember_password = true;
    }

    fn validate(&self) -> Result<(), String> {
        if self.form.address.trim().is_empty() {
            return Err("Host is required".into());
        }
        if self.form.username.trim().is_empty() {
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

        if self.remember_password && !self.password.is_empty() {
            if let Err(e) = self.form.store_password(&self.password) {
                self.status = Status::Error(format!("Saved host, but not the password: {e:#}"));
                return true;
            }
        } else {
            self.form.delete_password();
        }
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
        if let Err(e) = self.validate() {
            self.status = Status::Error(e);
            return;
        }
        if self.password.is_empty() {
            self.status = Status::Error("Password is required".into());
            return;
        }
        if !self.save() {
            return;
        }

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                self.status = Status::Error(format!("Cannot locate own executable: {e}"));
                return;
            }
        };
        let host = self.form.clone();
        let mut cmd = Command::new(exe);
        cmd.arg(&host.address)
            .args(["--username", &host.username])
            .args(["--width", &host.width.to_string()])
            .args(["--height", &host.height.to_string()])
            .arg("--password-stdin")
            // Make sure values from the launcher's environment don't override the form.
            .env_remove("RDPVNCDESK_PASSWORD")
            .env_remove("RDPVNCDESK_USER")
            .env_remove("RDPVNCDESK_DOMAIN")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped());
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
        if let Some(mut stdin) = child.stdin.take() {
            let _ = writeln!(stdin, "{}", self.password);
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
                let detail = ended
                    .stderr
                    .lines()
                    .rev()
                    .find(|l| !l.trim().is_empty())
                    .unwrap_or("unknown error")
                    .to_owned();
                Status::Error(format!("{}: {detail}", ended.label))
            };
        }
    }
}

impl eframe::App for Launcher {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.poll_sessions();
        let ctx = ui.ctx().clone();

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
                    ui.text_edit_singleline(&mut self.form.username);
                    ui.end_row();

                    ui.label("Domain");
                    ui.add(egui::TextEdit::singleline(&mut self.form.domain).hint_text("optional"));
                    ui.end_row();

                    ui.label("Password");
                    ui.add(egui::TextEdit::singleline(&mut self.password).password(true));
                    ui.end_row();

                    ui.label("");
                    ui.checkbox(
                        &mut self.remember_password,
                        "Remember password in system keychain",
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
                if ui.button("▶ Connect").clicked() {
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
                    ui.colored_label(ui.visuals().error_fg_color, msg);
                }
            }
            if self.running > 0 {
                ui.weak(format!("Open sessions: {}", self.running));
            }
        });
    }
}
