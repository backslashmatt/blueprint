//! A compact binary form of a [`Model`], for the index (DR 0009). JSON
//! stays the external format; this one is internal and versioned.
//!
//! Layout, all integers LEB128 varints unless noted:
//!
//! - `BPM1` and [`FORMAT`];
//! - the string table: a count, then each string's length and bytes;
//! - the system table: a count, then for each its ecosystem tag (a string
//!   index) and its directory (a count of string indices);
//! - the id table: a count, then for each its level (1–4), system index and
//!   path (a count of string indices). Ids are in the model's order, and
//!   the first ids are the nodes, one each, in the same order;
//! - the nodes: a count, then for each its kind, name, source, parent (id
//!   index plus one, or zero), confidence, visibility and code;
//! - the edges: a count, then for each its key (from, to, kind),
//!   confidence, evidence keys and dependency kinds.
//!
//! Decoding shares one id between every place that names it, and builds
//! the model's maps in bulk with [`Model::from_trusted_parts`]. It never
//! panics: anything malformed is an [`Error`].

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::{
    Code, Confidence, DepKind, Ecosystem, Edge, EdgeKey, EdgeKind, Level, Model, Node, NodeId,
    NodeKind, RepoPath, SystemKey, Visibility,
};

/// The version of this layout. Bump it on any change.
pub const FORMAT: u32 = 1;

const MAGIC: &[u8; 4] = b"BPM1";

/// Why bytes could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error(String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "cannot decode model: {}", self.0)
    }
}

impl std::error::Error for Error {}

fn bad<T>(what: &str) -> Result<T, Error> {
    Err(Error(what.to_owned()))
}

/// Encodes `model`.
#[must_use]
pub fn encode(model: &Model) -> Vec<u8> {
    let mut strings = Table::default();
    let mut systems: BTreeMap<&SystemKey, usize> = BTreeMap::new();
    let mut ids: BTreeMap<&NodeId, usize> = BTreeMap::new();
    let mut id_list: Vec<&NodeId> = Vec::new();
    for n in model.nodes() {
        ids.insert(&n.id, id_list.len());
        id_list.push(&n.id);
    }
    // Every id an edge names is a node's (the model checks that), but be
    // safe: add any that is not.
    for e in model.edges() {
        for id in [&e.key.from, &e.key.to]
            .into_iter()
            .chain(e.evidence.iter().flat_map(|k| [&k.from, &k.to]))
        {
            if !ids.contains_key(id) {
                ids.insert(id, id_list.len());
                id_list.push(id);
            }
        }
    }
    let mut system_list: Vec<&SystemKey> = Vec::new();
    for id in &id_list {
        if !systems.contains_key(id.system_key()) {
            systems.insert(id.system_key(), system_list.len());
            system_list.push(id.system_key());
        }
    }

    let mut body = Vec::new();
    put(&mut body, system_list.len() as u64);
    for s in &system_list {
        put(&mut body, strings.index(s.ecosystem.tag()));
        put_path(&mut body, &mut strings, &s.dir.0);
    }
    put(&mut body, id_list.len() as u64);
    for id in &id_list {
        put(&mut body, u64::from(id.level().number()));
        put(&mut body, systems[id.system_key()] as u64);
        put_path(&mut body, &mut strings, id.path());
    }
    put(&mut body, model.nodes().count() as u64);
    for n in model.nodes() {
        put(&mut body, node_kind(n.kind));
        put(&mut body, strings.index(&n.name));
        put_path(&mut body, &mut strings, &n.source.0);
        put(
            &mut body,
            n.parent.as_ref().map_or(0, |p| ids[p] as u64 + 1),
        );
        put(&mut body, confidence(n.confidence));
        put(&mut body, n.visibility.map_or(0, |v| visibility(v) + 1));
        match &n.code {
            None => put(&mut body, 0),
            Some(c) => {
                put(&mut body, 1);
                put(&mut body, u64::from(c.lines.0));
                put(&mut body, u64::from(c.lines.1));
                put(&mut body, c.body_line.map_or(0, |l| u64::from(l) + 1));
                body.extend_from_slice(&c.signature_hash.to_le_bytes());
                match c.body_hash {
                    None => put(&mut body, 0),
                    Some(h) => {
                        put(&mut body, 1);
                        body.extend_from_slice(&h.to_le_bytes());
                    }
                }
            }
        }
    }
    put(&mut body, model.edges().count() as u64);
    let key = |body: &mut Vec<u8>, k: &EdgeKey| {
        put(body, ids[&k.from] as u64);
        put(body, ids[&k.to] as u64);
        put(body, edge_kind(k.kind));
    };
    for e in model.edges() {
        key(&mut body, &e.key);
        put(&mut body, confidence(e.confidence));
        put(&mut body, e.evidence.len() as u64);
        for k in &e.evidence {
            key(&mut body, k);
        }
        put(&mut body, e.dependency.len() as u64);
        for d in &e.dependency {
            put(&mut body, dep_kind(*d));
        }
    }

    let mut out = Vec::with_capacity(body.len() + strings.bytes + 16);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&FORMAT.to_le_bytes());
    put(&mut out, strings.list.len() as u64);
    for s in &strings.list {
        put(&mut out, s.len() as u64);
        out.extend_from_slice(s.as_bytes());
    }
    out.extend_from_slice(&body);
    out
}

/// Decodes bytes written by [`encode`].
///
/// # Errors
/// If the bytes are not a model of this [`FORMAT`]: wrong magic or
/// version, truncated, or an index or tag out of range.
pub fn decode(bytes: &[u8]) -> Result<Model, Error> {
    let mut r = Reader { bytes, at: 0 };
    if r.take(4)? != MAGIC {
        return bad("not a model");
    }
    let version = r.take(4)?;
    if version != FORMAT.to_le_bytes() {
        return bad("another format version");
    }
    let n = r.count()?;
    let mut strings = Vec::with_capacity(n);
    for _ in 0..n {
        let len = r.count()?;
        let s = std::str::from_utf8(r.take(len)?).or_else(|_| bad("a string is not UTF-8"))?;
        strings.push(s.to_owned());
    }
    let string = |i: u64| -> Result<&String, Error> {
        usize::try_from(i)
            .ok()
            .and_then(|i| strings.get(i))
            .map_or_else(|| bad("string index out of range"), Ok)
    };
    let path = |r: &mut Reader<'_>| -> Result<Vec<String>, Error> {
        let n = r.count()?;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            v.push(string(r.u64()?)?.clone());
        }
        Ok(v)
    };

    let n = r.count()?;
    let mut systems = Vec::with_capacity(n);
    for _ in 0..n {
        let tag = string(r.u64()?)?;
        let ecosystem = Ecosystem::from_tag(tag).map_or_else(|| bad("unknown ecosystem"), Ok)?;
        systems.push(SystemKey {
            ecosystem,
            dir: RepoPath(path(&mut r)?),
        });
    }
    let n = r.count()?;
    let mut ids = Vec::with_capacity(n);
    for _ in 0..n {
        let level = match r.u64()? {
            1 => Level::System,
            2 => Level::Module,
            3 => Level::Component,
            4 => Level::Code,
            _ => return bad("unknown level"),
        };
        let system = r.index(&systems)?.clone();
        ids.push(NodeId::within(level, system, path(&mut r)?));
    }
    let n = r.count()?;
    if n > ids.len() {
        return bad("more nodes than ids");
    }
    let mut nodes = Vec::with_capacity(n);
    for id in ids.iter().take(n) {
        let kind = from_node_kind(r.u64()?)?;
        let name = string(r.u64()?)?.clone();
        let source = RepoPath(path(&mut r)?);
        let parent = match r.u64()? {
            0 => None,
            i => Some(r.index_at(&ids, i - 1)?.clone()),
        };
        let confidence = from_confidence(r.u64()?)?;
        let visibility = match r.u64()? {
            0 => None,
            v => Some(from_visibility(v - 1)?),
        };
        let code = match r.u64()? {
            0 => None,
            1 => {
                let lines = (r.u32()?, r.u32()?);
                let body_line = match r.u64()? {
                    0 => None,
                    l => Some(u32::try_from(l - 1).or_else(|_| bad("line out of range"))?),
                };
                let signature_hash = r.fixed64()?;
                let body_hash = match r.u64()? {
                    0 => None,
                    1 => Some(r.fixed64()?),
                    _ => return bad("bad body hash flag"),
                };
                Some(Code {
                    lines,
                    body_line,
                    signature_hash,
                    body_hash,
                })
            }
            _ => return bad("bad code flag"),
        };
        nodes.push(Node {
            id: id.clone(),
            kind,
            name,
            source,
            parent,
            confidence,
            visibility,
            code,
        });
    }
    let key = |r: &mut Reader<'_>| -> Result<EdgeKey, Error> {
        Ok(EdgeKey {
            from: r.index(&ids)?.clone(),
            to: r.index(&ids)?.clone(),
            kind: from_edge_kind(r.u64()?)?,
        })
    };
    let n = r.count()?;
    let mut edges = Vec::with_capacity(n);
    for _ in 0..n {
        let k = key(&mut r)?;
        let confidence = from_confidence(r.u64()?)?;
        let mut evidence = BTreeSet::new();
        for _ in 0..r.count()? {
            evidence.insert(key(&mut r)?);
        }
        let mut dependency = BTreeSet::new();
        for _ in 0..r.count()? {
            dependency.insert(from_dep_kind(r.u64()?)?);
        }
        edges.push(Edge {
            key: k,
            confidence,
            evidence,
            dependency,
        });
    }
    if r.at != bytes.len() {
        return bad("trailing bytes");
    }
    Ok(Model::from_trusted_parts(nodes, edges))
}

/// Strings, each stored once.
#[derive(Default)]
struct Table<'a> {
    index: std::collections::HashMap<&'a str, u64>,
    list: Vec<&'a str>,
    bytes: usize,
}

impl<'a> Table<'a> {
    fn index(&mut self, s: &'a str) -> u64 {
        if let Some(&i) = self.index.get(s) {
            return i;
        }
        let i = self.list.len() as u64;
        self.index.insert(s, i);
        self.list.push(s);
        self.bytes += s.len() + 2;
        i
    }
}

fn put(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn put_path<'a>(out: &mut Vec<u8>, strings: &mut Table<'a>, path: &'a [String]) {
    put(out, path.len() as u64);
    for s in path {
        put(out, strings.index(s));
    }
}

struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.at.checked_add(n).filter(|&e| e <= self.bytes.len());
        let Some(end) = end else {
            return bad("truncated");
        };
        let out = &self.bytes[self.at..end];
        self.at = end;
        Ok(out)
    }

    fn u64(&mut self) -> Result<u64, Error> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = self.take(1)?[0];
            v |= u64::from(b & 0x7f) << shift;
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        bad("varint too long")
    }

    fn u32(&mut self) -> Result<u32, Error> {
        u32::try_from(self.u64()?).or_else(|_| bad("value out of range"))
    }

    fn fixed64(&mut self) -> Result<u64, Error> {
        let b = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_le_bytes(a))
    }

    /// A count of things still to read: never more than the bytes left, so
    /// a damaged count cannot make a huge allocation.
    fn count(&mut self) -> Result<usize, Error> {
        let n = usize::try_from(self.u64()?).or_else(|_| bad("count out of range"))?;
        if n > self.bytes.len() - self.at {
            return bad("count larger than the data");
        }
        Ok(n)
    }

    fn index<'t, T>(&mut self, table: &'t [T]) -> Result<&'t T, Error> {
        let i = self.u64()?;
        self.index_at(table, i)
    }

    fn index_at<'t, T>(&self, table: &'t [T], i: u64) -> Result<&'t T, Error> {
        usize::try_from(i)
            .ok()
            .and_then(|i| table.get(i))
            .map_or_else(|| bad("index out of range"), Ok)
    }
}

fn node_kind(k: NodeKind) -> u64 {
    match k {
        NodeKind::System => 0,
        NodeKind::Unowned => 1,
        NodeKind::Module => 2,
        NodeKind::Directory => 3,
        NodeKind::Component => 4,
        NodeKind::Function => 5,
    }
}

fn from_node_kind(v: u64) -> Result<NodeKind, Error> {
    Ok(match v {
        0 => NodeKind::System,
        1 => NodeKind::Unowned,
        2 => NodeKind::Module,
        3 => NodeKind::Directory,
        4 => NodeKind::Component,
        5 => NodeKind::Function,
        _ => return bad("unknown node kind"),
    })
}

fn confidence(c: Confidence) -> u64 {
    match c {
        Confidence::Unknown => 0,
        Confidence::Inferred => 1,
        Confidence::Exact => 2,
    }
}

fn from_confidence(v: u64) -> Result<Confidence, Error> {
    Ok(match v {
        0 => Confidence::Unknown,
        1 => Confidence::Inferred,
        2 => Confidence::Exact,
        _ => return bad("unknown confidence"),
    })
}

fn visibility(v: Visibility) -> u64 {
    match v {
        Visibility::Private => 0,
        Visibility::Restricted => 1,
        Visibility::Public => 2,
    }
}

fn from_visibility(v: u64) -> Result<Visibility, Error> {
    Ok(match v {
        0 => Visibility::Private,
        1 => Visibility::Restricted,
        2 => Visibility::Public,
        _ => return bad("unknown visibility"),
    })
}

fn edge_kind(k: EdgeKind) -> u64 {
    match k {
        EdgeKind::DependsOn => 0,
        EdgeKind::Calls => 1,
        EdgeKind::Implements => 2,
        EdgeKind::Extends => 3,
        EdgeKind::Reads => 4,
        EdgeKind::Writes => 5,
        EdgeKind::Exposes => 6,
    }
}

fn from_edge_kind(v: u64) -> Result<EdgeKind, Error> {
    Ok(match v {
        0 => EdgeKind::DependsOn,
        1 => EdgeKind::Calls,
        2 => EdgeKind::Implements,
        3 => EdgeKind::Extends,
        4 => EdgeKind::Reads,
        5 => EdgeKind::Writes,
        6 => EdgeKind::Exposes,
        _ => return bad("unknown edge kind"),
    })
}

fn dep_kind(d: DepKind) -> u64 {
    match d {
        DepKind::Normal => 0,
        DepKind::Build => 1,
    }
}

fn from_dep_kind(v: u64) -> Result<DepKind, Error> {
    Ok(match v {
        0 => DepKind::Normal,
        1 => DepKind::Build,
        _ => return bad("unknown dependency kind"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Model {
        let sys = |dir: &str| SystemKey {
            ecosystem: Ecosystem::Cargo,
            dir: RepoPath::parse(dir).unwrap_or_else(|_| RepoPath::root()),
        };
        let mut m = Model::default();
        let node = |id: NodeId, kind, parent: Option<NodeId>, code: Option<Code>| Node {
            id,
            kind,
            name: "n/a %#:".into(),
            source: RepoPath::parse("src/lib.rs").unwrap_or_else(|_| RepoPath::root()),
            parent,
            confidence: Confidence::Inferred,
            visibility: Some(Visibility::Restricted),
            code,
        };
        let a = NodeId::system(sys("a"));
        let b = NodeId::system(sys(""));
        let am = NodeId::within(Level::Module, sys("a"), vec!["x".into()]);
        let bm = NodeId::within(Level::Module, sys(""), vec![]);
        let f = NodeId::within(Level::Component, sys("a"), vec!["x".into(), "F".into()]);
        for n in [
            node(a.clone(), NodeKind::System, None, None),
            node(b.clone(), NodeKind::System, None, None),
            node(am.clone(), NodeKind::Module, Some(a.clone()), None),
            node(bm.clone(), NodeKind::Module, Some(b.clone()), None),
            node(
                f,
                NodeKind::Component,
                Some(am.clone()),
                Some(Code {
                    lines: (3, 9),
                    body_line: Some(4),
                    signature_hash: u64::MAX,
                    body_hash: Some(7),
                }),
            ),
        ] {
            m.add_node(n).unwrap_or_else(|e| panic!("{e}"));
        }
        let low = EdgeKey {
            from: am,
            to: bm,
            kind: EdgeKind::DependsOn,
        };
        for e in [
            Edge {
                key: low.clone(),
                confidence: Confidence::Exact,
                evidence: BTreeSet::new(),
                dependency: BTreeSet::new(),
            },
            Edge {
                key: EdgeKey {
                    from: a,
                    to: b,
                    kind: EdgeKind::DependsOn,
                },
                confidence: Confidence::Exact,
                evidence: BTreeSet::from([low]),
                dependency: BTreeSet::from([DepKind::Normal, DepKind::Build]),
            },
        ] {
            m.add_edge(e).unwrap_or_else(|e| panic!("{e}"));
        }
        m
    }

    #[test]
    fn a_model_round_trips() {
        let m = sample();
        let bytes = encode(&m);
        assert_eq!(decode(&bytes), Ok(m.clone()));
        assert_eq!(decode(&encode(&Model::default())), Ok(Model::default()));
        assert_eq!(encode(&m), bytes, "deterministic");
    }

    #[test]
    fn damaged_bytes_are_an_error_never_a_panic() {
        let bytes = encode(&sample());
        for len in 0..bytes.len() {
            assert!(decode(&bytes[..len]).is_err(), "prefix of {len} bytes");
        }
        let mut longer = bytes.clone();
        longer.push(0);
        assert!(decode(&longer).is_err(), "trailing byte");
        // Every single-byte change either fails or decodes to some model.
        for i in 0..bytes.len() {
            for v in [0u8, 1, 0x7f, 0x80, 0xff] {
                let mut b = bytes.clone();
                b[i] = v;
                let _ = decode(&b);
            }
        }
    }
}
