//! The code model as nested transition tables, for state machine frameworks
//! whose transitions stay inside one table (Boost.SML, Boost.MSM, ...): a
//! table per composite state holding the transitions between its substates,
//! and a table for the machine. A port of fsm-editor-vscode/src/codegen/tables.ts.
//!
//! Such frameworks can't express what UML transitions often do: leave a
//! composite state from one of its substates, enter a nested state directly,
//! go through entry/exit points, forks, joins or history. The tables do it
//! with two mechanisms that keep UML's order of exits, effects and entries:
//!
//! - Continuations (leaving from inside): the substate takes the event without
//!   changing state and posts a continuation event, which the table of the
//!   composite state being left takes right away (`continue` action). One
//!   continuation per state left, when effects come between the exits.
//! - Routing (entering inside): every region starts in an initial pseudostate
//!   (`init`) whose anonymous transitions enter the region by default, through
//!   a route set by the transition entering the composite state (`route`
//!   action), or restore its history. Routes may run effects (entry points).
//!
//! Completion events, deferral, timers and submachines are left to the
//! runtime of the template: the tables only say where they are taken.
//! `notes` lists where the tables work around a missing feature, `problems`
//! what they can't express, so that templates can warn or refuse with their
//! own wording.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use serde::Serialize;

use crate::codemodel::{CodeCandidate, CodeModel, CodeState, CodeVia, Ref, Step};
use crate::expressions::Condition;

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeTables {
    /// One table per composite state, innermost first, then the machine's table (last).
    pub list: Vec<CodeTable>,
    /// Events used by the tables besides the machine's events, numbered from 0.
    pub events: Vec<TableEvent>,
    /// For each timer (code model `timers` index): its event.
    pub timer_events: Vec<Ref>,
    /// For each state: its completion event, or null when nothing waits for it.
    pub completion_events: Vec<Option<Ref>>,
    /// Postconditions of protocol machines, checked after their transition (`postcondition` action).
    pub postconditions: Vec<TablePostcondition>,
    /// Number of region entry routes, the first two being reserved (0 default entry, 1 deep resume).
    pub routes: usize,
    /// The machine's own entry points: the routes of its regions (`route` actions) to start through each.
    pub entry_points: Vec<TableEntryPoint>,
    /// Where the tables work around something the frameworks lack.
    pub notes: Vec<TableNote>,
    /// What they can't express.
    pub problems: Vec<TableNote>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TablePostcondition {
    pub index: usize,
    pub element: String,
    pub condition: Condition,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableEntryPoint {
    pub name: String,
    pub routes: Vec<TableAction>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CodeTable {
    /// The composite state owning the table, or null for the machine's.
    pub state: Option<Ref>,
    /// The state's name, or the machine's.
    pub name: String,
    pub top: bool,
    pub regions: Vec<TableRegion>,
    /// States directly in its regions.
    pub states: Vec<Rc<CodeState>>,
    /// Pseudostates of the table (initial, off, choice and exiting pseudostates).
    pub pseudostates: Vec<TableState>,
    pub rows: Vec<TableRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableRegion {
    pub index: usize,
    pub name: String,
    /// Pseudostate the region starts in when its owner is entered.
    pub init: TableState,
    /// Machine's table only: pseudostate the region waits in until the machine starts.
    pub off: Option<TableState>,
}

/// A state of the table, or one of its pseudostates.
#[derive(Debug, Clone, Serialize)]
pub struct TableState {
    /// `state` or `pseudo`.
    pub kind: &'static str,
    pub name: String,
    /// State index, -1 for pseudostates.
    pub index: i64,
    /// A composite state, which has a table of its own.
    pub composite: bool,
    /// `state`, `init`, `off`, `choice` or `exiting`.
    pub role: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableEvent {
    pub index: usize,
    /// Unique identifier among the table events.
    pub name: String,
    /// `start`, `stop`, `timer`, `completion`, `continuation` or `exitPoint`.
    pub kind: &'static str,
    /// Completion and exit point events: the state; timers: the timer.
    #[serde(rename = "ref")]
    pub reference: Option<Ref>,
    /// Exit point events: the submachine's exit point.
    pub point: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct TableRow {
    pub source: TableState,
    /// The source is its region's initial state.
    pub initial: bool,
    /// `event`: a machine event (code model `events`); `table`: a table event; `entry`/`exit`: the source's entry or
    /// exit (a `stateEntry` or `stateExit` action); null: anonymous, taken as soon as the source is entered.
    pub trigger: Option<Trigger>,
    /// All must hold, evaluated in order.
    pub guard: Vec<GuardTerm>,
    pub actions: Vec<TableAction>,
    /// null: an internal transition, without exit or entry.
    pub target: Option<TableState>,
    /// What the row implements.
    pub comment: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Trigger {
    pub kind: &'static str,
    pub name: String,
    pub index: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum GuardTerm {
    Condition {
        condition: Condition,
    },
    /// Protocol machines: when false, remember `label` for a protocol violation.
    Precondition {
        condition: Condition,
        label: String,
    },
    /// Joins: these states are active and completed.
    Requires {
        states: Vec<Ref>,
    },
    /// Offers the event to the submachine of `state`: true when it used it.
    Offer {
        state: Ref,
        submachine: String,
        event: String,
    },
    /// The region is entered through `route`.
    Routed {
        region: usize,
        route: usize,
    },
    /// The region is entered through `route`, which restores its history, and `state` was its last active state.
    Restores {
        region: usize,
        route: usize,
        state: Ref,
    },
    /// The region is entered by default (route 0, or route 1 without history).
    Defaults {
        region: usize,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum TableAction {
    /// Entering `state`: mark it active, run its `entry` steps, start its submachine, then see whether it completes.
    StateEntry {
        state: Rc<CodeState>,
    },
    /// Leaving `state` (its regions have been left): run its `exit` steps, then record it as its region's history.
    StateExit {
        state: Rc<CodeState>,
    },
    Call {
        action: String,
    },
    Postcondition {
        index: usize,
    },
    Terminate,
    ExitMachine {
        point: String,
    },
    /// Post a continuation, taken before anything else.
    Continue {
        event: Ref,
    },
    /// Enter `region` through `route` the next time it is entered; `history` 0 none, 1 shallow, 2 deep.
    Route {
        region: usize,
        route: usize,
        history: u8,
    },
    /// Start the submachine of `state` through its entry point `point` (index `point_index`) when entered.
    SubmachineAt {
        state: Ref,
        submachine: String,
        point: String,
        point_index: i64,
    },
    /// The region is being entered: forget its route.
    Entering {
        region: usize,
    },
    /// The region is being entered through its history, `state`: forget its route, and pass deep history on.
    Restore {
        region: usize,
        state: Ref,
    },
    /// Keep the event until the configuration changes.
    Defer,
    /// A choice without an enabled branch.
    NoBranch {
        label: String,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct TableNote {
    /// `leave`, `enter`, `entryPoint`, `exitPoint`, `fork`, `join`, `history`, `local`, `orthogonal`; problems:
    /// `topRegions`, `localRegions`, `unexpected`.
    pub kind: &'static str,
    /// xmi:id of the element, for locations ('' when unknown).
    pub id: String,
    /// The element: a transition label, a pseudostate or state name.
    pub element: String,
    /// Kind-specific: the state taking the event (`leave`), the composite state left or entered, the region, the signal.
    pub state: String,
    pub detail: String,
}

pub fn to_tables(cm: &CodeModel) -> CodeTables {
    Lowering::new(cm).run()
}

// ------------------------------------------------------------------ lowering

/// Route 1 resumes a region deeply (route 0, the default entry, needs no constant).
const DEEP_ROUTE: usize = 1;

struct Draft {
    table: CodeTable,
    /// Rows with their sort keys: source order, rank, order of addition.
    rows: Vec<(TableRow, i64, i64, usize)>,
}

#[derive(Default)]
struct Found {
    /// It left a composite state from inside: from, through.
    left: Option<(String, String)>,
    /// It routed into a composite state: state, target.
    entered: Option<(String, String)>,
}

/// What is being lowered, to name its continuations and report notes.
#[derive(Clone)]
struct Origin {
    key: String,
    label: String,
    id: String,
    /// The pseudostates it goes through, each noted.
    via: Vec<CodeVia>,
    /// Those of `via` on the way being lowered, without the branches of its choices (each a way of its own): they
    /// explain leaving or entering a composite state from inside, which then needs no note of its own.
    way: Vec<CodeVia>,
    /// For a branch of a choice: its way, whether the origin notes its pseudostates or not.
    branch_way: Option<Vec<CodeVia>>,
    /// Set while lowering, shared with the branches of its choices.
    found: Rc<RefCell<Found>>,
}

impl Origin {
    fn new(key: String, label: String, id: String, via: Vec<CodeVia>, way: Vec<CodeVia>) -> Origin {
        Origin { key, label, id, via, way, branch_way: None, found: Rc::default() }
    }
}

/// What lowering found (see `Found`), and the way that may explain it: None for that of the origin it is found for.
#[derive(Clone)]
struct Finding {
    left: Option<(String, String)>,
    entered: Option<(String, String)>,
    way: Option<Vec<CodeVia>>,
}

struct Hop {
    exit: usize,
    calls: Vec<Step>,
}

struct Lowering<'a> {
    cm: &'a CodeModel,
    drafts: HashMap<i64, Draft>,
    events: Vec<TableEvent>,
    event_names: HashSet<String>,
    continuations: HashMap<String, Ref>,
    hops_done: HashSet<String>,
    route_keys: HashMap<String, usize>,
    route_count: usize,
    /// What lowering each route found, for the origins that take the route later.
    route_findings: HashMap<usize, Vec<Finding>>,
    /// The routes being lowered: the key of the origin they are lowered for, and what they found so far.
    routing: Vec<(String, Vec<Finding>)>,
    choice_count: usize,
    seq: usize,
    postconditions: Vec<TablePostcondition>,
    notes: Vec<TableNote>,
    problems: Vec<TableNote>,
    completion_events: Vec<Option<Ref>>,
    timer_events: Vec<Ref>,
}

/// The pseudostates of `via` on the way through `steps`, without those of the branches of its choices.
fn way_via(via: &[CodeVia], steps: &[Step]) -> Vec<CodeVia> {
    fn visit<'s>(steps: &'s [Step], branches: &mut HashSet<&'s str>) {
        for st in steps {
            match st {
                Step::Choice { branches: list, .. } => {
                    for v in list.iter().flat_map(|b| &b.via) {
                        branches.insert(v.id.as_str());
                    }
                }
                Step::RestoreHistory { default_steps, .. } => visit(default_steps, branches),
                _ => {}
            }
        }
    }
    let mut branches = HashSet::new();
    visit(steps, &mut branches);
    via.iter().filter(|v| !branches.contains(v.id.as_str())).cloned().collect()
}

fn pseudo(name: String, role: &'static str) -> TableState {
    TableState { kind: "pseudo", name, index: -1, composite: false, role }
}

fn note(kind: &'static str, id: &str, element: &str, state: &str, detail: &str) -> TableNote {
    TableNote {
        kind,
        id: id.to_string(),
        element: element.to_string(),
        state: state.to_string(),
        detail: detail.to_string(),
    }
}

fn region_of_step(cm: &CodeModel, st: &Step) -> Option<usize> {
    match st {
        Step::Enter { state_index, .. } => Some(cm.states[*state_index].region_index),
        Step::EnterRegion { region_index, .. }
        | Step::RestoreHistory { region_index, .. }
        | Step::Choice { region_index, .. } => Some(*region_index),
        _ => None,
    }
}

fn ident(text: &str) -> String {
    let mut s = String::new();
    let mut in_run = false;
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '_' {
            s.push(c);
            in_run = false;
        } else if !in_run {
            s.push('_');
            in_run = true;
        }
    }
    let s = s.trim_matches('_');
    let s = if s.is_empty() { "transition" } else { s };
    if s.starts_with(|c: char| c.is_ascii_digit()) { format!("_{s}") } else { s.to_string() }
}

fn dedupe(notes: Vec<TableNote>) -> Vec<TableNote> {
    let mut seen = HashSet::new();
    notes.into_iter().filter(|n| seen.insert(format!("{}|{}|{}|{}", n.kind, n.id, n.element, n.detail))).collect()
}

fn steps_json(steps: &[Step]) -> String {
    serde_json::to_string(steps).unwrap_or_default()
}

impl<'a> Lowering<'a> {
    fn new(cm: &'a CodeModel) -> Lowering<'a> {
        Lowering {
            cm,
            drafts: HashMap::new(),
            events: Vec::new(),
            event_names: HashSet::new(),
            continuations: HashMap::new(),
            hops_done: HashSet::new(),
            route_keys: HashMap::new(),
            route_count: 2,
            route_findings: HashMap::new(),
            routing: Vec::new(),
            choice_count: 0,
            seq: 0,
            postconditions: Vec::new(),
            notes: Vec::new(),
            problems: Vec::new(),
            completion_events: vec![None; cm.states.len()],
            timer_events: Vec::new(),
        }
    }

    fn run(mut self) -> CodeTables {
        let cm = self.cm;
        // Tables: composite states, innermost first, then the machine's.
        let mut order: Vec<i64> = Vec::new();
        fn visit(cm: &CodeModel, s: usize, order: &mut Vec<i64>) {
            for r in &cm.states[s].regions {
                for c in &cm.regions[r.index].states {
                    visit(cm, c.index, order);
                }
            }
            if !cm.states[s].regions.is_empty() {
                order.push(s as i64);
            }
        }
        for r in &cm.top_regions {
            for c in &r.states {
                visit(cm, c.index, &mut order);
            }
        }
        order.push(-1);
        for &key in &order {
            self.draft(key);
        }

        // Events: timers first (their names are fixed), then start and stop.
        for t in &cm.timers {
            let e = self.event(&t.name, "timer", Some(Ref { name: t.name.clone(), index: t.index }), "");
            self.timer_events.push(e);
        }
        let start = self.event("start", "start", None, "");
        let stop = self.event("stop", "stop", None, "");

        // The machine's regions wait in `off` until started, and go back there when stopped.
        let top_regions = self.draft(-1).table.regions.clone();
        for r in &top_regions {
            let off = r.off.clone().expect("the machine's regions have an off pseudostate");
            self.add(
                -1,
                row(
                    off.clone(),
                    Some(table_trigger(&start)),
                    vec![],
                    vec![],
                    Some(r.init.clone()),
                    format!("start {}", r.name),
                ),
                0,
            );
            for s in &cm.regions[r.index].states {
                self.add(
                    -1,
                    row(
                        self.state_of(s.index),
                        Some(table_trigger(&stop)),
                        vec![],
                        vec![],
                        Some(off.clone()),
                        "stop".into(),
                    ),
                    9,
                );
            }
            self.add(-1, row(r.init.clone(), Some(table_trigger(&stop)), vec![], vec![], Some(off), "stop".into()), 9);
        }

        // Entry and exit of every state.
        for s in &cm.states {
            let key = self.table_of_state(s.index);
            let src = self.state_of(s.index);
            let entry = Trigger { kind: "entry", name: "entry".into(), index: -1 };
            let exit = Trigger { kind: "exit", name: "exit".into(), index: -1 };
            self.add(
                key,
                row(
                    src.clone(),
                    Some(entry),
                    vec![],
                    vec![TableAction::StateEntry { state: s.clone() }],
                    None,
                    String::new(),
                ),
                -1,
            );
            self.add(
                key,
                row(src, Some(exit), vec![], vec![TableAction::StateExit { state: s.clone() }], None, String::new()),
                -1,
            );
        }

        // Region entries: default, deep resume.
        for r in &cm.regions {
            self.region_entries(r.index);
        }

        // The machine's entry points.
        let mut entry_points = Vec::new();
        for p in &cm.entry_points {
            let label = format!("entry point {}", p.name);
            let origin = Origin::new(label.clone(), label, p.id.clone(), Vec::new(), Vec::new());
            let top: Vec<usize> = cm.top_regions.iter().map(|r| r.index).collect();
            let (actions, end) = self.route_regions(&top, &p.steps, 0, &origin, None);
            if end < p.steps.len() {
                self.problems.push(note("unexpected", &p.id, &origin.label, "", p.steps[end].kind()));
            }
            self.note_origin(&origin);
            entry_points.push(TableEntryPoint { name: p.name.clone(), routes: actions });
        }

        // Transitions of each state.
        for s in &cm.states {
            self.state_rows(s);
        }

        self.orthogonal_notes();

        let mut list = Vec::new();
        for key in order {
            let mut d = self.drafts.remove(&key).unwrap();
            d.rows.sort_by_key(|&(_, order, rank, seq)| (order, rank, seq));
            let mut table = d.table;
            table.rows = d.rows.into_iter().map(|(row, ..)| row).collect();
            // The initial mark goes on the first row of each initial pseudostate.
            let mut marked = HashSet::new();
            let role = if table.top { "off" } else { "init" };
            for row in &mut table.rows {
                let initial = row.source.role == role;
                row.initial = initial && !marked.contains(&row.source.name);
                if initial {
                    marked.insert(row.source.name.clone());
                }
            }
            list.push(table);
        }
        CodeTables {
            list,
            events: self.events,
            timer_events: self.timer_events,
            completion_events: self.completion_events,
            postconditions: self.postconditions,
            routes: self.route_count,
            entry_points,
            notes: dedupe(self.notes),
            problems: dedupe(self.problems),
        }
    }

    // -------------------------------------------------------------- tables, states, events

    fn draft(&mut self, key: i64) -> &mut Draft {
        let cm = self.cm;
        self.drafts.entry(key).or_insert_with(|| {
            let state = if key < 0 { None } else { Some(&cm.states[key as usize]) };
            let regions: Vec<_> = match state {
                Some(s) => s.regions.iter().map(|r| &cm.regions[r.index]).collect(),
                None => cm.top_regions.iter().collect(),
            };
            let mut pseudostates = Vec::new();
            let table_regions = regions
                .iter()
                .map(|r| {
                    let init = pseudo(format!("{}_init", r.name), "init");
                    let off = if state.is_some() { None } else { Some(pseudo(format!("{}_off", r.name), "off")) };
                    pseudostates.push(init.clone());
                    if let Some(off) = &off {
                        pseudostates.push(off.clone());
                    }
                    TableRegion { index: r.index, name: r.name.clone(), init, off }
                })
                .collect();
            Draft {
                table: CodeTable {
                    state: state.map(|s| Ref { name: s.name.clone(), index: s.index }),
                    name: state.map_or(cm.machine.name.clone(), |s| s.name.clone()),
                    top: state.is_none(),
                    regions: table_regions,
                    states: regions.iter().flat_map(|r| r.states.iter().map(|s| cm.states[s.index].clone())).collect(),
                    pseudostates,
                    rows: Vec::new(),
                },
                rows: Vec::new(),
            }
        })
    }

    /// Key of the table holding state `s` (its parent state, -1 for the machine).
    fn table_of_state(&self, s: usize) -> i64 {
        self.cm.states[s].parent_index
    }

    fn table_of_region(&self, r: usize) -> i64 {
        self.cm.regions[r].owner_index
    }

    fn state_of(&self, s: usize) -> TableState {
        let st = &self.cm.states[s];
        TableState {
            kind: "state",
            name: st.name.clone(),
            index: s as i64,
            composite: !st.regions.is_empty(),
            role: "state",
        }
    }

    fn init(&mut self, r: usize) -> TableState {
        let key = self.table_of_region(r);
        self.draft(key).table.regions.iter().find(|x| x.index == r).unwrap().init.clone()
    }

    fn exiting(&mut self, key: i64) -> TableState {
        let d = self.draft(key);
        if let Some(p) = d.table.pseudostates.iter().find(|x| x.role == "exiting") {
            return p.clone();
        }
        let p = pseudo(format!("{}_exiting", d.table.name), "exiting");
        d.table.pseudostates.push(p.clone());
        p
    }

    fn event(&mut self, base: &str, kind: &'static str, reference: Option<Ref>, point: &str) -> Ref {
        let mut name = base.to_string();
        let mut i = 2;
        while self.event_names.contains(&name) {
            name = format!("{base}_{i}");
            i += 1;
        }
        self.event_names.insert(name.clone());
        let index = self.events.len();
        self.events.push(TableEvent { index, name: name.clone(), kind, reference, point: point.to_string() });
        Ref { name, index }
    }

    fn completion_event(&mut self, s: usize) -> Ref {
        if let Some(e) = &self.completion_events[s] {
            return e.clone();
        }
        let name = self.cm.states[s].name.clone();
        let e = self.event(&format!("{name}_done"), "completion", Some(Ref { name, index: s }), "");
        self.completion_events[s] = Some(e.clone());
        e
    }

    /// Adds a row; rows are sorted by source, then `rank` (offers, own transitions, a parent's local ones, deferral,
    /// stop), then order of addition.
    fn add(&mut self, key: i64, row: TableRow, rank: i64) {
        let seq = self.seq;
        self.seq += 1;
        let d = self.draft(key);
        let ix = if row.source.kind == "state" {
            d.table.states.iter().position(|s| s.index as i64 == row.source.index).map_or(-1, |i| i as i64)
        } else {
            let pos = d.table.pseudostates.iter().position(|p| p.name == row.source.name).map_or(-1, |i| i as i64);
            d.table.states.len() as i64 + pos
        };
        d.rows.push((row, if ix < 0 { i64::MAX } else { ix }, rank, seq));
    }

    fn is_ancestor(&self, a: usize, s: usize) -> bool {
        let mut p = self.cm.states[s].parent_index;
        while p >= 0 {
            if p as usize == a {
                return true;
            }
            p = self.cm.states[p as usize].parent_index;
        }
        false
    }

    // -------------------------------------------------------------- regions

    fn region_entries(&mut self, r: usize) {
        let cm = self.cm;
        let region = &cm.regions[r];
        let key = self.table_of_region(r);
        let init = self.init(r);
        let origin = Origin::new(
            format!("initial of {}", region.name),
            format!("initial transition of {}", region.display_name),
            String::new(),
            Vec::new(),
            Vec::new(),
        );
        if !region.default_steps.is_empty() {
            self.lower_init(r, vec![GuardTerm::Defaults { region: r }], &region.default_steps, &origin);
        }
        // Deep resume (resume(), deep history above): the region's last active state, or its default entry.
        for s in &region.states {
            if cm.states[s.index].is_final {
                continue;
            }
            let guard = vec![GuardTerm::Restores { region: r, route: DEEP_ROUTE, state: s.clone() }];
            let actions = vec![TableAction::Restore { region: r, state: s.clone() }];
            self.add(
                key,
                row(init.clone(), None, guard, actions, Some(self.state_of(s.index)), format!("resume {}", s.name)),
                1,
            );
        }
        self.note_origin(&origin);
    }

    /// The route entering region `r` through `steps` (prefix calls, then the region's entry); created once per
    /// distinct steps.
    fn route(&mut self, r: usize, steps: &[Step], origin: &Origin) -> usize {
        let key = format!("{r}|{}", steps_json(steps));
        if let Some(&known) = self.route_keys.get(&key) {
            // A route lowered for another origin: what lowering it found applies to this one too.
            for f in self.route_findings.get(&known).cloned().unwrap_or_default() {
                self.find(origin, f);
            }
            return known;
        }
        let route = self.route_count;
        self.route_count += 1;
        self.route_keys.insert(key, route);
        self.routing.push((origin.key.clone(), Vec::new()));
        if let Some(Step::RestoreHistory { deep, region, default_steps, .. }) = steps.last() {
            let prefix = &steps[..steps.len() - 1];
            let table_key = self.table_of_region(r);
            for s in &self.cm.regions[r].states {
                if self.cm.states[s.index].is_final {
                    continue;
                }
                let guard = vec![GuardTerm::Restores { region: r, route, state: s.clone() }];
                let mut actions = call_actions(prefix);
                actions.push(TableAction::Restore { region: r, state: s.clone() });
                let comment = format!("{} history of {region}: {}", if *deep { "deep" } else { "shallow" }, s.name);
                let init = self.init(r);
                self.add(table_key, row(init, None, guard, actions, Some(self.state_of(s.index)), comment), 1);
            }
            // Without history: the history's default transition, or the region's default entry.
            let own =
                matches!(default_steps.as_slice(), [Step::EnterRegion { region_index, .. }] if *region_index == r);
            let mut steps = prefix.to_vec();
            steps.extend(if own { self.cm.regions[r].default_steps.iter() } else { default_steps.iter() }.cloned());
            self.lower_init(r, vec![GuardTerm::Routed { region: r, route }], &steps, origin);
        } else {
            self.lower_init(r, vec![GuardTerm::Routed { region: r, route }], steps, origin);
        }
        if let Some((_, findings)) = self.routing.pop() {
            self.route_findings.insert(route, findings);
        }
        route
    }

    /// Records what lowering for `origin` found, unless a pseudostate on the way explains it; the routes being
    /// lowered keep it too.
    fn find(&mut self, origin: &Origin, f: Finding) {
        let way: Vec<&CodeVia> = match &f.way {
            None => origin.way.iter().collect(),
            Some(way) => way.iter().filter(|v| origin.via.iter().any(|x| x.id == v.id)).collect(),
        };
        let explained = |kinds: &[&str]| way.iter().any(|v| kinds.contains(&v.kind));
        {
            let mut found = origin.found.borrow_mut();
            if let Some(left) = &f.left
                && !explained(&["exitPoint", "join"])
            {
                found.left = Some(left.clone());
            }
            if let Some(entered) = &f.entered
                && found.entered.is_none()
                && !explained(&["entryPoint", "fork", "shallowHistory", "deepHistory"])
            {
                found.entered = Some(entered.clone());
            }
        }
        // For a route, the way of a branch of its origin's choices is that branch's.
        for (key, findings) in &mut self.routing {
            let way = if f.way.is_none() && *key != origin.key { origin.branch_way.clone() } else { f.way.clone() };
            findings.push(Finding { way, ..f.clone() });
        }
    }

    /// An anonymous row of the region's initial pseudostate doing `steps`.
    fn lower_init(&mut self, r: usize, guard: Vec<GuardTerm>, steps: &[Step], origin: &Origin) {
        let init = self.init(r);
        let key = self.table_of_region(r);
        self.lower_fire(init, key, None, guard, steps, origin, 2, vec![TableAction::Entering { region: r }]);
    }

    // -------------------------------------------------------------- states

    fn state_rows(&mut self, s: &Rc<CodeState>) {
        let cm = self.cm;
        let key = self.table_of_state(s.index);
        let src = self.state_of(s.index);
        // A submachine gets the events it knows first.
        if let Some(submachine) = &s.submachine
            && let Some(sub) = cm.submachines.iter().find(|x| x.href == submachine.href)
        {
            for name in &sub.events {
                let Some(e) = cm.events.iter().find(|x| &x.name == name) else { continue };
                let trigger = Trigger { kind: "event", name: e.name.clone(), index: e.index as i64 };
                let guard = vec![GuardTerm::Offer {
                    state: Ref { name: s.name.clone(), index: s.index },
                    submachine: sub.name.clone(),
                    event: e.name.clone(),
                }];
                self.add(
                    key,
                    row(src.clone(), Some(trigger), guard, vec![], None, format!("{} submachine", sub.name)),
                    0,
                );
            }
        }
        for h in &s.handlers {
            let trigger = if h.kind == "event" {
                let index = cm.events.iter().find(|e| e.name == h.signal).map_or(-1, |e| e.index as i64);
                Trigger { kind: "event", name: h.signal.clone(), index }
            } else {
                table_trigger(&self.timer_events[h.signal_index - cm.events.len()])
            };
            self.candidates(s, trigger, &h.candidates);
        }
        for d in &s.deferred {
            let Some(e) = cm.events.iter().find(|x| x.name == d.name) else { continue };
            let trigger = Trigger { kind: "event", name: e.name.clone(), index: e.index as i64 };
            self.add(
                key,
                row(src.clone(), Some(trigger), vec![], vec![TableAction::Defer], None, format!("defer {}", e.name)),
                8,
            );
        }
        if !s.completion.is_empty() {
            let e = self.completion_event(s.index);
            self.candidates(s, table_trigger(&e), &s.completion);
        }
        for x in &s.exits {
            let e = self.event(
                &format!("{}_{}", s.name, x.point),
                "exitPoint",
                Some(Ref { name: s.name.clone(), index: s.index }),
                &x.point,
            );
            self.candidates(s, table_trigger(&e), &x.candidates);
        }
    }

    fn candidates(&mut self, s: &CodeState, trigger: Trigger, cands: &[CodeCandidate]) {
        let cm = self.cm;
        let local = cands.iter().any(|c| cm.transitions[c.transition_index].kind == "local");
        if local && s.regions.len() > 1 {
            for c in cands {
                let tr = &cm.transitions[c.transition_index];
                if tr.kind == "local" {
                    self.problems.push(note("localRegions", &tr.id, &tr.label, &s.name, ""));
                }
            }
            return;
        }
        for c in cands {
            let tr = &cm.transitions[c.transition_index];
            let mut guard = Vec::new();
            if !c.requires.is_empty() {
                guard.push(GuardTerm::Requires { states: c.requires.clone() });
            }
            if let Some(g) = &c.guard {
                guard.push(GuardTerm::Condition { condition: g.clone() });
            }
            if let Some(p) = &c.precondition {
                guard.push(GuardTerm::Precondition { condition: p.clone(), label: c.label.clone() });
            }
            let way = way_via(&tr.via, &tr.steps);
            let origin = Origin::new(format!("t{}", tr.index), tr.label.clone(), tr.id.clone(), tr.via.clone(), way);
            if !local {
                let src = self.state_of(s.index);
                let key = self.table_of_state(s.index);
                self.lower_fire(src, key, Some(trigger.clone()), guard, &tr.steps, &origin, 1, vec![]);
            } else {
                // A local transition doesn't leave `s`: its substates take the event, after their own transitions. The
                // other transitions of `s` for this trigger go along, to keep their order.
                let Some(region) = s.regions.first().map(|r| r.index) else { continue };
                let mut steps: &[Step] = &tr.steps;
                if tr.kind == "local" {
                    self.notes.push(note("local", &tr.id, &tr.label, &s.name, ""));
                    match steps.first() {
                        Some(Step::ExitRegion { region_index, .. }) if *region_index == region => steps = &steps[1..],
                        _ => {
                            self.problems.push(note("unexpected", &tr.id, &tr.label, &s.name, "local transition"));
                            continue;
                        }
                    }
                }
                for y in &cm.regions[region].states {
                    let own: Vec<Step> = if tr.kind == "local" {
                        let mut own = vec![Step::Exit { state: y.name.clone(), state_index: y.index }];
                        own.extend(steps.iter().cloned());
                        own
                    } else {
                        steps.to_vec()
                    };
                    let src = self.state_of(y.index);
                    let key = self.table_of_state(y.index);
                    self.lower_fire(src, key, Some(trigger.clone()), guard.clone(), &own, &origin, 2, vec![]);
                }
            }
            self.note_origin(&origin);
        }
    }

    // -------------------------------------------------------------- transitions

    /// Rows doing `steps` from `src` (in table `key`): exits and effects, possibly through continuations to the
    /// tables of the composite states left, then the entries in the table of the last state left.
    #[allow(clippy::too_many_arguments)]
    fn lower_fire(
        &mut self,
        src: TableState,
        key: i64,
        trigger: Option<Trigger>,
        guard: Vec<GuardTerm>,
        steps: &[Step],
        origin: &Origin,
        rank: i64,
        first: Vec<TableAction>,
    ) {
        let cm = self.cm;
        // Exits and the effects after each: the hops.
        let mut hops: Vec<Hop> = Vec::new();
        let mut i = 0;
        let mut lead: Vec<Step> = Vec::new();
        if let Some(Step::ExitRegion { .. }) = steps.first() {
            self.problems.push(note("topRegions", &origin.id, &origin.label, "", ""));
            return;
        }
        while i < steps.len() && matches!(steps[i], Step::Exit { .. } | Step::Call { .. }) {
            let st = &steps[i];
            i += 1;
            if let Step::Exit { state, state_index } = st {
                if let Some(prev) = hops.last()
                    && !self.is_ancestor(*state_index, prev.exit)
                {
                    self.problems.push(note("unexpected", &origin.id, &origin.label, state, "exit"));
                    return;
                }
                hops.push(Hop { exit: *state_index, calls: Vec::new() });
            } else if let Some(last) = hops.last_mut() {
                last.calls.push(st.clone());
            } else {
                lead.push(st.clone());
            }
        }
        let entries = &steps[i..];
        let comment = origin.label.clone();

        if hops.is_empty() {
            let Some((actions, target)) = self.lower_entries(entries, key, origin, None) else { return };
            let mut all = first;
            all.extend(call_actions(&lead));
            all.extend(actions);
            self.add(key, row(src, trigger, guard, all, target, comment), rank);
            return;
        }
        if !lead.is_empty() {
            self.problems.push(note("unexpected", &origin.id, &origin.label, "", "effect before exit"));
            return;
        }
        let mut from = 0;
        if src.kind == "state" && hops[0].exit as i64 == src.index {
            // The source leaves itself in its own table.
            if hops.len() == 1 {
                let exited = cm.states[src.index as usize].region_index;
                let Some((actions, target)) = self.lower_entries(entries, key, origin, Some(exited)) else { return };
                let mut all = first;
                all.extend(call_actions(&hops[0].calls));
                all.extend(actions);
                self.add(key, row(src, trigger, guard, all, target, comment), rank);
                return;
            }
            let next = self.continuation(origin, 1);
            let mut all = first;
            all.extend(call_actions(&hops[0].calls));
            all.push(TableAction::Continue { event: next });
            let target = self.exiting(key);
            self.add(key, row(src, trigger, guard, all, Some(target), comment), rank);
            from = 1;
        } else {
            // Leaving from inside: the source takes the event, the table of the state left continues.
            let owner = key;
            let inside = if src.kind == "state" {
                self.is_ancestor(hops[0].exit, src.index as usize)
            } else {
                owner == hops[0].exit as i64 || (owner >= 0 && self.is_ancestor(hops[0].exit, owner as usize))
            };
            if !inside {
                self.problems.push(note(
                    "unexpected",
                    &origin.id,
                    &origin.label,
                    &cm.states[hops[0].exit].name,
                    "exit",
                ));
                return;
            }
            let next = self.continuation(origin, 0);
            // A pseudostate (choice) has to be left, or it would be taken again.
            let target = if src.kind == "state" { None } else { Some(self.exiting(key)) };
            let mut all = first;
            all.push(TableAction::Continue { event: next });
            let from_name = if src.kind == "state" { src.name.clone() } else { "a choice".to_string() };
            self.add(key, row(src, trigger, guard, all, target, comment), rank);
            let through = cm.states[hops[hops.len() - 1].exit].name.clone();
            self.find(origin, Finding { left: Some((from_name, through)), entered: None, way: None });
        }
        for k in from..hops.len() {
            self.hop(origin, &hops, k, entries);
        }
    }

    /// The row of the table of `hops[k].exit` taking its continuation (once per transition and hop).
    fn hop(&mut self, origin: &Origin, hops: &[Hop], k: usize, entries: &[Step]) {
        let cm = self.cm;
        let event = self.continuation(origin, k);
        if !self.hops_done.insert(format!("{}|{k}", origin.key)) {
            return;
        }
        let x = hops[k].exit;
        let key = self.table_of_state(x);
        let last = k == hops.len() - 1;
        let mut actions = call_actions(&hops[k].calls);

        let target = if last {
            let Some((entry_actions, entry_target)) =
                self.lower_entries(entries, key, origin, Some(cm.states[x].region_index))
            else {
                return;
            };
            actions.extend(entry_actions);
            entry_target
        } else {
            actions.push(TableAction::Continue { event: self.continuation(origin, k + 1) });
            Some(self.exiting(key))
        };
        let comment = format!("{}: leave {}", origin.label, cm.states[x].name);
        self.add(key, row(self.state_of(x), Some(table_trigger(&event)), vec![], actions, target, comment), 1);
    }

    fn continuation(&mut self, origin: &Origin, k: usize) -> Ref {
        let key = format!("{}|{k}", origin.key);
        if let Some(e) = self.continuations.get(&key) {
            return e.clone();
        }
        let e = self.event(&format!("{}_{}", ident(&origin.label), k + 1), "continuation", None, "");
        self.continuations.insert(key, e.clone());
        e
    }

    /// Actions and target of the entries `steps`, all in table `key`. `exited_region`: region of the last state left.
    fn lower_entries(
        &mut self,
        steps: &[Step],
        key: i64,
        origin: &Origin,
        exited_region: Option<usize>,
    ) -> Option<(Vec<TableAction>, Option<TableState>)> {
        let cm = self.cm;
        let mut actions = Vec::new();
        let mut target: Option<TableState> = None;
        let fail = |this: &mut Self, detail: &str| {
            let kind = if detail == "regions" { "topRegions" } else { "unexpected" };
            this.problems.push(note(kind, &origin.id, &origin.label, "", detail));
            None
        };
        let mut i = 0;
        while i < steps.len() {
            let st = &steps[i];
            match st {
                Step::Call { action } => {
                    actions.push(TableAction::Call { action: action.clone() });
                    i += 1;
                }
                Step::Enter { state_index, .. } => {
                    if target.is_some() {
                        return fail(self, "regions");
                    }
                    if self.table_of_state(*state_index) != key {
                        return fail(self, "enter");
                    }
                    target = Some(self.state_of(*state_index));
                    let (inner, end) = self.inside(*state_index, steps, i + 1, origin);
                    actions.extend(inner);
                    i = end;
                }
                Step::Choice { .. } => {
                    if target.is_some() {
                        return fail(self, "regions");
                    }
                    target = Some(self.choice(st, key, origin));
                    i += 1;
                }
                Step::RestoreHistory { region_index, deep, .. } => {
                    if target.is_some() {
                        return fail(self, "regions");
                    }
                    if self.table_of_region(*region_index) != key {
                        return fail(self, "region");
                    }
                    let route = self.route(*region_index, std::slice::from_ref(st), origin);
                    actions.push(TableAction::Route {
                        region: *region_index,
                        route,
                        history: if *deep { 2 } else { 1 },
                    });
                    target = Some(self.init(*region_index));
                    i += 1;
                }
                Step::EnterRegion { region_index, .. } => {
                    if target.is_some() {
                        return fail(self, "regions");
                    }
                    if self.table_of_region(*region_index) != key {
                        return fail(self, "region");
                    }
                    target = Some(self.init(*region_index));
                    i += 1;
                }
                Step::Terminate => {
                    actions.push(TableAction::Terminate);
                    i += 1;
                }
                Step::ExitMachine { point } => {
                    actions.push(TableAction::ExitMachine { point: point.clone() });
                    let top = exited_region.map(|r| &cm.regions[r]).filter(|r| r.top);
                    let Some(region) = top else { return fail(self, "exitMachine") };
                    let index = region.index;
                    target = self.draft(-1).table.regions.iter().find(|r| r.index == index).and_then(|r| r.off.clone());
                    i += 1;
                }
                Step::Check { element, condition, .. } => {
                    let index = self.postcondition(element, condition);
                    actions.push(TableAction::Postcondition { index });
                    i += 1;
                }
                other => return fail(self, other.kind()),
            }
        }
        Some((actions, target))
    }

    fn postcondition(&mut self, element: &str, condition: &Condition) -> usize {
        if let Some(p) = self.postconditions.iter().find(|x| x.element == element && x.condition == *condition) {
            return p.index;
        }
        let index = self.postconditions.len();
        self.postconditions.push(TablePostcondition {
            index,
            element: element.to_string(),
            condition: condition.clone(),
        });
        index
    }

    /// Steps from `i` entering the inside of state `s` (just entered): routes for its regions, its submachine's
    /// entry point.
    fn inside(&mut self, s: usize, steps: &[Step], i: usize, origin: &Origin) -> (Vec<TableAction>, usize) {
        let regions: Vec<usize> = self.cm.states[s].regions.iter().map(|r| r.index).collect();
        self.route_regions(&regions, steps, i, origin, Some(s))
    }

    /// Steps from `i` entering `region_list` (of state `s`, or the machine's): the routes to enter them that way.
    fn route_regions(
        &mut self,
        region_list: &[usize],
        steps: &[Step],
        mut i: usize,
        origin: &Origin,
        s: Option<usize>,
    ) -> (Vec<TableAction>, usize) {
        let cm = self.cm;
        let state = s.map(|s| &cm.states[s]);
        let regions: HashSet<usize> = region_list.iter().copied().collect();
        let mut actions = Vec::new();
        let mut prefix: Vec<Step> = Vec::new();
        let mut first = true;
        while i < steps.len() {
            let st = &steps[i];
            if matches!(st, Step::Call { .. }) && first && !regions.is_empty() {
                prefix.push(st.clone());
                i += 1;
                continue;
            }
            if let (Some(state), Step::StartSubmachine { state_index, point, .. }) = (state, st)
                && *state_index == state.index
            {
                if let Some(point) = point {
                    let href = state.submachine.as_ref().map(|x| x.href.as_str());
                    let sub = cm.submachines.iter().find(|x| Some(x.href.as_str()) == href);
                    actions.push(TableAction::SubmachineAt {
                        state: Ref { name: state.name.clone(), index: state.index },
                        submachine: sub.map_or(String::new(), |x| x.name.clone()),
                        point: point.clone(),
                        point_index: sub
                            .map_or(-1, |x| x.entry_points.iter().position(|p| p == point).map_or(-1, |p| p as i64)),
                    });
                }
                i += 1;
                continue;
            }
            let Some(r) = region_of_step(cm, st).filter(|r| regions.contains(r)) else { break };
            let start = i;
            i = match st {
                Step::Enter { state_index, .. } => self.skip_inside(*state_index, steps, i + 1),
                _ => i + 1,
            };
            let region_steps = &steps[start..i];
            let pre: Vec<Step> = if first { prefix.clone() } else { Vec::new() };
            first = false;
            if let Step::EnterRegion { .. } = st {
                if !pre.is_empty() {
                    let mut route_steps = pre;
                    route_steps.extend(cm.regions[r].default_steps.iter().cloned());
                    let route = self.route(r, &route_steps, origin);
                    actions.push(TableAction::Route { region: r, route, history: 0 });
                }
                continue;
            }
            // The region's own default entry needs no route.
            if pre.is_empty() && region_steps == cm.regions[r].default_steps.as_slice() {
                continue;
            }
            if let Some(state) = state {
                let target = match st {
                    Step::Enter { state, .. } => state.clone(),
                    Step::Choice { .. } => "a choice".to_string(),
                    _ => "its history".to_string(),
                };
                self.find(origin, Finding { left: None, entered: Some((state.name.clone(), target)), way: None });
            }
            let history = match st {
                Step::RestoreHistory { deep, .. } => {
                    if *deep {
                        2
                    } else {
                        1
                    }
                }
                _ => 0,
            };
            let mut route_steps = pre;
            route_steps.extend(region_steps.iter().cloned());
            let route = self.route(r, &route_steps, origin);
            actions.push(TableAction::Route { region: r, route, history });
        }
        (actions, i)
    }

    /// End of the steps entering the inside of state `s`, from `i`.
    fn skip_inside(&self, s: usize, steps: &[Step], mut i: usize) -> usize {
        let cm = self.cm;
        let regions: HashSet<usize> = cm.states[s].regions.iter().map(|r| r.index).collect();
        let mut first = true;
        while i < steps.len() {
            let st = &steps[i];
            if matches!(st, Step::Call { .. }) && first && !regions.is_empty() {
                i += 1;
                continue;
            }
            if matches!(st, Step::StartSubmachine { state_index, .. } if *state_index == s) {
                i += 1;
                continue;
            }
            let Some(_) = region_of_step(cm, st).filter(|r| regions.contains(r)) else { break };
            first = false;
            i = match st {
                Step::Enter { state_index, .. } => self.skip_inside(*state_index, steps, i + 1),
                _ => i + 1,
            };
        }
        i
    }

    /// A choice pseudostate in table `key`, with an anonymous row per branch.
    fn choice(&mut self, st: &Step, key: i64, origin: &Origin) -> TableState {
        let Step::Choice { label, branches, has_else, .. } = st else { unreachable!() };
        self.choice_count += 1;
        let p = pseudo(format!("choice_{}", self.choice_count), "choice");
        self.draft(key).table.pseudostates.push(p.clone());
        for (n, b) in branches.iter().enumerate() {
            let guard = match &b.guard {
                Some(g) => vec![GuardTerm::Condition { condition: g.clone() }],
                None => Vec::new(),
            };
            // Each branch is a way of its own. Only pseudostates the origin notes count (default entries and the
            // machine's entry points note none).
            let branch_way = way_via(&b.via, &b.steps);
            let way = branch_way.iter().filter(|v| origin.via.iter().any(|x| x.id == v.id)).cloned().collect();
            let branch_key = format!("{}|{}.{n}", origin.key, p.name);
            let branch = Origin { key: branch_key, way, branch_way: Some(branch_way), ..origin.clone() };
            self.lower_fire(p.clone(), key, None, guard, &b.steps, &branch, 1, vec![]);
        }
        if !has_else {
            let actions = vec![TableAction::NoBranch { label: label.clone() }];
            self.add(key, row(p.clone(), None, vec![], actions, None, "no branch can be taken".into()), 1);
        }
        p
    }

    // -------------------------------------------------------------- notes

    /// What lowering `origin` worked around: the pseudostates it went through, and what none of them explains.
    fn note_origin(&mut self, origin: &Origin) {
        for v in origin
            .via
            .iter()
            .filter(|v| ["exitPoint", "join", "entryPoint", "fork", "shallowHistory", "deepHistory"].contains(&v.kind))
        {
            let (kind, detail) = match v.kind {
                "shallowHistory" => ("history", "shallow"),
                "deepHistory" => ("history", "deep"),
                k => (k, ""),
            };
            self.notes.push(note(kind, &v.id, &v.name, "", detail));
        }
        let found = origin.found.borrow();
        if let Some((from, through)) = &found.left {
            self.notes.push(note("leave", &origin.id, &origin.label, from, through));
        }
        if let Some((state, target)) = &found.entered {
            self.notes.push(note("enter", &origin.id, &origin.label, state, target));
        }
    }

    /// Orthogonal regions that react to the same signal run one after the other: a later region's guards see the
    /// earlier ones' effects.
    fn orthogonal_notes(&mut self) {
        let cm = self.cm;
        let owners: Vec<Option<&Rc<CodeState>>> =
            std::iter::once(None).chain(cm.states.iter().filter(|s| s.regions.len() > 1).map(Some)).collect();
        for owner in owners {
            let regions: Vec<usize> = match owner {
                Some(o) => o.regions.iter().map(|r| r.index).collect(),
                None => cm.top_regions.iter().map(|r| r.index).collect(),
            };
            if regions.len() < 2 {
                continue;
            }
            // Signal → (region, guarded), in order of discovery.
            let mut signals: Vec<(String, Vec<(usize, bool)>)> = Vec::new();
            for &r in &regions {
                let mut inside = Vec::new();
                fn walk(cm: &CodeModel, rr: usize, inside: &mut Vec<usize>) {
                    for s in &cm.regions[rr].states {
                        inside.push(s.index);
                        for sub in &cm.states[s.index].regions {
                            walk(cm, sub.index, inside);
                        }
                    }
                }
                walk(cm, r, &mut inside);
                for s in inside {
                    for h in &cm.states[s].handlers {
                        let guarded = h.candidates.iter().any(|c| c.guard.is_some() || c.precondition.is_some());
                        let list = match signals.iter().position(|(name, _)| *name == h.signal) {
                            Some(p) => &mut signals[p].1,
                            None => {
                                signals.push((h.signal.clone(), Vec::new()));
                                &mut signals.last_mut().unwrap().1
                            }
                        };
                        match list.iter_mut().find(|x| x.0 == r) {
                            Some(known) => known.1 |= guarded,
                            None => list.push((r, guarded)),
                        }
                    }
                }
            }
            let name = owner.map_or(cm.machine.name.as_str(), |o| o.name.as_str());
            let id = owner.map_or("", |o| o.id.as_str());
            for (signal, list) in &signals {
                if list.len() < 2 || !list[1..].iter().any(|x| x.1) {
                    continue;
                }
                self.notes.push(note("orthogonal", id, name, name, signal));
            }
        }
    }
}

fn row(
    source: TableState,
    trigger: Option<Trigger>,
    guard: Vec<GuardTerm>,
    actions: Vec<TableAction>,
    target: Option<TableState>,
    comment: String,
) -> TableRow {
    TableRow { source, initial: false, trigger, guard, actions, target, comment }
}

fn table_trigger(e: &Ref) -> Trigger {
    Trigger { kind: "table", name: e.name.clone(), index: e.index as i64 }
}

fn call_actions(steps: &[Step]) -> Vec<TableAction> {
    steps
        .iter()
        .filter_map(|s| match s {
            Step::Call { action } => Some(TableAction::Call { action: action.clone() }),
            _ => None,
        })
        .collect()
}
