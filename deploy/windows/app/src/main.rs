#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod cloud;
mod config;
mod worker;

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui;

use crate::cloud::CloudClient;
use crate::config::{UiState, WorkerSnapshot};
use crate::worker::{WorkerProcess, WorkerStatus};

const APP_TITLE: &str = "Beenet";

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 640.0])
            .with_min_inner_size([380.0, 560.0])
            .with_title(APP_TITLE),
        ..Default::default()
    };
    eframe::run_native(
        APP_TITLE,
        options,
        Box::new(|cc| Ok(Box::new(BeenetApp::new(cc)))),
    )
}

enum UiEvent {
    Message(String),
    LoginProgress { user_code: String, url: String },
    LoggedIn { token: String, email: String },
    Points(Option<i64>),
    Status(WorkerStatus),
    Busy(bool),
}

struct BeenetApp {
    snapshot: WorkerSnapshot,
    config_path: PathBuf,
    ui: UiState,
    status: WorkerStatus,
    busy: bool,
    message: Option<String>,
    login_url: Option<String>,
    user_code: Option<String>,
    show_settings: bool,
    events: Receiver<UiEvent>,
    tx: Sender<UiEvent>,
    last_status: Instant,
}

impl BeenetApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        setup_cjk_fonts(&cc.egui_ctx);
        let (tx, events) = mpsc::channel();
        let (config_path, snapshot) = WorkerSnapshot::load_or_create();
        let ui = UiState::load();
        let app = Self {
            snapshot,
            config_path,
            ui,
            status: WorkerStatus::default(),
            busy: false,
            message: None,
            login_url: None,
            user_code: None,
            show_settings: false,
            events,
            tx: tx.clone(),
            last_status: Instant::now() - Duration::from_secs(2),
        };
        app.refresh_status();
        if !app.ui.token.is_empty() {
            app.refresh_points();
        }
        app
    }

    fn save_config(&mut self) {
        if let Err(error) = self.snapshot.save(&self.config_path) {
            self.message = Some(error);
        }
    }

    fn refresh_status(&self) {
        let tx = self.tx.clone();
        let config = self.config_path.clone();
        thread::spawn(move || {
            match WorkerProcess::run(&config, "status", &[], None) {
                Ok(output) => {
                    let _ = tx.send(UiEvent::Status(WorkerStatus::parse(&output)));
                }
                Err(error) => {
                    let _ = tx.send(UiEvent::Message(error));
                }
            }
        });
    }

    fn refresh_points(&self) {
        let tx = self.tx.clone();
        let token = self.ui.token.clone();
        if token.is_empty() {
            return;
        }
        thread::spawn(move || {
            let points = CloudClient::points(&token).ok();
            let _ = tx.send(UiEvent::Points(points));
        });
    }

    fn login(&mut self) {
        self.busy = true;
        self.message = Some("正在连接 Cloud…".into());
        let tx = self.tx.clone();
        thread::spawn(move || {
            let started = match CloudClient::start_device_login() {
                Ok(value) => value,
                Err(error) => {
                    let _ = tx.send(UiEvent::Message(error));
                    let _ = tx.send(UiEvent::Busy(false));
                    return;
                }
            };
            let _ = tx.send(UiEvent::LoginProgress {
                user_code: started.user_code.clone(),
                url: started.verification_uri.clone(),
            });
            let _ = open::that(&started.verification_uri);
            let deadline = Instant::now() + Duration::from_secs(started.expires_in.max(30));
            while Instant::now() < deadline {
                match CloudClient::poll_device_login(&started.device_code) {
                    Ok(poll) if poll.status == "approved" => {
                        if let (Some(token), Some(user)) = (poll.token, poll.user) {
                            let _ = tx.send(UiEvent::LoggedIn {
                                token,
                                email: user.email,
                            });
                            let _ = tx.send(UiEvent::Busy(false));
                            return;
                        }
                    }
                    Ok(_) => {}
                    Err(error) => {
                        let _ = tx.send(UiEvent::Message(error));
                        let _ = tx.send(UiEvent::Busy(false));
                        return;
                    }
                }
                thread::sleep(Duration::from_secs(2));
            }
            let _ = tx.send(UiEvent::Message("登录超时，请再点一次登录".into()));
            let _ = tx.send(UiEvent::Busy(false));
        });
    }

    fn logout(&mut self) {
        self.ui.clear();
        self.message = Some("已退出 Cloud".into());
        self.login_url = None;
        self.user_code = None;
    }

    fn start_or_enroll(&mut self) {
        self.save_config();
        if self.ui.token.is_empty() {
            self.message = Some("请先登录 Cloud 平台".into());
            self.show_settings = true;
            return;
        }
        self.busy = true;
        let tx = self.tx.clone();
        let config = self.config_path.clone();
        let token = self.ui.token.clone();
        let name = self.snapshot.name.clone();
        let region = self.snapshot.region.clone();
        let has_identity = self.snapshot.has_identity();
        thread::spawn(move || {
            let result = if has_identity {
                start_worker(&config)
            } else {
                enroll_then_start(&config, &token, &name, &region)
            };
            match result {
                Ok(message) => {
                    let _ = tx.send(UiEvent::Message(message));
                    if let Ok(output) = WorkerProcess::run(&config, "status", &[], None) {
                        let _ = tx.send(UiEvent::Status(WorkerStatus::parse(&output)));
                    }
                }
                Err(error) => {
                    let _ = tx.send(UiEvent::Message(error));
                }
            }
            let _ = tx.send(UiEvent::Busy(false));
        });
    }

    fn stop_worker(&mut self) {
        self.busy = true;
        let tx = self.tx.clone();
        let config = self.config_path.clone();
        thread::spawn(move || {
            match WorkerProcess::run(&config, "stop", &[], None) {
                Ok(_) => {
                    let _ = tx.send(UiEvent::Message("已停止贡献".into()));
                }
                Err(error) => {
                    let _ = tx.send(UiEvent::Message(error));
                }
            }
            if let Ok(output) = WorkerProcess::run(&config, "status", &[], None) {
                let _ = tx.send(UiEvent::Status(WorkerStatus::parse(&output)));
            }
            let _ = tx.send(UiEvent::Busy(false));
        });
    }

    fn sync_name_region(&mut self) {
        self.save_config();
        let Some(peer_id) = self
            .status
            .peer_id
            .clone()
            .filter(|value| !value.is_empty())
        else {
            return;
        };
        if self.ui.token.is_empty() {
            return;
        }
        let tx = self.tx.clone();
        let token = self.ui.token.clone();
        let name = self.snapshot.name.clone();
        let region = self.snapshot.region.clone();
        thread::spawn(move || {
            match CloudClient::claim_worker(&token, &peer_id, &name, &region, None) {
                Ok(()) => {
                    let _ = tx.send(UiEvent::Message("名称和地区已同步到 Cloud".into()));
                }
                Err(error) => {
                    let _ = tx.send(UiEvent::Message(error));
                }
            }
        });
    }

    fn drain_events(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                UiEvent::Message(message) => self.message = Some(message),
                UiEvent::LoginProgress { user_code, url } => {
                    self.user_code = Some(user_code.clone());
                    self.login_url = Some(url);
                    self.message = Some(format!("已打开浏览器，登录后回到 App。配对码 {user_code}"));
                }
                UiEvent::LoggedIn { token, email } => {
                    self.ui.token = token;
                    self.ui.email = email.clone();
                    self.ui.save();
                    self.user_code = None;
                    self.login_url = None;
                    self.message = Some(format!("已登录 {email}"));
                    self.refresh_points();
                }
                UiEvent::Points(points) => self.ui.points = points,
                UiEvent::Status(status) => self.status = status,
                UiEvent::Busy(busy) => self.busy = busy,
            }
        }
    }
}

fn enroll_then_start(
    config: &PathBuf,
    token: &str,
    name: &str,
    region: &str,
) -> Result<String, String> {
    let minted = CloudClient::mint_bootstrap_token(token)?;
    let output = WorkerProcess::run(
        config,
        "enroll",
        &["--join-token-stdin"],
        Some(&format!("{}\n", minted.token_value)),
    )?;
    let peer_id = WorkerProcess::parse_enroll(&output)
        .ok_or_else(|| "入网成功但没有返回 peer_id".to_string())?;
    CloudClient::claim_worker(token, &peer_id, name, region, Some(&minted.id))?;
    start_worker(config)?;
    Ok("入网完成，已开始贡献".into())
}

fn start_worker(config: &PathBuf) -> Result<String, String> {
    WorkerProcess::run(config, "start", &[], None)?;
    Ok("已开始贡献".into())
}

impl eframe::App for BeenetApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        if self.last_status.elapsed() >= Duration::from_secs(1) {
            self.last_status = Instant::now();
            self.refresh_status();
        }
        ctx.request_repaint_after(Duration::from_millis(400));

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.vertical_centered(|ui| {
                let title = if self.status.running && self.status.heartbeat {
                    "贡献中"
                } else if self.status.running {
                    "未在线"
                } else {
                    "已暂停"
                };
                ui.heading(title);
                ui.add_space(6.0);
                ui.label(status_line(self));
            });
            ui.add_space(18.0);
            ui.label("算力");
            ui.add_space(6.0);
            for preset in WorkerSnapshot::PRESETS {
                let selected = self.snapshot.cpu_percent == preset.cpu_percent
                    && self.snapshot.memory_mb == preset.memory_mb;
                let label = format!(
                    "{}  ·  {}% CPU / {} MB / {} pids",
                    preset.label, preset.cpu_percent, preset.memory_mb, preset.pids_max
                );
                if ui.selectable_label(selected, label).clicked() && !self.busy {
                    self.snapshot.cpu_percent = preset.cpu_percent;
                    self.snapshot.memory_mb = preset.memory_mb;
                    self.snapshot.pids_max = preset.pids_max;
                    self.save_config();
                    self.message = Some(format!(
                        "已改为「{}」。停止后再开始贡献才会换配额。",
                        preset.label
                    ));
                }
            }

            if let Some(message) = &self.message {
                ui.add_space(12.0);
                ui.colored_label(egui::Color32::from_rgb(70, 90, 82), message);
            }
            if let Some(url) = &self.login_url {
                ui.add_space(6.0);
                if ui.link("打不开浏览器的话，点这里打开登录页").clicked() {
                    let _ = open::that(url);
                }
            }

            ui.add_space(18.0);
            let primary = if self.ui.token.is_empty() {
                if self.busy {
                    if self.user_code.is_some() {
                        "等待浏览器登录…"
                    } else {
                        "正在连接 Cloud…"
                    }
                } else {
                    "登录 Cloud 平台"
                }
            } else if self.busy {
                "正在切换…"
            } else if self.status.running {
                "停止贡献"
            } else {
                "开始贡献"
            };
            ui.add_enabled_ui(!self.busy, |ui| {
                if ui
                    .add_sized([ui.available_width(), 42.0], egui::Button::new(primary))
                    .clicked()
                {
                    if self.ui.token.is_empty() {
                        self.login();
                    } else if self.status.running {
                        self.stop_worker();
                    } else {
                        self.start_or_enroll();
                    }
                }
            });
            ui.add_space(12.0);
            ui.horizontal(|ui| {
                if ui.button("设置").clicked() {
                    self.show_settings = !self.show_settings;
                }
                if ui.button("日志").clicked() {
                    open_logs(&self.snapshot.wasm_cache_dir);
                }
            });

            if self.show_settings {
                ui.add_space(16.0);
                ui.separator();
                ui.add_space(10.0);
                ui.heading("设置");
                ui.add_space(8.0);
                if self.ui.token.is_empty() {
                    ui.label("登录后，这个节点会记到你的 Cloud 账号下。");
                } else {
                    ui.label(format!("账号  {}", self.ui.email));
                    if let Some(points) = self.ui.points {
                        ui.label(format!("积分  {points}"));
                    }
                    ui.horizontal(|ui| {
                        if ui.button("刷新积分").clicked() {
                            self.refresh_points();
                        }
                        if ui.button("退出登录").clicked() {
                            self.logout();
                        }
                    });
                }
                ui.add_space(10.0);
                ui.label("名称");
                ui.text_edit_singleline(&mut self.snapshot.name);
                ui.label("地区");
                ui.text_edit_singleline(&mut self.snapshot.region);
                ui.label("缓存目录（安装时选择，身份文件也在这里）");
                ui.add_enabled(
                    false,
                    egui::TextEdit::singleline(&mut self.snapshot.wasm_cache_dir),
                );
                ui.add_space(8.0);
                if ui.button("保存名称和地区").clicked() {
                    self.sync_name_region();
                }
                ui.small("名称和地区保存在本机，登录后会同步到 Cloud。换配额需要先停止再开始。");
            }
        });
    }
}

fn status_line(app: &BeenetApp) -> String {
    let mut parts = Vec::new();
    if !app.snapshot.name.trim().is_empty() {
        parts.push(app.snapshot.name.trim().to_string());
    }
    if !app.snapshot.region.trim().is_empty() {
        parts.push(app.snapshot.region.trim().to_string());
    }
    if app.ui.token.is_empty() {
        parts.push("未登录".into());
    } else if app.status.running && app.status.heartbeat {
        parts.push("已在线".into());
    } else if app.status.running {
        parts.push("进程在跑，尚未被网络确认".into());
    } else {
        parts.push("已停止".into());
    }
    parts.join(" · ")
}

fn open_logs(wasm_cache_dir: &str) {
    let cache = PathBuf::from(wasm_cache_dir);
    let log = cache
        .parent()
        .unwrap_or(&cache)
        .join("logs")
        .join("beenet-worker.log");
    if log.exists() {
        let _ = open::that(log);
    } else {
        let _ = open::that(cache);
    }
}

fn setup_cjk_fonts(ctx: &egui::Context) {
    let candidates = [
        r"C:\Windows\Fonts\msyh.ttc",
        r"C:\Windows\Fonts\msyh.ttf",
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ];
    for path in candidates {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mut fonts = egui::FontDefinitions::default();
        fonts.font_data.insert(
            "cjk".into(),
            std::sync::Arc::new(egui::FontData::from_owned(bytes)),
        );
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Proportional) {
            family.insert(0, "cjk".into());
        }
        if let Some(family) = fonts.families.get_mut(&egui::FontFamily::Monospace) {
            family.push("cjk".into());
        }
        ctx.set_fonts(fonts);
        return;
    }
}
