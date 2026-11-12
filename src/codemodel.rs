//! Turns an FsmModel into a language-neutral code model: everything a code
//! template needs, with the UML semantics already worked out. A port of
//! `toCodeModel()` (fsm-editor-vscode/src/codegen/codeModel.ts), producing the
//! same model; docs/CODEGEN.md there documents it.
//!
//! - The state hierarchy becomes tables (states, regions, their indices).
//! - Every transition becomes a list of steps (exit, call, enter, ...) in UML
//!   order: exits from the innermost active state up to the transition's
//!   domain, then effects, then entries down to the targets, with default
//!   entry of the other regions. Choices become `choice` steps evaluated at run
//!   time. Junctions are static: they are expanded into one transition per
//!   path, with the guards combined.
//! - Joins become a transition that needs all its source states completed.
//! - Guards are trees (`Condition`), never text in some target language.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use serde::{Serialize, Serializer};

use crate::expressions::{self as expr, Condition};
use crate::model::{FsmModel, MachineKind, ModelIndex, PointKind, Transition, TransitionKind, VertexType, js_trim};
use crate::tables::{CodeTables, to_tables};

// ------------------------------------------------------------------ code model

/// A number as JavaScript prints it: integers without a fraction.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Num(pub f64);

impl Serialize for Num {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        if self.0.fract() == 0.0 && self.0.abs() < 9_007_199_254_740_992.0 {
            s.serialize_i64(self.0 as i64)
        } else {
            s.serialize_f64(self.0)
        }
    }
}

impl std::fmt::Display for Num {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.0.fract() == 0.0 && self.0.abs() < 9_007_199_254_740_992.0 {
            write!(f, "{}", self.0 as i64)
        } else {
            write!(f, "{}", self.0)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Ref {
    pub name: String,
    pub index: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeModel {
    pub machine: CodeMachine,
    /// All regions, outermost first. The first ones are the machine's top-level regions.
    pub regions: Vec<CodeRegion>,
    pub top_regions: Vec<CodeRegion>,
    /// All states and final states, in depth-first order.
    pub states: Vec<Rc<CodeState>>,
    /// Named events this machine accepts, including those of its submachines.
    pub events: Vec<CodeEvent>,
    /// Time events: one per `after(...)` trigger text and source state.
    pub timers: Vec<CodeTimer>,
    /// Events, then timers, numbered from 0.
    pub signals: Vec<CodeSignal>,
    /// Functions called by entry/exit behaviors and effects.
    pub actions: Vec<String>,
    /// Functions called by guards, invariants and pre/postconditions.
    pub conditions: Vec<String>,
    /// Functions called by do activities.
    pub activities: Vec<String>,
    /// Everything that can fire, with its steps. Referenced by the candidates of the states.
    pub transitions: Vec<CodeTransition>,
    /// Distinct machines referenced by submachine states.
    pub submachines: Vec<CodeSubmachine>,
    /// The machine's own entry points (used when it is a submachine).
    pub entry_points: Vec<CodeEntryPoint>,
    /// The machine's own exit points.
    pub exit_points: Vec<CodeExitPoint>,
    /// Steps of the default entry into the machine.
    pub start: Vec<Step>,
    /// States with an invariant.
    pub invariants: Vec<Rc<CodeState>>,
    pub features: CodeFeatures,
    /// The machine as nested transition tables, for frameworks without transitions across composite states (see tables.rs).
    pub tables: CodeTables,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeMachine {
    /// Identifier (letters, digits, `_`).
    pub name: String,
    pub display_name: String,
    /// xmi:id, as used in hrefs.
    pub id: String,
    pub kind: &'static str,
    pub protocol: bool,
    pub context: String,
    pub documentation: String,
    /// Path of the `.fsm` file, as given to the generator.
    pub source_file: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeRegion {
    pub index: usize,
    /// Unique identifier, e.g. `Root`, `On_region`, `Playing_audio`.
    pub name: String,
    pub display_name: String,
    pub top: bool,
    /// Owning state, or null for a top-level region.
    pub owner: Option<Ref>,
    pub owner_index: i64,
    pub states: Vec<Ref>,
    /// Default entry: the transition of the initial pseudostate. Empty when the region has none.
    pub default_steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeState {
    pub index: usize,
    /// xmi:id of the state.
    pub id: String,
    /// Unique identifier among the states.
    pub name: String,
    pub display_name: String,
    /// `simple`, `composite`, `submachine`, `final`.
    pub kind: &'static str,
    pub is_final: bool,
    /// The region containing the state.
    pub region: Ref,
    pub region_index: usize,
    /// The enclosing state, or null at the top level.
    pub parent: Option<Ref>,
    pub parent_index: i64,
    pub depth: usize,
    pub regions: Vec<Ref>,
    /// Own entry: entry behavior, starting do activities and timers.
    pub entry: Vec<Step>,
    /// Own exit: stopping the submachine, timers and do activities, then the exit behavior.
    pub exit: Vec<Step>,
    pub activities: Vec<String>,
    pub timers: Vec<Ref>,
    /// Deferred events (signal refs).
    pub deferred: Vec<Ref>,
    pub invariant: Option<Condition>,
    pub stereotype: String,
    pub submachine: Option<SubmachineRef>,
    /// `immediate`, `activities`, `regions`, `submachine` or `never` (final states).
    pub completes: &'static str,
    /// Transitions triggered by events and timers, grouped by signal, in priority order.
    pub handlers: Vec<CodeHandler>,
    /// Completion transitions (no trigger).
    pub completion: Vec<CodeCandidate>,
    /// Submachine states: what to do when the submachine leaves through one of its exit points.
    pub exits: Vec<CodeExit>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmachineRef {
    pub machine: String,
    pub href: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeExit {
    pub point: String,
    pub candidates: Vec<CodeCandidate>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeHandler {
    pub signal: String,
    pub signal_index: usize,
    pub kind: &'static str,
    pub candidates: Vec<CodeCandidate>,
    /// The last candidate has no condition at all, so the handler always takes a transition.
    pub exhaustive: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeCandidate {
    pub transition: String,
    pub transition_index: usize,
    pub guard: Option<Condition>,
    /// Protocol state machines: the transition is taken only when it holds; otherwise it is a protocol violation.
    pub precondition: Option<Condition>,
    /// Joins: every one of these states must be active and completed.
    pub requires: Vec<Ref>,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeTransition {
    pub index: usize,
    /// xmi:id of the transition leaving the source (of the join for joins).
    pub id: String,
    pub name: String,
    /// Readable description, e.g. `Paused -> Stopped (after(5m))`.
    pub label: String,
    /// Pseudostates the transition goes through, in the order reached: those of its own way through junctions, and
    /// those of every branch of its choices.
    pub via: Vec<CodeVia>,
    /// `external`, `internal`, `local`, `join` or `exitPoint`.
    pub kind: &'static str,
    /// Main source state; the transition is skipped when it is no longer active.
    pub source: Ref,
    pub source_index: usize,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CodeVia {
    pub kind: &'static str,
    /// Name of the pseudostate, or of the state owning an entry/exit point (`On.fault`).
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeEvent {
    pub name: String,
    pub index: usize,
    pub signal_index: i64,
    /// False for events only handled by submachines.
    pub own: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeTimer {
    pub name: String,
    pub index: usize,
    pub signal_index: i64,
    pub ms: Num,
    pub trigger: String,
    pub state: String,
    pub state_index: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeSignal {
    pub name: String,
    pub index: usize,
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeSubmachine {
    /// Machine name, used as the class name of the generated submachine.
    pub name: String,
    pub href: String,
    pub events: Vec<String>,
    pub entry_points: Vec<String>,
    pub exit_points: Vec<String>,
    /// It uses time events, itself or through its own submachines.
    pub timers: bool,
    /// Submachine states using it.
    pub states: Vec<Ref>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeEntryPoint {
    pub name: String,
    pub id: String,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeExitPoint {
    pub name: String,
    pub id: String,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeFeatures {
    pub timers: bool,
    /// A submachine uses time events, itself or through its own submachines.
    pub submachine_timers: bool,
    /// `timers` or `submachineTimers`: the machine needs a clock.
    pub any_timers: bool,
    pub activities: bool,
    pub history: bool,
    pub deferral: bool,
    pub choices: bool,
    pub joins: bool,
    pub terminate: bool,
    pub submachines: bool,
    pub entry_points: bool,
    pub exit_points: bool,
    pub invariants: bool,
    pub preconditions: bool,
    pub postconditions: bool,
    /// Any constraint checked at run time (invariants, pre/postconditions).
    pub constraints: bool,
}

/// What a template prints. See docs/CODEGEN.md for the meaning of each kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Step {
    Call { action: String },
    StartActivity { activity: String },
    StopActivity { activity: String },
    StartTimer { timer: String, timer_index: usize, ms: Num },
    CancelTimer { timer: String, timer_index: usize },
    Exit { state: String, state_index: usize },
    ExitRegion { region: String, region_index: usize },
    Enter { state: String, state_index: usize },
    EnterRegion { region: String, region_index: usize },
    RestoreHistory { region: String, region_index: usize, deep: bool, default_steps: Vec<Step> },
    Choice { label: String, branches: Vec<ChoiceBranch>, has_else: bool, region: String, region_index: usize },
    StartSubmachine { state: String, state_index: usize, point: Option<String> },
    StopSubmachine { state: String, state_index: usize },
    ExitMachine { point: String },
    Terminate,
    Check { constraint: &'static str, element: String, condition: Condition },
    Transition { id: String, label: String },
}

impl Step {
    pub fn kind(&self) -> &'static str {
        match self {
            Step::Call { .. } => "call",
            Step::StartActivity { .. } => "startActivity",
            Step::StopActivity { .. } => "stopActivity",
            Step::StartTimer { .. } => "startTimer",
            Step::CancelTimer { .. } => "cancelTimer",
            Step::Exit { .. } => "exit",
            Step::ExitRegion { .. } => "exitRegion",
            Step::Enter { .. } => "enter",
            Step::EnterRegion { .. } => "enterRegion",
            Step::RestoreHistory { .. } => "restoreHistory",
            Step::Choice { .. } => "choice",
            Step::StartSubmachine { .. } => "startSubmachine",
            Step::StopSubmachine { .. } => "stopSubmachine",
            Step::ExitMachine { .. } => "exitMachine",
            Step::Terminate => "terminate",
            Step::Check { .. } => "check",
            Step::Transition { .. } => "transition",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChoiceBranch {
    /// None for `else` (always last) or an unguarded branch.
    pub guard: Option<Condition>,
    pub is_else: bool,
    pub steps: Vec<Step>,
    /// Pseudostates the branch goes through after the choice, in the order reached.
    pub via: Vec<CodeVia>,
}

/// A choice step; `has_else` tells whether the last branch is always taken.
fn choice(label: String, branches: Vec<ChoiceBranch>, region: Ref) -> Step {
    let has_else = branches.last().is_some_and(|b| b.is_else || b.guard.is_none());
    Step::Choice { label, branches, has_else, region: region.name, region_index: region.index }
}

pub struct Options {
    /// Code models of the machines referenced by submachine states, keyed by href as written in the model.
    pub submachines: HashMap<String, Rc<CodeModel>>,
    /// Path of the `.fsm` file, shown in generated comments.
    pub source_file: String,
}

/// A model the code generator can't handle.
pub struct CodeGenError {
    pub message: String,
    /// xmi:id of the element it is about.
    pub id: String,
}

impl CodeGenError {
    fn new(id: &str, message: String) -> CodeGenError {
        CodeGenError { message, id: id.to_string() }
    }
}

pub fn to_code_model(model: &FsmModel, options: &Options) -> Result<CodeModel, CodeGenError> {
    Builder::new(model, options).build()
}

// ------------------------------------------------------------------ helpers

/// A way through a compound transition: taken when `guard` holds, doing `steps`, through the pseudostates `via`.
#[derive(Clone)]
struct Alt {
    guard: Option<Condition>,
    steps: Vec<Step>,
    via: Vec<CodeVia>,
}

fn one(steps: Vec<Step>) -> Vec<Alt> {
    vec![Alt { guard: None, steps, via: Vec::new() }]
}

/// The pseudostates of `a`, then those of `b` not reached already.
fn join_via(a: &[CodeVia], b: &[CodeVia]) -> Vec<CodeVia> {
    let mut out = a.to_vec();
    out.extend(b.iter().filter(|v| !a.iter().any(|x| x.id == v.id)).cloned());
    out
}

/// The pseudostates of all of `ways`, in order.
fn union_via<'v>(ways: impl IntoIterator<Item = &'v Vec<CodeVia>>) -> Vec<CodeVia> {
    ways.into_iter().fold(Vec::new(), |all, via| join_via(&all, via))
}

fn and(a: Option<Condition>, b: Option<Condition>) -> Option<Condition> {
    let (a, b) = match (a, b) {
        (None, b) => return b,
        (a, None) => return a,
        (Some(a), Some(b)) => (a, b),
    };
    let mut args = Vec::new();
    for c in [a, b] {
        match c {
            Condition::And { args: inner } => args.extend(inner),
            c => args.push(c),
        }
    }
    Some(Condition::And { args })
}

fn or(mut list: Vec<Condition>) -> Option<Condition> {
    match list.len() {
        0 => None,
        1 => list.pop(),
        _ => Some(Condition::Or {
            args: list
                .into_iter()
                .flat_map(|c| match c {
                    Condition::Or { args } => args,
                    c => vec![c],
                })
                .collect(),
        }),
    }
}

fn not(c: Condition) -> Condition {
    match c {
        Condition::Not { arg } => *arg,
        c => Condition::Not { arg: Box::new(c) },
    }
}

/// The kinds of pseudostates in a cycle, for its error: `Junctions`, `Junctions and choices`, ...
fn cycle_kinds(cycle: impl Iterator<Item = VertexType> + Clone) -> String {
    let kinds: [(&str, &[VertexType]); 6] = [
        ("junctions", &[VertexType::Junction]),
        ("choices", &[VertexType::Choice]),
        ("history pseudostates", &[VertexType::ShallowHistory, VertexType::DeepHistory]),
        ("entry points", &[VertexType::EntryPoint]),
        ("exit points", &[VertexType::ExitPoint]),
        ("forks", &[VertexType::Fork]),
    ];
    let found: Vec<&str> =
        kinds.iter().filter(|(_, types)| cycle.clone().any(|t| types.contains(&t))).map(|(name, _)| *name).collect();
    let text = match found.split_last() {
        None => "pseudostates".to_string(),
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    };
    let mut chars = text.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

/// Every combination of a way through `a` followed by a way through `b`.
fn seq(a: Vec<Alt>, b: Vec<Alt>) -> Vec<Alt> {
    let mut out = Vec::new();
    for x in &a {
        for y in &b {
            let mut steps = x.steps.clone();
            steps.extend(y.steps.iter().cloned());
            out.push(Alt { guard: and(x.guard.clone(), y.guard.clone()), steps, via: join_via(&x.via, &y.via) });
        }
    }
    out
}

/// Identifier made of letters, digits and `_`, not starting with a digit.
pub fn ident(text: &str, fallback: &str) -> String {
    let mut s = String::new();
    let mut in_run = false;
    for c in js_trim(text).chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            s.push(c);
            in_run = false;
        } else if !in_run {
            s.push('_');
            in_run = true;
        }
    }
    let mut s = s.trim_matches('_').to_string();
    if s.is_empty() {
        s = fallback.to_string();
    }
    if s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

/// `text` with every run of two or more `_` made one.
fn collapse_underscores(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if !(c == '_' && out.ends_with('_')) {
            out.push(c);
        }
    }
    out
}

/// Makes names unique by appending _2, _3, ...
#[derive(Default)]
struct Names {
    used: HashSet<String>,
}

impl Names {
    fn take(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut i = 2;
        while self.used.contains(&name) {
            name = format!("{base}_{i}");
            i += 1;
        }
        self.used.insert(name.clone());
        name
    }
}

/// A set that keeps the order of insertion.
#[derive(Default)]
struct OrderedSet {
    list: Vec<String>,
    seen: HashSet<String>,
}

impl OrderedSet {
    fn add(&mut self, s: &str) {
        if self.seen.insert(s.to_string()) {
            self.list.push(s.to_string());
        }
    }
}

#[derive(Clone, Copy)]
enum SetKind {
    Actions,
    Activities,
}

fn type_name(t: VertexType) -> &'static str {
    match t {
        VertexType::Final => "final",
        VertexType::Choice => "choice",
        VertexType::Junction => "junction",
        VertexType::Fork => "fork",
        VertexType::Join => "join",
        VertexType::Terminate => "terminate",
        VertexType::ShallowHistory => "history",
        VertexType::DeepHistory => "deepHistory",
        VertexType::EntryPoint => "entry",
        VertexType::ExitPoint => "exit",
        VertexType::ConnectionPointRef => "point",
        VertexType::Initial => "initial",
        other => other.as_str(),
    }
}

fn is_else(t: &Transition) -> bool {
    js_trim(&t.guard) == "else"
}

/// A compiled transition (index in `transitions`), taken when the guard holds.
type Fireable = (usize, Option<Condition>);

struct RegionEntry<'a> {
    id: &'a str,
    name: &'a str,
    owner: Option<usize>,
    pos: usize,
}

// ------------------------------------------------------------------ builder

struct Builder<'a> {
    model: &'a FsmModel,
    options: &'a Options,
    ix: ModelIndex<'a>,
    top_region_ids: Vec<&'a str>,
    state_list: Vec<usize>,
    region_list: Vec<RegionEntry<'a>>,
    state_index: HashMap<&'a str, usize>,
    region_index: HashMap<&'a str, usize>,
    state_names: Vec<String>,
    region_names: Vec<String>,

    signals: Vec<CodeSignal>,
    events: Vec<CodeEvent>,
    timers: Vec<CodeTimer>,
    timer_by_key: HashMap<String, usize>,
    actions: OrderedSet,
    conditions: OrderedSet,
    activities: OrderedSet,
    transitions: Vec<CodeTransition>,
    transition_names: Names,
    fire_cache: HashMap<String, Vec<Fireable>>,
    features: CodeFeatures,
    /// Pseudostates on the way being compiled, to report cycles.
    pseudostate_path: Vec<usize>,
}

impl<'a> Builder<'a> {
    fn new(model: &'a FsmModel, options: &'a Options) -> Builder<'a> {
        Builder {
            model,
            options,
            ix: ModelIndex::new(model),
            top_region_ids: model.regions.iter().map(|r| r.id.as_str()).collect(),
            state_list: Vec::new(),
            region_list: Vec::new(),
            state_index: HashMap::new(),
            region_index: HashMap::new(),
            state_names: Vec::new(),
            region_names: Vec::new(),
            signals: Vec::new(),
            events: Vec::new(),
            timers: Vec::new(),
            timer_by_key: HashMap::new(),
            actions: OrderedSet::default(),
            conditions: OrderedSet::default(),
            activities: OrderedSet::default(),
            transitions: Vec::new(),
            transition_names: Names::default(),
            fire_cache: HashMap::new(),
            features: CodeFeatures::default(),
            pseudostate_path: Vec::new(),
        }
    }

    fn walk(&mut self, region: &'a str, name: &'a str, owner: Option<usize>, pos: usize) {
        self.region_index.insert(region, self.region_list.len());
        self.region_list.push(RegionEntry { id: region, name, owner, pos });
        for v in self.ix.children_of(region) {
            let vx = self.ix.v(v);
            if !matches!(vx.vtype, VertexType::State | VertexType::Final) || self.ix.is_border(v) {
                continue;
            }
            self.state_index.insert(&vx.id, self.state_list.len());
            self.state_list.push(v);
            for (i, r) in vx.regions.iter().enumerate() {
                self.walk(&r.id, &r.name, Some(v), i);
            }
        }
    }

    fn build(mut self) -> Result<CodeModel, CodeGenError> {
        let m = self.model;
        // Structure, depth first.
        for (i, r) in m.regions.iter().enumerate() {
            self.walk(&r.id, &r.name, None, i);
        }
        self.name_states();
        self.name_regions();

        // Signals: named events first (own, then submachines'), then timers.
        for t in &m.transitions {
            for trig in &t.triggers {
                if expr::time_trigger_ms(trig).is_none() {
                    self.event(js_trim(trig), true);
                }
            }
        }
        for v in &m.vertices {
            for d in &v.deferrable {
                self.event(js_trim(d), true);
            }
        }
        let subs = self.submachine_models()?;
        for (_, sub) in &subs {
            for e in &sub.events {
                self.event(&e.name, false);
            }
        }
        for t in &m.transitions {
            let Some(s) = self.ix.vertex(&t.source) else { continue };
            if !self.is_state(s) {
                continue;
            }
            for trig in &t.triggers {
                if expr::time_trigger_ms(trig).is_some() {
                    self.timer(s, trig)?;
                }
            }
        }
        self.signals = self
            .events
            .iter()
            .map(|e| CodeSignal { name: e.name.clone(), index: 0, kind: "event" })
            .chain(self.timers.iter().map(|t| CodeSignal { name: t.name.clone(), index: 0, kind: "timer" }))
            .collect();
        for (i, s) in self.signals.iter_mut().enumerate() {
            s.index = i;
        }
        for (i, e) in self.events.iter_mut().enumerate() {
            e.signal_index = i as i64;
        }
        let event_count = self.events.len();
        for (i, t) in self.timers.iter_mut().enumerate() {
            t.signal_index = (event_count + i) as i64;
        }

        let mut states = Vec::new();
        for i in 0..self.state_list.len() {
            states.push(Rc::new(self.state(self.state_list[i], i)?));
        }
        let mut regions = Vec::new();
        for i in 0..self.region_list.len() {
            regions.push(self.region(i)?);
        }
        let mut entry_points = Vec::new();
        for v in self.machine_points(VertexType::EntryPoint) {
            entry_points.push(self.entry_point(v)?);
        }
        let exit_points: Vec<CodeExitPoint> = self
            .machine_points(VertexType::ExitPoint)
            .into_iter()
            .map(|v| CodeExitPoint { name: ident(&self.ix.v(v).name, "exit"), id: self.ix.v(v).id.clone() })
            .collect();
        self.features.entry_points = !entry_points.is_empty();
        self.features.exit_points = !exit_points.is_empty();
        self.features.submachines = !subs.is_empty();
        self.features.timers = !self.timers.is_empty();
        let uses_timers = |sub: &CodeModel| sub.features.timers || sub.features.submachine_timers;
        self.features.submachine_timers = subs.iter().any(|(_, s)| uses_timers(s));
        self.features.any_timers = self.features.timers || self.features.submachine_timers;
        self.features.activities = !self.activities.list.is_empty();
        self.features.constraints =
            self.features.invariants || self.features.preconditions || self.features.postconditions;

        let submachines = subs
            .iter()
            .map(|(href, sub)| CodeSubmachine {
                name: sub.machine.name.clone(),
                href: href.clone(),
                events: sub.events.iter().map(|e| e.name.clone()).collect(),
                entry_points: sub.entry_points.iter().map(|p| p.name.clone()).collect(),
                exit_points: sub.exit_points.iter().map(|p| p.name.clone()).collect(),
                timers: uses_timers(sub),
                states: states
                    .iter()
                    .filter(|s| s.submachine.as_ref().is_some_and(|x| &x.href == href))
                    .map(|s| Ref { name: s.name.clone(), index: s.index })
                    .collect(),
            })
            .collect();

        let mut start = Vec::new();
        for r in self.top_region_ids.clone() {
            start.push(self.enter_region_step(r)?);
        }
        let transitions = std::mem::take(&mut self.transitions);
        let mut code = CodeModel {
            machine: CodeMachine {
                name: ident(&m.name, "StateMachine"),
                display_name: m.name.clone(),
                id: m.id.clone(),
                kind: m.kind.as_str(),
                protocol: m.kind == MachineKind::Protocol,
                context: m.context.clone(),
                documentation: m.documentation.clone(),
                source_file: self.options.source_file.clone(),
            },
            top_regions: regions.iter().filter(|r| r.top).cloned().collect(),
            regions,
            invariants: states.iter().filter(|s| s.invariant.is_some()).cloned().collect(),
            states,
            events: std::mem::take(&mut self.events),
            timers: std::mem::take(&mut self.timers),
            signals: std::mem::take(&mut self.signals),
            actions: std::mem::take(&mut self.actions.list),
            conditions: std::mem::take(&mut self.conditions.list),
            activities: std::mem::take(&mut self.activities.list),
            transitions,
            submachines,
            entry_points,
            exit_points,
            start,
            features: self.features.clone(),
            tables: CodeTables::default(),
        };
        code.tables = to_tables(&code);
        Ok(code)
    }

    // -------------------------------------------------------------- names

    fn name_states(&mut self) {
        let base = |v: usize| {
            let v = self.ix.v(v);
            ident(&v.name, if v.vtype == VertexType::Final { "Final" } else { "State" })
        };
        let mut count: HashMap<String, usize> = HashMap::new();
        for &v in &self.state_list {
            *count.entry(base(v)).or_default() += 1;
        }
        let mut names = Names::default();
        for &v in &self.state_list {
            let mut name = base(v);
            if count[&name] > 1 {
                // Qualify duplicates with their enclosing states: On_Final, Playing_Idle.
                let mut path: Vec<String> =
                    self.ix.ancestors(v).iter().rev().map(|&a| ident(&self.ix.v(a).name, "State")).collect();
                if !path.is_empty() {
                    path.push(name);
                    name = path.join("_");
                }
            }
            self.state_names.push(names.take(&name));
        }
    }

    fn name_regions(&mut self) {
        let mut names = Names::default();
        for r in &self.region_list {
            let name = match r.owner {
                None => {
                    if self.model.regions.len() == 1 {
                        "Root".to_string()
                    } else {
                        format!("Root_{}", ident(r.name, &format!("region{}", r.pos + 1)))
                    }
                }
                Some(o) => {
                    let ov = self.ix.v(o);
                    let owner = &self.state_names[self.state_index[ov.id.as_str()]];
                    let suffix = if !r.name.is_empty() {
                        ident(r.name, "x")
                    } else if ov.regions.len() == 1 {
                        "region".to_string()
                    } else {
                        format!("region{}", r.pos + 1)
                    };
                    format!("{owner}_{suffix}")
                }
            };
            self.region_names.push(names.take(&name));
        }
    }

    fn state_ref(&self, v: usize) -> Result<Ref, CodeGenError> {
        match self.state_index.get(self.ix.v(v).id.as_str()) {
            Some(&index) => Ok(Ref { name: self.state_names[index].clone(), index }),
            None => Err(CodeGenError::new(
                &self.ix.v(v).id,
                format!("{} is not in a region of the state machine.", self.label(Some(v))),
            )),
        }
    }

    fn region_ref(&self, id: &str) -> Result<Ref, CodeGenError> {
        match self.region_index.get(id) {
            Some(&index) => Ok(Ref { name: self.region_names[index].clone(), index }),
            None => Err(CodeGenError::new(id, format!("Unknown region '{id}'."))),
        }
    }

    fn submachine(&self, href: Option<&str>) -> Option<&'a Rc<CodeModel>> {
        self.options.submachines.get(href.unwrap_or(""))
    }

    fn label(&self, v: Option<usize>) -> String {
        let Some(v) = v else { return "?".to_string() };
        let vx = self.ix.v(v);
        if let Some(&i) = self.state_index.get(vx.id.as_str()) {
            return self.state_names[i].clone();
        }
        if vx.vtype == VertexType::ConnectionPointRef {
            let owner = self.ix.vertex(&vx.parent);
            let sub = self.submachine(owner.and_then(|o| self.ix.v(o).submachine.as_deref()));
            let point = sub.and_then(|s| {
                s.entry_points
                    .iter()
                    .map(|p| (&p.id, &p.name))
                    .chain(s.exit_points.iter().map(|p| (&p.id, &p.name)))
                    .find(|(id, _)| **id == vx.point_ref)
                    .map(|(_, name)| name)
            });
            if let (Some(o), Some(point)) = (owner, point) {
                return format!("{}.{point}", self.label(Some(o)));
            }
        }
        if vx.name.is_empty() { type_name(vx.vtype).to_string() } else { vx.name.clone() }
    }

    // -------------------------------------------------------------- signals

    /// Registers a named event; all of them are known before the states are built.
    fn event(&mut self, name: &str, own: bool) -> usize {
        if let Some(i) = self.events.iter().position(|e| e.name == name) {
            self.events[i].own |= own;
            return i;
        }
        self.events.push(CodeEvent { name: name.to_string(), index: self.events.len(), signal_index: -1, own });
        self.events.len() - 1
    }

    fn timer(&mut self, state: usize, trigger: &str) -> Result<usize, CodeGenError> {
        let text: String = trigger.chars().filter(|c| !c.is_whitespace()).collect();
        let key = format!("{}|{text}", self.ix.v(state).id);
        if let Some(&known) = self.timer_by_key.get(&key) {
            return Ok(known);
        }
        let r = self.state_ref(state)?;
        let inner = text.strip_prefix("after(").and_then(|s| s.strip_suffix(')')).unwrap_or(&text);
        let mut name = ident(&format!("{}_after_{inner}", r.name), "x");
        while self.events.iter().any(|e| e.name == name) || self.timers.iter().any(|t| t.name == name) {
            name.push('_');
        }
        let timer = CodeTimer {
            name,
            index: self.timers.len(),
            signal_index: -1,
            ms: Num(expr::time_trigger_ms(&text).unwrap_or(0.0)),
            trigger: text,
            state: r.name,
            state_index: r.index,
        };
        self.timer_by_key.insert(key, timer.index);
        self.timers.push(timer);
        Ok(self.timers.len() - 1)
    }

    fn signal_of(&mut self, state: usize, trigger: &str) -> Result<CodeSignal, CodeGenError> {
        let index = if expr::time_trigger_ms(trigger).is_some() {
            let t = self.timer(state, trigger)?;
            self.timers[t].signal_index
        } else {
            let e = self.event(js_trim(trigger), true);
            self.events[e].signal_index
        };
        Ok(self.signals[index as usize].clone())
    }

    // -------------------------------------------------------------- text

    /// The calls of a behavior of the element `id`.
    fn calls(&mut self, text: &str, what: &str, id: &str, into: SetKind) -> Result<Vec<String>, CodeGenError> {
        let calls = expr::parse_actions(text).map_err(|e| CodeGenError::new(id, format!("{what}: {e}")))?;
        let set = match into {
            SetKind::Actions => &mut self.actions,
            SetKind::Activities => &mut self.activities,
        };
        for c in &calls {
            set.add(c);
        }
        Ok(calls)
    }

    /// A constraint of the element `id`.
    fn condition(&mut self, text: &str, what: &str, id: &str) -> Result<Option<Condition>, CodeGenError> {
        let c = expr::parse_condition(text).map_err(|e| CodeGenError::new(id, format!("{what}: {e}")))?;
        fn visit(c: &Condition, set: &mut OrderedSet) {
            match c {
                Condition::Call { name } => set.add(name),
                Condition::Not { arg } => visit(arg, set),
                Condition::And { args } | Condition::Or { args } => args.iter().for_each(|a| visit(a, set)),
            }
        }
        if let Some(c) = &c {
            visit(c, &mut self.conditions);
        }
        Ok(c)
    }

    fn guard_of(&mut self, t: &Transition) -> Result<Option<Condition>, CodeGenError> {
        if is_else(t) {
            return Ok(None);
        }
        let what = format!("Guard of {}", self.transition_label(t));
        self.condition(&t.guard, &what, &t.id)
    }

    /// Taking the transition (one segment of a compound transition): its `transition` step, for tracing, then its
    /// effect.
    fn effect(&mut self, t: &Transition) -> Result<Vec<Step>, CodeGenError> {
        let label = self.transition_label(t);
        let calls = self.calls(&t.effect, &format!("Effect of {label}"), &t.id, SetKind::Actions)?;
        let mut steps = vec![Step::Transition { id: t.id.clone(), label }];
        steps.extend(calls.into_iter().map(|action| Step::Call { action }));
        Ok(steps)
    }

    fn transition_label(&self, t: &Transition) -> String {
        let s = self.label(self.ix.vertex(&t.source));
        let g = self.label(self.ix.vertex(&t.target));
        if t.triggers.is_empty() { format!("{s} -> {g}") } else { format!("{s} -> {g} ({})", t.triggers.join(", ")) }
    }

    // -------------------------------------------------------------- geometry of the hierarchy

    fn vertex(&self, id: &str) -> Result<usize, CodeGenError> {
        self.ix.vertex(id).ok_or_else(|| CodeGenError::new(id, format!("Unknown vertex '{id}'.")))
    }

    /// The region a vertex sits in; the owner's region for entry/exit points and connection point references.
    fn region_of(&self, v: usize) -> Result<&'a str, CodeGenError> {
        if self.ix.is_border(v) {
            return Ok(&self.ix.v(self.vertex(&self.ix.v(v).parent)?).parent);
        }
        Ok(&self.ix.v(v).parent)
    }

    /// Regions containing `v`, outermost first.
    fn chain(&self, v: usize) -> Result<Vec<&'a str>, CodeGenError> {
        let mut out = Vec::new();
        let mut r = Some(self.region_of(v)?);
        let mut seen = HashSet::new();
        while let Some(region) = r.filter(|r| !r.is_empty() && !seen.contains(r)) {
            seen.insert(region);
            out.insert(0, region);
            r = self.ix.region_owner(region).flatten().map(|o| self.ix.v(o).parent.as_str());
        }
        Ok(out)
    }

    /// Innermost region containing all of `vs`; None when they are in different top-level regions.
    fn common_region(&self, vs: &[usize]) -> Result<Option<&'a str>, CodeGenError> {
        let chains = vs.iter().map(|&v| self.chain(v)).collect::<Result<Vec<_>, _>>()?;
        if chains.is_empty() {
            return Ok(None);
        }
        let mut common = None;
        let mut i = 0;
        while chains.iter().all(|c| i < c.len() && c[i] == chains[0][i]) {
            common = Some(chains[0][i]);
            i += 1;
        }
        Ok(common)
    }

    /// The vertex directly in `region` (or at the top level when None) on the way to `v`.
    fn child_on_path(&self, v: usize, region: Option<&str>) -> Result<usize, CodeGenError> {
        let in_region = |x: usize| -> Result<bool, CodeGenError> {
            let r = self.region_of(x)?;
            Ok(match region {
                None => self.top_region_ids.contains(&r),
                Some(region) => r == region,
            })
        };
        if in_region(v)? {
            return Ok(v);
        }
        for a in self.ix.ancestors(v) {
            if in_region(a)? {
                return Ok(a);
            }
        }
        Err(CodeGenError::new(&self.ix.v(v).id, format!("{} is not inside the expected region.", self.label(Some(v)))))
    }

    fn is_state(&self, v: usize) -> bool {
        self.state_index.contains_key(self.ix.v(v).id.as_str())
    }

    // -------------------------------------------------------------- steps

    fn enter_step(&self, v: usize) -> Result<Step, CodeGenError> {
        let r = self.state_ref(v)?;
        Ok(Step::Enter { state: r.name, state_index: r.index })
    }

    fn exit_step(&self, v: usize) -> Result<Step, CodeGenError> {
        let r = self.state_ref(v)?;
        Ok(Step::Exit { state: r.name, state_index: r.index })
    }

    fn enter_region_step(&self, id: &str) -> Result<Step, CodeGenError> {
        let r = self.region_ref(id)?;
        Ok(Step::EnterRegion { region: r.name, region_index: r.index })
    }

    fn exit_region_step(&self, id: &str) -> Result<Step, CodeGenError> {
        let r = self.region_ref(id)?;
        Ok(Step::ExitRegion { region: r.name, region_index: r.index })
    }

    /// `alts` as steps: a choice evaluated in `region` when there are several ways.
    fn alts_to_steps(&mut self, mut alts: Vec<Alt>, label: &str, region: &str) -> Result<Vec<Step>, CodeGenError> {
        if alts.len() == 1 && alts[0].guard.is_none() {
            return Ok(alts.pop().unwrap().steps);
        }
        self.features.choices = true;
        let branches = alts
            .into_iter()
            .map(|a| ChoiceBranch { guard: a.guard, is_else: false, steps: a.steps, via: a.via })
            .collect();
        Ok(vec![choice(label.to_string(), branches, self.region_ref(region)?)])
    }

    /// The pseudostate `v` as an element of `via`.
    fn via_of(&self, v: usize) -> CodeVia {
        let vx = self.ix.v(v);
        let on_border = vx.vtype != VertexType::ConnectionPointRef && self.ix.is_border(v);
        let name = if on_border {
            format!("{}.{}", self.label(self.ix.vertex(&vx.parent)), self.label(Some(v)))
        } else {
            self.label(Some(v))
        };
        CodeVia { kind: vx.vtype.as_str(), name, id: vx.id.clone() }
    }

    /// Exits for a transition from `pos` whose domain is `domain`.
    fn exits_from(&self, pos: usize, domain: Option<&str>) -> Result<Vec<Step>, CodeGenError> {
        let Some(domain) = domain else {
            return self.top_region_ids.iter().map(|r| self.exit_region_step(r)).collect();
        };
        let root = self.child_on_path(pos, Some(domain))?;
        Ok(if self.is_state(root) { vec![self.exit_step(root)?] } else { Vec::new() })
    }

    /// One segment of a compound transition, from `pos` (a state or a pseudostate already reached).
    fn segment(&mut self, pos: usize, t: &'a Transition) -> Result<Vec<Alt>, CodeGenError> {
        let target = self.vertex(&t.target)?;
        if t.kind == TransitionKind::Internal {
            return Ok(one(self.effect(t)?));
        }
        let pv = self.ix.v(pos);
        if t.kind == TransitionKind::Local
            && self.is_state(pos)
            && !pv.regions.is_empty()
            && (target == pos || self.ix.is_inside(target, &pv.id))
        {
            if target == pos {
                let mut steps =
                    pv.regions.iter().map(|r| self.exit_region_step(&r.id)).collect::<Result<Vec<_>, _>>()?;
                steps.extend(self.effect(t)?);
                for r in &pv.regions {
                    steps.push(self.enter_region_step(&r.id)?);
                }
                return Ok(one(steps));
            }
            let chain = self.chain(target)?;
            let region = &pv
                .regions
                .iter()
                .find(|r| chain.contains(&r.id.as_str()))
                .ok_or_else(|| CodeGenError::new(&t.id, "Unknown region.".to_string()))?
                .id;
            let mut steps = vec![self.exit_region_step(region)?];
            steps.extend(self.effect(t)?);
            let entered = self.enter_region(region, &[target])?;
            return Ok(seq(one(steps), entered));
        }
        let domain = self.common_region(&[pos, target])?;
        let mut steps = self.exits_from(pos, domain)?;
        steps.extend(self.effect(t)?);
        let entered = self.enter_domain(domain, &[target])?;
        Ok(seq(one(steps), entered))
    }

    fn enter_domain(&mut self, domain: Option<&str>, targets: &[usize]) -> Result<Vec<Alt>, CodeGenError> {
        if let Some(domain) = domain {
            return self.enter_region(domain, targets);
        }
        let mut alts = one(Vec::new());
        for r in self.top_region_ids.clone() {
            let mut inside = Vec::new();
            for &t in targets {
                if self.chain(t)?.first() == Some(&r) {
                    inside.push(t);
                }
            }
            let entered = self.enter_region(r, &inside)?;
            alts = seq(alts, entered);
        }
        Ok(alts)
    }

    /// Enters `region` (currently inactive) so that every target gets entered; default entry when there is none.
    fn enter_region(&mut self, region: &str, targets: &[usize]) -> Result<Vec<Alt>, CodeGenError> {
        if targets.is_empty() {
            return Ok(one(vec![self.enter_region_step(region)?]));
        }
        for &t in targets {
            if self.region_of(t)? == region {
                return self.reach(t, region);
            }
        }
        let state = self.child_on_path(targets[0], Some(region))?;
        let mut inside = Vec::new();
        for &t in targets {
            if self.child_on_path(t, Some(region))? == state {
                inside.push(t);
            }
        }
        let mut alts = one(vec![self.enter_step(state)?]);
        for r in &self.ix.v(state).regions {
            let mut targets = Vec::new();
            for &t in &inside {
                if self.chain(t)?.contains(&r.id.as_str()) {
                    targets.push(t);
                }
            }
            let entered = self.enter_region(&r.id, &targets)?;
            alts = seq(alts, entered);
        }
        Ok(alts)
    }

    /// Arriving at `v`, which sits directly in `region`: every way on from it goes through it.
    fn reach(&mut self, v: usize, region: &str) -> Result<Vec<Alt>, CodeGenError> {
        let vx = self.ix.v(v);
        let machine_point =
            matches!(vx.vtype, VertexType::EntryPoint | VertexType::ExitPoint) && !self.ix.has_vertex(&vx.parent);
        if matches!(vx.vtype, VertexType::State | VertexType::Final | VertexType::Initial | VertexType::Terminate)
            || machine_point
        {
            return self.arrive(v, region);
        }
        // Reaching a pseudostate again on its own way would never end. Reported at that pseudostate.
        if let Some(cycle) = self.pseudostate_path.iter().position(|&x| self.ix.v(x).id == vx.id) {
            let kinds = cycle_kinds(self.pseudostate_path[cycle..].iter().map(|&x| self.ix.v(x).vtype));
            return Err(CodeGenError::new(&vx.id, format!("{kinds} form a cycle.")));
        }
        self.pseudostate_path.push(v);
        let own = [self.via_of(v)];
        let alts = self.arrive(v, region);
        self.pseudostate_path.pop();
        Ok(alts?.into_iter().map(|a| Alt { via: join_via(&own, &a.via), ..a }).collect())
    }

    /// The ways on from `v` (see reach()).
    fn arrive(&mut self, v: usize, region: &str) -> Result<Vec<Alt>, CodeGenError> {
        let vx = self.ix.v(v);
        let outs = self.ix.outgoing(&vx.id);
        match vx.vtype {
            VertexType::State => {
                let mut alts = one(vec![self.enter_step(v)?]);
                for r in &vx.regions {
                    alts = seq(alts, one(vec![self.enter_region_step(&r.id)?]));
                }
                if vx.submachine.is_some() {
                    alts = seq(alts, one(vec![self.start_submachine(v, None)?]));
                }
                Ok(alts)
            }
            VertexType::Final => Ok(one(vec![self.enter_step(v)?])),
            VertexType::EntryPoint => {
                let Some(owner) = self.ix.vertex(&vx.parent) else {
                    return Ok(one(Vec::new())); // the machine's own entry point: see entry_point()
                };
                let mut steps = vec![self.enter_step(owner)?];
                for t in &outs {
                    steps.extend(self.effect(t)?);
                }
                let mut alts = one(steps);
                let targets = outs.iter().map(|t| self.vertex(&t.target)).collect::<Result<Vec<_>, _>>()?;
                for r in &self.ix.v(owner).regions {
                    let mut inside = Vec::new();
                    for &t in &targets {
                        if self.chain(t)?.contains(&r.id.as_str()) {
                            inside.push(t);
                        }
                    }
                    let entered = self.enter_region(&r.id, &inside)?;
                    alts = seq(alts, entered);
                }
                Ok(alts)
            }
            VertexType::ConnectionPointRef => {
                if vx.point_kind == PointKind::Exit {
                    return Ok(one(Vec::new()));
                }
                let owner = self.vertex(&vx.parent)?;
                let point = self.point_name(owner, v, PointKind::Entry)?;
                Ok(one(vec![self.enter_step(owner)?, self.start_submachine(owner, Some(point))?]))
            }
            VertexType::ExitPoint => {
                if !self.ix.has_vertex(&vx.parent) {
                    return Ok(one(vec![Step::ExitMachine { point: ident(&vx.name, "exit") }]));
                }
                // The owner was exited on the way here; continue with the transitions leaving the point.
                self.branches(v, &outs)
            }
            VertexType::Choice => {
                self.features.choices = true;
                let ordered: Vec<&Transition> =
                    outs.iter().filter(|t| !is_else(t)).chain(outs.iter().filter(|t| is_else(t))).copied().collect();
                let r = self.region_ref(region)?;
                let label = format!("choice in {}", r.name);
                let mut branches = Vec::new();
                for t in ordered {
                    let guard = self.guard_of(t)?;
                    let ways = self.segment(v, t)?;
                    let via = union_via(ways.iter().map(|w| &w.via));
                    let steps = self.alts_to_steps(ways, &label, region)?;
                    branches.push(ChoiceBranch { guard, is_else: is_else(t), steps, via });
                }
                // One way through the choice, going through the pseudostates of all its branches.
                let via = union_via(branches.iter().map(|b| &b.via));
                Ok(vec![Alt { guard: None, steps: vec![choice(label, branches, r)], via }])
            }
            VertexType::Junction => self.branches(v, &outs),
            VertexType::ShallowHistory | VertexType::DeepHistory => {
                self.features.history = true;
                let r = self.region_ref(region)?;
                let (default_steps, via) = match outs.first() {
                    Some(t) => {
                        let ways = self.segment(v, t)?;
                        let via = union_via(ways.iter().map(|w| &w.via));
                        (self.alts_to_steps(ways, "history default", region)?, via)
                    }
                    None => (vec![self.enter_region_step(region)?], Vec::new()),
                };
                let step = Step::RestoreHistory {
                    region: r.name,
                    region_index: r.index,
                    deep: vx.vtype == VertexType::DeepHistory,
                    default_steps,
                };
                Ok(vec![Alt { guard: None, steps: vec![step], via }])
            }
            VertexType::Fork => {
                let mut effects = Vec::new();
                for t in &outs {
                    effects.extend(self.effect(t)?);
                }
                let mut targets = Vec::new();
                for t in &outs {
                    let target = self.vertex(&t.target)?;
                    if target != v {
                        targets.push(target);
                    }
                }
                let entered = self.enter_region(region, &targets)?;
                Ok(seq(one(effects), entered))
            }
            VertexType::Terminate => {
                self.features.terminate = true;
                Ok(one(vec![Step::Terminate]))
            }
            // Joins are compiled as a whole (see join()); initial pseudostates only start regions.
            _ => Ok(one(Vec::new())),
        }
    }

    /// The transitions leaving a static pseudostate (junction, state exit point) as alternative ways.
    fn branches(&mut self, v: usize, outs: &[&'a Transition]) -> Result<Vec<Alt>, CodeGenError> {
        if outs.is_empty() {
            return Ok(one(Vec::new()));
        }
        let guarded: Vec<&Transition> = outs.iter().filter(|t| !is_else(t)).copied().collect();
        let mut guards = Vec::new();
        for t in &guarded {
            guards.push(self.guard_of(t)?);
        }
        let mut out = Vec::new();
        for (i, t) in guarded.iter().enumerate() {
            let segment = self.segment(v, t)?;
            out.extend(seq(vec![Alt { guard: guards[i].clone(), steps: Vec::new(), via: Vec::new() }], segment));
        }
        if let Some(else_t) = outs.iter().find(|t| is_else(t)) {
            let others = or(guards.into_iter().flatten().collect());
            let segment = self.segment(v, else_t)?;
            out.extend(seq(vec![Alt { guard: others.map(not), steps: Vec::new(), via: Vec::new() }], segment));
        }
        Ok(out)
    }

    fn start_submachine(&self, state: usize, point: Option<String>) -> Result<Step, CodeGenError> {
        let r = self.state_ref(state)?;
        Ok(Step::StartSubmachine { state: r.name, state_index: r.index, point })
    }

    // -------------------------------------------------------------- submachines

    fn submachine_models(&self) -> Result<Vec<(String, Rc<CodeModel>)>, CodeGenError> {
        let mut out: Vec<(String, Rc<CodeModel>)> = Vec::new();
        for &v in &self.state_list {
            let Some(href) = &self.ix.v(v).submachine else { continue };
            if out.iter().any(|(h, _)| h == href) {
                continue;
            }
            match self.options.submachines.get(href) {
                Some(sub) => out.push((href.clone(), sub.clone())),
                None => {
                    return Err(CodeGenError::new(
                        &self.ix.v(v).id,
                        format!("The state machine '{href}' used by {} was not provided.", self.label(Some(v))),
                    ));
                }
            }
        }
        Ok(out)
    }

    fn point_name(&self, owner: usize, r: usize, kind: PointKind) -> Result<String, CodeGenError> {
        let ov = self.ix.v(owner);
        let rv = self.ix.v(r);
        let sub = self.submachine(ov.submachine.as_deref());
        let found = sub.and_then(|s| match kind {
            PointKind::Entry => s.entry_points.iter().find(|p| p.id == rv.point_ref).map(|p| p.name.clone()),
            PointKind::Exit => s.exit_points.iter().find(|p| p.id == rv.point_ref).map(|p| p.name.clone()),
        });
        found.ok_or_else(|| {
            CodeGenError::new(
                &rv.id,
                format!(
                    "{} references {} point '{}', which '{}' does not have.",
                    self.label(Some(owner)),
                    kind.as_str(),
                    rv.point_ref,
                    ov.submachine.as_deref().unwrap_or("undefined")
                ),
            )
        })
    }

    // -------------------------------------------------------------- transitions

    /// The fireable transitions for `t` from `pos`: one per way through its junctions.
    fn fireables(
        &mut self,
        key: String,
        t: &'a Transition,
        pos: usize,
        prefix: Vec<Step>,
        kind: &'static str,
        label: &str,
    ) -> Result<Vec<Fireable>, CodeGenError> {
        if let Some(cached) = self.fire_cache.get(&key) {
            return Ok(cached.clone());
        }
        let own = if self.is_state(pos) { Vec::new() } else { vec![self.via_of(pos)] };
        let segment = self.segment(pos, t)?;
        let mut alts = seq(one(prefix), segment);
        let post = if self.model.kind == MachineKind::Protocol {
            self.condition(&t.postcondition, &format!("Postcondition of {label}"), &t.id)?
        } else {
            None
        };
        if let Some(post) = post {
            self.features.postconditions = true;
            alts = seq(
                alts,
                one(vec![Step::Check { constraint: "postcondition", element: label.to_string(), condition: post }]),
            );
        }
        let source = self.owner_state_of(pos)?;
        let target = self.label(self.ix.vertex(&t.target));
        let trig = match t.triggers.first() {
            Some(trig) => trig.clone(),
            None if kind == "exitPoint" => self.label(Some(pos)).rsplit('.').next().unwrap_or("").to_string(),
            None => "done".to_string(),
        };
        let base = collapse_underscores(&ident(&format!("{}_{trig}_{target}", source.name), "x"));
        let count = alts.len();
        let mut list = Vec::new();
        for (i, a) in alts.into_iter().enumerate() {
            let name = self.transition_names.take(&if count > 1 { format!("{base}_{}", i + 1) } else { base.clone() });
            let index = self.transitions.len();
            self.transitions.push(CodeTransition {
                index,
                id: t.id.clone(),
                name,
                label: label.to_string(),
                via: join_via(&own, &a.via),
                kind,
                source_index: source.index,
                source: source.clone(),
                steps: a.steps,
            });
            list.push((index, a.guard));
        }
        self.fire_cache.insert(key, list.clone());
        Ok(list)
    }

    fn owner_state_of(&self, pos: usize) -> Result<Ref, CodeGenError> {
        if self.is_state(pos) {
            return self.state_ref(pos);
        }
        match self.ix.owner_state(pos) {
            Some(owner) => self.state_ref(owner),
            None => {
                Err(CodeGenError::new(&self.ix.v(pos).id, format!("{} has no owning state.", self.label(Some(pos)))))
            }
        }
    }

    fn candidates(
        &mut self,
        t: &'a Transition,
        pos: usize,
        prefix: Vec<Step>,
        kind: Option<&'static str>,
    ) -> Result<Vec<CodeCandidate>, CodeGenError> {
        let label = self.transition_label(t);
        let guard = self.guard_of(t)?;
        let pre = if self.model.kind == MachineKind::Protocol {
            self.condition(&t.precondition, &format!("Precondition of {label}"), &t.id)?
        } else {
            None
        };
        if pre.is_some() {
            self.features.preconditions = true;
        }
        let k = kind.unwrap_or(match t.kind {
            TransitionKind::Internal => "internal",
            TransitionKind::Local => "local",
            TransitionKind::External => "external",
        });
        let key = format!("{}|{}", t.id, self.ix.v(pos).id);
        let fireables = self.fireables(key, t, pos, prefix, k, &label)?;
        Ok(fireables
            .into_iter()
            .map(|(tr, g)| CodeCandidate {
                transition: self.transitions[tr].name.clone(),
                transition_index: tr,
                guard: and(guard.clone(), g),
                precondition: pre.clone(),
                requires: Vec::new(),
                label: label.clone(),
            })
            .collect())
    }

    /// A join: fires from the completion of any source once all sources have completed.
    fn join(&mut self, j: usize) -> Result<Vec<CodeCandidate>, CodeGenError> {
        self.features.joins = true;
        let jv = self.ix.v(j);
        let incoming = self.ix.incoming(&jv.id);
        let mut sources = Vec::new();
        for t in &incoming {
            let s = self.vertex(&t.source)?;
            if self.is_state(s) {
                sources.push(s);
            }
        }
        let out = self.ix.outgoing(&jv.id).into_iter().next();
        let requires = sources.iter().map(|&s| self.state_ref(s)).collect::<Result<Vec<_>, _>>()?;
        let names: Vec<&str> = requires.iter().map(|r| r.name.as_str()).collect();
        let label = match out {
            Some(o) => format!("join {} -> {}", names.join(" + "), self.label(self.ix.vertex(&o.target))),
            None => format!("join {}", names.join(" + ")),
        };
        let key = format!("join|{}", jv.id);
        let guard = match out {
            Some(o) => self.guard_of(o)?,
            None => None,
        };
        let tr = match self.fire_cache.get(&key) {
            Some(cached) => cached[0].0,
            None => {
                let target = out.map(|o| self.vertex(&o.target)).transpose()?;
                let mut ends = sources.clone();
                ends.extend(target);
                let domain = self.common_region(&ends)?;
                let mut exits = Vec::new();
                match domain {
                    None => {
                        for r in self.top_region_ids.clone() {
                            exits.push(self.exit_region_step(r)?);
                        }
                    }
                    Some(domain) => {
                        for &s in &sources {
                            let root = self.child_on_path(s, Some(domain))?;
                            if self.is_state(root) {
                                let index = self.state_ref(root)?.index;
                                if !exits
                                    .iter()
                                    .any(|e| matches!(e, Step::Exit { state_index, .. } if *state_index == index))
                                {
                                    exits.push(self.exit_step(root)?);
                                }
                            }
                        }
                    }
                }
                let mut steps = exits;
                for t in &incoming {
                    steps.extend(self.effect(t)?);
                }
                if let Some(o) = out {
                    steps.extend(self.effect(o)?);
                }
                let entered = match target {
                    Some(target) => self.enter_domain(domain, &[target])?,
                    None => one(Vec::new()),
                };
                let alts = seq(one(steps), entered);
                let via = join_via(&[self.via_of(j)], &union_via(alts.iter().map(|a| &a.via)));
                let source = requires
                    .first()
                    .cloned()
                    .ok_or_else(|| CodeGenError::new(&jv.id, format!("{label} has no source state.")))?;
                let name = self.transition_names.take(&ident(&format!("join_{}", names.join("_")), "x"));
                let steps = self.alts_to_steps(alts, &label, domain.unwrap_or(self.top_region_ids[0]))?;
                let index = self.transitions.len();
                self.transitions.push(CodeTransition {
                    index,
                    id: jv.id.clone(),
                    name,
                    label: label.clone(),
                    via,
                    kind: "join",
                    source_index: source.index,
                    source,
                    steps,
                });
                self.fire_cache.insert(key, vec![(index, None)]);
                index
            }
        };
        Ok(vec![CodeCandidate {
            transition: self.transitions[tr].name.clone(),
            transition_index: tr,
            guard,
            precondition: None,
            requires,
            label,
        }])
    }

    // -------------------------------------------------------------- states and regions

    fn state(&mut self, v: usize, index: usize) -> Result<CodeState, CodeGenError> {
        let vx = self.ix.v(v);
        let name = self.state_names[index].clone();
        let is_final = vx.vtype == VertexType::Final;
        let owner = self.ix.region_owner(&vx.parent).flatten();
        let regions = vx.regions.iter().map(|r| self.region_ref(&r.id)).collect::<Result<Vec<_>, _>>()?;
        let activities = if is_final {
            Vec::new()
        } else {
            self.calls(&vx.do_activity, &format!("do of {name}"), &vx.id, SetKind::Activities)?
        };
        let timer_steps: Vec<CodeTimer> = self.timers.iter().filter(|t| t.state_index == index).cloned().collect();
        let timers = timer_steps.iter().map(|t| Ref { name: t.name.clone(), index: t.index }).collect();

        let mut entry: Vec<Step> = self
            .calls(&vx.entry, &format!("entry of {name}"), &vx.id, SetKind::Actions)?
            .into_iter()
            .map(|action| Step::Call { action })
            .collect();
        entry.extend(activities.iter().map(|a| Step::StartActivity { activity: a.clone() }));
        entry.extend(timer_steps.iter().map(|t| Step::StartTimer {
            timer: t.name.clone(),
            timer_index: t.index,
            ms: t.ms,
        }));
        let mut exit = Vec::new();
        if vx.submachine.is_some() {
            exit.push(Step::StopSubmachine { state: name.clone(), state_index: index });
        }
        exit.extend(timer_steps.iter().map(|t| Step::CancelTimer { timer: t.name.clone(), timer_index: t.index }));
        exit.extend(activities.iter().rev().map(|a| Step::StopActivity { activity: a.clone() }));
        exit.extend(
            self.calls(&vx.exit, &format!("exit of {name}"), &vx.id, SetKind::Actions)?
                .into_iter()
                .map(|action| Step::Call { action }),
        );

        let deferred: Vec<Ref> = vx
            .deferrable
            .iter()
            .map(|d| {
                let e = self.event(js_trim(d), true);
                Ref { name: self.events[e].name.clone(), index: self.events[e].signal_index as usize }
            })
            .collect();
        if !deferred.is_empty() {
            self.features.deferral = true;
        }
        let invariant =
            if is_final { None } else { self.condition(&vx.invariant, &format!("Invariant of {name}"), &vx.id)? };
        if invariant.is_some() {
            self.features.invariants = true;
        }

        // Transitions leaving the state.
        let mut handlers: Vec<CodeHandler> = Vec::new();
        let mut completion = Vec::new();
        for t in self.ix.outgoing(&vx.id) {
            let Some(target) = self.ix.vertex(&t.target) else { continue };
            if t.triggers.is_empty() {
                if self.ix.v(target).vtype == VertexType::Join {
                    completion.extend(self.join(target)?);
                } else {
                    completion.extend(self.candidates(t, v, Vec::new(), None)?);
                }
                continue;
            }
            let cands = self.candidates(t, v, Vec::new(), None)?;
            for trig in &t.triggers {
                let sig = self.signal_of(v, trig)?;
                let h = match handlers.iter().position(|x| x.signal == sig.name) {
                    Some(h) => h,
                    None => {
                        handlers.push(CodeHandler {
                            signal: sig.name.clone(),
                            signal_index: sig.index,
                            kind: sig.kind,
                            candidates: Vec::new(),
                            exhaustive: false,
                        });
                        handlers.len() - 1
                    }
                };
                handlers[h].candidates.extend(cands.iter().cloned());
            }
        }
        for h in &mut handlers {
            h.exhaustive = h
                .candidates
                .last()
                .is_some_and(|l| l.guard.is_none() && l.precondition.is_none() && l.requires.is_empty());
        }

        // Submachine states: transitions leaving its exit point references.
        let mut exits = Vec::new();
        if vx.submachine.is_some() {
            for r in self.ix.connection_points(v) {
                let rv = self.ix.v(r);
                if rv.vtype != VertexType::ConnectionPointRef || rv.point_kind != PointKind::Exit {
                    continue;
                }
                let point = self.point_name(v, r, PointKind::Exit)?;
                let mut candidates = Vec::new();
                for t in self.ix.outgoing(&rv.id) {
                    let prefix = vec![self.exit_step(v)?];
                    candidates.extend(self.candidates(t, r, prefix, Some("exitPoint"))?);
                }
                exits.push(CodeExit { point, candidates });
            }
        }

        let kind = if is_final {
            "final"
        } else if vx.submachine.is_some() {
            "submachine"
        } else if !regions.is_empty() {
            "composite"
        } else {
            "simple"
        };
        let completes = match kind {
            "final" => "never",
            "submachine" => "submachine",
            "composite" => "regions",
            _ if !activities.is_empty() => "activities",
            _ => "immediate",
        };
        let parent = owner.map(|o| self.state_ref(o)).transpose()?;
        let region = self.region_ref(&vx.parent)?;
        Ok(CodeState {
            index,
            id: vx.id.clone(),
            display_name: if !vx.name.is_empty() {
                vx.name.clone()
            } else if is_final {
                "final".to_string()
            } else {
                name.clone()
            },
            name,
            kind,
            is_final,
            region_index: region.index,
            region,
            parent_index: parent.as_ref().map_or(-1, |p| p.index as i64),
            parent,
            depth: self.ix.ancestors(v).len(),
            regions,
            entry,
            exit,
            activities,
            timers,
            deferred,
            invariant,
            stereotype: vx.stereotype.clone(),
            submachine: match &vx.submachine {
                Some(href) => Some(SubmachineRef {
                    machine: self.options.submachines.get(href).map(|s| s.machine.name.clone()).unwrap_or_default(),
                    href: href.clone(),
                }),
                None => None,
            },
            completes,
            handlers,
            completion,
            exits,
        })
    }

    fn region(&mut self, index: usize) -> Result<CodeRegion, CodeGenError> {
        let (id, name_attr, owner) = {
            let r = &self.region_list[index];
            (r.id, r.name, r.owner)
        };
        let initial = self.ix.initial_of(id);
        let out = initial.and_then(|i| self.ix.outgoing(&self.ix.v(i).id).into_iter().next());
        let owner = owner.map(|o| self.state_ref(o)).transpose()?;
        let name = self.region_names[index].clone();
        let default_steps = match (initial, out) {
            (Some(initial), Some(out)) => {
                let segment = self.segment(initial, out)?;
                self.alts_to_steps(segment, &format!("initial of {name}"), id)?
            }
            _ => Vec::new(),
        };
        let states = self
            .state_list
            .iter()
            .enumerate()
            .filter(|&(_, &v)| self.ix.v(v).parent == id)
            .map(|(i, _)| Ref { name: self.state_names[i].clone(), index: i })
            .collect();
        Ok(CodeRegion {
            index,
            display_name: if name_attr.is_empty() { name.clone() } else { name_attr.to_string() },
            name,
            top: owner.is_none(),
            owner_index: owner.as_ref().map_or(-1, |o| o.index as i64),
            owner,
            states,
            default_steps,
        })
    }

    fn machine_points(&self, vtype: VertexType) -> Vec<usize> {
        (0..self.model.vertices.len())
            .filter(|&i| {
                let v = &self.model.vertices[i];
                v.vtype == vtype && self.top_region_ids.contains(&v.parent.as_str())
            })
            .collect()
    }

    fn entry_point(&mut self, v: usize) -> Result<CodeEntryPoint, CodeGenError> {
        let vx = self.ix.v(v);
        let outs = self.ix.outgoing(&vx.id);
        let mut alts = one(Vec::new());
        for t in &outs {
            let segment = self.segment(v, t)?;
            alts = seq(alts, segment);
        }
        let mut entered = HashSet::new();
        for t in &outs {
            let target = self.vertex(&t.target)?;
            if let Some(first) = self.chain(target)?.first() {
                entered.insert(*first);
            }
        }
        let name = ident(&vx.name, "entry");
        let mut steps = self.alts_to_steps(alts, &format!("entry point {name}"), &vx.parent)?;
        for r in self.top_region_ids.clone() {
            if !entered.contains(r) {
                steps.push(self.enter_region_step(r)?);
            }
        }
        Ok(CodeEntryPoint { name, id: vx.id.clone(), steps })
    }
}
