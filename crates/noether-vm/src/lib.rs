//! A tree-walking executor for NIR. It shares the field rules with compiled
//! programs through `noether-rt`, needs no linker, and runs in the browser.

use noether_nir::*;
use noether_rt as rt;
use noether_rt::{Packet, Tile};
use noether_sema::Module;
use noether_syntax::Cmp;

enum Flow {
    Next,
    Return(i64),
}

struct Resolved {
    ptr: *mut Packet,
    centre: i64,
    frame: bool,
    tile: Option<*mut Tile>,
}

struct Frame {
    words: Vec<u64>,
    index: usize,
}

impl Frame {
    fn packet(&mut self, i: usize) -> *mut Packet {
        unsafe { (self.words.as_mut_ptr().add(4) as *mut Packet).add(i) }
    }

    fn tile(&mut self) -> *mut Tile {
        self.words.as_mut_ptr() as *mut Tile
    }
}

struct Vm<'a> {
    module: &'a Module,
    program: &'a Program,
    packets: Vec<Packet>,
    frame_meta: Vec<Vec<i64>>,
    frame: Option<Frame>,
    tick: u64,
    serial: u64,
    frozen: bool,
    deferred: bool,
    sweep: i64,
    steps: u64,
    budget: u64,
    tmin: u64,
}

impl Vm<'_> {
    fn advance(&mut self) {
        if !self.frozen {
            self.tick += 1;
        }
    }

    fn tile(&mut self, field: FieldId, x: i64) -> *mut Tile {
        match field {
            FieldId::Frame => self.frame.as_mut().unwrap().tile(),
            FieldId::Global => rt::noe_tile_at(x),
        }
    }

    fn heat(&mut self, tile: *mut Tile, q: u64, bits: u64) {
        if self.deferred {
            rt::noe_defer(tile, q);
            rt::noe_erased(bits);
        } else {
            rt::heat(tile, q, self.tick);
            rt::noe_thermo(tile, self.tick, bits);
        }
    }

    fn read(&mut self, r: &Read) -> i64 {
        let x = match &r.loc {
            Loc::Const(x) => *x,
            Loc::Lattice { base, stride, index, off, .. } => {
                let i = self.expr(index);
                i.wrapping_mul(*stride).wrapping_add(base.wrapping_add(*off))
            }
            Loc::Dynamic(e) => self.expr(e),
        };
        self.advance();
        if r.friction > 0 {
            let tile = self.tile(r.field, x);
            self.heat(tile, r.friction, 0);
        }
        match r.field {
            FieldId::Global => rt::noe_read_global(x, r.k as i64, self.tick),
            FieldId::Frame => {
                let f = self.frame.as_mut().unwrap();
                let (ptr, index) = (f.words.as_mut_ptr(), f.index);
                let meta = &self.frame_meta[index];
                let n = (meta.len() / 2) as i64;
                unsafe { rt::noe_read_frame(ptr, meta.as_ptr(), n, x, r.k as i64, self.tick) }
            }
        }
    }

    fn expr(&mut self, e: &Expr) -> i64 {
        match e {
            Expr::Const(v) => *v,
            Expr::SweepIndex => self.sweep,
            Expr::Read(r) => self.read(r),
            Expr::Square(r) => {
                let v = self.read(r);
                v.wrapping_mul(v)
            }
            Expr::Scaled { base, stride, index, off } => {
                let i = self.expr(index);
                i.wrapping_mul(*stride).wrapping_add(base.wrapping_add(*off))
            }
            Expr::Shifted(v, off) => self.expr(v).wrapping_add(*off),
            Expr::Call(id, args) => {
                let values: Vec<i64> = args.iter().map(|a| self.expr(a)).collect();
                self.call(*id, &values)
            }
            Expr::Absorb => rt::noe_absorb(),
            Expr::Absorbc => rt::noe_absorbc(),
        }
    }

    fn call(&mut self, id: usize, args: &[i64]) -> i64 {
        let program = self.program;
        let func = &program.funcs[id];
        self.serial += 1;
        let mut words = vec![0u64; 4 + func.centres.len() * 2];
        words[0] = self.tmin;
        words[2] = self.serial;
        let mut frame = Frame { words, index: id };
        for (i, v) in args.iter().enumerate() {
            unsafe { (*frame.packet(i)).v = *v };
        }
        let saved = self.frame.replace(frame);
        let flow = self.ops(&func.body);
        self.frame = saved;
        match flow {
            Flow::Return(v) => v,
            Flow::Next => 0,
        }
    }

    fn cond(&mut self, c: &Cond) -> bool {
        let (l, r) = (self.expr(&c.left), self.expr(&c.right));
        match c.op {
            Cmp::Lt => l < r,
            Cmp::Gt => l > r,
            Cmp::Le => l <= r,
            Cmp::Ge => l >= r,
            Cmp::Eq => l == r,
            Cmp::Ne => l != r,
        }
    }

    fn packet(&mut self, field: FieldId, index: usize) -> *mut Packet {
        match field {
            FieldId::Frame => self.frame.as_mut().unwrap().packet(index),
            FieldId::Global => unsafe { self.packets.as_mut_ptr().add(index) },
        }
    }

    fn place(&mut self, p: &Place) -> Resolved {
        match p {
            Place::Packet { field, index } => {
                let centre = match field {
                    FieldId::Global => self.module.global.packets[*index].c,
                    FieldId::Frame => {
                        let f = self.frame.as_ref().unwrap().index;
                        self.program.funcs[f].centres[*index]
                    }
                };
                let ptr = self.packet(*field, *index);
                let tile = self.tile(*field, centre);
                Resolved { ptr, centre, frame: *field == FieldId::Frame, tile: Some(tile) }
            }
            Place::Elem { field, first, count, base, stride, index } => {
                let i = self.expr(index);
                if i < 0 || i as usize >= *count {
                    rt::noe_trap(1, i);
                }
                let ptr = self.packet(*field, first + i as usize);
                let centre = i.wrapping_mul(*stride).wrapping_add(*base);
                let tile = self.tile(*field, centre);
                Resolved { ptr, centre, frame: *field == FieldId::Frame, tile: Some(tile) }
            }
            Place::Ptr(e) => {
                let x = self.expr(e);
                Resolved { ptr: rt::noe_resolve(x), centre: x, frame: false, tile: None }
            }
            Place::NotAPacket => rt::noe_trap(0, 0),
        }
    }

    fn ops(&mut self, ops: &[Op]) -> Flow {
        for op in ops {
            if let Flow::Return(v) = self.op(op) {
                return Flow::Return(v);
            }
        }
        Flow::Next
    }

    fn op(&mut self, op: &Op) -> Flow {
        self.steps += 1;
        if self.steps > self.budget {
            rt::noe_trap(3, 0);
        }
        match op {
            Op::Write { place, value, friction } => {
                let t = self.place(place);
                let v = self.expr(value);
                self.advance();
                let old = unsafe { (*t.ptr).v };
                let bits = (old ^ v).count_ones() as u64;
                let tile = t.tile.unwrap_or_else(|| rt::noe_tile_at(t.centre));
                self.heat(tile, friction + bits, bits);
                unsafe { (*t.ptr).v = v };
            }
            Op::Ladder(p, delta) => {
                let t = self.place(p);
                self.advance();
                unsafe { (*t.ptr).v = (*t.ptr).v.wrapping_add(*delta) };
            }
            Op::Flip(p) => {
                let t = self.place(p);
                self.advance();
                unsafe { (*t.ptr).v = (*t.ptr).v.wrapping_neg() };
            }
            Op::Swap(a, b) => {
                let (a, b) = (self.place(a), self.place(b));
                self.advance();
                unsafe { std::ptr::swap(a.ptr, b.ptr) };
                // Only the values trade places; each packet keeps its clock.
                unsafe { std::mem::swap(&mut (*a.ptr).clock, &mut (*b.ptr).clock) };
            }
            Op::Fuse(a, b) => {
                let (a, b) = (self.place(a), self.place(b));
                if a.frame != b.frame || a.centre != b.centre {
                    rt::noe_trap(2, 0);
                }
                self.advance();
                unsafe { (*a.ptr).v = (*a.ptr).v.wrapping_add((*b.ptr).v) };
            }
            Op::If(c, then, other) => {
                let branch = if self.cond(c) { then } else { other };
                return self.ops(branch);
            }
            Op::While(c, body) => {
                while self.cond(c) {
                    if let Flow::Return(v) = self.ops(body) {
                        return Flow::Return(v);
                    }
                    self.steps += 1;
                    if self.steps > self.budget {
                        rt::noe_trap(3, 0);
                    }
                }
            }
            Op::Symmetric(s) => {
                let saved = (self.frozen, self.deferred, self.sweep);
                self.frozen = true;
                match s.sweep {
                    None => {
                        if let Flow::Return(v) = self.ops(&s.body) {
                            return Flow::Return(v);
                        }
                    }
                    Some(count) => {
                        self.deferred = true;
                        for k in 0..count {
                            self.sweep = k as i64;
                            self.ops(&s.body);
                        }
                        rt::noe_flush(self.tick);
                    }
                }
                (self.frozen, self.deferred, self.sweep) = saved;
            }
            Op::Emit(e) => {
                let v = self.expr(e);
                rt::noe_emit(v);
            }
            Op::EmitC(e) => {
                let v = self.expr(e);
                rt::noe_emitc(v);
            }
            Op::EmitN(e) => {
                let v = self.expr(e);
                rt::noe_emitn(v);
            }
            Op::EmitS(bytes) => unsafe { rt::noe_emits(bytes.as_ptr(), bytes.len() as i64) },
            Op::Return(e) => return Flow::Return(self.expr(e)),
            Op::Eval(e) => {
                self.expr(e);
            }
            Op::Create { fermion, addr, friction } => {
                let x = self.expr(addr);
                self.advance();
                rt::noe_create(x, *fermion as i64, *friction, self.tick);
            }
            Op::Annihilate { addr, friction } => {
                let x = self.expr(addr);
                self.advance();
                rt::noe_annihilate(x, *friction, self.tick);
            }
        }
        Flow::Next
    }
}

/// Lowers without optimizations: this executor always runs the full rule.
fn prepare(module: &Module) -> Program {
    lower(module, Options { isolate: false, symmetry: false })
}

fn execute(module: &Module, program: &Program, budget: u64, tables: &mut Field) -> u64 {
    let frame_meta = program
        .funcs
        .iter()
        .map(|f| {
            f.centres.iter().zip(&f.masses).flat_map(|(c, m)| [*c, *m as i64]).collect()
        })
        .collect();
    let mut vm = Vm {
        module,
        program,
        packets: std::mem::take(&mut tables.packets),
        frame_meta,
        frame: None,
        tick: 0,
        serial: 0,
        frozen: false,
        deferred: false,
        sweep: 0,
        steps: 0,
        budget,
        tmin: module.cfg.tmin,
    };
    vm.ops(&program.main);
    vm.tick
}

/// Static storage that the runtime points into for the whole run.
struct Field {
    packets: Vec<Packet>,
    tiles: Vec<Tile>,
}

fn boot(module: &Module) -> Field {
    let t = module.tables();
    let mut field = Field {
        packets: vec![Packet::default(); module.global.packets.len()],
        tiles: vec![Tile::default(); t.keys.len()],
    };
    unsafe {
        rt::noe_init(
            t.desc.as_ptr(),
            field.packets.as_mut_ptr(),
            t.meta.as_ptr(),
            field.tiles.as_mut_ptr(),
            t.keys.as_ptr(),
        );
    }
    field
}

/// Runs on the process's stdin and stdout, exiting like a compiled program.
pub fn run(module: &Module, budget: u64, thermo: Option<String>) {
    let program = prepare(module);
    let mut field = boot(module);
    let recording = thermo.is_some();
    if recording {
        rt::start_thermo(thermo, 1 << 20);
    }
    let tick = execute(module, &program, budget, &mut field);
    rt::noe_finish(tick);
}

/// Runs with memory I/O and always records temperatures. A trap ends in a
/// panic; the host then collects the results with `noether_rt::summary`.
///
/// With `interactive`, running out of input stops the run instead of reading
/// end of file. Programs are deterministic, so a host can add input and run
/// again from the start to continue where this run stopped.
pub fn run_embedded(
    module: &Module,
    budget: u64,
    input: Vec<u8>,
    samples: usize,
    interactive: bool,
) -> u64 {
    let program = Box::leak(Box::new(prepare(module)));
    let field = Box::leak(Box::new(boot(module)));
    rt::embed(input, interactive);
    rt::start_thermo(None, samples);
    let tick = execute(module, program, budget, field);
    rt::noe_finish(tick);
    tick
}
