//! Command-line interface: `fsm-generator <files...> --template <template>`.
//!
//! Reads UML state machines (`.fsm` files: XMI 2.5.1), validates them, builds
//! their code models (the same as the `fsm` CLI of fsm-editor-vscode) and
//! renders them with a Jinja template (MiniJinja).

mod codemodel;
mod expressions;
mod generate;
mod languages;
mod model;
mod paths;
mod render;
mod sha256;
mod tables;
mod validation;
mod xmi;
mod xml;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use generate::{FileIssue, GenerateError, GeneratedFile, generate};
use paths::{absolute, display, expand_glob, slash};
use render::{Renderer, TemplateError, parse_template};

const USAGE: &str = "Usage: fsm-generator <files...> --template <template> [--out <folder>] [options]

Generates code from UML state machines (.fsm files) with a Jinja template.
The machines are validated first: nothing is generated when they have errors.
Machines used by submachine states are read and validated, but only the
given files are generated. Files can be glob patterns; quote them (\"models/**/*.fsm\") so every shell
passes them through unchanged.

Options:
  -t, --template <file>  Template (.jinja). A bare name such as \"ts\" means
                         templates/<name>.jinja next to this program.
  -o, --out <folder>     Output folder (default: the current folder).
      --check            Write nothing: report the files of the output folder
                         that are missing, or out of date because they don't
                         hold the model hash of their .fsm file any more.
  -h, --help             Show this help.
  -v, --version          Show the version.

Exit codes: 0 success, 1 errors in the state machines or the template (with
--check, also files missing or out of date), 2 bad usage.";

/// Why the program stops.
enum Failure {
    /// Bad usage: exit code 2.
    Usage(String),
    /// Exit code 1.
    Error(String),
}

struct Args {
    files: Vec<String>,
    template: String,
    out: String,
    check: bool,
}

enum Command {
    Help,
    Version,
    Run(Args),
}

fn parse_args(argv: &[String]) -> Result<Command, Failure> {
    let mut files = Vec::new();
    let mut template = None;
    let mut out = ".".to_string();
    let mut check = false;
    let value = |i: usize, flag: &str| match argv.get(i + 1) {
        Some(v) if !v.starts_with('-') => Ok(v.clone()),
        _ => Err(Failure::Usage(format!("{flag} needs a value."))),
    };
    let mut i = 0;
    while i < argv.len() {
        let a = &argv[i];
        let (flag, inline) = match a.split_once('=') {
            Some((f, v)) if a.starts_with("--") => (f, Some(v.to_string())),
            _ => (a.as_str(), None),
        };
        match flag {
            "-h" | "--help" => return Ok(Command::Help),
            "-v" | "--version" => return Ok(Command::Version),
            "-t" | "--template" => {
                template = Some(match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        value(i - 1, flag)?
                    }
                })
            }
            "-o" | "--out" => {
                out = match inline {
                    Some(v) => v,
                    None => {
                        i += 1;
                        value(i - 1, flag)?
                    }
                }
            }
            "--check" => check = true,
            _ => {
                if a.starts_with('-') && a != "-" {
                    return Err(Failure::Usage(format!("Unknown option {a}.")));
                }
                files.push(a.clone());
            }
        }
        i += 1;
    }
    match (files.is_empty(), template) {
        (true, None) => Ok(Command::Help),
        (true, Some(_)) => Err(Failure::Usage("No .fsm files given.".to_string())),
        (false, None) => Err(Failure::Usage("Missing --template.".to_string())),
        (false, Some(template)) => Ok(Command::Run(Args { files, template, out, check })),
    }
}

/// The files matching the patterns, without duplicates.
fn sources(patterns: &[String]) -> Result<Vec<PathBuf>, Failure> {
    let mut files: Vec<PathBuf> = Vec::new();
    for p in patterns {
        let found = expand_glob(p);
        if found.is_empty() {
            return Err(Failure::Usage(format!("No files match '{p}'.")));
        }
        for f in found {
            if !files.contains(&f) {
                files.push(f);
            }
        }
    }
    Ok(files)
}

/// The template file: a path, or a bare name looked up in the `templates` folder next to the program (or, for
/// `cargo run`, of the project).
fn template_path(name: &str) -> Result<PathBuf, Failure> {
    // A bare name is a file of the current folder only when there is one: a folder named like the template
    // (e.g. the output folder of `-t ts -o ts`) is not.
    if name.contains(['/', '\\']) || name.to_lowercase().ends_with(".jinja") || Path::new(name).is_file() {
        return Ok(absolute(name));
    }
    let exe = std::env::current_exe().ok().and_then(|e| e.canonicalize().ok());
    let exe_dir = exe.as_deref().and_then(Path::parent).map(Path::to_path_buf).unwrap_or_default();
    let mut dirs = vec![exe_dir.join("templates")];
    if let Some(project) = exe_dir.parent().and_then(Path::parent) {
        dirs.push(project.join("templates"));
    }
    for d in &dirs {
        let f = d.join(format!("{name}.jinja"));
        if f.exists() {
            return Ok(f);
        }
    }
    let looked: Vec<String> = dirs.iter().map(|d| slash(&d.to_string_lossy())).collect();
    Err(Failure::Usage(format!("Template '{name}' not found (looked for {name}.jinja in {}).", looked.join(", "))))
}

/// Line of the element with xmi:id `id` in `file`.
fn line_of(file: &str, id: &str) -> usize {
    if id.is_empty() {
        return 1;
    }
    match std::fs::read_to_string(absolute(file)) {
        Ok(text) => match text.find(&format!("xmi:id=\"{id}\"")) {
            Some(i) => text[..i].matches('\n').count() + 1,
            None => 1,
        },
        Err(_) => 1,
    }
}

/// `file:line: severity: message`, the format editors and CI problem matchers understand.
fn report(issues: &[FileIssue]) {
    for i in issues {
        eprintln!("{}:{}: {}: {}", i.file, line_of(&i.file, &i.id), i.severity.as_str(), i.message);
    }
}

fn template_failure(file: &Path, e: TemplateError) -> Failure {
    match e.line {
        Some(line) => Failure::Error(format!("{}:{line}: {}", display(file), e.message)),
        None => Failure::Error(format!("{}: {}", display(file), e.message)),
    }
}

fn run(args: Args) -> Result<(), Failure> {
    let tpl_file = template_path(&args.template)?;
    let text = std::fs::read_to_string(&tpl_file)
        .map_err(|_| Failure::Usage(format!("Cannot read the template {}.", display(&tpl_file))))?;
    let template = parse_template(&text).map_err(|e| template_failure(&tpl_file, e))?;
    let name = tpl_file.file_name().map_or("template".to_string(), |n| n.to_string_lossy().into_owned());
    let renderer = Renderer::new(&template, &name).map_err(|e| template_failure(&tpl_file, e))?;
    let result = match generate(&sources(&args.files)?, &renderer) {
        Ok(result) => result,
        Err(GenerateError::Failed { message, issues }) => {
            report(&issues);
            return Err(Failure::Error(message));
        }
        Err(GenerateError::Template(e)) => return Err(template_failure(&tpl_file, e)),
    };
    report(&result.issues);
    let out = absolute(&args.out);
    if args.check {
        return check(&result.files, &out);
    }
    for f in &result.files {
        let target = f.path.split('/').fold(out.clone(), |p, part| p.join(part));
        let write = || -> std::io::Result<()> {
            if let Some(dir) = target.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&target, &f.content)
        };
        write().map_err(|e| Failure::Error(format!("Cannot write {}: {e}", display(&target))))?;
        println!("wrote {}", display(&target));
    }
    Ok(())
}

/// `--check`: compares the files of the output folder with the model hashes of their `.fsm` files instead of
/// writing them. Only files in which the template writes their machine's hash (`machine.modelHash`) are checked:
/// up to date when the file in the folder holds that hash, missing when it can't be read, out of date otherwise.
fn check(files: &[GeneratedFile], out: &Path) -> Result<(), Failure> {
    let mut failed = 0;
    for f in files {
        let Some(at) = f.content.find(&f.model_hash).filter(|_| !f.model_hash.is_empty()) else {
            continue;
        };
        let target = f.path.split('/').fold(out.to_path_buf(), |p, part| p.join(part));
        // Read as Node.js does with 'utf8': invalid sequences become U+FFFD.
        let text = std::fs::read(&target).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        if text.as_deref().is_some_and(|t| t.contains(&f.model_hash)) {
            println!("up to date {}", display(&target));
            continue;
        }
        failed += 1;
        let (line, message) = match text {
            None => (1, format!("Missing: not generated from {} yet.", f.source)),
            Some(_) => (
                f.content[..at].matches('\n').count() + 1,
                format!(
                    "Out of date: {} has changed since it was generated (its model hash is now {}).",
                    f.source, f.model_hash
                ),
            ),
        };
        eprintln!("{}:{line}: error: {message}", display(&target));
    }
    match failed {
        0 => Ok(()),
        1 => Err(Failure::Error("1 generated file is missing or out of date.".to_string())),
        n => Err(Failure::Error(format!("{n} generated files are missing or out of date."))),
    }
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let result = parse_args(&argv).and_then(|command| match command {
        Command::Help => {
            println!("{USAGE}");
            Ok(())
        }
        Command::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Command::Run(args) => run(args),
    });
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("fsm-generator: {message}\nRun 'fsm-generator --help' for usage.");
            ExitCode::from(2)
        }
        Err(Failure::Error(message)) => {
            eprintln!("fsm-generator: {message}");
            ExitCode::from(1)
        }
    }
}
