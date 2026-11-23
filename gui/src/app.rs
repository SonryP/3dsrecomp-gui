//! the window, a game card at the top, a page per thing to do, and the log at the bottom
//! simple mode is one page that makes the game an exe, advanced has every page

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Align, Color32, Layout, RichText, Sense, Stroke, TextureHandle, Vec2};
use serde::{Deserialize, Serialize};

use crate::report::{Report, Totals};
use crate::theme::{self, ACCENT, BAD, GOOD, WARN, WEAK};
use crate::toolchain::{Recipe, Tools, RECIPES};
use crate::worker::{self, Job, Level, Msg, Out, Port, TitleInfo};

#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
enum Page {
    /// simple mode's only page
    Exe,
    /// the setup window, which installs tools
    Setup,
    #[default]
    Build,
    Analyze,
    Port,
    Hints,
    Settings,
}

impl Page {
    const ALL: [Page; 5] = [Page::Build, Page::Analyze, Page::Port, Page::Hints, Page::Settings];

    fn label(self) -> &'static str {
        match self {
            Page::Exe => "🎮  Make an exe",
            Page::Setup => "🔧  Setup",
            Page::Build => "🔨  Build",
            Page::Analyze => "🔍  Analyze",
            Page::Port => "📦  Port",
            Page::Hints => "💡  Hints",
            Page::Settings => "⚙  Settings",
        }
    }

    fn blurb(self) -> &'static str {
        match self {
            Page::Setup => "Installs the tools making an exe takes.",
            Page::Exe => {
                "Turns the game into a Windows program of its own, its code recompiled and Zakuro running everything else."
            }
            Page::Build => "Recompile the game into a library Zakuro loads on its own, or into a folder you pick.",
            Page::Analyze => "See how much of the game's code the recompiler finds, without building anything.",
            Page::Port => "Make the game a cargo project with the recompiled code linked into Zakuro, and build it if you like.",
            Page::Hints => "Print the game's section for hints.txt, from where Zakuro had to interpret its last library.",
            Page::Settings => "The tools it uses and where things go.",
        }
    }
}

/// what's kept between runs
#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Settings {
    /// every page instead of just making an exe
    advanced: bool,
    /// the setup window came up once already, on the first launch that lacked something
    setup_seen: bool,
    page: Page,
    /// where simple mode puts the exe, next to the game when empty
    exe_dir: String,
    /// port only writes the project, w/o building it
    port_project_only: bool,
    rom: String,
    build_to_folder: bool,
    build_dir: String,
    overrides: String,
    port_dir: String,
    port_name: String,
    zakuro: String,
    hints_file: String,
    /// empty finds one on its own
    compiler: String,
    background: bool,
}

/// a job in progress
struct Running {
    what: Page,
    cancel: Arc<AtomicBool>,
    started: Instant,
    stage: String,
    progress: Option<(usize, usize)>,
    /// which of a port's steps it's on
    step: usize,
}

struct Line {
    level: Level,
    text: String,
}

enum Picked {
    File,
    Folder,
}

pub struct App {
    settings: Settings,
    out: Out,
    rx: Receiver<Msg>,
    logo: Option<TextureHandle>,
    title: Option<Result<TitleInfo, String>>,
    reading: bool,
    /// the compiler, ar and cargo found, None while looking
    tools: Option<Tools>,
    running: Option<Running>,
    /// the exe a port made last
    exe: Option<PathBuf>,
    /// the setup window is open
    show_setup: bool,
    /// what the setup is installing now
    installing: Vec<&'static Recipe>,
    /// how the last job went, shown on its page
    outcome: Option<(Page, bool, String)>,
    log: Vec<Line>,
    report: Option<Box<Report>>,
    hints: Option<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext) -> App {
        theme::apply(&cc.egui_ctx);
        let settings: Settings = cc.storage.and_then(|storage| eframe::get_value(storage, eframe::APP_KEY)).unwrap_or_default();
        let (tx, rx) = channel();
        let out = Out::new(tx, cc.egui_ctx.clone());
        let logo = eframe::icon_data::from_png_bytes(crate::ICON).ok().map(|icon| {
            let image = egui::ColorImage::from_rgba_unmultiplied([icon.width as usize, icon.height as usize], &icon.rgba);
            cc.egui_ctx.load_texture("logo", image, egui::TextureOptions::LINEAR)
        });
        let mut app = App {
            settings,
            out,
            rx,
            logo,
            title: None,
            reading: false,
            tools: None,
            running: None,
            exe: None,
            show_setup: false,
            installing: Vec::new(),
            outcome: None,
            log: Vec::new(),
            report: None,
            hints: None,
        };
        worker::detect(app.out.clone(), worker::compiler_from(&app.settings.compiler));
        // a game given on the command line (or dropped on the exe) comes before the last one
        if let Some(rom) = std::env::args_os().nth(1).map(PathBuf::from).filter(|path| path.is_file()) {
            app.open(rom);
        } else if !app.settings.rom.is_empty() && Path::new(&app.settings.rom).is_file() {
            let rom = PathBuf::from(&app.settings.rom);
            app.open(rom);
        } else {
            app.settings.rom.clear();
        }
        app
    }

    /// picks the game at path and reads its header
    fn open(&mut self, path: PathBuf) {
        self.settings.rom = path.display().to_string();
        self.title = None;
        self.reading = true;
        self.report = None;
        self.hints = None;
        worker::inspect(self.out.clone(), path);
    }

    fn take_messages(&mut self) {
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                Msg::Log(level, text) => self.log.push(Line { level, text }),
                Msg::Stage(stage) => {
                    if let Some(running) = &mut self.running {
                        running.stage = stage;
                    }
                }
                Msg::Progress { done, total } => {
                    if let Some(running) = &mut self.running {
                        running.progress = Some((done, total));
                    }
                }
                Msg::Title(title) => {
                    // a header read for a game no longer picked
                    let current = title.as_ref().map_or(true, |info| info.path.display().to_string() == self.settings.rom);
                    if current {
                        self.reading = false;
                        self.title = Some(title);
                    }
                }
                Msg::Tools(tools) => {
                    // the first launch that lacks something helps set it up, once
                    if !self.settings.setup_seen && !tools.exe_ready() {
                        self.show_setup = true;
                    }
                    self.settings.setup_seen = true;
                    self.tools = Some(*tools);
                }
                Msg::Step(step) => {
                    if let Some(running) = &mut self.running {
                        running.step = step;
                        running.progress = None;
                    }
                }
                Msg::Exe(path) => self.exe = Some(path),
                Msg::Analysis(report) => self.report = Some(report),
                Msg::Hints(text) => self.hints = Some(text),
                Msg::Done { ok, summary } => {
                    let what = self.running.take().map(|running| running.what);
                    self.outcome = what.map(|what| (what, ok, summary));
                    // whatever got installed (or half of it) shows up once it's looked for again
                    if what == Some(Page::Setup) {
                        self.installing.clear();
                        self.redetect();
                    }
                    // a build installs a lib, so the card's state changes
                    if ok && what == Some(Page::Build) && !self.settings.rom.is_empty() {
                        worker::inspect(self.out.clone(), PathBuf::from(&self.settings.rom));
                    }
                }
            }
        }
    }

    fn start(&mut self, job: Job, what: Page) {
        let cancel = Arc::new(AtomicBool::new(false));
        self.outcome = None;
        self.exe = None;
        // a blank line between jobs
        if !self.log.is_empty() {
            self.log.push(Line { level: Level::Info, text: String::new() });
        }
        self.running = Some(Running {
            what,
            cancel: cancel.clone(),
            started: Instant::now(),
            stage: "Starting".into(),
            progress: None,
            step: 0,
        });
        worker::run(self.out.clone(), job, cancel);
    }

    fn rom(&self) -> Option<PathBuf> {
        matches!(self.title, Some(Ok(_))).then(|| PathBuf::from(&self.settings.rom))
    }

    fn library_compiler(&self) -> Option<recomp3ds::compile::Compiler> {
        self.tools.as_ref().and_then(|tools| tools.library.clone().ok())
    }

    /// looks for the tools again, after something got installed or the settings changed
    fn redetect(&mut self) {
        self.tools = None;
        worker::detect(self.out.clone(), worker::compiler_from(&self.settings.compiler));
    }

    /// the port job the settings describe, for the advanced page or (w/ simple) simple mode's exe
    fn port_job(&self, simple: bool) -> Option<Job> {
        let tools = self.tools.as_ref()?;
        let (compiler, ar) = tools.exe.clone().ok()?;
        let rom = self.rom()?;
        let Some(Ok(info)) = &self.title else { return None };
        if simple {
            // the project lives in the cache, and every game shares one target folder so Zakuro gets compiled once
            let work = recomp3ds::build::cache_dir(info.program_id);
            let target = work.parent().map(|root| root.join("programs"))?;
            let copy_to = optional(&self.settings.exe_dir).or_else(|| rom.parent().map(Path::to_owned))?;
            return Some(Job::Port(Port {
                rom,
                dir: work.join("program"),
                overrides: None,
                name: None,
                zakuro: None,
                compiler,
                ar,
                cargo: Some(tools.cargo.clone().ok()?),
                target: Some(target),
                copy_to: Some(copy_to),
            }));
        }
        let cargo = if self.settings.port_project_only { None } else { Some(tools.cargo.clone().ok()?) };
        Some(Job::Port(Port {
            rom,
            dir: PathBuf::from(self.settings.port_dir.trim()),
            overrides: optional(&self.settings.overrides),
            name: Some(self.settings.port_name.trim().to_owned()).filter(|name| !name.is_empty()),
            zakuro: optional(&self.settings.zakuro),
            compiler,
            ar,
            cargo,
            target: None,
            copy_to: None,
        }))
    }
}

/// None for an empty field
fn optional(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    (!text.is_empty()).then(|| PathBuf::from(text))
}

/// opens a folder (or the folder holding a file, w/ it selected) in the system's file manager
fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        let mut command = std::process::Command::new("explorer");
        if path.is_file() {
            command.arg(format!("/select,{}", path.display()));
        } else {
            command.arg(path);
        }
        let _ = command.spawn();
    }
    #[cfg(not(windows))]
    {
        let dir = if path.is_file() { path.parent().unwrap_or(path) } else { path };
        let opener = if cfg!(target_os = "macos") { "open" } else { "xdg-open" };
        let _ = std::process::Command::new(opener).arg(dir).spawn();
    }
}

fn section(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).strong().size(15.0));
}

fn hint(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).color(WEAK).size(12.5));
}

/// a text field for a path w/ a button to browse for one, and one to clear it
fn path_field(ui: &mut egui::Ui, value: &mut String, placeholder: &str, kinds: &[Picked], filter: Option<(&str, &[&str])>) {
    ui.horizontal(|ui| {
        let gap = ui.spacing().item_spacing.x;
        let buttons = kinds.len() as f32 * (84.0 + gap) + if value.is_empty() { 0.0 } else { 28.0 + gap };
        // the field's own margins go on top of its width
        let width = (ui.available_width() - buttons - 2.0 * ui.spacing().button_padding.x).max(120.0);
        ui.add(egui::TextEdit::singleline(value).hint_text(placeholder).desired_width(width));
        for kind in kinds {
            let label = match kind {
                Picked::File => "📄 File…",
                Picked::Folder => "📁 Folder…",
            };
            if ui.add_sized([84.0, 28.0], egui::Button::new(label)).clicked() {
                let mut dialog = rfd::FileDialog::new();
                if let Some(start) =
                    optional(value).and_then(|path| if path.is_dir() { Some(path) } else { path.parent().map(Path::to_owned) })
                {
                    dialog = dialog.set_directory(start);
                }
                let picked = match kind {
                    Picked::File => {
                        if let Some((name, extensions)) = filter {
                            dialog = dialog.add_filter(name, extensions);
                        }
                        dialog.add_filter("All files", &["*"]).pick_file()
                    }
                    Picked::Folder => dialog.pick_folder(),
                };
                if let Some(path) = picked {
                    *value = path.display().to_string();
                }
            }
        }
        if !value.is_empty() && ui.add_sized([28.0, 28.0], egui::Button::new("✖")).on_hover_text("Clear").clicked() {
            value.clear();
        }
    });
}

/// the big button that starts a page's job
fn run_button(ui: &mut egui::Ui, text: &str, enabled: bool) -> bool {
    let button = egui::Button::new(RichText::new(text).size(15.5).strong().color(Color32::WHITE))
        .fill(if enabled { ACCENT } else { theme::ACCENT_DIM.gamma_multiply(0.5) })
        .min_size(Vec2::new(170.0, 38.0));
    ui.add_enabled(enabled, button).clicked()
}

/// a little dot and text, for statuses
fn status(ui: &mut egui::Ui, color: Color32, text: &str) -> egui::Response {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(10.0), Sense::hover());
        ui.painter().circle_filled(rect.center(), 4.5, color);
        ui.add(egui::Label::new(RichText::new(text).color(theme::TEXT)).wrap());
    })
    .response
}

impl eframe::App for App {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.settings);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.take_messages();
        let ctx = ui.ctx().clone();

        // games dropped on the window
        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .map(|file| file.path().to_owned())
                .filter(|path| !path.as_os_str().is_empty())
                .collect()
        });
        if let Some(path) = dropped.into_iter().next() {
            if self.running.is_none() {
                self.open(path);
            }
        }
        if self.running.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }

        // panels sit flush against each other, each puts the spacing back inside
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        self.header(ui);
        if self.settings.advanced {
            self.sidebar(ui);
        }
        self.log_panel(ui);
        egui::CentralPanel::default_margins()
            .frame(egui::Frame::new().fill(theme::BG).inner_margin(egui::Margin::same(18)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = theme::SPACING;
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    if self.settings.advanced {
                        self.game_card(ui);
                        ui.add_space(12.0);
                        self.page(ui);
                    } else {
                        // a column of its own, not stretched across a wide window
                        let width = ui.available_width().min(760.0);
                        let side = (ui.available_width() - width) / 2.0;
                        ui.horizontal(|ui| {
                            ui.add_space(side);
                            ui.vertical(|ui| {
                                ui.set_width(width);
                                self.game_card(ui);
                                ui.add_space(12.0);
                                self.exe_page(ui);
                            });
                        });
                    }
                });
            });

        self.setup_window(&ctx);
        self.drop_overlay(&ctx);
    }
}

impl App {
    fn header(&mut self, ui: &mut egui::Ui) {
        egui::Panel::top("header")
            .resizable(false)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(16, 10)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = theme::SPACING;
                ui.horizontal(|ui| {
                    if let Some(logo) = &self.logo {
                        ui.add(egui::Image::new(logo).fit_to_exact_size(Vec2::splat(34.0)));
                    }
                    ui.vertical(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;
                        ui.label(RichText::new("3dsrecomp").size(19.0).strong().color(Color32::WHITE));
                        ui.label(RichText::new("static recompiler for 3DS games").size(11.5).color(WEAK));
                    });
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let busy = self.running.is_some();
                        ui.add_enabled_ui(!busy, |ui| {
                            ui.spacing_mut().item_spacing.x = 0.0;
                            for (advanced, text) in [(true, "Advanced"), (false, "Simple")] {
                                let selected = self.settings.advanced == advanced;
                                let button =
                                    egui::Button::new(RichText::new(text).color(if selected { Color32::WHITE } else { WEAK }))
                                        .fill(if selected { theme::ACCENT_DIM } else { theme::CARD })
                                        .min_size(Vec2::new(84.0, 28.0));
                                if ui.add(button).on_disabled_hover_text("Wait for the job to finish").clicked() {
                                    self.settings.advanced = advanced;
                                }
                            }
                        });
                        ui.add_space(16.0);
                        let response = match (&self.tools, self.settings.advanced) {
                            (None, _) => status(ui, WEAK, "Looking for tools…"),
                            (Some(tools), false) if tools.exe_ready() => status(ui, GOOD, "Ready to make exes"),
                            (Some(_), false) => status(ui, BAD, "A tool is missing"),
                            (Some(tools), true) => match &tools.library {
                                Ok(compiler) => status(ui, GOOD, &format!("C compiler: {}", short(compiler))),
                                Err(_) => status(ui, BAD, "No usable C compiler, see Settings"),
                            },
                        };
                        if self.settings.advanced
                            && response.interact(Sense::click()).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
                        {
                            self.settings.page = Page::Settings;
                        }
                    });
                });
            });
    }

    fn sidebar(&mut self, ui: &mut egui::Ui) {
        egui::Panel::left("pages")
            .resizable(false)
            .exact_size(176.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(10, 14)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = egui::vec2(theme::SPACING.x, 4.0);
                for page in Page::ALL {
                    if page == Page::Settings {
                        ui.add_space(10.0);
                        ui.separator();
                        ui.add_space(6.0);
                    }
                    let selected = self.settings.page == page;
                    let busy = self.running.as_ref().is_some_and(|running| running.what == page);
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 36.0), Sense::click());
                    let fill = if selected {
                        theme::CARD_HOVER
                    } else if response.hovered() {
                        theme::CARD
                    } else {
                        Color32::TRANSPARENT
                    };
                    ui.painter().rect_filled(rect, 8.0, fill);
                    if selected {
                        let bar = egui::Rect::from_min_size(rect.min + Vec2::new(0.0, 8.0), Vec2::new(3.0, rect.height() - 16.0));
                        ui.painter().rect_filled(bar, 2.0, ACCENT);
                    }
                    let color = if selected { Color32::WHITE } else { theme::TEXT };
                    ui.painter().text(
                        rect.left_center() + Vec2::new(14.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        page.label(),
                        egui::FontId::proportional(14.5),
                        color,
                    );
                    if busy {
                        let spinner = egui::Rect::from_center_size(rect.right_center() - Vec2::new(16.0, 0.0), Vec2::splat(14.0));
                        egui::Spinner::new().size(14.0).color(ACCENT).paint_at(ui, spinner);
                    }
                    if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() {
                        self.settings.page = page;
                    }
                }
                ui.with_layout(Layout::bottom_up(Align::Min), |ui| {
                    ui.label(RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(11.0).color(WEAK));
                    ui.hyperlink_to(
                        RichText::new("Overrides guide").size(12.0),
                        "https://github.com/fearkov/3dsrecomp/blob/main/docs/overrides.md",
                    );
                });
            });
    }

    fn game_card(&mut self, ui: &mut egui::Ui) {
        let busy = self.running.is_some();
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            match &self.title {
                _ if self.reading => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().color(ACCENT));
                        ui.label(format!("Reading {}…", self.settings.rom));
                    });
                }
                None => {
                    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 96.0), Sense::click());
                    let hovered = response.hovered() && !busy;
                    let stroke = Stroke::new(1.5, if hovered { ACCENT } else { theme::BORDER });
                    ui.painter().rect(
                        rect,
                        10.0,
                        if hovered { theme::CARD_HOVER } else { theme::CARD },
                        stroke,
                        egui::StrokeKind::Inside,
                    );
                    ui.painter().text(
                        rect.center() - Vec2::new(0.0, 12.0),
                        egui::Align2::CENTER_CENTER,
                        "🎮  Drop a game here, or click to pick one",
                        egui::FontId::proportional(17.0),
                        Color32::WHITE,
                    );
                    ui.painter().text(
                        rect.center() + Vec2::new(0.0, 16.0),
                        egui::Align2::CENTER_CENTER,
                        "a decrypted .3ds, .cci, .cia or .cxi dump of a game you own",
                        egui::FontId::proportional(12.5),
                        WEAK,
                    );
                    if response.on_hover_cursor(egui::CursorIcon::PointingHand).clicked() && !busy {
                        self.pick_rom();
                    }
                }
                Some(Err(error)) => {
                    let error = error.clone();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("⚠").size(22.0).color(BAD));
                        ui.vertical(|ui| {
                            ui.label(RichText::new("That file can't be used").strong().color(Color32::WHITE));
                            ui.label(RichText::new(error).color(WEAK));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.add_enabled(!busy, egui::Button::new("Pick another…")).clicked() {
                                self.pick_rom();
                            }
                        });
                    });
                }
                Some(Ok(info)) => {
                    let info = info.clone();
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("🎮").size(30.0));
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 3.0;
                            ui.label(RichText::new(&info.name).size(20.0).strong().color(Color32::WHITE));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new(format!("{:016X}", info.program_id)).monospace().color(WEAK))
                                    .on_hover_text("Title ID");
                                ui.label(RichText::new("·").color(WEAK));
                                let modules = match info.modules {
                                    0 => "no modules".to_owned(),
                                    1 => "1 module".to_owned(),
                                    n => format!("{n} modules"),
                                };
                                ui.label(RichText::new(modules).color(WEAK));
                            });
                            ui.label(RichText::new(info.path.display().to_string()).size(11.5).color(WEAK));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                            if ui.add_enabled(!busy, egui::Button::new("Change…")).clicked() {
                                self.pick_rom();
                            }
                        });
                    });
                    // only advanced mode builds the library Zakuro loads
                    if !self.settings.advanced {
                        return;
                    }
                    ui.add_space(4.0);
                    ui.separator();
                    ui.horizontal(|ui| match &info.installed {
                        Some(path) if info.broken => {
                            status(ui, BAD, "The installed library was built for Cygwin and crashes Zakuro, build it again");
                            let path = path.clone();
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("Show file").clicked() {
                                    reveal(&path);
                                }
                            });
                        }
                        Some(path) => {
                            status(ui, GOOD, "A recompiled library is installed, Zakuro uses it for this game");
                            let path = path.clone();
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui.small_button("Show file").clicked() {
                                    reveal(&path);
                                }
                            });
                        }
                        None => {
                            status(ui, WARN, "Not built yet, Zakuro interprets this game for now");
                        }
                    });
                }
            }
        });
    }

    fn pick_rom(&mut self) {
        let mut dialog =
            rfd::FileDialog::new().add_filter("3DS games", &["3ds", "cci", "cia", "cxi", "app"]).add_filter("All files", &["*"]);
        if let Some(dir) = Path::new(&self.settings.rom).parent().filter(|dir| dir.is_dir()) {
            dialog = dialog.set_directory(dir);
        }
        if let Some(path) = dialog.pick_file() {
            self.open(path);
        }
    }

    fn page(&mut self, ui: &mut egui::Ui) {
        let page = self.settings.page;
        ui.label(RichText::new(page.label()).size(22.0).strong().color(Color32::WHITE));
        hint(ui, page.blurb());
        ui.add_space(6.0);
        match page {
            Page::Build => self.build_page(ui),
            Page::Analyze => self.analyze_page(ui),
            Page::Port => self.port_page(ui),
            Page::Hints => self.hints_page(ui),
            Page::Settings => self.settings_page(ui),
            Page::Exe | Page::Setup => self.exe_page(ui),
        }
    }

    /// the run button, or the progress and a stop button while this page's job goes
    fn controls(&mut self, ui: &mut egui::Ui, page: Page, text: &str, ready: Result<(), &str>, cancelable: bool) -> bool {
        let mut start = false;
        ui.add_space(4.0);
        match &self.running {
            Some(running) if running.what == page => {
                let elapsed = running.started.elapsed().as_secs();
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(ACCENT));
                    ui.label(RichText::new(&running.stage).strong());
                    ui.label(RichText::new(format!("{}:{:02}", elapsed / 60, elapsed % 60)).color(WEAK).monospace());
                    if cancelable {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let stopping = running.cancel.load(Ordering::Relaxed);
                            let label = if stopping { "Stopping…" } else { "■ Stop" };
                            if ui.add_enabled(!stopping, egui::Button::new(label).min_size(Vec2::new(90.0, 30.0))).clicked() {
                                running.cancel.store(true, Ordering::Relaxed);
                            }
                        });
                    }
                });
                if let Some((done, total)) = running.progress {
                    let fraction = done as f32 / total.max(1) as f32;
                    ui.add(egui::ProgressBar::new(fraction).fill(ACCENT).desired_height(10.0).corner_radius(5.0));
                    hint(
                        ui,
                        &format!("{done} of {total} files compiled, the biggest ones go first so it speeds up toward the end"),
                    );
                } else {
                    ui.add(egui::ProgressBar::new(0.0).animate(true).fill(ACCENT).desired_height(10.0).corner_radius(5.0));
                }
            }
            Some(running) => {
                ui.horizontal(|ui| {
                    run_button(ui, text, false);
                    hint(
                        ui,
                        &format!("Wait for {} to finish", running.what.label().trim_start_matches(|c: char| !c.is_alphabetic())),
                    );
                });
            }
            None => {
                ui.horizontal(|ui| {
                    start = run_button(ui, text, ready.is_ok());
                    if let Err(why) = ready {
                        hint(ui, why);
                    }
                });
                if let Some((_, ok, summary)) = self.outcome.as_ref().filter(|(what, _, _)| *what == page) {
                    let (icon, color) = if *ok { ("✔", GOOD) } else { ("✖", BAD) };
                    ui.label(RichText::new(format!("{icon}  {summary}")).color(color).strong());
                }
            }
        }
        start
    }

    fn needs(&self, compiler: bool) -> Result<(), &'static str> {
        if self.rom().is_none() {
            return Err("Pick a game first");
        }
        match &self.tools {
            None if compiler => Err("Looking for a C compiler…"),
            Some(tools) if compiler && tools.library.is_err() => Err("No usable C compiler, see Settings"),
            _ => Ok(()),
        }
    }

    /// what making an exe still needs, if anything
    fn needs_exe(&self) -> Result<(), &'static str> {
        if self.rom().is_none() {
            return Err("Pick a game first");
        }
        match &self.tools {
            None => Err("Looking for tools…"),
            Some(tools) if !tools.exe_ready() => Err("Install what's missing first"),
            _ => Ok(()),
        }
    }

    /// simple mode, the game in and an exe out
    fn exe_page(&mut self, ui: &mut egui::Ui) {
        ui.label(RichText::new(Page::Exe.label()).size(22.0).strong().color(Color32::WHITE));
        hint(ui, Page::Exe.blurb());
        ui.add_space(6.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section(ui, "Where the exe goes");
            path_field(ui, &mut self.settings.exe_dir, "the game's folder", &[Picked::Folder], None);
            hint(ui, "The exe still reads your game for everything but its code, so keep the game where it is. A Play .bat next to the exe starts it with this game.");
        });
        if let Some(tools) = self.tools.clone().filter(|tools| !tools.exe_ready()) {
            ui.add_space(10.0);
            self.missing_tools(ui, &tools);
        }
        ui.add_space(10.0);
        if self.controls(ui, Page::Exe, "▶  Make the exe", self.needs_exe(), true) {
            if let Some(job) = self.port_job(true) {
                self.start(job, Page::Exe);
            }
        }
        let running = self.running.as_ref().filter(|running| running.what == Page::Exe).map(|running| running.step);
        let finished = self.outcome.as_ref().filter(|(what, _, _)| *what == Page::Exe).map(|(_, ok, _)| *ok);
        if running.is_some() || finished.is_some() {
            ui.add_space(6.0);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                for (i, step) in worker::PORT_STEPS.iter().enumerate() {
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(18.0), Sense::hover());
                        let state = match (running, finished) {
                            (Some(at), _) => i.cmp(&at),
                            (None, Some(true)) => std::cmp::Ordering::Less,
                            _ => std::cmp::Ordering::Greater,
                        };
                        let color = match state {
                            std::cmp::Ordering::Less => {
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "✔",
                                    egui::FontId::proportional(14.0),
                                    GOOD,
                                );
                                theme::TEXT
                            }
                            std::cmp::Ordering::Equal => {
                                egui::Spinner::new().size(14.0).color(ACCENT).paint_at(ui, rect.shrink(2.0));
                                Color32::WHITE
                            }
                            std::cmp::Ordering::Greater => {
                                ui.painter().circle_stroke(rect.center(), 5.0, Stroke::new(1.5, theme::BORDER));
                                WEAK
                            }
                        };
                        ui.label(RichText::new(*step).color(color));
                    });
                }
                if running == Some(2) {
                    hint(ui, "The first exe takes a while: Zakuro gets downloaded and compiled once, later games reuse it.");
                }
            });
        }
        if let (Some(exe), Some(true)) = (self.exe.clone(), finished) {
            ui.add_space(6.0);
            theme::card().show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.label(RichText::new("Your exe is ready").size(17.0).strong().color(GOOD));
                ui.label(RichText::new(exe.display().to_string()).monospace().color(WEAK));
                ui.horizontal(|ui| {
                    if ui.add(egui::Button::new("▶  Play").fill(ACCENT).min_size(Vec2::new(100.0, 32.0))).clicked() {
                        let mut command = std::process::Command::new(&exe);
                        command.arg(&self.settings.rom);
                        if let Some(dir) = exe.parent() {
                            command.current_dir(dir);
                        }
                        if let Err(error) = command.spawn() {
                            self.log
                                .push(Line { level: Level::Error, text: format!("could not start {}, {error}", exe.display()) });
                        }
                    }
                    if ui.add(egui::Button::new("📁  Show in folder").min_size(Vec2::new(140.0, 32.0))).clicked() {
                        reveal(&exe);
                    }
                });
            });
        }
    }

    /// what's missing for making exes, w/ a way to the setup
    fn missing_tools(&mut self, ui: &mut egui::Ui, tools: &Tools) {
        theme::card().stroke(Stroke::new(1.0, WARN.gamma_multiply(0.6))).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new("⚠  Something this needs isn't installed").strong().color(WARN));
            let missing: Vec<&str> = RECIPES.iter().filter(|recipe| !tools.has(recipe)).map(|recipe| recipe.name).collect();
            if missing.is_empty() {
                // something found but unusable, like an LLVM w/o llvm-ar
                for error in [tools.exe.as_ref().err(), tools.cargo.as_ref().err(), tools.linker.as_ref().err()].into_iter().flatten() {
                    ui.label(error);
                }
            } else {
                ui.label(format!("Missing: {}", missing.join(", ")));
            }
            ui.horizontal(|ui| {
                if ui.add(egui::Button::new(RichText::new("🔧  Set up the tools").color(Color32::WHITE)).fill(ACCENT)).clicked() {
                    self.show_setup = true;
                }
                if ui.button("Check again").clicked() {
                    self.redetect();
                }
            });
        });
    }

    /// the window that installs what making an exe takes, w/ winget
    fn setup_window(&mut self, ctx: &egui::Context) {
        if !self.show_setup {
            return;
        }
        let installing = self.running.as_ref().filter(|running| running.what == Page::Setup);
        let busy = self.running.is_some();
        let now = installing.map(|running| (self.installing.get(running.step).map(|recipe| recipe.tool), running.stage.clone(), running.started.elapsed().as_secs()));
        let frame = theme::card().inner_margin(egui::Margin::same(22)).shadow(egui::Shadow { offset: [0, 8], blur: 28, spread: 0, color: Color32::from_black_alpha(140) });
        let modal = egui::Modal::new(egui::Id::unique("setup")).frame(frame).show(ctx, |ui| {
            ui.set_width(580.0);
            ui.spacing_mut().item_spacing = theme::SPACING;
            ui.label(RichText::new("🔧  Set up 3dsrecomp").size(21.0).strong().color(Color32::WHITE));
            hint(ui, "Making a game into an exe takes three free tools. Whatever's missing can be installed from here with winget, Windows' own package manager. Windows asks for permission for each one, and installing accepts its license.");
            ui.add_space(4.0);
            let Some(tools) = self.tools.clone() else {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(ACCENT));
                    ui.label("Looking for what's installed…");
                });
                return;
            };
            for recipe in &RECIPES {
                let has = tools.has(recipe);
                egui::Frame::new().fill(theme::BG).corner_radius(8.0).inner_margin(egui::Margin::symmetric(12, 10)).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        let (rect, _) = ui.allocate_exact_size(Vec2::splat(20.0), Sense::hover());
                        if now.as_ref().is_some_and(|(tool, _, _)| *tool == Some(recipe.tool)) {
                            egui::Spinner::new().size(16.0).color(ACCENT).paint_at(ui, rect.shrink(2.0));
                        } else if has {
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "✔", egui::FontId::proportional(16.0), GOOD);
                        } else {
                            ui.painter().circle_stroke(rect.center(), 6.0, Stroke::new(1.5, WARN));
                        }
                        // room on the right for the button
                        let text_width = (ui.available_width() - 100.0).max(160.0);
                        ui.vertical(|ui| {
                            ui.set_max_width(text_width);
                            ui.spacing_mut().item_spacing.y = 2.0;
                            ui.label(RichText::new(recipe.name).strong().color(Color32::WHITE));
                            let what = RichText::new(format!("{}, {}", recipe.what, recipe.size)).size(12.0).color(WEAK);
                            ui.add(egui::Label::new(what).wrap());
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if has {
                                ui.label(RichText::new("Installed").color(GOOD));
                            } else if tools.winget {
                                if ui.add_enabled(!busy, egui::Button::new("Install")).clicked() {
                                    self.install(vec![recipe]);
                                }
                            } else {
                                ui.hyperlink_to("Download", recipe.page);
                            }
                        });
                    });
                });
            }
            ui.add_space(6.0);
            let missing: Vec<&'static Recipe> = RECIPES.iter().filter(|recipe| !tools.has(recipe)).collect();
            if let Some((_, stage, elapsed)) = &now {
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().color(ACCENT));
                    ui.add(egui::Label::new(RichText::new(stage).strong()).truncate());
                });
                ui.horizontal(|ui| {
                    hint(ui, &format!("{}:{:02} · the log at the bottom shows everything", elapsed / 60, elapsed % 60));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let stopping = self.running.as_ref().is_some_and(|running| running.cancel.load(Ordering::Relaxed));
                        if ui.add_enabled(!stopping, egui::Button::new(if stopping { "Stopping…" } else { "■ Stop" })).clicked() {
                            if let Some(running) = &self.running {
                                running.cancel.store(true, Ordering::Relaxed);
                            }
                        }
                    });
                });
                if self.installing.get(self.running.as_ref().map_or(0, |running| running.step)).is_some_and(|recipe| recipe.tool == crate::toolchain::Tool::BuildTools) {
                    hint(ui, "The Visual Studio Installer shows its own window while it works, this one takes the longest.");
                }
                return;
            }
            if let Some((_, false, summary)) = self.outcome.as_ref().filter(|(what, _, _)| *what == Page::Setup) {
                ui.label(RichText::new(format!("✖  {summary}")).color(BAD));
            }
            if missing.is_empty() {
                ui.label(RichText::new("✔  Everything's ready, you can make exes now").strong().color(GOOD));
                if let Err(error) = &tools.exe {
                    ui.label(RichText::new(error).color(WARN));
                }
            } else if !tools.winget {
                hint(ui, "winget isn't available here, so download them from the links, install them, then check again.");
            }
            ui.horizontal(|ui| {
                if !missing.is_empty() && tools.winget {
                    let label = format!("Install what's missing ({})", missing.len());
                    let button = egui::Button::new(RichText::new(label).strong().color(Color32::WHITE)).fill(ACCENT).min_size(Vec2::new(0.0, 34.0));
                    if ui.add_enabled(!busy, button).clicked() {
                        self.install(missing.clone());
                    }
                }
                if ui.add_enabled(!busy, egui::Button::new("Check again").min_size(Vec2::new(0.0, 34.0))).clicked() {
                    self.redetect();
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let close = if missing.is_empty() { "Done" } else { "Skip for now" };
                    if ui.add(egui::Button::new(close).min_size(Vec2::new(0.0, 34.0))).clicked() {
                        self.show_setup = false;
                    }
                });
            });
        });
        // Esc or a click outside closes it, but not while it's installing
        if modal.should_close() && now.is_none() {
            self.show_setup = false;
        }
    }

    /// starts installing recipes w/ winget
    fn install(&mut self, recipes: Vec<&'static Recipe>) {
        self.installing = recipes.clone();
        self.start(Job::Install(recipes), Page::Setup);
    }

    fn build_page(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section(ui, "Where it goes");
            ui.radio_value(&mut self.settings.build_to_folder, false, "Install it for Zakuro (recommended)");
            hint(ui, "    Works in the cache and puts the library where Zakuro finds it on its own.");
            ui.radio_value(&mut self.settings.build_to_folder, true, "Keep everything in a folder");
            if self.settings.build_to_folder {
                path_field(ui, &mut self.settings.build_dir, "the folder for the C and the library", &[Picked::Folder], None);
            }
            ui.add_space(8.0);
            section(ui, "Overrides (optional)");
            hint(ui, "A C file, or a folder of them, with functions written by hand for fixes, mods or faster versions.");
            path_field(ui, &mut self.settings.overrides, "none", &[Picked::File, Picked::Folder], Some(("C files", &["c"])));
            ui.add_space(8.0);
            ui.checkbox(&mut self.settings.background, "Compile in the background")
                .on_hover_text("Below normal priority with a core left free, for when you're playing meanwhile");
        });
        ui.add_space(10.0);
        let mut ready = self.needs(true);
        if ready.is_ok() && self.settings.build_to_folder && self.settings.build_dir.trim().is_empty() {
            ready = Err("Pick the folder to build in");
        }
        if self.controls(ui, Page::Build, "▶  Build", ready, true) {
            let job = Job::Build {
                rom: self.rom().unwrap(),
                dir: if self.settings.build_to_folder { optional(&self.settings.build_dir) } else { None },
                overrides: optional(&self.settings.overrides),
                compiler: self.library_compiler(),
                background: self.settings.background,
            };
            self.start(job, Page::Build);
        }
        hint(ui, "The build takes a while for big games, the log below shows how it goes.");
    }

    fn port_page(&mut self, ui: &mut egui::Ui) {
        let default_name = match &self.title {
            Some(Ok(info)) => recomp3ds::port::package_name(&info.name),
            _ => "from the game's title".to_owned(),
        };
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section(ui, "Project folder");
            path_field(ui, &mut self.settings.port_dir, "where the cargo project goes", &[Picked::Folder], None);
            ui.add_space(6.0);
            section(ui, "Program name (optional)");
            ui.add(egui::TextEdit::singleline(&mut self.settings.port_name).hint_text(default_name).desired_width(f32::INFINITY));
            ui.add_space(6.0);
            section(ui, "Zakuro checkout (optional)");
            hint(ui, "A local Zakuro to build against, instead of the one on GitHub.");
            path_field(ui, &mut self.settings.zakuro, "the GitHub one", &[Picked::Folder], None);
            ui.add_space(6.0);
            section(ui, "Overrides (optional)");
            path_field(ui, &mut self.settings.overrides, "none", &[Picked::File, Picked::Folder], Some(("C files", &["c"])));
            ui.add_space(6.0);
            ui.checkbox(&mut self.settings.port_project_only, "Only write the project, don't build the exe")
                .on_hover_text("Leaves cargo build --release to you");
        });
        if let Some(tools) =
            self.tools.clone().filter(|tools| tools.exe.is_err() || (!self.settings.port_project_only && tools.cargo.is_err()))
        {
            ui.add_space(10.0);
            self.missing_tools(ui, &tools);
        }
        ui.add_space(10.0);
        let mut ready = self.needs(false);
        if ready.is_ok() && self.settings.port_dir.trim().is_empty() {
            ready = Err("Pick the project folder");
        }
        if ready.is_ok() && self.port_job(false).is_none() {
            ready = Err(if self.tools.is_none() { "Looking for tools…" } else { "Install what's missing first" });
        }
        let label = if self.settings.port_project_only { "▶  Make the project" } else { "▶  Make and build it" };
        if self.controls(ui, Page::Port, label, ready, true) {
            if let Some(job) = self.port_job(false) {
                self.start(job, Page::Port);
            }
        }
        let ported = self.outcome.as_ref().is_some_and(|(what, ok, _)| *what == Page::Port && *ok);
        if ported && !self.settings.port_dir.is_empty() && self.running.is_none() {
            let dir = PathBuf::from(&self.settings.port_dir);
            ui.horizontal(|ui| {
                match &self.exe {
                    Some(exe) => hint(ui, &format!("Built {}, run it with the path to the game.", exe.display())),
                    None => hint(ui, "Build it with cargo build --release in the project folder."),
                }
                if ui.small_button("Open folder").clicked() {
                    reveal(self.exe.as_deref().unwrap_or(&dir));
                }
            });
        }
    }

    fn analyze_page(&mut self, ui: &mut egui::Ui) {
        if self.controls(ui, Page::Analyze, "▶  Analyze", self.needs(false), false) {
            self.report = None;
            self.start(Job::Analyze { rom: self.rom().unwrap() }, Page::Analyze);
        }
        let Some(report) = &self.report else { return };
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let all = report.all();
            ui.horizontal(|ui| {
                big_number(ui, &all.functions.to_string(), "functions");
                big_number(ui, &format!("{:.1}%", all.share(all.code) * 100.0), "of the code reached");
                big_number(ui, &report.modules.to_string(), "modules");
                big_number(ui, &format!("{} KiB", all.text / 1024), "of code");
            });
            ui.add_space(6.0);
            coverage_bar(ui, &all);
            ui.horizontal(|ui| {
                legend(ui, theme::CODE, "code");
                legend(ui, theme::LITERAL, "literals");
                legend(ui, theme::UNREACHED, "unreached");
            });
            if report.hinted > 0 {
                hint(ui, &format!("{} new functions came from where Zakuro interpreted", report.hinted));
            }
        });
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            let all = report.all();
            section(ui, "By program");
            egui::Grid::new("totals").striped(true).num_columns(10).spacing([18.0, 6.0]).show(ui, |ui| {
                for heading in
                    ["", "KiB", "functions", "code", "literals", "unreached", "indirect", "switches", "svc", "dead ends"]
                {
                    ui.label(RichText::new(heading).color(WEAK).size(12.5));
                }
                ui.end_row();
                let row = |ui: &mut egui::Ui, name: &str, totals: &Totals| {
                    ui.label(RichText::new(name).strong());
                    let percent = |bytes: usize| format!("{:.1}%", totals.share(bytes) * 100.0);
                    for cell in [
                        (totals.text / 1024).to_string(),
                        totals.functions.to_string(),
                        percent(totals.code),
                        percent(totals.literal),
                        percent(totals.unreached()),
                        totals.indirect.to_string(),
                        totals.switches.to_string(),
                        totals.svc.to_string(),
                        totals.dead_ends.to_string(),
                    ] {
                        ui.label(RichText::new(cell).monospace());
                    }
                    ui.end_row();
                };
                row(ui, "executable", &report.executable);
                row(ui, "modules", &report.module_totals);
                row(ui, "all", &all);
            });
        });
        ui.add_space(10.0);
        ui.columns(2, |columns| {
            theme::card().show(&mut columns[0], |ui| {
                ui.set_width(ui.available_width());
                section(ui, "How functions were found");
                egui::Grid::new("found").num_columns(2).spacing([24.0, 4.0]).show(ui, |ui| {
                    for (how, count) in &report.found_by {
                        ui.label(*how);
                        ui.label(RichText::new(count.to_string()).monospace());
                        ui.end_row();
                    }
                    ui.label("thumb functions");
                    ui.label(RichText::new(report.thumb.to_string()).monospace());
                    ui.end_row();
                    ui.label("instructions");
                    ui.label(RichText::new(report.instructions.to_string()).monospace());
                    ui.end_row();
                });
                ui.add_space(4.0);
                let largest: Vec<String> = report.largest.iter().map(|(size, entry)| format!("{size} at {entry:08X}")).collect();
                hint(ui, &format!("largest: {}", largest.join(", ")));
                hint(ui, &format!("analysis took {:.2?}", report.took));
            });
            theme::card().show(&mut columns[1], |ui| {
                ui.set_width(ui.available_width());
                section(ui, "Largest unreached runs");
                egui::Grid::new("gaps").striped(true).num_columns(3).spacing([16.0, 4.0]).show(ui, |ui| {
                    for (name, start, length) in &report.gaps {
                        ui.label(name);
                        ui.label(RichText::new(format!("0x{start:08X}")).monospace());
                        ui.label(RichText::new(format!("{length} B")).monospace().color(WEAK));
                        ui.end_row();
                    }
                });
            });
        });
    }

    fn hints_page(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section(ui, "Hints file (optional)");
            hint(ui, "Leave empty to use the hints Zakuro wrote next to the game's library.");
            path_field(ui, &mut self.settings.hints_file, "Zakuro's", &[Picked::File], Some(("Hints", &["hints", "txt"])));
        });
        ui.add_space(10.0);
        if self.controls(ui, Page::Hints, "▶  Find hints", self.needs(false), false) {
            self.hints = None;
            self.start(Job::Hints { rom: self.rom().unwrap(), file: optional(&self.settings.hints_file) }, Page::Hints);
        }
        let Some(text) = &self.hints else { return };
        let text = text.clone();
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                section(ui, "Section for hints.txt");
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.button("💾 Save…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().set_file_name("hints.txt").save_file() {
                            if let Err(error) = std::fs::write(&path, &text) {
                                self.log.push(Line {
                                    level: Level::Error,
                                    text: format!("could not write {}, {error}", path.display()),
                                });
                            }
                        }
                    }
                    if ui.button("📋 Copy").clicked() {
                        ui.ctx().copy_text(text.clone());
                    }
                });
            });
            egui::ScrollArea::vertical().max_height(260.0).show(ui, |ui| {
                ui.add(egui::TextEdit::multiline(&mut text.as_str()).code_editor().desired_width(f32::INFINITY));
            });
        });
    }

    fn settings_page(&mut self, ui: &mut egui::Ui) {
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section(ui, "C compiler");
            hint(ui, "Leave empty to find one on its own, on PATH or where LLVM, MSYS2 and WinLibs install. Making an exe needs LLVM's clang with Rust's default (MSVC) toolchain. A command with arguments like \"zig cc\" works too.");
            let before = self.settings.compiler.clone();
            path_field(ui, &mut self.settings.compiler, "find one on its own", &[Picked::File], Some(("Programs", &["exe"])));
            if before != self.settings.compiler && worth_checking(&before, &self.settings.compiler) {
                self.redetect();
            }
            match self.tools.clone() {
                None => {
                    ui.horizontal(|ui| {
                        ui.add(egui::Spinner::new().size(14.0).color(ACCENT));
                        ui.label("Checking…");
                    });
                }
                Some(tools) => {
                    egui::Grid::new("tools").num_columns(2).spacing([16.0, 6.0]).show(ui, |ui| {
                        ui.label(RichText::new("For libraries").color(WEAK));
                        match &tools.library {
                            Ok(compiler) => status(ui, GOOD, &compiler.describe()),
                            Err(error) => status(ui, BAD, error),
                        };
                        ui.end_row();
                        ui.label(RichText::new("For exes").color(WEAK));
                        match &tools.exe {
                            Ok((compiler, ar)) => status(ui, GOOD, &format!("{} with {}", compiler.describe(), ar.display())),
                            Err(error) => status(ui, BAD, error),
                        };
                        ui.end_row();
                        ui.label(RichText::new("Cargo").color(WEAK));
                        match &tools.cargo {
                            Ok(cargo) => status(ui, GOOD, &cargo.display().to_string()),
                            Err(error) => status(ui, BAD, error),
                        };
                        ui.end_row();
                        if cfg!(windows) {
                            ui.label(RichText::new("Build Tools").color(WEAK));
                            match &tools.linker {
                                Ok(path) if path.as_os_str().is_empty() => status(ui, GOOD, "not needed"),
                                Ok(path) => status(ui, GOOD, &path.display().to_string()),
                                Err(error) => status(ui, BAD, error),
                            };
                            ui.end_row();
                        }
                    });
                    ui.horizontal(|ui| {
                        if ui.button("🔧  Open setup").clicked() {
                            self.show_setup = true;
                        }
                        if ui.button("Check again").clicked() {
                            self.redetect();
                        }
                    });
                }
            }
        });
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section(ui, "Folders");
            let library = recomp3ds::abi::library_dir();
            folder_row(ui, "Installed libraries", library.as_deref());
            let cache = match &self.title {
                Some(Ok(info)) => Some(recomp3ds::build::cache_dir(info.program_id)),
                _ => recomp3ds::build::cache_dir(0).parent().map(Path::to_owned),
            };
            folder_row(ui, "Build cache", cache.as_deref());
        });
        ui.add_space(10.0);
        theme::card().show(ui, |ui| {
            ui.set_width(ui.available_width());
            section(ui, "About");
            hint(ui, "3dsrecomp turns a game's code into C, which compiles into a library Zakuro loads instead of interpreting. You need your own decrypted dump of a game you own; what it makes comes from the game, so don't share it.");
            ui.hyperlink_to("Zakuro on GitHub", "https://github.com/fearkov/zakuro");
        });
    }

    fn log_panel(&mut self, ui: &mut egui::Ui) {
        egui::Panel::bottom("log")
            .resizable(true)
            .default_size(160.0)
            .min_size(90.0)
            .frame(egui::Frame::new().fill(theme::PANEL).inner_margin(egui::Margin::symmetric(14, 8)))
            .show(ui, |ui| {
                ui.spacing_mut().item_spacing = theme::SPACING;
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Log").strong());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Clear").clicked() {
                            self.log.clear();
                        }
                        if ui.small_button("Copy").clicked() {
                            let text: Vec<&str> = self.log.iter().map(|line| line.text.as_str()).collect();
                            ui.ctx().copy_text(text.join("\n"));
                        }
                    });
                });
                egui::Frame::new().fill(theme::BG).corner_radius(6.0).inner_margin(egui::Margin::same(8)).show(ui, |ui| {
                    egui::ScrollArea::vertical().auto_shrink([false, false]).stick_to_bottom(true).show(ui, |ui| {
                        ui.spacing_mut().item_spacing.y = 2.0;
                        if self.log.is_empty() {
                            ui.label(RichText::new("What a job does shows up here.").monospace().color(WEAK));
                        }
                        for line in &self.log {
                            let color = match line.level {
                                Level::Info => theme::TEXT,
                                Level::Good => GOOD,
                                Level::Warn => WARN,
                                Level::Error => BAD,
                            };
                            ui.add(egui::Label::new(RichText::new(&line.text).monospace().color(color)).wrap());
                        }
                    });
                });
            });
    }

    fn drop_overlay(&self, ctx: &egui::Context) {
        let hovering = ctx.input(|input| !input.raw.hovered_files.is_empty());
        if !hovering || self.running.is_some() {
            return;
        }
        let screen = ctx.content_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("drop")));
        painter.rect_filled(screen, 0.0, Color32::from_black_alpha(190));
        painter.rect_stroke(screen.shrink(16.0), 14.0, Stroke::new(2.5, ACCENT), egui::StrokeKind::Inside);
        painter.text(
            screen.center(),
            egui::Align2::CENTER_CENTER,
            "Drop the game to open it",
            egui::FontId::proportional(24.0),
            Color32::WHITE,
        );
    }
}

/// whether a new compiler field is worth checking, every keystroke would run one per letter otherwise
/// a compiler's program name w/o its folder, for the header
fn short(compiler: &recomp3ds::compile::Compiler) -> String {
    let name = compiler.program.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    std::iter::once(name).chain(compiler.leading.iter().cloned()).collect::<Vec<_>>().join(" ")
}

fn worth_checking(before: &str, after: &str) -> bool {
    after.is_empty() || Path::new(after.trim()).is_file() || (after.len() as isize - before.len() as isize).abs() > 1
}

fn folder_row(ui: &mut egui::Ui, name: &str, path: Option<&Path>) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(name).strong());
        match path {
            Some(path) => {
                ui.label(RichText::new(path.display().to_string()).monospace().color(WEAK));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui
                        .add_enabled(path.exists(), egui::Button::new("Open").small())
                        .on_disabled_hover_text("Nothing there yet")
                        .clicked()
                    {
                        reveal(path);
                    }
                });
            }
            None => {
                ui.label(RichText::new("nowhere on this system").color(WEAK));
            }
        }
    });
}

fn big_number(ui: &mut egui::Ui, value: &str, label: &str) {
    egui::Frame::new().fill(theme::BG).corner_radius(8.0).inner_margin(egui::Margin::symmetric(14, 10)).show(ui, |ui| {
        ui.set_min_width(120.0);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.label(RichText::new(value).size(22.0).strong().color(Color32::WHITE));
            ui.label(RichText::new(label).size(12.0).color(WEAK));
        });
    });
}

/// how the code splits into code, literals and unreached, as one bar
fn coverage_bar(ui: &mut egui::Ui, totals: &Totals) {
    let (rect, response) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 14.0), Sense::hover());
    let painter = ui.painter();
    painter.rect_filled(rect, 7.0, theme::UNREACHED);
    let code = rect.width() * totals.share(totals.code);
    let literal = rect.width() * totals.share(totals.literal);
    let mut clip = painter.clone();
    clip.set_clip_rect(rect);
    // drawn as one rounded piece then the literal part over it, so only the outer ends are rounded
    clip.rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(code + literal, rect.height())), 7.0, theme::LITERAL);
    clip.rect_filled(egui::Rect::from_min_size(rect.min, Vec2::new(code, rect.height())), 7.0, theme::CODE);
    response.on_hover_text(format!(
        "code {:.1}%, literals {:.1}%, unreached {:.1}%",
        totals.share(totals.code) * 100.0,
        totals.share(totals.literal) * 100.0,
        totals.share(totals.unreached()) * 100.0
    ));
}

fn legend(ui: &mut egui::Ui, color: Color32, text: &str) {
    ui.spacing_mut().item_spacing.x = 4.0;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(10.0, 10.0), Sense::hover());
    ui.painter().rect_filled(rect, 2.0, color);
    ui.label(RichText::new(text).size(12.0).color(WEAK));
    ui.add_space(10.0);
}
