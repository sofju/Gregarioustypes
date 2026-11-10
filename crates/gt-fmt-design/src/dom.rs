//! A tiny XML tree built with quick-xml, for the IDML parts.

use quick_xml::events::Event;
use quick_xml::Reader;

#[derive(Debug, Default, Clone)]
pub struct El {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub kids: Vec<Node>,
}

#[derive(Debug, Clone)]
pub enum Node {
    El(El),
    Text(String),
    /// Processing instruction (`<?ACE 18?>` = current page number).
    Pi(String),
}

impl El {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs.iter().find(|(a, _)| a == k).map(|(_, v)| v.as_str())
    }

    pub fn f(&self, k: &str) -> Option<f32> {
        self.attr(k).and_then(|v| v.trim().parse().ok())
    }

    pub fn els(&self) -> impl Iterator<Item = &El> {
        self.kids.iter().filter_map(|n| if let Node::El(e) = n { Some(e) } else { None })
    }

    pub fn child(&self, name: &str) -> Option<&El> {
        self.els().find(|e| e.name == name)
    }

    /// First descendant named `name` (depth first).
    pub fn find(&self, name: &str) -> Option<&El> {
        for e in self.els() {
            if e.name == name {
                return Some(e);
            }
            if let Some(f) = e.find(name) {
                return Some(f);
            }
        }
        None
    }

    /// All descendants named `name`.
    pub fn find_all<'a>(&'a self, name: &str, out: &mut Vec<&'a El>) {
        for e in self.els() {
            if e.name == name {
                out.push(e);
            }
            e.find_all(name, out);
        }
    }

    pub fn text(&self) -> String {
        let mut s = String::new();
        for k in &self.kids {
            match k {
                Node::Text(t) => s.push_str(t),
                Node::El(e) => s.push_str(&e.text()),
                Node::Pi(_) => {}
            }
        }
        s
    }

    /// `<Properties><Name>value</Name>` text.
    pub fn prop(&self, name: &str) -> Option<String> {
        self.child("Properties").and_then(|p| p.child(name)).map(|e| e.text())
    }
}

fn local(name: &[u8]) -> String {
    let s = String::from_utf8_lossy(name);
    match s.rsplit_once(':') {
        Some((_, l)) => l.to_string(),
        None => s.to_string(),
    }
}

pub fn parse(xml: &str) -> Result<El, String> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    let mut stack: Vec<El> = vec![El { name: "#root".into(), ..Default::default() }];
    loop {
        match r.read_event() {
            Ok(Event::Start(e)) => {
                let attrs = e
                    .attributes()
                    .flatten()
                    .map(|a| (local(a.key.as_ref().as_bytes()), a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.to_string()).unwrap_or_default()))
                    .collect();
                stack.push(El { name: local(e.name().as_ref().as_bytes()), attrs, kids: Vec::new() });
            }
            Ok(Event::Empty(e)) => {
                let attrs = e
                    .attributes()
                    .flatten()
                    .map(|a| (local(a.key.as_ref().as_bytes()), a.normalized_value(quick_xml::XmlVersion::Implicit1_0).map(|v| v.to_string()).unwrap_or_default()))
                    .collect();
                let el = El { name: local(e.name().as_ref().as_bytes()), attrs, kids: Vec::new() };
                stack.last_mut().unwrap().kids.push(Node::El(el));
            }
            Ok(Event::End(_)) => {
                if stack.len() > 1 {
                    let el = stack.pop().unwrap();
                    stack.last_mut().unwrap().kids.push(Node::El(el));
                }
            }
            Ok(Event::Text(t)) => {
                let s = t.xml10_content().to_string();
                if !s.is_empty() {
                    stack.last_mut().unwrap().kids.push(Node::Text(s));
                }
            }
            Ok(Event::CData(t)) => {
                stack.last_mut().unwrap().kids.push(Node::Text(t.xml10_content().to_string()));
            }
            Ok(Event::GeneralRef(g)) => {
                let s = match g.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => match g.as_ref() {
                        "amp" => "&".into(),
                        "lt" => "<".into(),
                        "gt" => ">".into(),
                        "quot" => "\"".into(),
                        "apos" => "'".into(),
                        _ => String::new(),
                    },
                };
                stack.last_mut().unwrap().kids.push(Node::Text(s));
            }
            Ok(Event::PI(p)) => {
                stack.last_mut().unwrap().kids.push(Node::Pi(format!("{} {}", p.target(), p.content().trim())));
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML error at {}: {e}", r.buffer_position())),
            _ => {}
        }
    }
    while stack.len() > 1 {
        let el = stack.pop().unwrap();
        stack.last_mut().unwrap().kids.push(Node::El(el));
    }
    let root = stack.pop().unwrap();
    let first = root.kids.into_iter().find_map(|n| if let Node::El(e) = n { Some(e) } else { None });
    first.ok_or_else(|| "empty XML".into())
}
