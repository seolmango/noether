//! noetherc: the Noether compiler driver.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

const USAGE: &str = "usage: noetherc <file.noe> [-o out.exe] [--run] [--thermo] [--stats] \
                     [--no-isolate] [--no-symmetry]";
const SYSTEM_LIBS: [&str; 9] = [
    "kernel32.lib", "ntdll.lib", "userenv.lib", "ws2_32.lib", "dbghelp.lib", "advapi32.lib",
    "bcrypt.lib", "msvcrt.lib", "legacy_stdio_definitions.lib",
];

struct Args {
    input: PathBuf,
    output: PathBuf,
    stats: bool,
    isolate: bool,
    symmetry: bool,
    run: bool,
    thermo: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut input = None;
    let mut output = None;
    let (mut stats, mut isolate, mut symmetry) = (false, true, true);
    let (mut run, mut thermo) = (false, false);
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "-o" => output = Some(PathBuf::from(it.next().ok_or(USAGE)?)),
            "--stats" => stats = true,
            "--run" => run = true,
            "--thermo" => thermo = true,
            "--no-isolate" => isolate = false,
            "--no-symmetry" => symmetry = false,
            _ if a.starts_with('-') => return Err(USAGE.to_string()),
            _ => input = Some(PathBuf::from(a)),
        }
    }
    let input = input.ok_or(USAGE)?;
    let output = output.unwrap_or_else(|| input.with_extension("exe"));
    Ok(Args { input, output, stats, isolate, symmetry, run, thermo })
}

fn link(object: &Path, output: &Path) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let runtime = exe.with_file_name("noether_rt.lib");
    if !runtime.exists() {
        return Err(format!("runtime library not found at {}", runtime.display()));
    }
    // PATH may hold an unrelated link.exe, so ask for the MSVC one by name.
    let tool = cc::windows_registry::find_tool("x86_64-pc-windows-msvc", "link.exe")
        .ok_or("MSVC link.exe not found; install the Visual Studio Build Tools")?;
    let mut cmd: Command = tool.to_command();
    cmd.arg("/NOLOGO")
        .arg(format!("/OUT:{}", output.display()))
        .arg("/SUBSYSTEM:CONSOLE")
        .arg("/STACK:1073741824")
        .arg("/NODEFAULTLIB:libcmt")
        .arg(object)
        .arg(runtime)
        .args(SYSTEM_LIBS);
    let out = cmd.output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "link failed:\n{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(())
}

fn run() -> Result<(), (u8, String)> {
    let args = parse_args().map_err(|e| (64, e))?;
    let src = std::fs::read_to_string(&args.input)
        .map_err(|e| (66, format!("{}: {e}", args.input.display())))?;
    let ast = noether_syntax::parse(&src).map_err(|e| (2, e.to_string()))?;
    let module = noether_sema::analyze(ast).map_err(|e| (2, e.to_string()))?;
    let thermo = args.thermo.then(|| {
        args.output.with_extension("thermo.csv").to_string_lossy().into_owned()
    });
    if args.run {
        // A tree walk needs more stack per call than compiled code does.
        return std::thread::Builder::new()
            .stack_size(1 << 33)
            .spawn(move || noether_vm::run(&module, u64::MAX, thermo))
            .map_err(|e| (70, e.to_string()))?
            .join()
            .map_err(|_| (70, "the executor panicked".to_string()));
    }
    let opts = noether_nir::Options { isolate: args.isolate, symmetry: args.symmetry };
    let program = noether_nir::lower(&module, opts);
    if args.stats {
        let s = &program.stats;
        eprintln!(
            "reads: {} ({} general), symmetric blocks: {} ({} with a fast version)",
            s.reads, s.general, s.symmetric, s.symmetric_fast
        );
    }
    let object =
        noether_codegen::compile(&module, &program, thermo.as_deref()).map_err(|e| (70, e))?;
    let obj_path = args.output.with_extension("obj");
    std::fs::write(&obj_path, object).map_err(|e| (73, e.to_string()))?;
    let linked = link(&obj_path, &args.output);
    let _ = std::fs::remove_file(&obj_path);
    linked.map_err(|e| (70, e))
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err((code, msg)) => {
            eprintln!("noether: {msg}");
            ExitCode::from(code)
        }
    }
}
