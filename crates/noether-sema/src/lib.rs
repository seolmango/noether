//! Semantic analysis: field layout, Pauli exclusion, name resolution and the
//! symmetry checks. See docs/spec.md, chapters 4, 5 and 9.

use noether_syntax::*;
use std::collections::{BTreeMap, HashMap};

#[derive(Debug, Clone)]
pub struct Config {
    pub h: u32,
    pub l: u32,
    pub c: u32,
    pub b: u32,
    pub tmin: u64,
    pub seed: u64,
    /// Pauli spacing D = 2 * lambda.
    pub spacing: i64,
}

impl Config {
    /// Exponent of the critical temperature for a mass, may be out of range.
    pub fn tc_exp(&self, mass: u32) -> i64 {
        2 * (self.l as i64 - self.h as i64) - mass as i64
    }

    /// Exponent of the widest possible packet of a mass.
    pub fn s_max(&self, mass: u32) -> u32 {
        let t = 63 - self.tmin.leading_zeros() as i64;
        (self.l as i64 - (t + mass as i64) / 2).max(0) as u32
    }
}

#[derive(Debug, Clone)]
pub struct Packet {
    pub c: i64,
    pub fermion: bool,
    pub mass: u32,
    pub name: String,
}

#[derive(Debug, Clone)]
pub struct Sym {
    pub base: i64,
    pub first: usize,
    pub count: Option<usize>,
    pub stride: i64,
}

#[derive(Debug, Clone, Default)]
pub struct Layout {
    pub packets: Vec<Packet>,
    pub names: HashMap<String, Sym>,
}

#[derive(Debug, Clone)]
pub struct Path {
    pub name: String,
    pub params: usize,
    pub layout: Layout,
    pub body: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub struct Module {
    pub cfg: Config,
    pub global: Layout,
    pub vacuum_base: i64,
    pub paths: Vec<Path>,
    pub path_ids: HashMap<String, usize>,
    pub body: Vec<Stmt>,
    /// Without `create` the vacuum stays empty for the whole run.
    pub has_create: bool,
}

fn exponent(cfg: &HashMap<&str, u64>, key: &str) -> Result<u32> {
    let v = cfg[key];
    if !v.is_power_of_two() {
        return err(0, format!("field parameter {key}={v} is not a power of two"));
    }
    Ok(v.trailing_zeros())
}

fn config(program: &[Stmt]) -> Result<Config> {
    let mut cfg: HashMap<&str, u64> = HashMap::from([
        ("hbar", 16),
        ("lambda", 1024),
        ("tau", 64),
        ("tile", 4096),
        ("tmin", 1),
        ("seed", 0),
    ]);
    for s in program {
        if let StmtKind::Field(params) = &s.kind {
            for (k, v) in params {
                match cfg.get_mut(k.as_str()) {
                    Some(slot) => *slot = *v,
                    None => return err(s.line, format!("unknown field parameter {k}")),
                }
            }
        }
    }
    let (h, l) = (exponent(&cfg, "hbar")?, exponent(&cfg, "lambda")?);
    if h > 32 || l > 32 {
        return err(0, "lambda and hbar are limited to 2^32");
    }
    Ok(Config {
        h,
        l,
        c: exponent(&cfg, "tau")?,
        b: exponent(&cfg, "tile")?,
        tmin: 1 << exponent(&cfg, "tmin")?,
        seed: cfg["seed"],
        spacing: 2 << l,
    })
}

fn declare(d: &Decl, line: usize, layout: &mut Layout, spacing: i64) -> Result<()> {
    if layout.names.contains_key(&d.name) {
        return err(line, format!("{} declared twice", d.name));
    }
    let base = match &d.at {
        None if !d.fermion => return err(line, "a boson must be placed with @"),
        None => layout.packets.iter().map(|p| p.c).max().map_or(0, |top| top + spacing),
        Some(Place::Lit(n)) => *n,
        Some(Place::Rel(name, off)) => match layout.names.get(name) {
            Some(sym) => sym.base + off,
            None => return err(line, format!("unknown name {name}")),
        },
    };
    let first = layout.packets.len();
    let stride = d.stride.unwrap_or(spacing);
    let count = d.count.map(|n| n as usize);
    for j in 0..count.unwrap_or(1) {
        layout.packets.push(Packet {
            c: base + j as i64 * stride,
            fermion: d.fermion,
            mass: d.mass,
            name: match count {
                Some(_) => format!("{}[{j}]", d.name),
                None => d.name.clone(),
            },
        });
    }
    layout.names.insert(d.name.clone(), Sym { base, first, count, stride });
    Ok(())
}

fn check_pauli(layout: &Layout, spacing: i64) -> Result<()> {
    let mut fermions: Vec<&Packet> = layout.packets.iter().filter(|p| p.fermion).collect();
    fermions.sort_by_key(|p| p.c);
    for w in fermions.windows(2) {
        let gap = w[1].c - w[0].c;
        if gap < spacing {
            return err(
                0,
                format!(
                    "Pauli violation: fermions {} and {} are {gap} apart, need {spacing}",
                    w[0].name, w[1].name
                ),
            );
        }
    }
    Ok(())
}

// ------------------------------------------------------------------ checks

#[derive(Clone, Copy, PartialEq)]
enum Shape {
    Scalar,
    Lattice,
    Index,
}

#[derive(Clone)]
enum Sym2 {
    None,
    Time,
    Translate { index: String, count: usize },
}

struct Checker<'a> {
    global: &'a Layout,
    paths: &'a HashMap<String, usize>,
    arity: Vec<usize>,
    local: HashMap<String, Shape>,
    in_path: bool,
    has_create: bool,
}

impl Checker<'_> {
    fn shape(&self, name: &str) -> Option<Shape> {
        if let Some(s) = self.local.get(name) {
            return Some(*s);
        }
        let sym = self.global.names.get(name)?;
        Some(if sym.count.is_none() { Shape::Scalar } else { Shape::Lattice })
    }

    fn target(&self, t: &Target, ln: usize) -> Result<()> {
        match t {
            Target::Named(n) => {
                if self.shape(n).is_none() {
                    return err(ln, format!("unknown name {n}"));
                }
                Ok(())
            }
            Target::Elem(n, i) => {
                match self.shape(n) {
                    None => return err(ln, format!("unknown name {n}")),
                    Some(Shape::Lattice) => {}
                    Some(_) => return err(ln, format!("{n} used with the wrong shape")),
                }
                self.value(i, ln)
            }
            Target::Ptr(v) => self.value(v, ln),
        }
    }

    fn addr(&self, a: &Addr, ln: usize) -> Result<()> {
        match a {
            Addr::Lit(_) => Ok(()),
            Addr::At(t, _) => self.target(t, ln),
        }
    }

    fn value(&self, v: &Value, ln: usize) -> Result<()> {
        match v {
            Value::Int(_) | Value::Absorb | Value::Absorbc => Ok(()),
            Value::Read(a, _) | Value::Intensity(a, _) => self.addr(a, ln),
            Value::AddrOf(a) => {
                if let Addr::At(Target::Named(n) | Target::Elem(n, _), _) = a {
                    if matches!(self.local.get(n), Some(Shape::Scalar | Shape::Lattice)) {
                        return err(
                            ln,
                            format!("{n} lives in a closed system and has no global address"),
                        );
                    }
                }
                self.addr(a, ln)
            }
            Value::Call { line, name, args } => {
                let Some(&id) = self.paths.get(name) else {
                    return err(*line, format!("unknown path {name}"));
                };
                if self.arity[id] != args.len() {
                    return err(*line, format!("{name} takes {} arguments", self.arity[id]));
                }
                args.iter().try_for_each(|a| self.value(a, ln))
            }
        }
    }

    fn cond(&self, c: &Cond, ln: usize) -> Result<()> {
        self.value(&c.left, ln)?;
        self.value(&c.right, ln)
    }

    fn block(&mut self, stmts: &[Stmt], sym: &Sym2, top: bool) -> Result<()> {
        for s in stmts {
            self.stmt(s, sym, top)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt, sym: &Sym2, top: bool) -> Result<()> {
        use StmtKind::*;
        let ln = s.line;
        match &s.kind {
            Decl(_) if !top => return err(ln, "declarations only at the top of a path"),
            Decl(_) => return Ok(()),
            Field(_) if self.in_path || !top => return err(ln, "field belongs at the top level"),
            Field(_) => return Ok(()),
            Path { .. } => return err(ln, "a path belongs at the top level"),
            Return(_) if !self.in_path => return err(ln, "return outside a path"),
            _ => {}
        }
        let forbidden_in_time = matches!(
            s.kind,
            Write(..) | Emit(_) | EmitC(_) | EmitN(_) | EmitS(_) | Create(..) | Annihilate(_) | Return(_)
                | CallStmt(_) | SymTranslate { .. }
        );
        let forbidden_in_sweep = matches!(
            s.kind,
            Emit(_) | EmitC(_) | EmitN(_) | EmitS(_) | Create(..) | Annihilate(_) | Return(_)
                | CallStmt(_) | While(..) | SymTime(_) | SymTranslate { .. }
        );
        match sym {
            Sym2::Time if forbidden_in_time => {
                return err(ln, "symmetry time broken: this statement is not reversible")
            }
            Sym2::Translate { .. } if forbidden_in_sweep => {
                return err(ln, "symmetry translate broken by this statement")
            }
            _ => {}
        }
        if !matches!(sym, Sym2::None) && impure(&s.kind) {
            return err(ln, "a symmetry block is a closed system");
        }
        match &s.kind {
            Write(t, v) => {
                self.target(t, ln)?;
                self.value(v, ln)?;
            }
            Raise(t) | Lower(t) | Flip(t) => self.target(t, ln)?,
            Swap(a, b) | Fuse(a, b) => {
                self.target(a, ln)?;
                self.target(b, ln)?;
            }
            If(c, a, b) => {
                self.cond(c, ln)?;
                self.sweep_cond(c, sym, ln)?;
                self.block(a, sym, false)?;
                self.block(b, sym, false)?;
            }
            While(c, b) => {
                self.cond(c, ln)?;
                self.block(b, sym, false)?;
            }
            SymTime(b) => self.block(b, &Sym2::Time, false)?,
            SymTranslate { index, lattice, body } => {
                let local = self.local.contains_key(lattice);
                let Some(g) = self.global.names.get(lattice).filter(|_| !local) else {
                    return err(ln, format!("{lattice} is not a global lattice"));
                };
                let Some(count) = g.count else {
                    return err(ln, format!("{lattice} is not a global lattice"));
                };
                let saved = self.local.insert(index.clone(), Shape::Index);
                let inner = Sym2::Translate { index: index.clone(), count };
                self.block(body, &inner, false)?;
                match saved {
                    Some(x) => self.local.insert(index.clone(), x),
                    None => self.local.remove(index),
                };
            }
            Emit(v) | EmitC(v) | EmitN(v) | Return(v) | CallStmt(v) => self.value(v, ln)?,
            Create(_, v) => {
                self.has_create = true;
                self.value(v, ln)?;
            }
            Annihilate(v) => self.value(v, ln)?,
            EmitS(_) | Field(_) | Decl(_) | Path { .. } => {}
        }
        if let Sym2::Translate { index, count } = sym {
            match &s.kind {
                Write(t, v) => {
                    self.sweep_target(t, index, *count, ln, true)?;
                    self.sweep_value(v, index, *count, ln)?;
                }
                Raise(t) | Lower(t) | Flip(t) => self.sweep_target(t, index, *count, ln, true)?,
                Swap(a, b) | Fuse(a, b) => {
                    self.sweep_target(a, index, *count, ln, true)?;
                    self.sweep_target(b, index, *count, ln, true)?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn sweep_cond(&self, c: &Cond, sym: &Sym2, ln: usize) -> Result<()> {
        if let Sym2::Translate { index, count } = sym {
            self.sweep_value(&c.left, index, *count, ln)?;
            self.sweep_value(&c.right, index, *count, ln)?;
        }
        Ok(())
    }

    fn sweep_target(
        &self,
        t: &Target,
        index: &str,
        count: usize,
        ln: usize,
        written: bool,
    ) -> Result<()> {
        match t {
            Target::Elem(n, i) => {
                let exact = Value::AddrOf(Addr::At(Target::Named(index.to_string()), 0));
                if **i != exact {
                    return err(
                        ln,
                        format!("symmetry translate broken: subscript of {n} is not {index}"),
                    );
                }
                match self.global.names.get(n).filter(|_| !self.local.contains_key(n)) {
                    None => err(ln, "symmetry translate broken: local lattice in a sweep"),
                    Some(sym) if sym.count.unwrap_or(0) < count => {
                        err(ln, format!("lattice {n} is shorter than the sweep"))
                    }
                    Some(_) => Ok(()),
                }
            }
            Target::Ptr(_) => err(ln, "symmetry translate broken: raw pointer"),
            Target::Named(n) if n == index => {
                err(ln, format!("{index} may only be used as a subscript"))
            }
            Target::Named(_) if written => {
                err(ln, format!("symmetry translate broken: a sweep may only change lat[{index}]"))
            }
            Target::Named(_) => Ok(()),
        }
    }

    fn sweep_value(&self, v: &Value, index: &str, count: usize, ln: usize) -> Result<()> {
        match v {
            Value::Read(Addr::At(t, _), _)
            | Value::Intensity(Addr::At(t, _), _)
            | Value::AddrOf(Addr::At(t, _)) => self.sweep_target(t, index, count, ln, false),
            _ => Ok(()),
        }
    }
}

fn value_impure(v: &Value) -> bool {
    match v {
        Value::Call { .. } | Value::Absorb | Value::Absorbc => true,
        Value::Int(_) => false,
        Value::Read(a, _) | Value::Intensity(a, _) | Value::AddrOf(a) => match a {
            Addr::Lit(_) => false,
            Addr::At(t, _) => target_impure(t),
        },
    }
}

fn target_impure(t: &Target) -> bool {
    match t {
        Target::Named(_) => false,
        Target::Elem(_, v) | Target::Ptr(v) => value_impure(v),
    }
}

fn impure(k: &StmtKind) -> bool {
    use StmtKind::*;
    match k {
        Write(t, v) => target_impure(t) || value_impure(v),
        Raise(t) | Lower(t) | Flip(t) => target_impure(t),
        Swap(a, b) | Fuse(a, b) => target_impure(a) || target_impure(b),
        If(c, ..) | While(c, _) => value_impure(&c.left) || value_impure(&c.right),
        _ => false,
    }
}

/// The static field as the runtime wants it.
pub struct Tables {
    /// h, l, c, b, tmin, seed, spacing, vacuum_base, n_packets, n_tiles
    pub desc: [i64; 10],
    /// Four words per packet: centre, mass, fermion, tile slot.
    pub meta: Vec<i64>,
    /// Tile key of each dense slot.
    pub keys: Vec<i64>,
    pub slots: BTreeMap<i64, usize>,
    pub packet_slot: Vec<usize>,
}

impl Module {
    pub fn tables(&self) -> Tables {
        let cfg = &self.cfg;
        let mut slots = BTreeMap::new();
        for p in &self.global.packets {
            let next = slots.len();
            slots.entry(p.c >> cfg.b).or_insert(next);
        }
        let packet_slot: Vec<usize> =
            self.global.packets.iter().map(|p| slots[&(p.c >> cfg.b)]).collect();
        let n = self.global.packets.len();
        let mut meta = Vec::with_capacity(n * 4);
        for (i, p) in self.global.packets.iter().enumerate() {
            meta.extend([p.c, p.mass as i64, p.fermion as i64, packet_slot[i] as i64]);
        }
        let mut keys = vec![0i64; slots.len()];
        for (key, slot) in &slots {
            keys[*slot] = *key;
        }
        let desc = [
            cfg.h as i64,
            cfg.l as i64,
            cfg.c as i64,
            cfg.b as i64,
            cfg.tmin as i64,
            cfg.seed as i64,
            cfg.spacing,
            self.vacuum_base,
            n as i64,
            slots.len() as i64,
        ];
        Tables { desc, meta, keys, slots, packet_slot }
    }
}

pub fn analyze(program: Vec<Stmt>) -> Result<Module> {
    let cfg = config(&program)?;
    let mut global = Layout::default();
    let mut body = Vec::new();
    let mut raw_paths = Vec::new();
    for s in program {
        match s.kind {
            StmtKind::Decl(ref d) => {
                declare(d, s.line, &mut global, cfg.spacing)?;
                body.push(s);
            }
            StmtKind::Path { name, params, body } => raw_paths.push((s.line, name, params, body)),
            _ => body.push(s),
        }
    }
    check_pauli(&global, cfg.spacing)?;
    let highest = global.packets.iter().map(|p| p.c).max().unwrap_or(-1);
    let vacuum_base = (((highest + cfg.spacing) >> cfg.b) + 1) << cfg.b;

    let mut paths = Vec::new();
    let mut path_ids = HashMap::new();
    for (line, name, params, stmts) in raw_paths {
        let mut layout = Layout::default();
        for p in &params {
            let d = Decl {
                fermion: true,
                name: p.clone(),
                count: None,
                at: None,
                stride: None,
                mass: 0,
            };
            declare(&d, line, &mut layout, cfg.spacing)?;
        }
        for s in &stmts {
            if let StmtKind::Decl(d) = &s.kind {
                declare(d, s.line, &mut layout, cfg.spacing)?;
            }
        }
        check_pauli(&layout, cfg.spacing)?;
        if path_ids.insert(name.clone(), paths.len()).is_some() {
            return err(line, format!("path {name} declared twice"));
        }
        paths.push(Path { name, params: params.len(), layout, body: stmts });
    }

    let arity: Vec<usize> = paths.iter().map(|p| p.params).collect();
    let mut ck = Checker {
        global: &global,
        paths: &path_ids,
        arity,
        local: HashMap::new(),
        in_path: false,
        has_create: false,
    };
    ck.block(&body, &Sym2::None, true)?;
    ck.in_path = true;
    for p in &paths {
        ck.local = p
            .layout
            .names
            .iter()
            .map(|(n, s)| {
                (n.clone(), if s.count.is_none() { Shape::Scalar } else { Shape::Lattice })
            })
            .collect();
        ck.block(&p.body, &Sym2::None, true)?;
    }
    let has_create = ck.has_create;
    Ok(Module { cfg, global, vacuum_base, paths, path_ids, body, has_create })
}
