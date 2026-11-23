//! runs what the window asks for on a thread of its own, telling it how it goes thru messages
//! the same steps the cli takes, just w/ the output going to the window instead of the terminal

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

use recomp3ds::build::{self, Event};
use recomp3ds::compile::{self, Compiler};
use recomp3ds::rom::Title;
use recomp3ds::{abi, discover, port, Mods};

use crate::report::Report;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Good,
    Warn,
    Error,
}

/// what the worker tells the window
pub enum Msg {
    Log(Level, String),
    /// what it's doing now, shown over the progress bar
    Stage(String),
    Progress {
        done: usize,
        total: usize,
    },
    Title(Result<TitleInfo, String>),
    Compiler(Result<String, String>),
    Analysis(Box<Report>),
    Hints(String),
    /// the job ended, w/ a line saying how
    Done {
        ok: bool,
        summary: String,
    },
}

/// what the window shows about the picked game
#[derive(Clone)]
pub struct TitleInfo {
    pub path: PathBuf,
    pub name: String,
    pub program_id: u64,
    pub modules: usize,
    pub installed: Option<PathBuf>,
}

/// the things the window can run
pub enum Job {
    Analyze {
        rom: PathBuf,
    },
    Build {
        rom: PathBuf,
        dir: Option<PathBuf>,
        overrides: Option<PathBuf>,
        compiler: Option<Compiler>,
        background: bool,
    },
    Port {
        rom: PathBuf,
        dir: PathBuf,
        overrides: Option<PathBuf>,
        name: Option<String>,
        zakuro: Option<PathBuf>,
        compiler: Option<Compiler>,
    },
    Hints {
        rom: PathBuf,
        file: Option<PathBuf>,
    },
}

/// sends to the window and wakes it up so it shows it right away
#[derive(Clone)]
pub struct Out {
    tx: Sender<Msg>,
    ctx: eframe::egui::Context,
}

impl Out {
    pub fn new(tx: Sender<Msg>, ctx: eframe::egui::Context) -> Out {
        Out { tx, ctx }
    }

    pub fn send(&self, msg: Msg) {
        // the window went away, nobody to tell
        let _ = self.tx.send(msg);
        self.ctx.request_repaint();
    }

    fn log(&self, level: Level, text: impl Into<String>) {
        self.send(Msg::Log(level, text.into()));
    }

    fn stage(&self, text: impl Into<String>) {
        self.send(Msg::Stage(text.into()));
    }
}

/// reads the game's header on a thread, for the card at the top
pub fn inspect(out: Out, rom: PathBuf) {
    std::thread::spawn(move || {
        let result = catch_unwind(AssertUnwindSafe(|| {
            let title = Title::load(&rom).map_err(|error| format!("could not load {}, {error}", rom.display()))?;
            let modules = recomp3ds::module_files(&title, Mods::default()).len();
            Ok(TitleInfo {
                name: title.exheader.title.clone(),
                program_id: title.program_id(),
                modules,
                installed: abi::installed(title.program_id()),
                path: rom.clone(),
            })
        }))
        .unwrap_or_else(|_| Err("reading the game crashed".to_owned()));
        out.send(Msg::Title(result));
    });
}

/// finds the C compiler (or checks the given one) on a thread, running it takes a moment
pub fn detect(out: Out, given: Option<Compiler>) {
    std::thread::spawn(move || {
        out.send(Msg::Compiler(compile::check(given.as_ref()).map(|compiler| compiler.describe())));
    });
}

/// what the settings' compiler field means, a program that's there whole or else a program plus args split by spaces, like CC
pub fn compiler_from(text: &str) -> Option<Compiler> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if Path::new(text).exists() {
        return Some(Compiler::new(text, &[]));
    }
    let mut words = text.split_whitespace();
    let program = words.next()?;
    let leading: Vec<&str> = words.collect();
    Some(Compiler::new(program, &leading))
}

/// runs job on a thread, cancel stops builds and ports at the next file
pub fn run(out: Out, job: Job, cancel: Arc<AtomicBool>) {
    std::thread::spawn(move || {
        let start = Instant::now();
        let result = catch_unwind(AssertUnwindSafe(|| match job {
            Job::Analyze { rom } => analyze(&out, &rom),
            Job::Build { rom, dir, overrides, compiler, background } => {
                build(&out, &rom, dir.as_deref(), overrides.as_deref(), compiler, background, &cancel)
            }
            Job::Port { rom, dir, overrides, name, zakuro, compiler } => {
                port(&out, &rom, &dir, overrides.as_deref(), name, zakuro.as_deref(), compiler, &cancel)
            }
            Job::Hints { rom, file } => hints(&out, &rom, file.as_deref()),
        }))
        .unwrap_or_else(|panic| {
            let why = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "no message".to_owned());
            Err(format!("it crashed, {why}"))
        });
        let took = format!("{:.1?}", start.elapsed());
        match result {
            Ok(summary) => {
                out.log(Level::Good, format!("{summary} ({took})"));
                out.send(Msg::Done { ok: true, summary });
            }
            Err(error) => {
                let stopped = error == "stopped";
                let summary = if stopped { "Stopped".to_owned() } else { error };
                out.log(if stopped { Level::Warn } else { Level::Error }, summary.clone());
                out.send(Msg::Done { ok: false, summary });
            }
        }
    });
}

/// a build's events, as log lines and progress
fn follow(out: &Out) -> impl Fn(Event) + Sync + '_ {
    move |event| match event {
        Event::Generated { files, bytes, overrides } => {
            out.log(Level::Info, format!("wrote {files} files, {} MiB of C, {overrides} overrides", bytes >> 20));
            out.stage("Compiling the C");
        }
        Event::Note(note) => out.log(Level::Info, note),
        Event::Compiled { done, total } => {
            out.send(Msg::Progress { done, total });
            if done == total || done * 10 / total != (done - 1) * 10 / total {
                out.log(Level::Info, format!("compiled {done} of {total}"));
            }
            if done == total {
                out.stage("Linking");
            }
        }
        Event::Built { library, took } => out.log(Level::Info, format!("built {} in {took:.1?}", library.display())),
        Event::Installed(path) => {
            out.log(Level::Good, format!("installed {}, Zakuro runs it for the game from now on", path.display()))
        }
    }
}

fn build(
    out: &Out,
    rom: &Path,
    dir: Option<&Path>,
    overrides: Option<&Path>,
    compiler: Option<Compiler>,
    background: bool,
    cancel: &AtomicBool,
) -> Result<String, String> {
    out.stage("Finding the code and writing it as C");
    out.log(Level::Info, format!("building {}", rom.display()));
    let options = build::Options { dir, overrides, cancel: Some(cancel), compiler, background, ..build::Options::default() };
    let library = build::build(rom, &options, &follow(out))?;
    Ok(if dir.is_some() { format!("Built {}", library.display()) } else { "Built and installed for Zakuro".to_owned() })
}

#[allow(clippy::too_many_arguments)]
fn port(
    out: &Out,
    rom: &Path,
    dir: &Path,
    overrides: Option<&Path>,
    name: Option<String>,
    zakuro: Option<&Path>,
    compiler: Option<Compiler>,
    cancel: &AtomicBool,
) -> Result<String, String> {
    let compiler = compile::check(compiler.as_ref())?;
    out.stage("Finding the code and writing it as C");
    out.log(Level::Info, format!("porting {} into {}", rom.display(), dir.display()));
    let events = follow(out);
    let generated = build::generate(rom, |_| dir.join("code"), overrides, Mods::default(), &events)?;
    if cancel.load(Ordering::Relaxed) {
        return Err("stopped".to_owned());
    }
    let (title, code) = (&generated.title, &generated.dir);
    let progress = |done, total| {
        events(Event::Compiled { done, total });
        !cancel.load(Ordering::Relaxed)
    };
    let objects = compile::objects(&compiler, code, &generated.sources, &progress, false)?;
    out.stage("Archiving");
    compile::archive(&objects, &code.join("librecomp.a"))?;
    out.log(Level::Info, format!("built {}", code.join("librecomp.a").display()));

    let name = name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| port::package_name(&title.exheader.title));
    port::write_project(dir, &name, &title.exheader.title, title.program_id(), zakuro)?;
    out.log(Level::Info, format!("wrote the project for {name}, build it with cargo build --release in {}", dir.display()));
    out.log(Level::Info, format!("then run target/release/{name} with the path to the game"));
    Ok(format!("Wrote the {name} project"))
}

fn analyze(out: &Out, rom: &Path) -> Result<String, String> {
    out.stage("Analyzing the code");
    let title = Title::load(rom).map_err(|error| format!("could not load {}, {error}", rom.display()))?;
    let programs = recomp3ds::programs(&title, Mods::default()).map_err(|error| format!("could not read the code, {error}"))?;
    let report = Report::new(&title, programs);
    let summary = format!("Found {} functions", report.all().functions);
    out.send(Msg::Analysis(Box::new(report)));
    Ok(summary)
}

/// the game's section for hints.txt, like the cli's hints
fn hints(out: &Out, rom: &Path, file: Option<&Path>) -> Result<String, String> {
    out.stage("Looking for new functions in the hints");
    let title = Title::load(rom).map_err(|error| format!("could not load {}, {error}", rom.display()))?;
    let programs = recomp3ds::programs(&title, Mods::default()).map_err(|error| format!("could not read the code, {error}"))?;
    let (_, program) = programs.iter().find(|(name, _)| name == "executable").ok_or("the game has no executable")?;
    let mut hinted = match file {
        Some(file) => {
            let text = std::fs::read_to_string(file).map_err(|error| format!("could not read {}, {error}", file.display()))?;
            build::read_hints(&text)
        }
        None => build::hints(title.program_id()),
    };
    let hash = build::code_hash(program);
    hinted.extend(build::bundled(title.program_id(), hash));
    let text = program.text.base..program.text.end();
    hinted.retain(|&address| text.contains(&(address & !1)));
    hinted.sort_unstable();
    hinted.dedup();
    let new = build::new_functions(&discover::analyze(program), &hinted);
    let mut section = format!("= {:016X} {hash:016X}\n", title.program_id());
    for address in &new {
        section.push_str(&format!("{address:08X}\n"));
    }
    out.send(Msg::Hints(section));
    Ok(format!("{} hinted spots start functions the analysis misses", new.len()))
}
