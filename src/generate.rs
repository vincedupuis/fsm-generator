//! The whole pipeline: read `.fsm` files (and the machines their submachine
//! states reference), validate them, build their code models and render them
//! with a template. The same steps and messages as
//! fsm-editor-vscode/src/codegen/generate.ts.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use crate::codemodel::{CodeModel, Options, to_code_model};
use crate::model::{FsmModel, SubmachineInfo, VertexType, machine_connection_points};
use crate::paths::{decode_uri, display, resolve};
use crate::render::{Renderer, TemplateError};
use crate::validation::{Severity, validate};
use crate::xmi::{from_xmi, model_hash};

pub struct FileIssue {
    /// The file, as displayed.
    pub file: String,
    /// xmi:id of the element ('' for the machine itself).
    pub id: String,
    pub severity: Severity,
    pub message: String,
}

/// A file to write, with the machine it was generated from.
pub struct GeneratedFile {
    pub path: String,
    pub content: String,
    /// The machine's `.fsm` file, as displayed (for a file shared by several machines, the first one).
    pub source: String,
    /// Its model hash (`model_hash` in xmi.rs).
    pub model_hash: String,
}

pub struct GenerateResult {
    pub files: Vec<GeneratedFile>,
    /// Validation problems and what the template reported.
    pub issues: Vec<FileIssue>,
}

pub enum GenerateError {
    /// Nothing was generated; `issues` say why (they may be empty).
    Failed {
        message: String,
        issues: Vec<FileIssue>,
    },
    Template(TemplateError),
}

impl GenerateError {
    fn new(message: String) -> GenerateError {
        GenerateError::Failed { message, issues: Vec::new() }
    }
}

struct Loaded {
    model: FsmModel,
    /// `model_hash` of the file.
    hash: String,
    /// href as written in this model → path of the loaded machine.
    refs: Vec<(String, PathBuf)>,
    code: Option<Rc<CodeModel>>,
}

#[derive(Default)]
struct Loader {
    loaded: HashMap<PathBuf, Loaded>,
    issues: Vec<FileIssue>,
    building: HashSet<PathBuf>,
}

impl Loader {
    fn check_id(path: &Path, model: &FsmModel, expected: Option<&str>) -> Result<(), GenerateError> {
        match expected {
            Some(id) if !id.is_empty() && model.id != id => {
                Err(GenerateError::new(format!("{} has no state machine with id '{id}'.", display(path))))
            }
            _ => Ok(()),
        }
    }

    fn load(&mut self, path: &Path, expected_id: Option<&str>) -> Result<(), GenerateError> {
        if let Some(known) = self.loaded.get(path) {
            return Loader::check_id(path, &known.model, expected_id);
        }
        let text = std::fs::read_to_string(path)
            .map_err(|_| GenerateError::new(format!("{} does not exist or cannot be read.", display(path))))?;
        let model = from_xmi(&text)
            .map_err(|e| GenerateError::new(format!("{} is not a valid state machine file: {e}", display(path))))?;
        Loader::check_id(path, &model, expected_id)?;
        // The file reads as XML, so it hashes.
        let hash = model_hash(&text).unwrap_or_default();
        let hrefs: Vec<(String, String)> = model
            .vertices
            .iter()
            .filter(|v| v.vtype == VertexType::State)
            .filter_map(|v| v.submachine.clone().map(|s| (v.id.clone(), s)))
            .collect();
        self.loaded.insert(path.to_path_buf(), Loaded { model, hash, refs: Vec::new(), code: None });

        // Submachines, then validation with what is known about them.
        let mut info: HashMap<String, SubmachineInfo> = HashMap::new();
        for (vertex, href) in hrefs {
            if self.loaded[path].refs.iter().any(|(h, _)| *h == href) {
                continue;
            }
            let mut parts = href.split('#');
            let file = parts.next().unwrap_or("");
            let id = parts.next().unwrap_or("");
            let dir = path.parent().unwrap_or(Path::new("/"));
            let sub_path = resolve(dir, &decode_uri(file));
            match self.load(&sub_path, Some(id)) {
                Ok(()) => {
                    let sub = &self.loaded[&sub_path].model;
                    let found =
                        SubmachineInfo { found: true, name: sub.name.clone(), points: machine_connection_points(sub) };
                    self.loaded.get_mut(path).unwrap().refs.push((href.clone(), sub_path));
                    info.insert(href, found);
                }
                Err(GenerateError::Failed { message, .. }) => {
                    // Reported by the validator as a missing machine; say why.
                    self.issues.push(FileIssue { file: display(path), id: vertex, severity: Severity::Error, message });
                    info.insert(href, SubmachineInfo::default());
                }
                Err(e) => return Err(e),
            }
        }
        let issues = validate(&self.loaded[path].model, &info);
        for i in issues {
            self.issues.push(FileIssue { file: display(path), id: i.id, severity: i.severity, message: i.message });
        }
        Ok(())
    }

    /// Stops the generation on the errors in the machines.
    fn failed(&mut self) -> GenerateError {
        let errors = self.issues.iter().filter(|i| i.severity == Severity::Error).count();
        let message = format!(
            "{errors} error{} in the state machine{}; nothing was generated.",
            plural(errors),
            plural(self.loaded.len())
        );
        GenerateError::Failed { message, issues: std::mem::take(&mut self.issues) }
    }

    /// The code model of a loaded machine, its submachines' first.
    fn code_of(&mut self, path: &Path) -> Result<Rc<CodeModel>, GenerateError> {
        let l = &self.loaded[path];
        if let Some(code) = &l.code {
            return Ok(code.clone());
        }
        if !self.building.insert(path.to_path_buf()) {
            return Err(GenerateError::new(format!(
                "{} uses itself as a submachine (directly or through other machines).",
                display(path)
            )));
        }
        let refs = l.refs.clone();
        let mut submachines = HashMap::new();
        for (href, sub) in refs {
            submachines.insert(href, self.code_of(&sub)?);
        }
        let options = Options { submachines, source_file: display(path), model_hash: self.loaded[path].hash.clone() };
        let code = match to_code_model(&self.loaded[path].model, &options) {
            Ok(code) => Rc::new(code),
            Err(e) => {
                // Reported at the element it is about, as a validation error would be.
                self.issues.push(FileIssue {
                    file: display(path),
                    id: e.id,
                    severity: Severity::Error,
                    message: e.message,
                });
                return Err(self.failed());
            }
        };
        self.loaded.get_mut(path).unwrap().code = Some(code.clone());
        self.building.remove(path);
        Ok(code)
    }
}

fn plural(n: usize) -> &'static str {
    if n > 1 { "s" } else { "" }
}

/// Generates the files of `sources` (absolute paths) with `renderer`.
pub fn generate(sources: &[PathBuf], renderer: &Renderer) -> Result<GenerateResult, GenerateError> {
    let mut loader = Loader::default();
    for s in sources {
        loader.load(s, None)?;
    }
    if loader.issues.iter().any(|i| i.severity == Severity::Error) {
        return Err(loader.failed());
    }

    let mut files: Vec<GeneratedFile> = Vec::new();
    let mut owner: HashMap<String, PathBuf> = HashMap::new();
    let mut done = HashSet::new();
    for path in sources {
        if !done.insert(path) {
            continue;
        }
        let code = loader.code_of(path)?;
        let (rendered, reported) = renderer.render(&code).map_err(GenerateError::Template)?;
        // What the template reports about this machine, as issues of its file.
        for r in reported {
            loader.issues.push(FileIssue { file: display(path), id: r.id, severity: r.severity, message: r.message });
        }
        for f in rendered {
            if let Some(other) = owner.get(&f.path) {
                // A file shared by several machines (a common header) is written once, when they agree on its content.
                if files.iter().find(|x| x.path == f.path).is_some_and(|x| x.content == f.content) {
                    continue;
                }
                return Err(GenerateError::new(format!(
                    "{} and {} both generate '{}': give the state machines different names.",
                    display(path),
                    display(other),
                    f.path
                )));
            }
            owner.insert(f.path.clone(), path.clone());
            files.push(GeneratedFile {
                path: f.path,
                content: f.content,
                source: display(path),
                model_hash: loader.loaded[path].hash.clone(),
            });
        }
    }
    let errors = loader.issues.iter().filter(|i| i.severity == Severity::Error).count();
    if errors > 0 {
        let message = format!("The template reported {errors} error{}; nothing was generated.", plural(errors));
        return Err(GenerateError::Failed { message, issues: loader.issues });
    }
    Ok(GenerateResult { files, issues: loader.issues })
}
