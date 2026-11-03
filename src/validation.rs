//! UML 2.5.1 well-formedness rules, the text grammar and reachability: the
//! same checks and messages as fsm-editor-vscode/src/validation.ts.

use std::collections::{HashMap, HashSet};

use crate::expressions as expr;
use crate::model::{FsmModel, MachineKind, ModelIndex, PointKind, SubmachineInfo, TransitionKind, VertexType, js_trim};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
            Severity::Info => "info",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Issue {
    /// Id of the offending vertex or transition ('' for the machine itself).
    pub id: String,
    pub severity: Severity,
    pub message: String,
}

fn type_label(t: VertexType) -> &'static str {
    match t {
        VertexType::State => "state",
        VertexType::Final => "final state",
        VertexType::Initial => "initial pseudostate",
        VertexType::ShallowHistory => "shallow history",
        VertexType::DeepHistory => "deep history",
        VertexType::Choice => "choice",
        VertexType::Junction => "junction",
        VertexType::Fork => "fork",
        VertexType::Join => "join",
        VertexType::EntryPoint => "entry point",
        VertexType::ExitPoint => "exit point",
        VertexType::ConnectionPointRef => "connection point reference",
        VertexType::Terminate => "terminate",
        VertexType::Comment => "comment",
    }
}

struct Checker<'a> {
    model: &'a FsmModel,
    ix: ModelIndex<'a>,
    submachines: &'a HashMap<String, SubmachineInfo>,
    issues: Vec<Issue>,
}

impl Checker<'_> {
    fn add(&mut self, id: &str, severity: Severity, message: String) {
        self.issues.push(Issue { id: id.to_string(), severity, message });
    }

    fn name(&self, v: Option<usize>) -> String {
        match v {
            None => "?".to_string(),
            Some(v) => {
                let v = self.ix.v(v);
                if v.name.is_empty() { type_label(v.vtype).to_string() } else { format!("'{}'", v.name) }
            }
        }
    }

    fn machine_name(&self, href: &str) -> String {
        match self.submachines.get(href).map(|s| s.name.as_str()) {
            Some(n) if !n.is_empty() => format!("'{n}'"),
            _ => format!("'{href}'"),
        }
    }
}

/// Checks the UML 2.5.1 well-formedness rules that apply to state machines.
/// `submachines` describes the machines referenced by submachine states, when known.
pub fn validate(model: &FsmModel, submachines: &HashMap<String, SubmachineInfo>) -> Vec<Issue> {
    let mut c = Checker { model, ix: ModelIndex::new(model), submachines, issues: Vec::new() };
    let protocol = model.kind == MachineKind::Protocol;
    use Severity::*;

    // Identity
    let mut seen = HashSet::new();
    for id in model.vertices.iter().map(|v| &v.id).chain(model.transitions.iter().map(|t| &t.id)) {
        if !seen.insert(id.as_str()) {
            c.add(id, Error, format!("Duplicate id '{id}'."));
        }
    }

    // Containment
    for (vi, v) in model.vertices.iter().enumerate() {
        match v.vtype {
            VertexType::EntryPoint | VertexType::ExitPoint => {
                // Either on the border of a state, or a connection point of the machine itself (top-level region).
                let owner = c.ix.vertex(&v.parent);
                let machine_level = model.regions.iter().any(|r| r.id == v.parent);
                if machine_level {
                    if v.name.is_empty() {
                        c.add(&v.id, Warning, "Connection points of the state machine need a name so submachine states can reference them.".into());
                    }
                } else if owner.is_none_or(|o| c.ix.v(o).vtype != VertexType::State) {
                    let msg = format!(
                        "{}: entry and exit points belong on the border of a state or at the top level of the state machine.",
                        c.name(Some(vi))
                    );
                    c.add(&v.id, Error, msg);
                } else if let Some(sub) = &c.ix.v(owner.unwrap()).submachine {
                    let msg = format!(
                        "A submachine state cannot own entry/exit points; use a connection point reference to a point of {} instead.",
                        c.machine_name(sub)
                    );
                    c.add(&v.id, Error, msg);
                } else if c.ix.v(owner.unwrap()).regions.is_empty() {
                    let msg =
                        format!("Only composite states can have entry/exit points; add a region to {}.", c.name(owner));
                    c.add(&v.id, Warning, msg);
                }
            }
            VertexType::ConnectionPointRef => {
                let owner = c.ix.vertex(&v.parent).map(|o| c.ix.v(o));
                if owner.is_none_or(|o| o.vtype != VertexType::State || o.submachine.is_none()) {
                    c.add(
                        &v.id,
                        Error,
                        "A connection point reference must be placed on the border of a submachine state.".into(),
                    );
                }
            }
            _ => {
                if c.ix.region_owner(&v.parent).is_none() {
                    let msg = format!("{} is not contained in a known region.", c.name(Some(vi)));
                    c.add(&v.id, Error, msg);
                }
            }
        }
    }

    // Regions
    let all_regions: Vec<_> =
        model.regions.iter().chain(model.vertices.iter().flat_map(|v| v.regions.iter())).collect();
    for r in all_regions {
        let kids: Vec<&crate::model::Vertex> = c.ix.children_of(&r.id).into_iter().map(|k| c.ix.v(k)).collect();
        let count = |t: VertexType| kids.iter().filter(|k| k.vtype == t).map(|k| k.id.as_str()).collect::<Vec<_>>();
        let owner = c.ix.region_owner(&r.id).flatten();
        let where_ = match owner {
            Some(_) => format!("region of {}", c.name(owner)),
            None => "top-level region".to_string(),
        };
        for (t, label) in [
            (VertexType::Initial, "initial pseudostate"),
            (VertexType::ShallowHistory, "shallow history"),
            (VertexType::DeepHistory, "deep history"),
        ] {
            for id in count(t).into_iter().skip(1) {
                c.add(id, Error, format!("A {where_} can contain at most one {label}."));
            }
        }
        let histories: Vec<&str> = kids
            .iter()
            .filter(|k| matches!(k.vtype, VertexType::ShallowHistory | VertexType::DeepHistory))
            .map(|k| k.id.as_str())
            .collect();
        if !histories.is_empty() && owner.is_none() {
            for id in histories {
                c.add(id, Warning, "History pseudostates are only meaningful inside a composite state.".into());
            }
        }
        // Default entry of a composite state targeted directly needs an initial pseudostate.
        if let Some(o) = owner
            && count(VertexType::Initial).is_empty()
        {
            let owner_v = c.ix.v(o);
            let entered = model.transitions.iter().any(|t| {
                t.target == owner_v.id
                    && t.kind != TransitionKind::Internal
                    && !c.ix.is_inside(c.ix.vertex(&t.source).unwrap_or(o), &owner_v.id)
            });
            if entered && kids.iter().any(|k| k.vtype == VertexType::State) {
                let region = if r.name.is_empty() { "region".to_string() } else { format!("region '{}'", r.name) };
                let msg =
                    format!("{} is entered by default but its {region} has no initial pseudostate.", c.name(owner));
                c.add(&owner_v.id, Warning, msg);
            }
        }
    }

    // Names unique within a region
    let mut by_region: HashMap<&str, HashSet<&str>> = HashMap::new();
    for v in &model.vertices {
        if v.vtype != VertexType::State || v.name.is_empty() {
            continue;
        }
        if !by_region.entry(&v.parent).or_default().insert(&v.name) {
            c.add(&v.id, Warning, format!("Another state named '{}' exists in the same region.", v.name));
        }
    }

    // Vertices
    for (vi, v) in model.vertices.iter().enumerate() {
        let out: Vec<_> = c.ix.outgoing(&v.id).into_iter().filter(|t| t.kind != TransitionKind::Internal).collect();
        let inc: Vec<_> = c.ix.incoming(&v.id).into_iter().filter(|t| t.kind != TransitionKind::Internal).collect();
        match v.vtype {
            VertexType::Initial => {
                if out.len() != 1 {
                    c.add(&v.id, Error, "An initial pseudostate must have exactly one outgoing transition.".into());
                }
                if !inc.is_empty() {
                    c.add(&v.id, Error, "An initial pseudostate cannot have incoming transitions.".into());
                }
                for t in &out {
                    if !t.guard.is_empty() {
                        c.add(
                            &t.id,
                            Error,
                            "The transition leaving an initial pseudostate cannot have a guard.".into(),
                        );
                    }
                }
            }
            VertexType::Final => {
                if !out.is_empty() {
                    c.add(&v.id, Error, "A final state cannot have outgoing transitions.".into());
                }
                if !v.regions.is_empty() || !v.entry.is_empty() || !v.exit.is_empty() || !v.do_activity.is_empty() {
                    c.add(&v.id, Error, "A final state cannot have regions or entry/exit/do behaviors.".into());
                }
            }
            VertexType::Terminate => {
                if !out.is_empty() {
                    c.add(&v.id, Error, "A terminate pseudostate cannot have outgoing transitions.".into());
                }
            }
            VertexType::ShallowHistory | VertexType::DeepHistory => {
                if out.len() > 1 {
                    c.add(
                        &v.id,
                        Error,
                        "A history pseudostate can have at most one outgoing (default) transition.".into(),
                    );
                }
            }
            VertexType::Fork => {
                if inc.len() != 1 {
                    c.add(&v.id, Error, "A fork must have exactly one incoming transition.".into());
                }
                if out.len() < 2 {
                    c.add(&v.id, Error, "A fork must have at least two outgoing transitions.".into());
                }
                for t in &out {
                    if !t.guard.is_empty() || !t.triggers.is_empty() {
                        c.add(&t.id, Error, "Transitions leaving a fork cannot have guards or triggers.".into());
                    }
                }
                let regions: Vec<Option<&str>> =
                    out.iter().map(|t| c.ix.vertex(&t.target).map(|g| c.ix.v(g).parent.as_str())).collect();
                if distinct(&regions) != regions.len() {
                    c.add(&v.id, Warning, "The targets of a fork should be in different orthogonal regions.".into());
                }
            }
            VertexType::Join => {
                if out.len() != 1 {
                    c.add(&v.id, Error, "A join must have exactly one outgoing transition.".into());
                }
                if inc.len() < 2 {
                    c.add(&v.id, Error, "A join must have at least two incoming transitions.".into());
                }
                for t in &inc {
                    if !t.guard.is_empty() || !t.triggers.is_empty() {
                        c.add(&t.id, Error, "Transitions entering a join cannot have guards or triggers.".into());
                    }
                }
                let regions: Vec<Option<&str>> =
                    inc.iter().map(|t| c.ix.vertex(&t.source).map(|s| c.ix.v(s).parent.as_str())).collect();
                if distinct(&regions) != regions.len() {
                    c.add(&v.id, Warning, "The sources of a join should be in different orthogonal regions.".into());
                }
            }
            VertexType::Choice | VertexType::Junction => {
                let kind = v.vtype.as_str();
                if inc.is_empty() {
                    c.add(&v.id, Error, format!("A {kind} must have at least one incoming transition."));
                }
                if out.is_empty() {
                    c.add(&v.id, Error, format!("A {kind} must have at least one outgoing transition."));
                }
                if out.len() > 1 {
                    if out.iter().any(|t| js_trim(&t.guard).is_empty()) {
                        c.add(
                            &v.id,
                            Warning,
                            format!("Every branch of a {kind} with several outgoing transitions should have a guard."),
                        );
                    }
                    if !out.iter().any(|t| js_trim(&t.guard) == "else") {
                        c.add(&v.id, Info, format!("Consider an [else] branch so the {kind} can always be left."));
                    }
                }
            }
            VertexType::EntryPoint => {
                let owner = c.ix.vertex(&v.parent);
                if owner.is_none() && out.is_empty() {
                    let msg =
                        format!("Entry point {} of the state machine has no outgoing transition.", c.name(Some(vi)));
                    c.add(&v.id, Warning, msg);
                }
                if let Some(o) = owner {
                    for t in &out {
                        if let Some(target) = c.ix.vertex(&t.target)
                            && !c.ix.is_inside(target, &c.ix.v(o).id)
                        {
                            let msg = format!(
                                "Transitions leaving entry point {} must target a vertex inside {}.",
                                c.name(Some(vi)),
                                c.name(Some(o))
                            );
                            c.add(&t.id, Error, msg);
                        }
                    }
                }
            }
            VertexType::ExitPoint => {
                let owner = c.ix.vertex(&v.parent);
                if owner.is_none() && !out.is_empty() {
                    for t in &out {
                        let msg = format!(
                            "Exit point {} of the state machine cannot have outgoing transitions; the referencing submachine state continues from its connection point reference.",
                            c.name(Some(vi))
                        );
                        c.add(&t.id, Error, msg);
                    }
                }
                if let Some(o) = owner {
                    for t in &out {
                        if let Some(target) = c.ix.vertex(&t.target) {
                            let oid = &c.ix.v(o).id;
                            if c.ix.v(target).id == *oid || c.ix.is_inside(target, oid) {
                                let msg = format!(
                                    "Transitions leaving exit point {} must target a vertex outside {}.",
                                    c.name(Some(vi)),
                                    c.name(Some(o))
                                );
                                c.add(&t.id, Error, msg);
                            }
                        }
                    }
                }
            }
            VertexType::ConnectionPointRef => {
                let owner = c.ix.vertex(&v.parent);
                let kind = v.point_kind;
                let owner_sub = owner.and_then(|o| c.ix.v(o).submachine.clone());
                let info = owner_sub.as_ref().and_then(|s| c.submachines.get(s));
                let target = info.filter(|i| i.found).and_then(|i| i.points.iter().find(|p| p.id == v.point_ref));
                let label = match target {
                    Some(t) if !t.name.is_empty() => format!("'{}'", t.name),
                    _ => "connection point reference".to_string(),
                };
                if v.point_ref.is_empty() {
                    c.add(
                        &v.id,
                        Error,
                        "The connection point reference does not refer to an entry or exit point of the submachine."
                            .into(),
                    );
                } else if let (Some(sub), Some(true)) = (&owner_sub, info.map(|i| i.found)) {
                    match target {
                        None => {
                            let msg = format!(
                                "{} has no entry or exit point with id '{}'.",
                                c.machine_name(sub),
                                v.point_ref
                            );
                            c.add(&v.id, Error, msg);
                        }
                        Some(t) if t.kind != kind => {
                            let msg = format!(
                                "{label} is an {} point of {}, but it is referenced as an {} point.",
                                t.kind.as_str(),
                                c.machine_name(sub),
                                kind.as_str()
                            );
                            c.add(&v.id, Error, msg);
                        }
                        _ => {}
                    }
                }
                if kind == PointKind::Entry {
                    if !out.is_empty() {
                        c.add(&v.id, Error, format!("Entry reference {label} cannot have outgoing transitions; execution continues at the entry point inside the submachine."));
                    }
                    if inc.is_empty() {
                        c.add(&v.id, Warning, format!("Entry reference {label} has no incoming transition."));
                    }
                    for t in &inc {
                        if let (Some(src), Some(o)) = (c.ix.vertex(&t.source), owner) {
                            let oid = &c.ix.v(o).id;
                            if c.ix.v(src).id == *oid || c.ix.is_inside(src, oid) {
                                let msg = format!(
                                    "Transitions into entry reference {label} must come from outside {}.",
                                    c.name(Some(o))
                                );
                                c.add(&t.id, Error, msg);
                            }
                        }
                    }
                } else {
                    if !inc.is_empty() {
                        c.add(&v.id, Error, format!("Exit reference {label} cannot have incoming transitions; it is reached when the submachine leaves through its exit point."));
                    }
                    if out.is_empty() {
                        c.add(&v.id, Warning, format!("Exit reference {label} has no outgoing transition."));
                    }
                }
                if let Some(o) = owner {
                    let oid = &c.ix.v(o).id;
                    let dup = model.vertices.iter().position(|x| {
                        !std::ptr::eq(x, v)
                            && x.vtype == VertexType::ConnectionPointRef
                            && x.parent == *oid
                            && !x.point_ref.is_empty()
                            && x.point_ref == v.point_ref
                    });
                    if dup.is_some_and(|d| d < vi) {
                        let msg = format!("{} already references {label}.", c.name(Some(o)));
                        c.add(&v.id, Warning, msg);
                    }
                }
            }
            VertexType::State => {
                if let Some(sub) = &v.submachine {
                    if c.submachines.get(sub).is_some_and(|i| !i.found) {
                        c.add(&v.id, Error, format!("The referenced state machine '{sub}' was not found."));
                    }
                    if !v.regions.is_empty() {
                        let msg = format!("Submachine state {} cannot also own regions.", c.name(Some(vi)));
                        c.add(&v.id, Error, msg);
                    }
                }
                if protocol && (!v.entry.is_empty() || !v.exit.is_empty() || !v.do_activity.is_empty()) {
                    let msg = format!(
                        "States of a protocol state machine cannot have entry/exit/do behaviors ({}).",
                        c.name(Some(vi))
                    );
                    c.add(&v.id, Error, msg);
                }
            }
            VertexType::Comment => {}
        }
    }

    // Transitions
    for t in &model.transitions {
        let s = c.ix.vertex(&t.source);
        let g = c.ix.vertex(&t.target);
        if s.is_none() {
            c.add(&t.id, Error, format!("Transition source '{}' does not exist.", t.source));
        }
        if g.is_none() {
            c.add(&t.id, Error, format!("Transition target '{}' does not exist.", t.target));
        }
        let (Some(s), Some(g)) = (s, g) else { continue };
        let (sv, gv) = (c.ix.v(s), c.ix.v(g));
        if sv.vtype == VertexType::Comment || gv.vtype == VertexType::Comment {
            c.add(&t.id, Error, "Comments cannot be connected by transitions.".into());
            continue;
        }
        if (sv.vtype.is_pseudostate() || sv.vtype == VertexType::ConnectionPointRef) && !t.triggers.is_empty() {
            let msg = format!("Transitions leaving a pseudostate cannot have triggers (from {}).", c.name(Some(s)));
            c.add(&t.id, Error, msg);
        }
        if t.kind == TransitionKind::Internal && (s != g || sv.vtype != VertexType::State) {
            c.add(&t.id, Error, "An internal transition must start and end on the same state.".into());
        }
        if t.kind == TransitionKind::Local {
            let composite = sv.vtype == VertexType::State && !sv.regions.is_empty();
            let container = if sv.vtype == VertexType::EntryPoint { &sv.parent } else { &sv.id };
            if !(composite || sv.vtype == VertexType::EntryPoint) || !(g == s || c.ix.is_inside(g, container)) {
                c.add(
                    &t.id,
                    Error,
                    "A local transition must go from a composite state to a vertex it contains.".into(),
                );
            }
        }
        if protocol {
            if !t.effect.is_empty() {
                c.add(&t.id, Error, "Protocol transitions cannot have effects.".into());
            }
            if !t.guard.is_empty() {
                c.add(&t.id, Warning, "Use a precondition instead of a guard in a protocol state machine.".into());
            }
        } else if !t.precondition.is_empty() || !t.postcondition.is_empty() {
            c.add(&t.id, Warning, "Pre/postconditions only apply to protocol state machines and are ignored.".into());
        }
        if sv.vtype == VertexType::State
            && gv.vtype == VertexType::State
            && t.triggers.is_empty()
            && t.guard.is_empty()
            && s != g
            && sv.regions.is_empty()
            && c.ix
                .outgoing(&sv.id)
                .iter()
                .filter(|o| o.kind != TransitionKind::Internal && o.triggers.is_empty())
                .count()
                > 1
        {
            let msg = format!("{} has several completion transitions without triggers or guards.", c.name(Some(s)));
            c.add(&t.id, Warning, msg);
        }
    }

    check_pseudostate_cycles(&mut c);
    check_text(&mut c);
    check_reachability(&mut c);
    c.issues
}

/// Pseudostates that transitions go through in a cycle: a compound transition taking the cycle would never reach a
/// state, and the code generator can't express it. One error per group of pseudostates linked by cycles, on its first
/// vertex, naming the shortest cycle through it.
fn check_pseudostate_cycles(c: &mut Checker) {
    let model = c.model;
    let ix = &c.ix;
    // The pseudostates a compound transition goes on from (the machine's own entry and exit points end it).
    let passes = |v: usize| {
        let v = ix.v(v);
        match v.vtype {
            VertexType::Junction
            | VertexType::Choice
            | VertexType::ShallowHistory
            | VertexType::DeepHistory
            | VertexType::Fork => true,
            VertexType::EntryPoint | VertexType::ExitPoint => ix.has_vertex(&v.parent),
            _ => false,
        }
    };
    let next = |v: usize| -> Vec<usize> {
        ix.outgoing(&ix.v(v).id)
            .iter()
            .filter(|t| t.kind != TransitionKind::Internal)
            .filter_map(|t| ix.vertex(&t.target))
            .filter(|&w| passes(w))
            .collect()
    };
    let previous = |v: usize| -> Vec<usize> {
        ix.incoming(&ix.v(v).id)
            .iter()
            .filter(|t| t.kind != TransitionKind::Internal)
            .filter_map(|t| ix.vertex(&t.source))
            .filter(|&w| passes(w))
            .collect()
    };
    // `start` and every vertex reached from it through `step`, breadth first, with the vertex each was reached from.
    let walk = |start: usize, step: &dyn Fn(usize) -> Vec<usize>| {
        let mut from: HashMap<usize, Option<usize>> = HashMap::from([(start, None)]);
        let mut queue = vec![start];
        let mut i = 0;
        while i < queue.len() {
            for w in step(queue[i]) {
                if let std::collections::hash_map::Entry::Vacant(e) = from.entry(w) {
                    e.insert(Some(queue[i]));
                    queue.push(w);
                }
            }
            i += 1;
        }
        (queue, from)
    };
    let mut done: HashSet<usize> = HashSet::new();
    let mut found: Vec<(usize, Vec<usize>)> = Vec::new();
    for v in 0..model.vertices.len() {
        // A vertex whose id is used again later is not the one transitions refer to.
        if !passes(v) || ix.vertex(&model.vertices[v].id) != Some(v) || done.contains(&v) {
            continue;
        }
        let (order, from) = walk(v, &next);
        // The shortest cycle through v ends with the first vertex reached that has a transition back to v.
        let Some(last) = order.iter().copied().find(|&u| next(u).contains(&v)) else { continue };
        let (_, backward) = walk(v, &previous);
        done.extend(order.iter().copied().filter(|u| backward.contains_key(u)));
        let mut cycle = vec![v];
        let mut u = Some(last);
        while let Some(x) = u.filter(|&x| x != v) {
            cycle.insert(1, x);
            u = from[&x];
        }
        cycle.push(v);
        found.push((v, cycle));
    }
    for (v, cycle) in found {
        let path: Vec<String> = cycle.iter().map(|&x| c.name(Some(x))).collect();
        let types: Vec<VertexType> = cycle.iter().map(|&x| model.vertices[x].vtype).collect();
        let msg = format!("{} form a cycle without a state: {}.", cycle_kinds(&types), path.join(" -> "));
        c.add(&model.vertices[v].id, Severity::Error, msg);
    }
}

/// The kinds of pseudostates in `cycle`: `Junctions`, `Junctions and choices`, ... (as in the code generator's error).
fn cycle_kinds(cycle: &[VertexType]) -> String {
    use VertexType::*;
    let kinds: [(&str, &[VertexType]); 6] = [
        ("junctions", &[Junction]),
        ("choices", &[Choice]),
        ("history pseudostates", &[ShallowHistory, DeepHistory]),
        ("entry points", &[EntryPoint]),
        ("exit points", &[ExitPoint]),
        ("forks", &[Fork]),
    ];
    let found: Vec<&str> =
        kinds.iter().filter(|(_, types)| cycle.iter().any(|t| types.contains(t))).map(|(kind, _)| *kind).collect();
    let text = match found.split_last() {
        Some((last, rest)) if !rest.is_empty() => format!("{} and {last}", rest.join(", ")),
        _ => found.first().copied().unwrap_or_default().to_string(),
    };
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |f| f.to_uppercase().collect::<String>() + chars.as_str())
}

fn distinct(items: &[Option<&str>]) -> usize {
    items.iter().collect::<HashSet<_>>().len()
}

/// Behaviors and conditions are argument-less function calls; triggers are event names or after(...).
fn check_text(c: &mut Checker) {
    let model = c.model;
    let report = |c: &mut Checker, id: &str, what: &str, r: Result<(), String>| {
        if let Err(e) = r {
            c.add(id, Severity::Error, format!("{what}: {e}"));
        }
    };
    for (vi, v) in model.vertices.iter().enumerate() {
        if v.vtype != VertexType::State {
            continue;
        }
        let n = c.name(Some(vi));
        report(c, &v.id, &format!("entry of {n}"), expr::check_actions(&v.entry));
        report(c, &v.id, &format!("exit of {n}"), expr::check_actions(&v.exit));
        report(c, &v.id, &format!("do of {n}"), expr::check_actions(&v.do_activity));
        report(c, &v.id, &format!("Invariant of {n}"), expr::check_condition(&v.invariant));
        if !v.stereotype.is_empty() {
            report(c, &v.id, &format!("Stereotype of {n}"), expr::check_name(&v.stereotype));
        }
        for d in &v.deferrable {
            report(c, &v.id, &format!("Deferrable event of {n}"), expr::check_event(d));
        }
    }
    for t in &model.transitions {
        let s = c.ix.vertex(&t.source).map(|s| c.ix.v(s).vtype);
        for trig in &t.triggers {
            report(c, &t.id, "Trigger", expr::check_trigger(trig));
        }
        let allow_else = matches!(s, Some(VertexType::Choice | VertexType::Junction));
        report(c, &t.id, "Guard", expr::check_guard(&t.guard, allow_else));
        report(c, &t.id, "Effect", expr::check_actions(&t.effect));
        report(c, &t.id, "Precondition", expr::check_condition(&t.precondition));
        report(c, &t.id, "Postcondition", expr::check_condition(&t.postcondition));
        let times = t.triggers.iter().any(|x| expr::time_trigger_ms(x).is_some());
        if times && s.is_some_and(|s| s != VertexType::State) {
            c.add(
                &t.id,
                Severity::Error,
                "Time triggers (after) can only be used on transitions leaving a state.".into(),
            );
        }
    }
}

fn check_reachability(c: &mut Checker) {
    let model = c.model;
    let root_initial = c.ix.initial_of(model.regions.first().map_or("", |r| r.id.as_str()));
    // The machine can also be entered through its own entry points (from a referencing submachine state).
    let machine_entries: Vec<usize> = (0..model.vertices.len())
        .filter(|&i| {
            let v = &model.vertices[i];
            v.vtype == VertexType::EntryPoint && model.regions.iter().any(|r| r.id == v.parent)
        })
        .collect();
    if root_initial.is_none() && machine_entries.is_empty() {
        if model.vertices.iter().any(|v| v.vtype == VertexType::State) {
            c.add("", Severity::Warning, "The top-level region has no initial pseudostate.".into());
        }
        return;
    }
    let mut reached: HashSet<String> = HashSet::new();
    let mut queue = std::collections::VecDeque::new();
    let visit = |v: Option<usize>, reached: &mut HashSet<String>, queue: &mut std::collections::VecDeque<usize>| {
        if let Some(v) = v
            && reached.insert(model.vertices[v].id.clone())
        {
            queue.push_back(v);
        }
    };
    visit(root_initial, &mut reached, &mut queue);
    for e in machine_entries {
        visit(Some(e), &mut reached, &mut queue);
    }
    while let Some(vi) = queue.pop_front() {
        // Entering a vertex enters its enclosing states and their other orthogonal regions.
        for a in c.ix.ancestors(vi) {
            visit(Some(a), &mut reached, &mut queue);
        }
        let v = &model.vertices[vi];
        if v.vtype == VertexType::State {
            for r in &v.regions {
                visit(c.ix.initial_of(&r.id), &mut reached, &mut queue);
            }
            for cp in c.ix.connection_points(vi) {
                let p = &model.vertices[cp];
                if p.vtype == VertexType::ExitPoint
                    || (p.vtype == VertexType::ConnectionPointRef && p.point_kind == PointKind::Exit)
                {
                    visit(Some(cp), &mut reached, &mut queue);
                }
            }
        }
        for t in c.ix.outgoing(&v.id) {
            visit(c.ix.vertex(&t.target), &mut reached, &mut queue);
        }
    }
    for (vi, v) in model.vertices.iter().enumerate() {
        if v.vtype == VertexType::State && !reached.contains(&v.id) {
            let msg = format!("State {} can never be entered.", c.name(Some(vi)));
            c.add(&v.id, Severity::Warning, msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Region, Transition, Vertex};

    /// A machine with states A and B, A entered first, and `vertices` (id, type, name, parent).
    fn machine(vertices: &[(&str, VertexType, &str, &str)]) -> FsmModel {
        let mut m = FsmModel {
            id: "sm".into(),
            name: "M".into(),
            kind: MachineKind::Behavioral,
            context: String::new(),
            documentation: String::new(),
            regions: vec![Region { id: "r".into(), name: String::new() }],
            vertices: vec![
                Vertex::new("A", VertexType::State, "A", "r"),
                Vertex::new("B", VertexType::State, "B", "r"),
                Vertex::new("i", VertexType::Initial, "", "r"),
            ],
            transitions: Vec::new(),
        };
        m.vertices.extend(vertices.iter().map(|&(id, t, name, parent)| Vertex::new(id, t, name, parent)));
        transition(&mut m, "i", "A", "");
        m
    }

    fn transition(m: &mut FsmModel, source: &str, target: &str, guard: &str) {
        m.transitions.push(Transition {
            id: format!("t{}", m.transitions.len()),
            source: source.into(),
            target: target.into(),
            kind: TransitionKind::External,
            triggers: if source == "A" { vec!["e".into()] } else { Vec::new() },
            guard: guard.into(),
            effect: String::new(),
            precondition: String::new(),
            postcondition: String::new(),
        });
    }

    fn cycles(m: &FsmModel) -> Vec<(String, &'static str, String)> {
        validate(m, &HashMap::new())
            .into_iter()
            .filter(|i| i.message.contains("form a cycle"))
            .map(|i| (i.id, i.severity.as_str(), i.message))
            .collect()
    }

    #[test]
    fn choices_leading_to_each_other() {
        let mut m = machine(&[("c1", VertexType::Choice, "c1", "r"), ("c2", VertexType::Choice, "c2", "r")]);
        transition(&mut m, "A", "c1", "");
        transition(&mut m, "c1", "c2", "g()");
        transition(&mut m, "c1", "B", "else");
        transition(&mut m, "c2", "c1", "h()");
        transition(&mut m, "c2", "B", "else");
        let msg = "Choices form a cycle without a state: 'c1' -> 'c2' -> 'c1'.";
        assert_eq!(cycles(&m), vec![("c1".into(), "error", msg.into())]);
    }

    #[test]
    fn choice_targeting_itself() {
        let mut m = machine(&[("c", VertexType::Choice, "c", "r")]);
        transition(&mut m, "A", "c", "");
        transition(&mut m, "c", "c", "g()");
        transition(&mut m, "c", "B", "else");
        let msg = "Choices form a cycle without a state: 'c' -> 'c'.";
        assert_eq!(cycles(&m), vec![("c".into(), "error", msg.into())]);
    }

    #[test]
    fn unnamed_junction_and_choice() {
        let mut m = machine(&[("c", VertexType::Choice, "", "r"), ("j", VertexType::Junction, "", "r")]);
        transition(&mut m, "A", "j", "");
        transition(&mut m, "j", "c", "");
        transition(&mut m, "c", "j", "g()");
        transition(&mut m, "c", "B", "else");
        let msg = "Junctions and choices form a cycle without a state: choice -> junction -> choice.";
        assert_eq!(cycles(&m), vec![("c".into(), "error", msg.into())]);
    }

    #[test]
    fn one_issue_for_cycles_sharing_pseudostates() {
        let mut m = machine(&[
            ("j1", VertexType::Junction, "j1", "r"),
            ("j2", VertexType::Junction, "j2", "r"),
            ("j3", VertexType::Junction, "j3", "r"),
            ("j4", VertexType::Junction, "j4", "r"),
        ]);
        transition(&mut m, "A", "j1", "");
        transition(&mut m, "j1", "j2", "a()");
        transition(&mut m, "j1", "j3", "b()");
        transition(&mut m, "j1", "B", "else");
        transition(&mut m, "j2", "j4", "");
        transition(&mut m, "j4", "j1", "");
        transition(&mut m, "j3", "j1", "");
        let msg = "Junctions form a cycle without a state: 'j1' -> 'j3' -> 'j1'.";
        assert_eq!(cycles(&m), vec![("j1".into(), "error", msg.into())]);
    }

    #[test]
    fn loops_through_states_and_machine_entry_points() {
        let mut m = machine(&[
            ("c", VertexType::Choice, "c", "r"),
            ("ep", VertexType::EntryPoint, "ep", "r"),
            ("j", VertexType::Junction, "j", "r"),
        ]);
        transition(&mut m, "A", "c", "");
        transition(&mut m, "c", "A", "g()");
        transition(&mut m, "c", "B", "else");
        transition(&mut m, "ep", "j", "");
        transition(&mut m, "j", "ep", "g()");
        transition(&mut m, "j", "B", "else");
        assert!(cycles(&m).is_empty());
    }
}
