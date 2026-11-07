//! NIR to native code through Cranelift. Field operations are lowered here:
//! temperature bookkeeping inline, the noisy or unproved cases in the runtime.

use cranelift_codegen::ir::condcodes::IntCC;
use cranelift_codegen::ir::types::{I128, I32, I64};
use cranelift_codegen::ir::{
    AbiParam, FuncRef, Function, GlobalValue, InstBuilder, MemFlagsData, Signature,
    StackSlotData, StackSlotKind, UserFuncName, Value,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module as _};
use cranelift_object::{ObjectBuilder, ObjectModule};
use noether_nir::*;
use noether_sema::{Config, Module};
use noether_syntax::Cmp;
use std::collections::{BTreeMap, HashMap};

const T_MAX: i64 = 1 << 62;
const PACKET: i64 = 16;
const FRAME_HEADER: i64 = 32;
const TRAP_SHAPE: i64 = 0;
const TRAP_INDEX: i64 = 1;
const TRAP_FUSE: i64 = 2;

const RT_FUNCS: [(&str, usize, bool); 20] = [
    ("noe_init", 5, false),
    ("noe_finish", 1, false),
    ("noe_thermo_start", 2, false),
    ("noe_thermo", 3, false),
    ("noe_erased", 1, false),
    ("noe_tile_at", 1, true),
    ("noe_read_global", 3, true),
    ("noe_read_frame", 6, true),
    ("noe_resolve", 1, true),
    ("noe_guard", 5, true),
    ("noe_defer", 2, false),
    ("noe_flush", 1, false),
    ("noe_emit", 1, false),
    ("noe_emitc", 1, false),
    ("noe_emitn", 1, false),
    ("noe_emits", 2, false),
    ("noe_absorb", 0, true),
    ("noe_absorbc", 0, true),
    ("noe_trap", 2, false),
    ("noe_zero", 2, false),
];

struct Globals {
    packets: DataId,
    tiles: DataId,
    ptile: DataId,
    state: DataId,
    meta: DataId,
    keys: DataId,
    desc: DataId,
}

struct Shared<'p> {
    cfg: Config,
    module: &'p Module,
    program: &'p Program,
    /// Static tile key to dense slot.
    slots: BTreeMap<i64, usize>,
    /// Tile slot of each global packet.
    packet_slot: Vec<usize>,
    globals: Globals,
    rt: HashMap<&'static str, FuncId>,
    funcs: Vec<FuncId>,
    frame_meta: Vec<DataId>,
    /// Temperature recording is compiled in.
    thermo: bool,
    frame_sizes: Vec<usize>,
}

/// Which tile a heat or temperature query is about.
#[derive(Clone, Copy)]
enum TileRef {
    Slot(usize),
    Frame,
    Addr(Value),
}

impl TileRef {
    fn same(self, other: TileRef) -> bool {
        match (self, other) {
            (TileRef::Slot(a), TileRef::Slot(b)) => a == b,
            (TileRef::Frame, TileRef::Frame) => true,
            (TileRef::Addr(a), TileRef::Addr(b)) => a == b,
            _ => false,
        }
    }
}

struct Resolved {
    /// Set when the packet lives in a register for this region.
    promoted: Option<Variable>,
    ptr: Value,
    centre: Value,
    field: FieldId,
    /// Tile, or the address it must be looked up from.
    tile: Option<TileRef>,
}

struct Gen<'a, 'b> {
    b: FunctionBuilder<'b>,
    m: &'a mut ObjectModule,
    sh: &'a Shared<'a>,
    tick: Variable,
    frame: Option<Value>,
    frame_index: Option<usize>,
    data: HashMap<DataId, GlobalValue>,
    fns: HashMap<FuncId, FuncRef>,
    strings: &'a mut Vec<(DataId, Vec<u8>)>,
    /// Inside a symmetric block: the clock stands still.
    frozen: bool,
    /// Fast version: the entry guard proved every read exact.
    assume_cold: bool,
    /// General version of a sweep: heat waits until the block ends.
    deferred: bool,
    sweep: Option<Variable>,
    /// Packets kept in registers inside the fast version of a block.
    promoted: HashMap<(FieldId, usize), Variable>,
}

impl Gen<'_, '_> {
    // ---------------------------------------------------------- basics

    fn iconst(&mut self, v: i64) -> Value {
        self.b.ins().iconst(I64, v)
    }

    fn sym(&mut self, id: DataId) -> Value {
        let gv = match self.data.get(&id) {
            Some(gv) => *gv,
            None => {
                let gv = self.m.declare_data_in_func(id, self.b.func);
                self.data.insert(id, gv);
                gv
            }
        };
        self.b.ins().symbol_value(I64, gv)
    }

    fn call_id(&mut self, id: FuncId, args: &[Value]) -> Option<Value> {
        let fr = match self.fns.get(&id) {
            Some(fr) => *fr,
            None => {
                let fr = self.m.declare_func_in_func(id, self.b.func);
                self.fns.insert(id, fr);
                fr
            }
        };
        let inst = self.b.ins().call(fr, args);
        self.b.inst_results(inst).first().copied()
    }

    fn rt(&mut self, name: &'static str, args: &[Value]) -> Option<Value> {
        let id = self.sh.rt[name];
        self.call_id(id, args)
    }

    fn load(&mut self, ptr: Value, off: i32) -> Value {
        self.b.ins().load(I64, MemFlagsData::trusted(), ptr, off)
    }

    fn store(&mut self, v: Value, ptr: Value, off: i32) {
        self.b.ins().store(MemFlagsData::trusted(), v, ptr, off);
    }

    fn tick_value(&mut self) -> Value {
        self.b.use_var(self.tick)
    }

    fn advance(&mut self) {
        if !self.frozen {
            let t = self.tick_value();
            let t = self.b.ins().iadd_imm_s(t, 1);
            self.b.def_var(self.tick, t);
        }
    }

    fn trap(&mut self, kind: i64, a: Value) {
        let k = self.iconst(kind);
        self.rt("noe_trap", &[k, a]);
    }

    // ---------------------------------------------------------- temperature

    fn tile_addr(&mut self, tile: TileRef) -> Value {
        match tile {
            TileRef::Slot(s) => {
                let base = self.sym(self.sh.globals.tiles);
                self.b.ins().iadd_imm_s(base, s as i64 * 16)
            }
            TileRef::Frame => self.frame.unwrap(),
            TileRef::Addr(v) => v,
        }
    }

    /// Lazy Newton cooling; returns the current temperature.
    fn cool(&mut self, tile: Value) -> Value {
        let cfg = &self.sh.cfg;
        let (c, tmin) = (cfg.c as i64, cfg.tmin as i64);
        let tick = self.tick_value();
        let now = self.b.ins().ushr_imm_s(tick, c);
        let t0 = self.load(tile, 0);
        let epoch = self.load(tile, 8);
        let stale = self.b.ins().icmp(IntCC::UnsignedGreaterThan, now, epoch);
        let cold = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, I64);
        self.b.set_cold_block(cold);
        self.b.ins().brif(stale, cold, &[], done, &[t0.into()]);
        self.b.switch_to_block(cold);
        self.b.seal_block(cold);
        let gap = self.b.ins().isub(now, epoch);
        let cap = self.iconst(63);
        let gap = self.b.ins().umin(gap, cap);
        let t = self.b.ins().ushr(t0, gap);
        let floor = self.iconst(tmin);
        let t = self.b.ins().umax(t, floor);
        self.store(t, tile, 0);
        self.store(now, tile, 8);
        self.b.ins().jump(done, &[t.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        self.b.block_params(done)[0]
    }

    /// Adds heat. Returns the new temperature when it is usable right away.
    fn heat(&mut self, tile: Value, q: Value, bits: Option<Value>) -> Option<Value> {
        if self.deferred {
            self.rt("noe_defer", &[tile, q]);
            if let (true, Some(b)) = (self.sh.thermo, bits) {
                self.rt("noe_erased", &[b]);
            }
            return None;
        }
        let t = self.cool(tile);
        let t = self.b.ins().iadd(t, q);
        let cap = self.iconst(T_MAX);
        let t = self.b.ins().umin(t, cap);
        self.store(t, tile, 0);
        if self.sh.thermo {
            let b = match bits {
                Some(b) => b,
                None => self.iconst(0),
            };
            let tick = self.tick_value();
            self.rt("noe_thermo", &[tile, tick, b]);
        }
        Some(t)
    }

    fn below_critical(&mut self, t: Value, tc_exp: i64) -> Option<Value> {
        if tc_exp >= 63 {
            return None; // always cold enough
        }
        let limit = self.iconst(1i64 << tc_exp.max(0));
        Some(self.b.ins().icmp(IntCC::UnsignedLessThan, t, limit))
    }

    fn warm_enough(&mut self, t: Value, lo_exp: i64) -> Option<Value> {
        if lo_exp <= 0 {
            return None; // every temperature is warm enough
        }
        let limit = self.iconst(1i64 << lo_exp.min(62));
        Some(self.b.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, t, limit))
    }

    /// The temperature of a tile, reusing what this read already loaded.
    fn known_temp(&mut self, tile: TileRef, seen: &mut Vec<(TileRef, Value)>) -> Value {
        if let Some((_, v)) = seen.iter().find(|(t, _)| t.same(tile)) {
            return *v;
        }
        let addr = self.tile_addr(tile);
        let t = self.cool(addr);
        seen.push((tile, t));
        t
    }

    // ---------------------------------------------------------- reads

    fn global_packet_slot(&self, index: usize) -> TileRef {
        TileRef::Slot(self.sh.packet_slot[index])
    }

    fn static_tile(&self, field: FieldId, x: i64) -> Option<TileRef> {
        match field {
            FieldId::Frame => Some(TileRef::Frame),
            FieldId::Global => {
                self.sh.slots.get(&(x >> self.sh.cfg.b)).map(|s| TileRef::Slot(*s))
            }
        }
    }

    fn packet_ptr(&mut self, field: FieldId, index: usize) -> (Value, i32) {
        match field {
            FieldId::Global => {
                (self.sym(self.sh.globals.packets), (index as i64 * PACKET) as i32)
            }
            FieldId::Frame => {
                (self.frame.unwrap(), (FRAME_HEADER + index as i64 * PACKET) as i32)
            }
        }
    }

    fn general(&mut self, field: FieldId, x: Value, k: u32) -> Value {
        let kv = self.iconst(k as i64);
        let tick = self.tick_value();
        match field {
            FieldId::Global => self.rt("noe_read_global", &[x, kv, tick]).unwrap(),
            FieldId::Frame => {
                let f = self.frame_index.unwrap();
                let meta = self.sym(self.sh.frame_meta[f]);
                let n = self.iconst(self.sh.frame_sizes[f] as i64);
                let frame = self.frame.unwrap();
                self.rt("noe_read_frame", &[frame, meta, n, x, kv, tick]).unwrap()
            }
        }
    }

    fn packet_value(&mut self, field: FieldId, index: usize) -> Value {
        if let Some(var) = self.promoted.get(&(field, index)) {
            return self.b.use_var(*var);
        }
        let (ptr, off) = self.packet_ptr(field, index);
        self.load(ptr, off)
    }

    fn get(&mut self, r: &Resolved) -> Value {
        match r.promoted {
            Some(var) => self.b.use_var(var),
            None => self.load(r.ptr, 0),
        }
    }

    fn set(&mut self, r: &Resolved, v: Value) {
        match r.promoted {
            Some(var) => self.b.def_var(var, v),
            None => self.store(v, r.ptr, 0),
        }
    }

    /// Sums a static term list, assuming no packet is noisy.
    fn sum_terms(&mut self, field: FieldId, terms: &[Term], shift: u32) -> Value {
        if shift == 0 {
            let mut acc = None;
            for t in terms {
                let v = self.packet_value(field, t.packet);
                acc = Some(match acc {
                    Some(a) => self.b.ins().iadd(a, v),
                    None => v,
                });
            }
            return acc.unwrap();
        }
        let mut acc = None;
        for t in terms {
            let v = self.packet_value(field, t.packet);
            let v = self.b.ins().sextend(I128, v);
            let w = self.iconst(t.weight);
            let w = self.b.ins().sextend(I128, w);
            let p = self.b.ins().imul(v, w);
            acc = Some(match acc {
                Some(a) => self.b.ins().iadd(a, p),
                None => p,
            });
        }
        let s = self.iconst(shift as i64);
        let r = self.b.ins().sshr(acc.unwrap(), s);
        self.b.ins().ireduce(I64, r)
    }

    fn fast_or_slow(
        &mut self,
        ok: Option<Value>,
        fast: impl FnOnce(&mut Self) -> Value,
        slow: impl FnOnce(&mut Self) -> Value,
    ) -> Value {
        let Some(ok) = ok else { return fast(self) };
        let fb = self.b.create_block();
        let sb = self.b.create_block();
        let done = self.b.create_block();
        self.b.append_block_param(done, I64);
        self.b.set_cold_block(sb);
        self.b.ins().brif(ok, fb, &[], sb, &[]);
        self.b.switch_to_block(fb);
        self.b.seal_block(fb);
        let v = fast(self);
        self.b.ins().jump(done, &[v.into()]);
        self.b.switch_to_block(sb);
        self.b.seal_block(sb);
        let v = slow(self);
        self.b.ins().jump(done, &[v.into()]);
        self.b.switch_to_block(done);
        self.b.seal_block(done);
        self.b.block_params(done)[0]
    }

    fn and(&mut self, a: Option<Value>, b: Option<Value>) -> Option<Value> {
        match (a, b) {
            (Some(a), Some(b)) => Some(self.b.ins().band(a, b)),
            (x, None) | (None, x) => x,
        }
    }

    fn read_terms(
        &mut self,
        r: &Read,
        x: i64,
        terms: &[Term],
        shift: u32,
        warm: &[(usize, i64)],
        friction_tile: TileRef,
        hot: Option<Value>,
    ) -> Value {
        let mut ok = None;
        if !self.assume_cold {
            let mut seen = Vec::new();
            if let Some(v) = hot {
                seen.push((friction_tile, v));
            }
            let tile_of = |g: &Self, packet: usize| match r.field {
                FieldId::Frame => TileRef::Frame,
                FieldId::Global => g.global_packet_slot(packet),
            };
            for t in terms {
                let tile = tile_of(self, t.packet);
                let temp = self.known_temp(tile, &mut seen);
                let tc = self.sh.cfg.tc_exp();
                let c = self.below_critical(temp, tc);
                ok = self.and(ok, c);
            }
            for (packet, lo_exp) in warm {
                let tile = tile_of(self, *packet);
                let temp = self.known_temp(tile, &mut seen);
                let c = self.warm_enough(temp, *lo_exp);
                ok = self.and(ok, c);
            }
        }
        let field = r.field;
        let k = r.k;
        self.fast_or_slow(
            ok,
            |g| {
                if terms.is_empty() {
                    g.iconst(0)
                } else {
                    g.sum_terms(field, terms, shift)
                }
            },
            |g| {
                let xv = g.iconst(x);
                g.general(field, xv, k)
            },
        )
    }

    fn read(&mut self, r: &Read) -> Value {
        match &r.loc {
            Loc::Const(x) => {
                let x = *x;
                self.advance();
                let tile = self.static_tile(r.field, x);
                let xv = self.iconst(x);
                let tile = match tile {
                    Some(t) => t,
                    None => {
                        let p = self.rt("noe_tile_at", &[xv]).unwrap();
                        TileRef::Addr(p)
                    }
                };
                let hot = self.friction(tile, r.friction);
                match &r.plan {
                    Plan::Terms(terms, shift, warm) => {
                        self.read_terms(r, x, terms, *shift, warm, tile, hot)
                    }
                    _ => self.general(r.field, xv, r.k),
                }
            }
            Loc::Lattice { first: _, count, base, stride, index, off } => {
                let i = self.expr(index);
                let scaled = self.b.ins().imul_imm_s(i, *stride);
                let x = self.b.ins().iadd_imm_s(scaled, base.wrapping_add(*off));
                self.advance();
                if let Plan::Lattice(runs) = &r.plan {
                    let inside = self.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, i, *count as i64);
                    let field = r.field;
                    let runs = runs.clone();
                    let (k, friction) = (r.k, r.friction);
                    self.fast_or_slow(
                        Some(inside),
                        |g| g.read_lattice(field, &runs, i, x, k, friction),
                        |g| g.read_at(field, x, k, friction),
                    )
                } else {
                    self.read_at(r.field, x, r.k, r.friction)
                }
            }
            Loc::Dynamic(e) => {
                let x = self.expr(e);
                self.advance();
                self.read_at(r.field, x, r.k, r.friction)
            }
        }
    }

    fn friction(&mut self, tile: TileRef, q: u64) -> Option<Value> {
        if q == 0 {
            return None;
        }
        let addr = self.tile_addr(tile);
        let qv = self.iconst(q as i64);
        self.heat(addr, qv, None)
    }

    /// Full rule at a run-time address.
    fn read_at(&mut self, field: FieldId, x: Value, k: u32, friction: u64) -> Value {
        if friction > 0 {
            let tile = match field {
                FieldId::Frame => TileRef::Frame,
                FieldId::Global => TileRef::Addr(self.rt("noe_tile_at", &[x]).unwrap()),
            };
            self.friction(tile, friction);
        }
        self.general(field, x, k)
    }

    fn element_tile(&mut self, field: FieldId, first: usize, i: Value) -> Value {
        match field {
            FieldId::Frame => self.frame.unwrap(),
            FieldId::Global => {
                let table = self.sym(self.sh.globals.ptile);
                let scaled = self.b.ins().ishl_imm_s(i, 3);
                let at = self.b.ins().iadd(table, scaled);
                let slot = self.load(at, first as i32 * 8);
                let tiles = self.sym(self.sh.globals.tiles);
                self.b.ins().iadd(tiles, slot)
            }
        }
    }

    fn read_lattice(
        &mut self,
        field: FieldId,
        runs: &[(usize, i64)],
        i: Value,
        x: Value,
        k: u32,
        friction: u64,
    ) -> Value {
        let hot = if friction > 0 {
            let tile = self.element_tile(field, runs[0].0, i);
            let q = self.iconst(friction as i64);
            self.heat(tile, q, None)
        } else {
            None
        };
        let mut ok = None;
        if !self.assume_cold {
            for (n, &(first, lo_exp)) in runs.iter().enumerate() {
                let temp = match hot {
                    Some(t) if n == 0 => t,
                    _ => {
                        let tile = self.element_tile(field, first, i);
                        self.cool(tile)
                    }
                };
                let tc = self.sh.cfg.tc_exp();
                let c = self.below_critical(temp, tc);
                ok = self.and(ok, c);
                let c = self.warm_enough(temp, lo_exp);
                ok = self.and(ok, c);
            }
        }
        self.fast_or_slow(
            ok,
            |g| {
                let scaled = g.b.ins().ishl_imm_s(i, 4);
                let mut acc = None;
                for &(first, _) in runs {
                    let (base, off) = g.packet_ptr(field, first);
                    let at = g.b.ins().iadd(base, scaled);
                    let v = g.load(at, off);
                    acc = Some(match acc {
                        Some(a) => g.b.ins().iadd(a, v),
                        None => v,
                    });
                }
                acc.unwrap()
            },
            |g| g.general(field, x, k),
        )
    }

    // ---------------------------------------------------------- expressions

    fn expr(&mut self, e: &Expr) -> Value {
        match e {
            Expr::Const(v) => self.iconst(*v),
            Expr::SweepIndex => self.b.use_var(self.sweep.unwrap()),
            Expr::Read(r) => self.read(r),
            Expr::Square(r) => {
                let v = self.read(r);
                self.b.ins().imul(v, v)
            }
            Expr::Scaled { base, stride, index, off } => {
                let i = self.expr(index);
                let s = self.b.ins().imul_imm_s(i, *stride);
                self.b.ins().iadd_imm_s(s, base.wrapping_add(*off))
            }
            Expr::Shifted(v, off) => {
                let v = self.expr(v);
                self.b.ins().iadd_imm_s(v, *off)
            }
            Expr::Call(id, args) => {
                let vals: Vec<Value> = args.iter().map(|a| self.expr(a)).collect();
                self.sync_tick_out();
                let f = self.sh.funcs[*id];
                let r = self.call_id(f, &vals).unwrap();
                self.sync_tick_in();
                r
            }
            Expr::Absorb => self.rt("noe_absorb", &[]).unwrap(),
            Expr::Absorbc => self.rt("noe_absorbc", &[]).unwrap(),
        }
    }

    fn sync_tick_out(&mut self) {
        let t = self.tick_value();
        let s = self.sym(self.sh.globals.state);
        self.store(t, s, 0);
    }

    fn sync_tick_in(&mut self) {
        let s = self.sym(self.sh.globals.state);
        let t = self.load(s, 0);
        self.b.def_var(self.tick, t);
    }

    fn cond(&mut self, c: &Cond) -> Value {
        let l = self.expr(&c.left);
        let r = self.expr(&c.right);
        let cc = match c.op {
            Cmp::Lt => IntCC::SignedLessThan,
            Cmp::Gt => IntCC::SignedGreaterThan,
            Cmp::Le => IntCC::SignedLessThanOrEqual,
            Cmp::Ge => IntCC::SignedGreaterThanOrEqual,
            Cmp::Eq => IntCC::Equal,
            Cmp::Ne => IntCC::NotEqual,
        };
        self.b.ins().icmp(cc, l, r)
    }

    // ---------------------------------------------------------- places

    fn trap_unless(&mut self, ok: Value, kind: i64, a: Value) {
        let good = self.b.create_block();
        let bad = self.b.create_block();
        self.b.set_cold_block(bad);
        self.b.ins().brif(ok, good, &[], bad, &[]);
        self.b.switch_to_block(bad);
        self.b.seal_block(bad);
        self.trap(kind, a);
        self.b.ins().jump(good, &[]);
        self.b.switch_to_block(good);
        self.b.seal_block(good);
    }

    fn place(&mut self, p: &Place) -> Resolved {
        match p {
            Place::Packet { field, index } => {
                let (base, off) = self.packet_ptr(*field, *index);
                let ptr = self.b.ins().iadd_imm_s(base, off as i64);
                let (c, tile) = match field {
                    FieldId::Frame => {
                        let f = self.frame_index.unwrap();
                        (self.sh.program.funcs[f].centres[*index], Some(TileRef::Frame))
                    }
                    FieldId::Global => (
                        self.sh.module.global.packets[*index].c,
                        Some(self.global_packet_slot(*index)),
                    ),
                };
                let centre = self.iconst(c);
                let promoted = self.promoted.get(&(*field, *index)).copied();
                Resolved { promoted, ptr, centre, field: *field, tile }
            }
            Place::Elem { field, first, count, base, stride, index } => {
                let i = self.expr(index);
                let inside = self.b.ins().icmp_imm_s(IntCC::UnsignedLessThan, i, *count as i64);
                self.trap_unless(inside, TRAP_INDEX, i);
                let (pbase, off) = self.packet_ptr(*field, *first);
                let scaled = self.b.ins().ishl_imm_s(i, 4);
                let at = self.b.ins().iadd(pbase, scaled);
                let ptr = self.b.ins().iadd_imm_s(at, off as i64);
                let s = self.b.ins().imul_imm_s(i, *stride);
                let centre = self.b.ins().iadd_imm_s(s, *base);
                let tile = self.element_tile(*field, *first, i);
                Resolved {
                    promoted: None,
                    ptr,
                    centre,
                    field: *field,
                    tile: Some(TileRef::Addr(tile)),
                }
            }
            Place::Ptr(e) => {
                let x = self.expr(e);
                let ptr = self.rt("noe_resolve", &[x]).unwrap();
                Resolved { promoted: None, ptr, centre: x, field: FieldId::Global, tile: None }
            }
            Place::NotAPacket => {
                let z = self.iconst(0);
                self.trap(TRAP_SHAPE, z);
                let ptr = self.sym(self.sh.globals.packets);
                Resolved { promoted: None, ptr, centre: z, field: FieldId::Global, tile: None }
            }
        }
    }

    fn tile_of(&mut self, r: &Resolved) -> Value {
        match r.tile {
            Some(t) => self.tile_addr(t),
            None => self.rt("noe_tile_at", &[r.centre]).unwrap(),
        }
    }

    // ---------------------------------------------------------- statements

    fn ops(&mut self, ops: &[Op]) {
        for op in ops {
            self.op(op);
        }
    }

    fn op(&mut self, op: &Op) {
        match op {
            Op::Write { place, value, friction } => {
                let target = self.place(place);
                let v = self.expr(value);
                self.advance();
                let old = self.load(target.ptr, 0);
                let diff = self.b.ins().bxor(old, v);
                let bits = self.b.ins().popcnt(diff);
                let q = self.b.ins().iadd_imm_s(bits, *friction as i64);
                let tile = self.tile_of(&target);
                self.heat(tile, q, Some(bits));
                self.store(v, target.ptr, 0);
            }
            Op::Ladder(p, delta) => {
                let t = self.place(p);
                self.advance();
                let v = self.get(&t);
                let v = self.b.ins().iadd_imm_s(v, *delta);
                self.set(&t, v);
            }
            Op::Flip(p) => {
                let t = self.place(p);
                self.advance();
                let v = self.get(&t);
                let v = self.b.ins().ineg(v);
                self.set(&t, v);
            }
            Op::Swap(a, b) => {
                let a = self.place(a);
                let b = self.place(b);
                self.advance();
                let (x, y) = (self.get(&a), self.get(&b));
                self.set(&a, y);
                self.set(&b, x);
            }
            Op::Fuse(a, b) => {
                let a = self.place(a);
                let b = self.place(b);
                let z = self.iconst(0);
                if a.field != b.field {
                    self.trap(TRAP_FUSE, z);
                } else {
                    let same = self.b.ins().icmp(IntCC::Equal, a.centre, b.centre);
                    self.trap_unless(same, TRAP_FUSE, z);
                }
                self.advance();
                let (x, y) = (self.get(&a), self.get(&b));
                let s = self.b.ins().iadd(x, y);
                self.set(&a, s);
            }
            Op::If(c, then, other) => {
                let v = self.cond(c);
                let tb = self.b.create_block();
                let eb = self.b.create_block();
                let done = self.b.create_block();
                self.b.ins().brif(v, tb, &[], eb, &[]);
                self.b.switch_to_block(tb);
                self.b.seal_block(tb);
                self.ops(then);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(eb);
                self.b.seal_block(eb);
                self.ops(other);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                self.b.seal_block(done);
            }
            Op::While(c, body) => {
                // Rotated: the condition is emitted at both ends so an
                // iteration takes one branch. It still runs once per iteration.
                let bb = self.b.create_block();
                let done = self.b.create_block();
                let v = self.cond(c);
                self.b.ins().brif(v, bb, &[], done, &[]);
                self.b.switch_to_block(bb);
                self.ops(body);
                let v = self.cond(c);
                self.b.ins().brif(v, bb, &[], done, &[]);
                self.b.seal_block(bb);
                self.b.switch_to_block(done);
                self.b.seal_block(done);
            }
            Op::Symmetric(s) => self.symmetric(s),
            Op::Emit(e) => {
                let v = self.expr(e);
                self.rt("noe_emit", &[v]);
            }
            Op::EmitC(e) => {
                let v = self.expr(e);
                self.rt("noe_emitc", &[v]);
            }
            Op::EmitN(e) => {
                let v = self.expr(e);
                self.rt("noe_emitn", &[v]);
            }
            Op::EmitS(bytes) => {
                let id = self
                    .m
                    .declare_anonymous_data(false, false)
                    .expect("string data");
                self.strings.push((id, bytes.clone()));
                let p = self.sym(id);
                let n = self.iconst(bytes.len() as i64);
                self.rt("noe_emits", &[p, n]);
            }
            Op::Return(e) => {
                let v = self.expr(e);
                self.sync_tick_out();
                self.b.ins().return_(&[v]);
                let dead = self.b.create_block();
                self.b.switch_to_block(dead);
                self.b.seal_block(dead);
            }
            Op::Eval(e) => {
                self.expr(e);
            }
        }
    }

    /// One symmetric region: guard, then the fast or the general version.
    fn symmetric(&mut self, s: &Symmetric) {
        let saved = (self.frozen, self.assume_cold, self.deferred);
        self.frozen = true;
        let Some(guards) = &s.guards else {
            self.version(s, false);
            (self.frozen, self.assume_cold, self.deferred) = saved;
            return;
        };
        let mut ok = None;
        for g in guards {
            let c = match *g {
                Guard::Run { first, count, tc_exp, lo_exp } => {
                    if tc_exp >= 63 && lo_exp <= 0 {
                        None
                    } else {
                        let hi = if tc_exp >= 63 { -1 } else { 1i64 << tc_exp.max(0) };
                        let lo = 1i64 << lo_exp.clamp(0, 62);
                        let (f, n) = (self.iconst(first as i64), self.iconst(count as i64));
                        let (h, l) = (self.iconst(hi), self.iconst(lo));
                        let t = self.tick_value();
                        let r = self.rt("noe_guard", &[f, n, h, l, t]).unwrap();
                        Some(self.b.ins().icmp_imm_s(IntCC::NotEqual, r, 0))
                    }
                }
                Guard::Frame { tc_exp, lo_exp } => {
                    let tile = self.frame.unwrap();
                    let t = self.cool(tile);
                    let hot = self.below_critical(t, tc_exp);
                    let cold = self.warm_enough(t, lo_exp);
                    self.and(hot, cold)
                }
            };
            ok = self.and(ok, c);
        }
        match ok {
            None => self.version(s, true),
            Some(ok) => {
                let fast = self.b.create_block();
                let slow = self.b.create_block();
                let done = self.b.create_block();
                self.b.ins().brif(ok, fast, &[], slow, &[]);
                self.b.switch_to_block(fast);
                self.b.seal_block(fast);
                self.version(s, true);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(slow);
                self.b.seal_block(slow);
                self.version(s, false);
                self.b.ins().jump(done, &[]);
                self.b.switch_to_block(done);
                self.b.seal_block(done);
            }
        }
        (self.frozen, self.assume_cold, self.deferred) = saved;
    }

    fn version(&mut self, s: &Symmetric, fast: bool) {
        self.assume_cold = fast;
        self.deferred = !fast && s.sweep.is_some();
        match s.sweep {
            None if fast && !s.promotable.is_empty() => {
                let mut vars = Vec::new();
                for &(field, index) in &s.promotable {
                    let var = self.b.declare_var(I64);
                    let v = self.packet_value(field, index);
                    self.b.def_var(var, v);
                    vars.push((field, index, var));
                }
                for &(field, index, var) in &vars {
                    self.promoted.insert((field, index), var);
                }
                self.ops(&s.body);
                self.promoted.clear();
                for (field, index, var) in vars {
                    let v = self.b.use_var(var);
                    let (ptr, off) = self.packet_ptr(field, index);
                    self.store(v, ptr, off);
                }
            }
            None => self.ops(&s.body),
            Some(count) => {
                let k = self.b.declare_var(I64);
                let zero = self.iconst(0);
                self.b.def_var(k, zero);
                let saved = self.sweep.replace(k);
                let body = self.b.create_block();
                let done = self.b.create_block();
                if count > 0 {
                    self.b.ins().jump(body, &[]);
                } else {
                    self.b.ins().jump(done, &[]);
                }
                self.b.switch_to_block(body);
                self.ops(&s.body);
                let i = self.b.use_var(k);
                let i = self.b.ins().iadd_imm_s(i, 1);
                self.b.def_var(k, i);
                let more = self.b.ins().icmp_imm_s(IntCC::SignedLessThan, i, count as i64);
                self.b.ins().brif(more, body, &[], done, &[]);
                self.b.seal_block(body);
                self.b.switch_to_block(done);
                self.b.seal_block(done);
                self.sweep = saved;
                if self.deferred {
                    let t = self.tick_value();
                    self.rt("noe_flush", &[t]);
                }
            }
        }
    }
}

fn zero_data(m: &mut ObjectModule, name: &str, size: usize) -> DataId {
    let id = m.declare_data(name, Linkage::Local, true, false).unwrap();
    let mut d = DataDescription::new();
    d.define_zeroinit(size.max(16));
    m.define_data(id, &d).unwrap();
    id
}

fn words_data(m: &mut ObjectModule, name: &str, words: &[i64]) -> DataId {
    let id = m.declare_data(name, Linkage::Local, false, false).unwrap();
    let mut d = DataDescription::new();
    let mut bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    bytes.resize(bytes.len().max(16), 0);
    d.define(bytes.into_boxed_slice());
    m.define_data(id, &d).unwrap();
    id
}

fn signature(m: &ObjectModule, params: usize, ret: bool) -> Signature {
    let mut s = m.make_signature();
    s.params.extend(std::iter::repeat_n(AbiParam::new(I64), params));
    if ret {
        s.returns.push(AbiParam::new(I64));
    }
    s
}

/// Compiles a program to the bytes of a native object file.
///
/// With `thermo`, the program records temperatures and writes them to that
/// file when it ends. The recording does not change what the program computes.
pub fn compile(
    module: &Module,
    program: &Program,
    thermo: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut flags = settings::builder();
    let set = |f: &mut settings::Builder, k: &str, v: &str| f.set(k, v).map_err(|e| e.to_string());
    set(&mut flags, "opt_level", "speed")?;
    set(&mut flags, "is_pic", "false")?;
    set(&mut flags, "enable_probestack", "true")?;
    set(&mut flags, "probestack_strategy", "inline")?;
    let isa = cranelift_native::builder()
        .map_err(|e| e.to_string())?
        .finish(settings::Flags::new(flags))
        .map_err(|e| e.to_string())?;
    let ob = ObjectBuilder::new(isa, "noether", cranelift_module::default_libcall_names())
        .map_err(|e| e.to_string())?;
    let mut m = ObjectModule::new(ob);

    // static layout
    let cfg = module.cfg.clone();
    let tables = module.tables();
    let (slots, packet_slot, meta, keys, desc) =
        (tables.slots, tables.packet_slot, tables.meta, tables.keys, tables.desc);
    let n = module.global.packets.len();
    let ptile: Vec<i64> = packet_slot.iter().map(|s| *s as i64 * 16).collect();
    let globals = Globals {
        packets: zero_data(&mut m, "noe_packets", n * 16),
        tiles: zero_data(&mut m, "noe_tiles", slots.len() * 16),
        ptile: words_data(&mut m, "noe_ptile", &ptile),
        state: zero_data(&mut m, "noe_state", 16),
        meta: words_data(&mut m, "noe_meta", &meta),
        keys: words_data(&mut m, "noe_keys", &keys),
        desc: words_data(&mut m, "noe_desc", &desc),
    };
    let mut rt = HashMap::new();
    for (name, params, ret) in RT_FUNCS {
        let sig = signature(&m, params, ret);
        let id = m.declare_function(name, Linkage::Import, &sig).map_err(|e| e.to_string())?;
        rt.insert(name, id);
    }
    let mut funcs = Vec::new();
    let mut frame_meta = Vec::new();
    let mut frame_sizes = Vec::new();
    for (i, f) in program.funcs.iter().enumerate() {
        let sig = signature(&m, f.params, true);
        let id = m
            .declare_function(&format!("noe_path_{i}_{}", f.name), Linkage::Local, &sig)
            .map_err(|e| e.to_string())?;
        funcs.push(id);
        frame_meta.push(words_data(&mut m, &format!("noe_fmeta_{i}"), &f.centres));
        frame_sizes.push(f.centres.len());
    }
    let sh = Shared {
        cfg,
        module,
        program,
        slots,
        packet_slot,
        globals,
        rt,
        funcs,
        frame_meta,
        thermo: thermo.is_some(),
        frame_sizes,
    };
    let mut strings = Vec::new();
    let mut fbc = FunctionBuilderContext::new();

    for (i, f) in program.funcs.iter().enumerate() {
        let sig = signature(&m, f.params, true);
        let mut func = Function::with_name_signature(UserFuncName::user(0, i as u32), sig);
        let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        builder.seal_block(entry);
        let size = FRAME_HEADER as u32 + f.centres.len() as u32 * PACKET as u32;
        let slot = builder.create_sized_stack_slot(StackSlotData::new(
            StackSlotKind::ExplicitSlot,
            size,
            4,
        ));
        let tick = builder.declare_var(I64);
        let mut g = Gen {
            b: builder,
            m: &mut m,
            sh: &sh,
            tick,
            frame: None,
            frame_index: Some(i),
            data: HashMap::new(),
            fns: HashMap::new(),
            strings: &mut strings,
            frozen: false,
            assume_cold: false,
            deferred: false,
            sweep: None,
            promoted: HashMap::new(),
        };
        let frame = g.b.ins().stack_addr(I64, slot, 0);
        g.frame = Some(frame);
        g.sync_tick_in();
        let state = g.sym(sh.globals.state);
        let serial = g.load(state, 8);
        let serial = g.b.ins().iadd_imm_s(serial, 1);
        g.store(serial, state, 8);
        let start = g.iconst(sh.cfg.call_temp() as i64);
        let zero = g.iconst(0);
        g.store(start, frame, 0);
        let tick = g.tick_value();
        let epoch = g.b.ins().ushr_imm_s(tick, sh.cfg.c as i64);
        g.store(epoch, frame, 8);
        g.store(serial, frame, 16);
        if f.centres.len() <= 24 {
            for j in 0..f.centres.len() {
                let off = (FRAME_HEADER + j as i64 * PACKET) as i32;
                g.store(zero, frame, off);
                g.store(zero, frame, off + 8);
            }
        } else {
            let start = g.b.ins().iadd_imm_s(frame, FRAME_HEADER);
            let bytes = g.iconst(f.centres.len() as i64 * PACKET);
            g.rt("noe_zero", &[start, bytes]);
        }
        let params: Vec<Value> = g.b.block_params(entry).to_vec();
        for (j, p) in params.iter().enumerate() {
            g.store(*p, frame, (FRAME_HEADER + j as i64 * PACKET) as i32);
        }
        g.ops(&f.body);
        g.sync_tick_out();
        let zero = g.iconst(0);
        g.b.ins().return_(&[zero]);
        let fc = g.m.target_config();
        g.b.finalize(fc);
        define(&mut m, sh.funcs[i], func)?;
    }

    // main
    let mut sig = m.make_signature();
    sig.params.push(AbiParam::new(I32));
    sig.params.push(AbiParam::new(I64));
    sig.returns.push(AbiParam::new(I32));
    let main_id = m.declare_function("main", Linkage::Export, &sig).map_err(|e| e.to_string())?;
    let mut func = Function::with_name_signature(UserFuncName::user(0, u32::MAX), sig);
    let mut builder = FunctionBuilder::new(&mut func, &mut fbc);
    let entry = builder.create_block();
    builder.append_block_params_for_function_params(entry);
    builder.switch_to_block(entry);
    builder.seal_block(entry);
    let tick = builder.declare_var(I64);
    let mut g = Gen {
        b: builder,
        m: &mut m,
        sh: &sh,
        tick,
        frame: None,
        frame_index: None,
        data: HashMap::new(),
        fns: HashMap::new(),
        strings: &mut strings,
        frozen: false,
        assume_cold: false,
        deferred: false,
        sweep: None,
        promoted: HashMap::new(),
    };
    let args = [
        g.sym(sh.globals.desc),
        g.sym(sh.globals.packets),
        g.sym(sh.globals.meta),
        g.sym(sh.globals.tiles),
        g.sym(sh.globals.keys),
    ];
    g.rt("noe_init", &args);
    if let Some(path) = thermo {
        let id = g.m.declare_anonymous_data(false, false).expect("path data");
        g.strings.push((id, path.as_bytes().to_vec()));
        let p = g.sym(id);
        let n = g.iconst(path.len() as i64);
        g.rt("noe_thermo_start", &[p, n]);
    }
    let zero = g.iconst(0);
    g.b.def_var(g.tick, zero);
    g.ops(&program.main);
    let tick = g.tick_value();
    g.rt("noe_finish", &[tick]);
    let code = g.b.ins().iconst(I32, 0);
    g.b.ins().return_(&[code]);
    let fc = g.m.target_config();
    g.b.finalize(fc);
    define(&mut m, main_id, func)?;

    for (id, bytes) in strings {
        let mut d = DataDescription::new();
        let mut b = bytes;
        b.push(0);
        d.define(b.into_boxed_slice());
        m.define_data(id, &d).map_err(|e| e.to_string())?;
    }
    m.finish().emit().map_err(|e| e.to_string())
}

fn define(m: &mut ObjectModule, id: FuncId, func: Function) -> Result<(), String> {
    let mut ctx = cranelift_codegen::Context::for_function(func);
    m.define_function(id, &mut ctx).map_err(|e| format!("{e:?}"))
}
