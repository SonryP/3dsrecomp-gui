//! runs what the window asks for on a thread of its own, telling it how it goes thru messages
//! the same steps the cli takes, just w/ the output going to the window instead of the terminal

use std::io::{BufRead, BufReader, Read};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

use recomp3ds::build::{self, Event};
use recomp3ds::compile::{self, Compiler};
use recomp3ds::rom::Title;
use recomp3ds::{abi, discover, port, Mods};

use crate::report::Report;
use crate::toolchain::{self, Recipe, Tools};

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
    /// which of a port's steps it's on, for simple mode's list
    Step(usize),
    Title(Result<TitleInfo, String>),
    Tools(Box<Tools>),
    /// a port made this exe
    Exe(PathBuf),
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
    /// the installed lib needs cygwin's runtime, made by a gcc building for cygwin, and crashes Zakuro
    pub broken: bool,
}

/// does the lib at path need cygwin's or msys's runtime, its import table names the dll in plain ascii
fn needs_cygwin(path: &Path) -> bool {
    let Ok(bytes) = std::fs::read(path) else { return false };
    let lower: Vec<u8> = bytes.iter().map(u8::to_ascii_lowercase).collect();
    [&b"msys-2.0.dll"[..], b"cygwin1.dll"].iter().any(|name| lower.windows(name.len()).any(|window| window == *name))
}

/// the things the window can run
pub enum Job {
    Analyze { rom: PathBuf },
    Build { rom: PathBuf, dir: Option<PathBuf>, overrides: Option<PathBuf>, compiler: Option<Compiler>, background: bool },
    Port(Port),
    /// installs tools w/ winget, one after another
    Install(Vec<&'static Recipe>),
    Hints { rom: PathBuf, file: Option<PathBuf> },
}

/// a game made into a program, the project written and maybe built and its exe copied out
pub struct Port {
    pub rom: PathBuf,
    /// where the cargo project goes
    pub dir: PathBuf,
    pub overrides: Option<PathBuf>,
    pub name: Option<String>,
    pub zakuro: Option<PathBuf>,
    /// a compiler and ar matching rust's toolchain
    pub compiler: Compiler,
    pub ar: PathBuf,
    /// cargo, to build the exe too
    pub cargo: Option<PathBuf>,
    /// a target folder shared between games, so Zakuro gets compiled once
    pub target: Option<PathBuf>,
    /// where the exe goes, w/ a launcher for the game next to it
    pub copy_to: Option<PathBuf>,
}

/// the steps a port goes thru, simple mode lists them
pub const PORT_STEPS: [&str; 4] =
    ["Write the game's code as C", "Compile the C", "Build the program with Zakuro", "Copy the exe and make a launcher"];

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
            let installed = abi::installed(title.program_id());
            Ok(TitleInfo {
                name: title.exheader.title.clone(),
                program_id: title.program_id(),
                modules,
                broken: installed.as_deref().is_some_and(needs_cygwin),
                installed,
                path: rom.clone(),
            })
        }))
        .unwrap_or_else(|_| Err("reading the game crashed".to_owned()));
        out.send(Msg::Title(result));
    });
}

/// finds the tools (w/ the compiler the settings name, if any) on a thread, running them takes a moment
pub fn detect(out: Out, given: Option<Compiler>) {
    std::thread::spawn(move || {
        out.send(Msg::Tools(Box::new(toolchain::detect(given))));
    });
}

/// keeps a console program from popping up a window when this app has no console (the release build)
pub fn quiet(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn GetConsoleWindow() -> *mut std::ffi::c_void;
        }
        // safe bc it takes nothing and only answers
        if unsafe { GetConsoleWindow() }.is_null() {
            command.creation_flags(CREATE_NO_WINDOW);
        }
    }
    #[cfg(not(windows))]
    let _ = command;
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
            Job::Port(job) => port(&out, job, &cancel),
            Job::Install(recipes) => install(&out, &recipes, &cancel),
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

fn port(out: &Out, job: Port, cancel: &AtomicBool) -> Result<String, String> {
    let stopped = || cancel.load(Ordering::Relaxed);
    let Port { rom, dir, overrides, name, zakuro, compiler, ar, cargo, target, copy_to } = job;
    out.send(Msg::Step(0));
    out.stage("Finding the code and writing it as C");
    out.log(Level::Info, format!("making {} into a program in {}", rom.display(), dir.display()));
    let events = follow(out);
    let generated = build::generate(&rom, |_| dir.join("code"), overrides.as_deref(), Mods::default(), &events)?;
    if stopped() {
        return Err("stopped".to_owned());
    }
    out.send(Msg::Step(1));
    let (title, code) = (&generated.title, &generated.dir);
    let progress = |done, total| {
        events(Event::Compiled { done, total });
        !stopped()
    };
    let objects = compile::objects(&compiler, code, &generated.sources, &progress, false)?;
    out.stage("Archiving");
    compile::archive_with(&ar, &objects, &code.join("librecomp.a"))?;
    out.log(Level::Info, format!("built {} w/ {}", code.join("librecomp.a").display(), compiler.describe()));

    let name = name.filter(|name| !name.trim().is_empty()).unwrap_or_else(|| port::package_name(&title.exheader.title));
    port::write_project(&dir, &name, &title.exheader.title, title.program_id(), zakuro.as_deref())?;
    out.log(Level::Info, format!("wrote the project for {name} in {}", dir.display()));
    let Some(cargo) = cargo else {
        out.log(Level::Info, format!("build it with cargo build --release in {}", dir.display()));
        return Ok(format!("Wrote the {name} project"));
    };

    out.send(Msg::Step(2));
    out.stage("Building the program with Zakuro, the first time takes a while");
    // a project built before keeps the Zakuro it got then, which may be too old for code from this 3dsrecomp
    if dir.join("Cargo.lock").is_file() && zakuro.is_none() {
        if let Err(error) = run_cargo(out, &cargo, &dir, target.as_deref(), &["update", "-p", "zakuro"], cancel) {
            out.log(Level::Warn, format!("could not update Zakuro, building w/ the one it has ({error})"));
        }
    }
    run_cargo(out, &cargo, &dir, target.as_deref(), &["build", "--release", "--color", "never"], cancel)?;
    let built =
        target.unwrap_or_else(|| dir.join("target")).join("release").join(format!("{name}{}", std::env::consts::EXE_SUFFIX));
    if !built.is_file() {
        return Err(format!("cargo finished but there's no {}", built.display()));
    }
    let Some(folder) = copy_to else {
        out.send(Msg::Exe(built.clone()));
        return Ok(format!("Built {}", built.display()));
    };

    out.send(Msg::Step(3));
    out.stage("Copying the exe");
    std::fs::create_dir_all(&folder).map_err(|error| format!("could not create {}, {error}", folder.display()))?;
    let exe = folder.join(built.file_name().unwrap_or_default());
    std::fs::copy(&built, &exe)
        .map_err(|error| format!("could not copy the exe to {} (is it running?), {error}", exe.display()))?;
    let launcher = write_launcher(&folder, &name, &exe, &rom)?;
    out.log(Level::Info, format!("wrote {}, which starts it w/ the game", launcher.display()));
    out.send(Msg::Exe(exe.clone()));
    Ok(format!("Made {}", exe.file_name().unwrap_or_default().to_string_lossy()))
}

/// a script next to the exe that starts it w/ the game, the program reads the game for everything but its code
fn write_launcher(folder: &Path, name: &str, exe: &Path, rom: &Path) -> Result<PathBuf, String> {
    let rom = std::path::absolute(rom).unwrap_or_else(|_| rom.to_owned());
    let exe_name = exe.file_name().unwrap_or_default().to_string_lossy().into_owned();
    let (path, script) = if cfg!(windows) {
        // % starts a variable in a batch file, doubled it's itself
        let rom = rom.display().to_string().replace('%', "%%");
        (folder.join(format!("Play {name}.bat")), format!("@echo off\r\nstart \"\" \"%~dp0{exe_name}\" \"{rom}\"\r\n"))
    } else {
        let rom = rom.display().to_string().replace('\'', "'\\''");
        (folder.join(format!("play-{name}.sh")), format!("#!/bin/sh\nexec \"$(dirname \"$0\")/{exe_name}\" '{rom}'\n"))
    };
    std::fs::write(&path, script).map_err(|error| format!("could not write {}, {error}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(path)
}

/// runs cargo w/ args in dir, its output going to the log, killed when cancel says so
fn run_cargo(
    out: &Out,
    cargo: &Path,
    dir: &Path,
    target: Option<&Path>,
    args: &[&str],
    cancel: &AtomicBool,
) -> Result<(), String> {
    let mut command = Command::new(cargo);
    command.args(args).current_dir(dir).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
    command.env("CARGO_TERM_PROGRESS_WHEN", "never");
    if let Some(target) = target {
        command.env("CARGO_TARGET_DIR", target);
    }
    quiet(&mut command);
    out.log(Level::Info, format!("cargo {}", args.join(" ")));
    let mut child = command.spawn().map_err(|error| format!("could not run cargo, {error}"))?;
    let stderr = child.stderr.take().expect("piped");
    let reader = {
        let out = out.clone();
        std::thread::spawn(move || {
            let mut compiled = 0;
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                let trimmed = line.trim_start();
                let level = if trimmed.starts_with("error") {
                    Level::Error
                } else if trimmed.starts_with("warning") {
                    Level::Warn
                } else {
                    Level::Info
                };
                if trimmed.starts_with("Compiling ") {
                    compiled += 1;
                    out.stage(format!("Building the program with Zakuro, {compiled} packages compiled"));
                }
                out.log(level, line);
            }
        })
    };
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("stopped".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(error) => return Err(format!("lost track of cargo, {error}")),
        }
    };
    let _ = reader.join();
    if status.success() {
        Ok(())
    } else {
        Err(format!("cargo {} failed, the log says why", args[0]))
    }
}

/// installs each recipe w/ winget, going on past one that fails so the rest still get installed
fn install(out: &Out, recipes: &[&'static Recipe], cancel: &AtomicBool) -> Result<String, String> {
    let mut failed = Vec::new();
    for (i, recipe) in recipes.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err("stopped".to_owned());
        }
        out.send(Msg::Step(i));
        out.stage(format!("Installing {} ({} of {})", recipe.name, i + 1, recipes.len()));
        match run_winget(out, recipe, cancel) {
            Ok(()) => out.log(Level::Good, format!("{} is installed", recipe.name)),
            Err(error) if error == "stopped" => return Err(error),
            Err(error) => {
                out.log(Level::Error, error);
                failed.push(recipe.name);
            }
        }
    }
    if failed.is_empty() {
        Ok(format!("Installed {}", recipes.iter().map(|recipe| recipe.name).collect::<Vec<_>>().join(", ")))
    } else {
        Err(format!("Could not install {}, the log says why", failed.join(", ")))
    }
}

/// runs winget for a recipe, its lines going to the log and its progress bars to the stage
fn run_winget(out: &Out, recipe: &Recipe, cancel: &AtomicBool) -> Result<(), String> {
    let mut command = Command::new("winget");
    command.args(recipe.winget).args(toolchain::WINGET_ALWAYS).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    quiet(&mut command);
    out.log(Level::Info, format!("winget {}", recipe.winget.join(" ")));
    let mut child = command.spawn().map_err(|error| format!("could not run winget, {error}"))?;
    let mut stdout = child.stdout.take().expect("piped");
    let name = recipe.name;
    let reader = {
        let out = out.clone();
        std::thread::spawn(move || {
            // winget redraws its progress w/ \r, so a line ends at either
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            let flush = |bytes: &mut Vec<u8>| {
                let line = String::from_utf8_lossy(bytes).trim().to_owned();
                bytes.clear();
                // a spinner alone is noise, a bar or a percentage shows as the stage, the rest goes to the log
                let spinner = line.chars().all(|c| matches!(c, '\\' | '|' | '/' | '-' | ' '));
                let progress = line.contains('█') || line.contains('▒') || line.ends_with('%') || line.contains(" MB / ");
                if line.is_empty() || spinner {
                } else if progress {
                    out.stage(format!("Installing {name}, {line}"));
                } else {
                    out.log(Level::Info, line);
                }
            };
            while let Ok(read) = stdout.read(&mut buffer) {
                if read == 0 {
                    break;
                }
                for &byte in &buffer[..read] {
                    if byte == b'\r' || byte == b'\n' {
                        flush(&mut bytes);
                    } else {
                        bytes.push(byte);
                    }
                }
            }
            flush(&mut bytes);
        })
    };
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            // the installer winget started may go on, but nothing waits for it
            let _ = child.kill();
            let _ = child.wait();
            let _ = reader.join();
            return Err("stopped".to_owned());
        }
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(100)),
            Err(error) => return Err(format!("lost track of winget, {error}")),
        }
    };
    let _ = reader.join();
    // winget's code for already installed, which is what we want anyway
    const ALREADY_INSTALLED: i32 = 0x8A15_002B_u32 as i32;
    match status.code() {
        Some(0) | Some(ALREADY_INSTALLED) => Ok(()),
        Some(code) => Err(format!("installing {} failed, winget said 0x{:08X}", recipe.name, code as u32)),
        None => Err(format!("installing {} was stopped", recipe.name)),
    }
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
