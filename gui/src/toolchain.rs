//! finds the tools the app needs: a C compiler that builds for this system (not cygwin), the ar matching it, and cargo
//! looks in the usual install folders too, an installer often leaves its compiler off PATH
//! and says how to install what's missing, w/ winget on windows

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use recomp3ds::compile::Compiler;

/// what's on this computer, found once at the start and again when asked
#[derive(Clone)]
pub struct Tools {
    /// for a library Zakuro loads, any compiler that builds for this system
    pub library: Result<Compiler, String>,
    /// for an exe, a compiler matching rust's toolchain plus its ar
    pub exe: Result<(Compiler, PathBuf), String>,
    /// cargo, which builds the exe
    pub cargo: Result<PathBuf, String>,
    /// MSVC's linker and libraries, which rust's MSVC toolchain and clang link w/ (always fine elsewhere)
    pub linker: Result<PathBuf, String>,
    /// winget runs, so the setup can install what's missing
    pub winget: bool,
}

impl Tools {
    pub fn exe_ready(&self) -> bool {
        self.exe.is_ok() && self.cargo.is_ok() && self.linker.is_ok()
    }

    /// whether what a recipe installs is there
    pub fn has(&self, recipe: &Recipe) -> bool {
        match recipe.tool {
            Tool::Rust => self.cargo.is_ok(),
            Tool::BuildTools => self.linker.is_ok(),
            Tool::Llvm => self.exe.is_ok(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Rust,
    BuildTools,
    Llvm,
}

/// a tool the setup can install, and how
pub struct Recipe {
    pub tool: Tool,
    pub name: &'static str,
    pub what: &'static str,
    /// about how much it downloads and takes
    pub size: &'static str,
    /// winget's arguments that install it
    pub winget: &'static [&'static str],
    /// where to get it by hand
    pub page: &'static str,
}

/// what making an exe needs, in the order the setup installs them
pub const RECIPES: [Recipe; 3] = [
    Recipe {
        tool: Tool::BuildTools,
        name: "Visual Studio C++ Build Tools",
        what: "Microsoft's linker and Windows libraries, what Rust and clang build programs with",
        size: "about 3 GB",
        winget: &[
            "install",
            "-e",
            "--id",
            "Microsoft.VisualStudio.2022.BuildTools",
            "--override",
            "--wait --passive --norestart --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended",
        ],
        page: "https://visualstudio.microsoft.com/visual-cpp-build-tools/",
    },
    Recipe {
        tool: Tool::Rust,
        name: "Rust",
        what: "builds the program around the game's code (cargo)",
        size: "about 600 MB",
        winget: &["install", "-e", "--id", "Rustlang.Rustup", "--silent"],
        page: "https://rustup.rs",
    },
    Recipe {
        tool: Tool::Llvm,
        name: "LLVM",
        what: "clang, which compiles the game's code",
        size: "about 2 GB",
        winget: &["install", "-e", "--id", "LLVM.LLVM", "--silent"],
        page: "https://github.com/llvm/llvm-project/releases/latest",
    },
];

/// what every winget install adds, so it runs w/o asking anything in a console nobody sees
pub const WINGET_ALWAYS: &[&str] = &["--accept-package-agreements", "--accept-source-agreements", "--disable-interactivity"];

/// what a compiler builds for
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// cygwin or msys, its output needs msys-2.0.dll
    Cygwin,
    /// clang for MSVC, links like rust's default windows toolchain
    Msvc,
    /// MinGW-w64, links like rust's gnu windows toolchain
    Mingw,
    /// anything else, like linux or macOS
    Native,
}

struct Found {
    compiler: Compiler,
    kind: Kind,
}

/// runs program w/ args, its stdout if it worked
fn output(program: &Path, args: &[&str]) -> Option<String> {
    let mut command = Command::new(program);
    command.args(args).stdin(Stdio::null()).stderr(Stdio::null());
    crate::worker::quiet(&mut command);
    let output = command.output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

/// compilers worth trying, the user's first, then PATH, then where installers put them
fn candidates(custom: Option<Compiler>) -> Vec<Compiler> {
    let mut list: Vec<Compiler> = custom.into_iter().collect();
    for name in ["gcc", "clang", "cc"] {
        list.push(Compiler::new(name, &[]));
    }
    if cfg!(windows) {
        let var = |name: &str| std::env::var_os(name).map(PathBuf::from);
        let mut paths: Vec<PathBuf> = Vec::new();
        for program_files in [var("ProgramFiles"), var("ProgramFiles(x86)")].into_iter().flatten() {
            paths.push(program_files.join("LLVM/bin/clang.exe"));
        }
        for root in ["C:/msys64", "C:/tools/msys64"] {
            for env in ["ucrt64", "mingw64", "clang64"] {
                paths.push(Path::new(root).join(env).join("bin/gcc.exe"));
                paths.push(Path::new(root).join(env).join("bin/clang.exe"));
            }
        }
        paths.push("C:/mingw64/bin/gcc.exe".into());
        paths.push("C:/TDM-GCC-64/bin/gcc.exe".into());
        if let Some(profile) = var("USERPROFILE") {
            paths.push(profile.join("scoop/apps/llvm/current/bin/clang.exe"));
            paths.push(profile.join("scoop/apps/mingw/current/bin/gcc.exe"));
        }
        // winget's WinLibs lands in a folder w/ a version in its name
        if let Some(packages) = var("LOCALAPPDATA").map(|dir| dir.join("Microsoft/WinGet/Packages")) {
            if let Ok(entries) = std::fs::read_dir(packages) {
                for entry in entries.flatten() {
                    if entry.file_name().to_string_lossy().starts_with("BrechtSanders.WinLibs") {
                        paths.push(entry.path().join("mingw64/bin/gcc.exe"));
                    }
                }
            }
        }
        list.extend(paths.into_iter().filter(|path| path.is_file()).map(|path| Compiler::new(path, &[])));
    }
    list
}

fn classify(compiler: Compiler) -> Option<Found> {
    let target = compiler.target()?;
    let kind = if target.contains("cygwin") || target.contains("msys") {
        Kind::Cygwin
    } else if target.contains("msvc") {
        Kind::Msvc
    } else if target.contains("mingw") || target.contains("windows-gnu") {
        Kind::Mingw
    } else {
        Kind::Native
    };
    Some(Found { compiler, kind })
}

/// the ar for a compiler's objects, llvm-ar for clang and ar for gcc, the one next to it first
fn ar_for(found: &Found) -> Option<PathBuf> {
    let names: &[&str] = if found.kind == Kind::Msvc { &["llvm-ar"] } else { &["ar", "llvm-ar"] };
    let dir = found.compiler.program.parent().filter(|dir| !dir.as_os_str().is_empty());
    for name in names {
        let file = format!("{name}{}", std::env::consts::EXE_SUFFIX);
        if let Some(path) = dir.map(|dir| dir.join(&file)).filter(|path| path.is_file()) {
            return Some(path);
        }
        if found.kind != Kind::Cygwin && output(Path::new(name), &["--version"]).is_some() {
            return Some(PathBuf::from(name));
        }
    }
    None
}

/// cargo on PATH, or where rustup puts it
fn cargo() -> Result<PathBuf, String> {
    let mut paths = vec![PathBuf::from("cargo")];
    if let Some(home) = std::env::var_os("CARGO_HOME").map(PathBuf::from) {
        paths.push(home.join("bin/cargo"));
    }
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from) {
        paths.push(home.join(".cargo/bin/cargo"));
    }
    paths
        .into_iter()
        .find(|path| output(path, &["--version"]).is_some())
        .ok_or_else(|| "Rust isn't installed, get it from rustup.rs".to_owned())
}

/// MSVC's C++ build tools, found the way rustc finds them, w/ vswhere
fn msvc_linker() -> Result<PathBuf, String> {
    let missing = || "Visual Studio's C++ Build Tools aren't installed, Rust and clang need Microsoft's linker".to_owned();
    let program_files = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from).ok_or_else(missing)?;
    let vswhere = program_files.join("Microsoft Visual Studio/Installer/vswhere.exe");
    let args = ["-latest", "-products", "*", "-requires", "Microsoft.VisualStudio.Component.VC.Tools.x86.x64", "-property", "installationPath"];
    output(&vswhere, &args).filter(|path| !path.is_empty()).map(PathBuf::from).ok_or_else(missing)
}

/// the target rust builds for, rustc's host (x86_64-pc-windows-msvc...)
fn rust_host(cargo: &Path) -> Option<String> {
    let rustc = cargo.with_file_name(format!("rustc{}", std::env::consts::EXE_SUFFIX));
    let rustc = if rustc.is_file() { rustc } else { PathBuf::from("rustc") };
    let info = output(&rustc, &["-vV"])?;
    info.lines().find_map(|line| line.strip_prefix("host: ")).map(str::to_owned)
}

/// looks for everything, custom being the compiler the settings name, if any
pub fn detect(custom: Option<Compiler>) -> Tools {
    let found: Vec<Found> = candidates(custom.clone()).into_iter().filter_map(classify).collect();
    let cygwin = found.iter().find(|found| found.kind == Kind::Cygwin).map(|found| found.compiler.describe());
    let not_windows = |name: &str| {
        format!("{name} builds for Cygwin/MSYS, not Windows, what it makes crashes outside MSYS. Install LLVM (clang) or MinGW-w64's gcc.")
    };

    let library = match &custom {
        // the one the settings name, if it's any good
        Some(compiler) => match found.iter().find(|found| found.compiler == *compiler) {
            Some(found) if found.kind == Kind::Cygwin => Err(not_windows(&compiler.describe())),
            Some(found) => Ok(found.compiler.clone()),
            None => Err(format!("{} doesn't run", compiler.describe())),
        },
        None => [Kind::Mingw, Kind::Msvc, Kind::Native]
            .iter()
            .find_map(|kind| found.iter().find(|found| found.kind == *kind))
            .map(|found| found.compiler.clone())
            .ok_or_else(|| match &cygwin {
                Some(name) => not_windows(name),
                None => "No C compiler found. Install LLVM (clang) or MinGW-w64's gcc.".to_owned(),
            }),
    };

    let cargo = cargo();
    // w/o rust yet, what rustup installs on windows, the MSVC toolchain
    let fallback = if cfg!(windows) { "x86_64-pc-windows-msvc".to_owned() } else { String::new() };
    let host = cargo.as_ref().ok().and_then(|cargo| rust_host(cargo)).unwrap_or(fallback);
    // the C has to link w/ what rust links, clang for MSVC on rust's default windows toolchain, MinGW's gcc on its gnu one
    let wanted = if host.contains("msvc") {
        Kind::Msvc
    } else if host.contains("windows-gnu") {
        Kind::Mingw
    } else {
        Kind::Native
    };
    let preferred =
        custom.as_ref().and_then(|custom| found.iter().find(|found| found.compiler == *custom && found.kind == wanted));
    let exe = match preferred.or_else(|| found.iter().find(|found| found.kind == wanted)) {
        Some(found) => match ar_for(found) {
            Some(ar) => Ok((found.compiler.clone(), ar)),
            None => Err(format!(
                "{} has no {} next to it",
                found.compiler.describe(),
                if wanted == Kind::Msvc { "llvm-ar" } else { "ar" }
            )),
        },
        None => Err(match wanted {
            Kind::Msvc => "Making an exe needs LLVM's clang, which links with Rust's MSVC toolchain. Install LLVM.".to_owned(),
            Kind::Mingw => "Making an exe needs MinGW-w64's gcc, which links with Rust's GNU toolchain.".to_owned(),
            _ => "Making an exe needs a C compiler.".to_owned(),
        }),
    };
    let linker = if host.contains("msvc") { msvc_linker() } else { Ok(PathBuf::new()) };
    let winget = cfg!(windows) && output(Path::new("winget"), &["--version"]).is_some();
    Tools { library, exe, cargo, linker, winget }
}
