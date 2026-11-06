//! NIR: Noether's mid-level IR, and the passes that erase physics where it can
//! be proved irrelevant. See docs/compiler.md.
//!
//! NIR is a structured tree, not a flat CFG. Noether has no variables outside
//! the field, so no value crosses a control-flow edge and no phi is needed, and
//! keeping regions intact makes symmetric blocks easy to emit in two versions.
//! Cranelift builds the CFG and SSA form.

use noether_sema::{Config, Layout, Module, Sym};
use noether_syntax::{Addr, Cmp, Cond as AstCond, Stmt, StmtKind, Target, Value};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldId {
    Global,
    Frame,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Const(i64),
    /// The running index of a sweep.
    SweepIndex,
    Read(Box<Read>),
    /// Intensity: the square of a read.
    Square(Box<Read>),
    /// wrap(base + index * stride + off)
    Scaled { base: i64, stride: i64, index: Box<Expr>, off: i64 },
    /// wrap(value + off)
    Shifted(Box<Expr>, i64),
    Call(usize, Vec<Expr>),
    Absorb,
    Absorbc,
}

#[derive(Debug, Clone)]
pub struct Read {
    pub field: FieldId,
    pub loc: Loc,
    pub k: u32,
    /// Friction heat for this read. Zero inside `symmetry time`.
    pub friction: u64,
    pub plan: Plan,
}

#[derive(Debug, Clone)]
pub enum Loc {
    Const(i64),
    Lattice { first: usize, count: usize, base: i64, stride: i64, index: Box<Expr>, off: i64 },
    Dynamic(Box<Expr>),
}

#[derive(Debug, Clone, Copy)]
pub struct Term {
    pub packet: usize,
    pub weight: i64,
}

/// What the isolation pass proved about a read.
#[derive(Debug, Clone)]
pub enum Plan {
    /// Nothing proved: run the full rule in the runtime.
    General,
    /// The contributing packets and their weights are static. The result is
    /// (sum of v * weight) >> shift unless a packet turns out to be noisy.
    Terms(Vec<Term>, u32),
    /// Element i of a lattice reads exactly element i of each listed run.
    Lattice(Vec<usize>),
}

#[derive(Debug, Clone)]
pub enum Place {
    Packet { field: FieldId, index: usize },
    Elem { field: FieldId, first: usize, count: usize, base: i64, stride: i64, index: Expr },
    Ptr(Expr),
    NotAPacket,
}

#[derive(Debug, Clone)]
pub struct Cond {
    pub op: Cmp,
    pub left: Expr,
    pub right: Expr,
}

/// A static run of packets that must stay below critical temperature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Guard {
    Run { first: usize, count: usize, tc_exp: i64 },
    Frame { tc_exp: i64 },
}

#[derive(Debug, Clone)]
pub struct Symmetric {
    /// None for `symmetry time`, the length for `symmetry translate`.
    pub sweep: Option<usize>,
    pub body: Vec<Op>,
    /// Some when a fast version exists, with the entry conditions.
    pub guards: Option<Vec<Guard>>,
    /// Packets the fast version may keep in registers. Empty unless every
    /// access in the block names its packet statically, so nothing can alias.
    pub promotable: Vec<(FieldId, usize)>,
}

#[derive(Debug, Clone)]
pub enum Op {
    Write { place: Place, value: Expr, friction: u64 },
    Ladder(Place, i64),
    Flip(Place),
    Swap(Place, Place),
    Fuse(Place, Place),
    If(Cond, Vec<Op>, Vec<Op>),
    While(Cond, Vec<Op>),
    Symmetric(Symmetric),
    Emit(Expr),
    EmitC(Expr),
    EmitN(Expr),
    EmitS(Vec<u8>),
    Return(Expr),
    Eval(Expr),
    Create { fermion: bool, addr: Expr, friction: u64 },
    Annihilate { addr: Expr, friction: u64 },
}

#[derive(Debug, Clone)]
pub struct Func {
    pub name: String,
    pub params: usize,
    pub centres: Vec<i64>,
    pub masses: Vec<u32>,
    pub body: Vec<Op>,
}

#[derive(Debug, Clone, Default)]
pub struct Stats {
    pub reads: usize,
    pub general: usize,
    pub symmetric: usize,
    pub symmetric_fast: usize,
}

#[derive(Debug, Clone)]
pub struct Program {
    pub funcs: Vec<Func>,
    pub main: Vec<Op>,
    pub stats: Stats,
}

#[derive(Debug, Clone, Copy)]
pub struct Options {
    /// Run the isolation pass. Off means every read takes the full rule.
    pub isolate: bool,
    /// Emit fast versions of symmetric blocks.
    pub symmetry: bool,
}

/// Packets of a field sorted by centre, for range queries.
struct Index {
    sorted: Vec<(i64, usize, u32)>,
}

impl Index {
    fn new(layout: &Layout) -> Self {
        let mut sorted: Vec<_> =
            layout.packets.iter().enumerate().map(|(i, p)| (p.c, i, p.mass)).collect();
        sorted.sort();
        Index { sorted }
    }
}

struct Lower<'a> {
    cfg: &'a Config,
    module: &'a Module,
    opts: Options,
    global: Index,
    frame: Option<(&'a Layout, Index)>,
    sweep: Option<String>,
    frozen: bool,
    lattice_memo: HashMap<(FieldId, usize, u32), Plan>,
    stats: Stats,
}

impl<'a> Lower<'a> {
    fn lookup(&self, name: &str) -> (FieldId, &Sym) {
        if let Some((layout, _)) = &self.frame {
            if let Some(sym) = layout.names.get(name) {
                return (FieldId::Frame, sym);
            }
        }
        (FieldId::Global, &self.module.global.names[name])
    }

    /// The isolation pass for one address known at compile time.
    fn plan_at(&self, field: FieldId, x: i64, k: u32) -> Plan {
        if !self.opts.isolate {
            return Plan::General;
        }
        let index = match field {
            FieldId::Global => &self.global,
            FieldId::Frame => &self.frame.as_ref().unwrap().1,
        };
        let q = k.max(self.cfg.l);
        let reach = 1i128 << q;
        if field == FieldId::Global
            && self.module.has_create
            && x as i128 + reach > self.module.vacuum_base as i128
        {
            return Plan::General;
        }
        let lo = x as i128 - reach;
        let start = index.sorted.partition_point(|p| (p.0 as i128) <= lo);
        let mut terms = Vec::new();
        let mut centred = true;
        for &(c, packet, mass) in &index.sorted[start..] {
            let d = (x as i128 - c as i128).abs();
            if c as i128 >= x as i128 + reach {
                break;
            }
            let s_max = self.cfg.s_max(mass);
            if d >= 1i128 << k.max(s_max) {
                continue; // out of reach at every temperature
            }
            let weight = if d == 0 {
                1i128 << q
            } else if k >= s_max {
                centred = false;
                ((1i128 << k) - d) << (q - k)
            } else {
                return Plan::General; // the weight would depend on temperature
            };
            terms.push(Term { packet, weight: weight as i64 });
        }
        if !terms.is_empty() && k <= self.cfg.h {
            return Plan::General; // every such read is noisy
        }
        if centred {
            terms.iter_mut().for_each(|t| t.weight = 1);
            terms.sort_by_key(|t| t.packet);
            Plan::Terms(terms, 0)
        } else {
            Plan::Terms(terms, q)
        }
    }

    /// Does every element of a lattice read exactly itself plus parallel runs?
    fn plan_lattice(&mut self, field: FieldId, first: usize, count: usize, base: i64, stride: i64,
                    k: u32) -> Plan {
        if let Some(p) = self.lattice_memo.get(&(field, first, k)) {
            return p.clone();
        }
        let mut runs: Option<Vec<usize>> = None;
        let mut result = Plan::Lattice(Vec::new());
        for j in 0..count {
            let x = base + j as i64 * stride;
            let Plan::Terms(terms, 0) = self.plan_at(field, x, k) else {
                result = Plan::General;
                break;
            };
            let shifted: Vec<usize> = terms.iter().map(|t| t.packet.wrapping_sub(j)).collect();
            match &runs {
                None => runs = Some(shifted),
                Some(r) if *r == shifted => {}
                _ => {
                    result = Plan::General;
                    break;
                }
            }
        }
        if let (Plan::Lattice(_), Some(r)) = (&result, runs) {
            result = Plan::Lattice(r);
        }
        if count == 0 {
            result = Plan::General;
        }
        self.lattice_memo.insert((field, first, k), result.clone());
        result
    }

    fn friction(&self, level: u32) -> u64 {
        if self.frozen && self.sweep.is_none() {
            0
        } else {
            let n = level as u64 + 1;
            n * n
        }
    }

    fn read(&mut self, a: &Addr, k: u32, level: u32) -> Box<Read> {
        let friction = self.friction(level);
        let (field, loc, plan) = match a {
            Addr::Lit(x) => {
                (FieldId::Global, Loc::Const(*x), self.plan_at(FieldId::Global, *x, k))
            }
            Addr::At(Target::Ptr(v), off) => {
                let e = self.value(v, level);
                let e = if *off == 0 { e } else { Expr::Shifted(Box::new(e), *off) };
                (FieldId::Global, Loc::Dynamic(Box::new(e)), Plan::General)
            }
            Addr::At(Target::Named(n), off) => {
                let (field, sym) = self.lookup(n);
                let x = sym.base.wrapping_add(*off);
                (field, Loc::Const(x), self.plan_at(field, x, k))
            }
            Addr::At(Target::Elem(n, i), off) => {
                let (field, sym) = self.lookup(n);
                let (first, count, base, stride) =
                    (sym.first, sym.count.unwrap(), sym.base, sym.stride);
                let index = Box::new(self.value(i, level));
                let plan = if *off == 0 {
                    self.plan_lattice(field, first, count, base, stride, k)
                } else {
                    Plan::General
                };
                let loc = Loc::Lattice { first, count, base, stride, index, off: *off };
                (field, loc, plan)
            }
        };
        self.stats.reads += 1;
        if matches!(plan, Plan::General) {
            self.stats.general += 1;
        }
        Box::new(Read { field, loc, k, friction, plan })
    }

    fn value(&mut self, v: &Value, level: u32) -> Expr {
        match v {
            Value::Int(n) => Expr::Const(*n),
            Value::Read(a, k) => Expr::Read(self.read(a, *k, level)),
            Value::Intensity(a, k) => Expr::Square(self.read(a, *k, level)),
            Value::AddrOf(a) => match a {
                Addr::Lit(n) => Expr::Const(*n),
                Addr::At(Target::Named(n), 0) if self.sweep.as_deref() == Some(n) => {
                    Expr::SweepIndex
                }
                Addr::At(Target::Named(n), off) => {
                    Expr::Const(self.lookup(n).1.base.wrapping_add(*off))
                }
                Addr::At(Target::Elem(n, i), off) => {
                    let (base, stride) = {
                        let s = self.lookup(n).1;
                        (s.base, s.stride)
                    };
                    let index = Box::new(self.value(i, level));
                    Expr::Scaled { base, stride, index, off: *off }
                }
                Addr::At(Target::Ptr(v), off) => {
                    Expr::Shifted(Box::new(self.value(v, level)), *off)
                }
            },
            Value::Call { name, args, .. } => {
                let id = self.module.path_ids[name];
                Expr::Call(id, args.iter().map(|a| self.value(a, level)).collect())
            }
            Value::Absorb => Expr::Absorb,
            Value::Absorbc => Expr::Absorbc,
        }
    }

    fn place(&mut self, t: &Target, level: u32) -> Place {
        match t {
            Target::Named(n) => {
                let (field, sym) = self.lookup(n);
                match sym.count {
                    None => Place::Packet { field, index: sym.first },
                    Some(_) => Place::NotAPacket,
                }
            }
            Target::Elem(n, i) => {
                let (field, sym) = self.lookup(n);
                let (first, count, base, stride) =
                    (sym.first, sym.count.unwrap(), sym.base, sym.stride);
                let index = self.value(i, level);
                Place::Elem { field, first, count, base, stride, index }
            }
            Target::Ptr(v) => Place::Ptr(self.value(v, level)),
        }
    }

    fn cond(&mut self, c: &AstCond, level: u32) -> Cond {
        let left = self.value(&c.left, level);
        let right = self.value(&c.right, level);
        Cond { op: c.op, left, right }
    }

    fn block(&mut self, stmts: &[Stmt]) -> Vec<Op> {
        stmts.iter().filter_map(|s| self.stmt(s)).collect()
    }

    fn stmt(&mut self, s: &Stmt) -> Option<Op> {
        use StmtKind::*;
        let lv = s.level;
        Some(match &s.kind {
            Field(_) | Decl(_) | Path { .. } => return None,
            Write(t, v) => {
                let place = self.place(t, lv);
                let value = self.value(v, lv);
                Op::Write { place, value, friction: self.friction(lv) }
            }
            Raise(t) => Op::Ladder(self.place(t, lv), 1),
            Lower(t) => Op::Ladder(self.place(t, lv), -1),
            Flip(t) => Op::Flip(self.place(t, lv)),
            Swap(a, b) => {
                let (a, b) = (self.place(a, lv), self.place(b, lv));
                Op::Swap(a, b)
            }
            Fuse(a, b) => {
                let (a, b) = (self.place(a, lv), self.place(b, lv));
                Op::Fuse(a, b)
            }
            If(c, a, b) => {
                let c = self.cond(c, lv);
                let (a, b) = (self.block(a), self.block(b));
                Op::If(c, a, b)
            }
            While(c, b) => {
                let c = self.cond(c, lv);
                Op::While(c, self.block(b))
            }
            SymTime(b) => {
                if self.frozen {
                    // Already inside a time-symmetric block: nothing new to prove.
                    let body = self.block(b);
                    return Some(Op::If(
                        Cond { op: Cmp::Eq, left: Expr::Const(0), right: Expr::Const(0) },
                        body,
                        Vec::new(),
                    ));
                }
                self.frozen = true;
                let body = self.block(b);
                self.frozen = false;
                self.symmetric(None, body)
            }
            SymTranslate { index, lattice, body } => {
                let count = self.module.global.names[lattice].count.unwrap();
                self.frozen = true;
                self.sweep = Some(index.clone());
                let body = self.block(body);
                self.sweep = None;
                self.frozen = false;
                self.symmetric(Some(count), body)
            }
            Emit(v) => Op::Emit(self.value(v, lv)),
            EmitC(v) => Op::EmitC(self.value(v, lv)),
            EmitN(v) => Op::EmitN(self.value(v, lv)),
            EmitS(b) => Op::EmitS(b.clone()),
            Return(v) => Op::Return(self.value(v, lv)),
            CallStmt(v) => Op::Eval(self.value(v, lv)),
            Create(fermion, v) => {
                Op::Create { fermion: *fermion, addr: self.value(v, lv), friction: self.friction(lv) }
            }
            Annihilate(v) => Op::Annihilate { addr: self.value(v, lv), friction: self.friction(lv) },
        })
    }

    /// The Noether pass: turn a checked symmetry into a fast version when
    /// every read in it can be proved exact from a single entry guard.
    fn symmetric(&mut self, sweep: Option<usize>, body: Vec<Op>) -> Op {
        self.stats.symmetric += 1;
        let mut guards = Some(Vec::new());
        if self.opts.symmetry {
            collect_ops(&body, self, &mut guards);
        } else {
            guards = None;
        }
        if let Some(g) = &mut guards {
            g.sort_by_key(|x| format!("{x:?}"));
            g.dedup();
            self.stats.symmetric_fast += 1;
        }
        let promotable = match (&guards, sweep) {
            (Some(_), None) => promotable(&body),
            _ => Vec::new(),
        };
        Op::Symmetric(Symmetric { sweep, body, guards, promotable })
    }

    fn mass_of(&self, field: FieldId, packet: usize) -> u32 {
        match field {
            FieldId::Global => self.module.global.packets[packet].mass,
            FieldId::Frame => self.frame.as_ref().unwrap().0.packets[packet].mass,
        }
    }
}

/// Scalars used by a block, or nothing if any access is not a plain name.
fn promotable(ops: &[Op]) -> Vec<(FieldId, usize)> {
    fn place(p: &Place, out: &mut Vec<(FieldId, usize)>) -> bool {
        if let Place::Packet { field, index } = p {
            out.push((*field, *index));
            return true;
        }
        false
    }
    fn expr(e: &Expr, out: &mut Vec<(FieldId, usize)>) -> bool {
        match e {
            Expr::Const(_) => true,
            Expr::Read(r) | Expr::Square(r) => match (&r.plan, &r.loc) {
                (Plan::Terms(terms, _), Loc::Const(_)) => {
                    out.extend(terms.iter().map(|t| (r.field, t.packet)));
                    true
                }
                _ => false,
            },
            _ => false,
        }
    }
    fn walk(ops: &[Op], out: &mut Vec<(FieldId, usize)>) -> bool {
        ops.iter().all(|op| match op {
            Op::Ladder(p, _) | Op::Flip(p) => place(p, out),
            Op::Swap(a, b) | Op::Fuse(a, b) => place(a, out) && place(b, out),
            Op::If(c, a, b) => {
                expr(&c.left, out) && expr(&c.right, out) && walk(a, out) && walk(b, out)
            }
            Op::While(c, b) => expr(&c.left, out) && expr(&c.right, out) && walk(b, out),
            _ => false,
        })
    }
    let mut out = Vec::new();
    if !walk(ops, &mut out) {
        return Vec::new();
    }
    out.sort_by_key(|(f, i)| (*f == FieldId::Frame, *i));
    out.dedup();
    out
}

fn collect_read(r: &Read, lw: &Lower, out: &mut Option<Vec<Guard>>) {
    let k = r.k;
    match (&r.plan, &r.loc) {
        (Plan::Terms(terms, _), _) => {
            for t in terms {
                let tc_exp = lw.cfg.tc_exp(lw.mass_of(r.field, t.packet));
                if let Some(g) = out {
                    g.push(match r.field {
                        FieldId::Global => Guard::Run { first: t.packet, count: 1, tc_exp },
                        FieldId::Frame => Guard::Frame { tc_exp },
                    });
                }
            }
            if !terms.is_empty() && k <= lw.cfg.h {
                *out = None;
            }
        }
        (Plan::Lattice(runs), Loc::Lattice { count, index, .. }) => {
            collect_expr(index, lw, out);
            for &first in runs {
                let tc_exp = lw.cfg.tc_exp(lw.mass_of(r.field, first));
                if let Some(g) = out {
                    g.push(match r.field {
                        FieldId::Global => Guard::Run { first, count: *count, tc_exp },
                        FieldId::Frame => Guard::Frame { tc_exp },
                    });
                }
            }
        }
        _ => *out = None,
    }
}

fn collect_expr(e: &Expr, lw: &Lower, out: &mut Option<Vec<Guard>>) {
    match e {
        Expr::Read(r) | Expr::Square(r) => collect_read(r, lw, out),
        Expr::Scaled { index, .. } => collect_expr(index, lw, out),
        Expr::Shifted(v, _) => collect_expr(v, lw, out),
        Expr::Call(_, args) => args.iter().for_each(|a| collect_expr(a, lw, out)),
        Expr::Const(_) | Expr::SweepIndex | Expr::Absorb | Expr::Absorbc => {}
    }
}

fn collect_place(p: &Place, lw: &Lower, out: &mut Option<Vec<Guard>>) {
    match p {
        Place::Elem { index, .. } => collect_expr(index, lw, out),
        Place::Ptr(e) => collect_expr(e, lw, out),
        Place::Packet { .. } | Place::NotAPacket => {}
    }
}

fn collect_ops(ops: &[Op], lw: &Lower, out: &mut Option<Vec<Guard>>) {
    for op in ops {
        match op {
            Op::Write { place, value, .. } => {
                collect_place(place, lw, out);
                collect_expr(value, lw, out);
            }
            Op::Ladder(p, _) | Op::Flip(p) => collect_place(p, lw, out),
            Op::Swap(a, b) | Op::Fuse(a, b) => {
                collect_place(a, lw, out);
                collect_place(b, lw, out);
            }
            Op::If(c, a, b) => {
                collect_expr(&c.left, lw, out);
                collect_expr(&c.right, lw, out);
                collect_ops(a, lw, out);
                collect_ops(b, lw, out);
            }
            Op::While(c, b) => {
                collect_expr(&c.left, lw, out);
                collect_expr(&c.right, lw, out);
                collect_ops(b, lw, out);
            }
            _ => {}
        }
    }
}

pub fn lower(module: &Module, opts: Options) -> Program {
    let mut lw = Lower {
        cfg: &module.cfg,
        module,
        opts,
        global: Index::new(&module.global),
        frame: None,
        sweep: None,
        frozen: false,
        lattice_memo: HashMap::new(),
        stats: Stats::default(),
    };
    let main = lw.block(&module.body);
    let mut funcs = Vec::new();
    for path in &module.paths {
        lw.frame = Some((&path.layout, Index::new(&path.layout)));
        lw.lattice_memo.retain(|key, _| key.0 == FieldId::Global);
        let body = lw.block(&path.body);
        funcs.push(Func {
            name: path.name.clone(),
            params: path.params,
            centres: path.layout.packets.iter().map(|p| p.c).collect(),
            masses: path.layout.packets.iter().map(|p| p.mass).collect(),
            body,
        });
    }
    Program { funcs, main, stats: lw.stats }
}
