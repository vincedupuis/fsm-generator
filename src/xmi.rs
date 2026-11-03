//! Reads state machine files: XMI 2.5.1 holding a UML 2.5.1 model (plus a UML
//! DI diagram, which code generation doesn't need). The same reading rules as
//! `fromXmi` in fsm-editor-vscode/src/xmi.ts, without the layout.

use std::collections::HashMap;

use crate::model::{FsmModel, MachineKind, PointKind, Region, Transition, TransitionKind, Vertex, VertexType, js_trim};
use crate::xml::{XmlElement, parse_xml};

const NS_XMI: &str = "http://www.omg.org/spec/XMI/20131001";
const NS_UML: &str = "http://www.omg.org/spec/UML/20161101";
const NS_UMLDI: &str = "http://www.omg.org/spec/UML/20161101/UMLDI";
const NS_DC: &str = "http://www.omg.org/spec/DD/20100524/DC";
const NS_DI: &str = "http://www.omg.org/spec/DD/20100524/DI";
const PROFILE_NS: &str = "https://github.com/vincedupuis/fsm-editor/profiles/FsmStereotypes";

fn type_of(el: &XmlElement) -> String {
    match el.attr("xmi:type") {
        Some(t) => t.to_string(),
        None if el.ns == NS_UML => format!("uml:{}", el.local),
        None => String::new(),
    }
}

fn id_of(el: &XmlElement) -> &str {
    el.attr("xmi:id").unwrap_or("")
}

fn kids<'a>(el: &'a XmlElement, name: &'a str) -> impl Iterator<Item = &'a XmlElement> + 'a {
    el.children.iter().filter(move |c| c.name == name)
}

fn kid<'a>(el: &'a XmlElement, name: &str) -> Option<&'a XmlElement> {
    el.children.iter().find(|c| c.name == name)
}

/// A reference property, written as an attribute (`a="id1 id2"`) or as child elements (`<a xmi:idref|href>`).
fn refs(el: &XmlElement, name: &str) -> Vec<String> {
    let mut out: Vec<String> = el.attr(name).unwrap_or("").split_whitespace().map(str::to_string).collect();
    for c in kids(el, name) {
        if let Some(r) = c.attr("xmi:idref").or_else(|| c.attr("href"))
            && !r.is_empty()
        {
            out.push(r.to_string());
        }
    }
    out
}

fn local_id(r: &str) -> &str {
    match r.find('#') {
        Some(i) => &r[i + 1..],
        None => r,
    }
}

/// Text of an opaque behavior or constraint (its first body).
fn body_of(el: Option<&XmlElement>) -> String {
    let Some(el) = el else { return String::new() };
    let spec = kid(el, "specification").unwrap_or(el);
    if let Some(b) = kid(spec, "body") {
        return js_trim(&b.text).to_string();
    }
    js_trim(spec.attr("body").or_else(|| spec.attr("value")).unwrap_or("")).to_string()
}

/// Parses a state machine file.
pub fn from_xmi(text: &str) -> Result<FsmModel, String> {
    let canonical: HashMap<&str, &str> =
        [(NS_XMI, "xmi"), (NS_UML, "uml"), (NS_UMLDI, "umldi"), (NS_DC, "dc"), (NS_DI, "di")].into_iter().collect();
    let root = parse_xml(text, &canonical).map_err(|e| e.0)?;
    if root.name != "xmi:XMI" && root.name != "uml:Model" {
        return Err(format!("Not an XMI document: the root element is <{}>, expected <xmi:XMI>.", root.name));
    }

    let mut elements_by_id: HashMap<&str, &XmlElement> = HashMap::new();
    let mut machine: Option<(&XmlElement, Option<&XmlElement>)> = None;
    fn visit<'a>(
        el: &'a XmlElement,
        parent: Option<&'a XmlElement>,
        by_id: &mut HashMap<&'a str, &'a XmlElement>,
        machine: &mut Option<(&'a XmlElement, Option<&'a XmlElement>)>,
    ) {
        let id = id_of(el);
        if !id.is_empty() {
            by_id.insert(id, el);
        }
        let t = type_of(el);
        if machine.is_none() && (t == "uml:StateMachine" || t == "uml:ProtocolStateMachine") {
            let context = parent.filter(|p| type_of(p) == "uml:Class");
            *machine = Some((el, context));
        }
        for c in &el.children {
            visit(c, Some(el), by_id, machine);
        }
    }
    visit(&root, None, &mut elements_by_id, &mut machine);
    let Some((sm, context)) = machine else {
        return Err("The document does not contain a UML state machine.".to_string());
    };

    let event_text = |trig: &XmlElement| -> String {
        let event_ref = trig
            .attr("event")
            .map(str::to_string)
            .or_else(|| refs(trig, "event").into_iter().next())
            .unwrap_or_default();
        let ev = elements_by_id.get(local_id(&event_ref)).copied();
        let trig_name = trig.attr("name").unwrap_or("");
        if let Some(ev) = ev {
            let t = type_of(ev);
            if t == "uml:TimeEvent" {
                let expr = kid(kid(ev, "when").unwrap_or(ev), "expr");
                let value = match expr {
                    Some(e) => e.attr("value").map(str::to_string).unwrap_or_else(|| body_of(Some(e))),
                    None => String::new(),
                };
                if !value.is_empty() {
                    return format!("after({value})");
                }
            }
            if t == "uml:SignalEvent" {
                let sig = elements_by_id.get(local_id(ev.attr("signal").unwrap_or(""))).copied();
                for name in [sig.and_then(|s| s.attr("name")), ev.attr("name"), Some(trig_name)] {
                    if let Some(n) = name.filter(|n| !n.is_empty()) {
                        return n.to_string();
                    }
                }
                return String::new();
            }
            if let Some(n) = ev.attr("name").filter(|n| !n.is_empty()) {
                return n.to_string();
            }
        }
        trig_name.to_string()
    };

    let id = id_of(sm);
    let mut model = FsmModel {
        id: if id.is_empty() { "sm".to_string() } else { id.to_string() },
        name: sm.attr("name").unwrap_or("").to_string(),
        kind: if type_of(sm) == "uml:ProtocolStateMachine" { MachineKind::Protocol } else { MachineKind::Behavioral },
        context: context.and_then(|c| c.attr("name")).unwrap_or("").to_string(),
        documentation: String::new(),
        regions: Vec::new(),
        vertices: Vec::new(),
        transitions: Vec::new(),
    };
    for c in kids(sm, "ownedComment") {
        if refs(c, "annotatedElement").contains(&model.id) {
            model.documentation = body_of(Some(c));
        }
    }

    struct Reader<'a, F: Fn(&XmlElement) -> String> {
        event_text: F,
        model: &'a mut FsmModel,
    }

    impl<F: Fn(&XmlElement) -> String> Reader<'_, F> {
        fn transition(&mut self, el: &XmlElement) {
            let end = |name: &str| {
                local_id(
                    &el.attr(name)
                        .map(str::to_string)
                        .or_else(|| refs(el, name).into_iter().next())
                        .unwrap_or_default(),
                )
                .to_string()
            };
            let kind = match el.attr("kind") {
                Some("internal") => TransitionKind::Internal,
                Some("local") => TransitionKind::Local,
                _ => TransitionKind::External,
            };
            let t = Transition {
                id: id_of(el).to_string(),
                source: end("source"),
                target: end("target"),
                kind,
                triggers: kids(el, "trigger").map(|t| (self.event_text)(t)).filter(|t| !t.is_empty()).collect(),
                guard: body_of(kid(el, "guard")),
                effect: body_of(kid(el, "effect")),
                precondition: body_of(kid(el, "preCondition")),
                postcondition: body_of(kid(el, "postCondition")),
            };
            self.model.transitions.push(t);
        }

        fn point(&mut self, el: &XmlElement, parent: &str) {
            let kind =
                if el.attr("kind") == Some("exitPoint") { VertexType::ExitPoint } else { VertexType::EntryPoint };
            self.model.vertices.push(Vertex::new(id_of(el), kind, el.attr("name").unwrap_or(""), parent));
        }

        fn vertex(&mut self, el: &XmlElement, parent: &str) {
            let t = type_of(el);
            let name = el.attr("name").unwrap_or("");
            if t == "uml:FinalState" {
                self.model.vertices.push(Vertex::new(id_of(el), VertexType::Final, name, parent));
                return;
            }
            if t == "uml:Pseudostate" {
                let kind = VertexType::pseudostate(el.attr("kind").unwrap_or("initial")).unwrap_or(VertexType::Initial);
                self.model.vertices.push(Vertex::new(id_of(el), kind, name, parent));
                return;
            }
            let mut v = Vertex::new(id_of(el), VertexType::State, name, parent);
            v.entry = body_of(kid(el, "entry"));
            v.exit = body_of(kid(el, "exit"));
            v.do_activity = body_of(kid(el, "doActivity"));
            v.invariant = body_of(kid(el, "stateInvariant"));
            v.deferrable =
                kids(el, "deferrableTrigger").map(|t| (self.event_text)(t)).filter(|t| !t.is_empty()).collect();
            v.submachine = refs(el, "submachine").into_iter().next();
            let state = self.model.vertices.len();
            let vid = v.id.clone();
            self.model.vertices.push(v);
            for cp in kids(el, "connectionPoint") {
                self.point(cp, &vid);
            }
            for c in kids(el, "connection") {
                let entry_ref = refs(c, "entry").into_iter().next();
                let exit_ref = refs(c, "exit").into_iter().next();
                let mut r = Vertex::new(id_of(c), VertexType::ConnectionPointRef, "", &vid);
                r.point_kind =
                    if exit_ref.is_some() && entry_ref.is_none() { PointKind::Exit } else { PointKind::Entry };
                r.point_ref = local_id(&entry_ref.or(exit_ref).unwrap_or_default()).to_string();
                self.model.vertices.push(r);
            }
            for r in kids(el, "region") {
                let region = Region { id: id_of(r).to_string(), name: r.attr("name").unwrap_or("").to_string() };
                self.model.vertices[state].regions.push(region);
                self.region(r);
            }
        }

        fn region(&mut self, el: &XmlElement) {
            let id = id_of(el);
            for c in &el.children {
                match c.name.as_str() {
                    "subvertex" => self.vertex(c, id),
                    "transition" => self.transition(c),
                    "ownedComment" => self.model.vertices.push(Vertex::new(
                        id_of(c),
                        VertexType::Comment,
                        c.attr("name").unwrap_or(""),
                        id,
                    )),
                    _ => {}
                }
            }
        }
    }

    let mut reader = Reader { event_text, model: &mut model };
    for r in kids(sm, "region") {
        reader.model.regions.push(Region { id: id_of(r).to_string(), name: r.attr("name").unwrap_or("").to_string() });
        reader.region(r);
    }
    if reader.model.regions.is_empty() {
        let id = format!("{}_region", reader.model.id);
        reader.model.regions.push(Region { id, name: String::new() });
    }
    let first_region = reader.model.regions[0].id.clone();
    for cp in kids(sm, "connectionPoint") {
        reader.point(cp, &first_region);
    }

    // Stereotype applications: elements of the embedded profile's namespace.
    for c in &root.children {
        if c.ns != PROFILE_NS {
            continue;
        }
        let base = local_id(c.attr("base_State").unwrap_or(""));
        // The last vertex with that id, as a lookup by id finds it.
        if let Some(v) = model.vertices.iter_mut().rev().find(|v| v.id == base) {
            v.stereotype = c.local.clone();
        }
    }
    Ok(model)
}
