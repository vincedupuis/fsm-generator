//! In-memory UML state machine model, as read from a `.fsm` file (see xmi.rs).
//!
//! The hierarchy is flat: every vertex points to its owner through `parent`,
//! which is either a region id (root region of the machine or a region of a
//! composite state) or, for connection points, the id of the owning state.
//!
//! Entry/exit points whose parent is a top-level region are connection points
//! of the state machine itself; a submachine state that references this machine
//! exposes them through `ConnectionPointRef` vertices placed on its border.
//!
//! Only what validation and code generation need is kept: no layout.

use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VertexType {
    State,
    Final,
    Initial,
    ShallowHistory,
    DeepHistory,
    Choice,
    Junction,
    Fork,
    Join,
    EntryPoint,
    ExitPoint,
    Terminate,
    ConnectionPointRef,
    Comment,
}

impl VertexType {
    /// The name used in the file format and in messages (`shallowHistory`, `entryPoint`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            VertexType::State => "state",
            VertexType::Final => "final",
            VertexType::Initial => "initial",
            VertexType::ShallowHistory => "shallowHistory",
            VertexType::DeepHistory => "deepHistory",
            VertexType::Choice => "choice",
            VertexType::Junction => "junction",
            VertexType::Fork => "fork",
            VertexType::Join => "join",
            VertexType::EntryPoint => "entryPoint",
            VertexType::ExitPoint => "exitPoint",
            VertexType::Terminate => "terminate",
            VertexType::ConnectionPointRef => "connectionPointRef",
            VertexType::Comment => "comment",
        }
    }

    /// Pseudostate kinds of `uml:Pseudostate` (`kind` attribute).
    pub fn pseudostate(kind: &str) -> Option<VertexType> {
        Some(match kind {
            "initial" => VertexType::Initial,
            "deepHistory" => VertexType::DeepHistory,
            "shallowHistory" => VertexType::ShallowHistory,
            "join" => VertexType::Join,
            "fork" => VertexType::Fork,
            "junction" => VertexType::Junction,
            "choice" => VertexType::Choice,
            "entryPoint" => VertexType::EntryPoint,
            "exitPoint" => VertexType::ExitPoint,
            "terminate" => VertexType::Terminate,
            _ => return None,
        })
    }

    pub fn is_pseudostate(self) -> bool {
        matches!(
            self,
            VertexType::Initial
                | VertexType::ShallowHistory
                | VertexType::DeepHistory
                | VertexType::Choice
                | VertexType::Junction
                | VertexType::Fork
                | VertexType::Join
                | VertexType::EntryPoint
                | VertexType::ExitPoint
                | VertexType::Terminate
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointKind {
    Entry,
    Exit,
}

impl PointKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PointKind::Entry => "entry",
            PointKind::Exit => "exit",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    External,
    Internal,
    Local,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MachineKind {
    Behavioral,
    Protocol,
}

impl MachineKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MachineKind::Behavioral => "behavioral",
            MachineKind::Protocol => "protocol",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Region {
    pub id: String,
    pub name: String,
}

/// A vertex. Texts are empty when absent.
#[derive(Debug, Clone)]
pub struct Vertex {
    pub id: String,
    pub vtype: VertexType,
    pub name: String,
    pub parent: String,
    pub regions: Vec<Region>,
    pub entry: String,
    pub exit: String,
    pub do_activity: String,
    pub deferrable: Vec<String>,
    pub invariant: String,
    /// Submachine state: XMI href of the referenced state machine, e.g. `Payment.fsm#sm`.
    pub submachine: Option<String>,
    pub stereotype: String,
    /// Connection point reference: xmi:id of the entry/exit point, inside the submachine's file.
    pub point_ref: String,
    pub point_kind: PointKind,
}

impl Vertex {
    pub fn new(id: &str, vtype: VertexType, name: &str, parent: &str) -> Vertex {
        Vertex {
            id: id.to_string(),
            vtype,
            name: name.to_string(),
            parent: parent.to_string(),
            regions: Vec::new(),
            entry: String::new(),
            exit: String::new(),
            do_activity: String::new(),
            deferrable: Vec::new(),
            invariant: String::new(),
            submachine: None,
            stereotype: String::new(),
            point_ref: String::new(),
            point_kind: PointKind::Entry,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Transition {
    pub id: String,
    pub source: String,
    pub target: String,
    pub kind: TransitionKind,
    pub triggers: Vec<String>,
    pub guard: String,
    pub effect: String,
    /// Protocol state machines only.
    pub precondition: String,
    pub postcondition: String,
}

#[derive(Debug, Clone)]
pub struct FsmModel {
    /// xmi:id of the state machine, used by other files to reference it.
    pub id: String,
    pub name: String,
    pub kind: MachineKind,
    pub context: String,
    pub documentation: String,
    pub regions: Vec<Region>,
    pub vertices: Vec<Vertex>,
    pub transitions: Vec<Transition>,
}

/// An entry or exit point of a machine, as seen by the machines using it as a submachine.
#[derive(Debug, Clone)]
pub struct ConnectionPointInfo {
    pub id: String,
    pub name: String,
    pub kind: PointKind,
}

/// Entry/exit points owned by the state machine itself (placed in a top-level region).
pub fn machine_connection_points(model: &FsmModel) -> Vec<ConnectionPointInfo> {
    model
        .vertices
        .iter()
        .filter(|v| {
            matches!(v.vtype, VertexType::EntryPoint | VertexType::ExitPoint)
                && model.regions.iter().any(|r| r.id == v.parent)
        })
        .map(|v| ConnectionPointInfo {
            id: v.id.clone(),
            name: v.name.clone(),
            kind: if v.vtype == VertexType::EntryPoint { PointKind::Entry } else { PointKind::Exit },
        })
        .collect()
}

/// What is known about a machine referenced by submachine states.
#[derive(Debug, Clone, Default)]
pub struct SubmachineInfo {
    pub found: bool,
    pub name: String,
    pub points: Vec<ConnectionPointInfo>,
}

/// Tree navigation over the flat model. Vertices are designated by their index in `model.vertices`.
pub struct ModelIndex<'a> {
    pub model: &'a FsmModel,
    /// Vertex id → index (the last vertex with that id).
    vertices: HashMap<&'a str, usize>,
    /// Region id → owning state (None for top-level regions).
    region_owner: HashMap<&'a str, Option<usize>>,
}

impl<'a> ModelIndex<'a> {
    pub fn new(model: &'a FsmModel) -> ModelIndex<'a> {
        let mut region_owner = HashMap::new();
        for r in &model.regions {
            region_owner.insert(r.id.as_str(), None);
        }
        let mut vertices = HashMap::new();
        for (i, v) in model.vertices.iter().enumerate() {
            vertices.insert(v.id.as_str(), i);
            for r in &v.regions {
                region_owner.insert(r.id.as_str(), Some(i));
            }
        }
        ModelIndex { model, vertices, region_owner }
    }

    pub fn v(&self, i: usize) -> &'a Vertex {
        &self.model.vertices[i]
    }

    pub fn vertex(&self, id: &str) -> Option<usize> {
        self.vertices.get(id).copied()
    }

    pub fn has_vertex(&self, id: &str) -> bool {
        self.vertices.contains_key(id)
    }

    /// Whether `id` is a known region; `Some(owner)` then, the owner being None for top-level regions.
    pub fn region_owner(&self, id: &str) -> Option<Option<usize>> {
        self.region_owner.get(id).copied()
    }

    pub fn children_of(&self, id: &str) -> Vec<usize> {
        (0..self.model.vertices.len()).filter(|&i| self.model.vertices[i].parent == id).collect()
    }

    pub fn connection_points(&self, state: usize) -> Vec<usize> {
        let id = &self.model.vertices[state].id;
        (0..self.model.vertices.len())
            .filter(|&i| {
                let v = &self.model.vertices[i];
                &v.parent == id
                    && matches!(
                        v.vtype,
                        VertexType::EntryPoint | VertexType::ExitPoint | VertexType::ConnectionPointRef
                    )
            })
            .collect()
    }

    pub fn outgoing(&self, id: &str) -> Vec<&'a Transition> {
        self.model.transitions.iter().filter(|t| t.source == id).collect()
    }

    pub fn incoming(&self, id: &str) -> Vec<&'a Transition> {
        self.model.transitions.iter().filter(|t| t.target == id).collect()
    }

    /// Vertices drawn on the border of a state (entry/exit points and connection point references).
    pub fn is_border(&self, v: usize) -> bool {
        let v = &self.model.vertices[v];
        v.vtype == VertexType::ConnectionPointRef
            || (matches!(v.vtype, VertexType::EntryPoint | VertexType::ExitPoint) && self.has_vertex(&v.parent))
    }

    /// The state that directly contains `v` (through a region, or as a connection point).
    pub fn owner_state(&self, v: usize) -> Option<usize> {
        if self.is_border(v) {
            return self.vertex(&self.model.vertices[v].parent);
        }
        self.region_owner(&self.model.vertices[v].parent).flatten()
    }

    /// Enclosing states, innermost first.
    pub fn ancestors(&self, v: usize) -> Vec<usize> {
        let mut out = Vec::new();
        let mut seen = vec![self.model.vertices[v].id.as_str()];
        let mut o = self.owner_state(v);
        while let Some(x) = o {
            let id = self.model.vertices[x].id.as_str();
            if seen.contains(&id) {
                break;
            }
            seen.push(id);
            out.push(x);
            o = self.owner_state(x);
        }
        out
    }

    pub fn is_inside(&self, v: usize, state_id: &str) -> bool {
        self.ancestors(v).iter().any(|&a| self.model.vertices[a].id == state_id)
    }

    pub fn initial_of(&self, region: &str) -> Option<usize> {
        self.model.vertices.iter().position(|v| v.parent == region && v.vtype == VertexType::Initial)
    }
}

/// `String.prototype.trim()`: Unicode white space and the byte order mark.
pub fn js_trim(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}
