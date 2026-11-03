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
mod tables;
mod validation;
mod xmi;
mod xml;

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use generate::{FileIssue, GenerateError, generate};
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
  -h, --help             Show this help.
  -v, --version          Show the version.

Exit codes: 0 success, 1 errors in the state machines or the template, 2 bad usage.";

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
        (false, Some(template)) => Ok(Command::Run(Args { files, template, out })),
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
    if name.contains(['/', '\\']) || name.to_lowercase().ends_with(".jinja") || Path::new(name).exists() {
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
