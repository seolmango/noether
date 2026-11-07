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

/// A recording of global variables over time, for the playground. Frames are
/// thinned evenly once `cap` is reached, so a long run keeps its whole span.
pub struct Film {
    /// Global packets on film, by index.
    pub shown: Vec<usize>,
    pub ticks: Vec<u64>,
    /// One value and one temperature per shown packet, frame after frame.
    pub values: Vec<i64>,
    pub temps: Vec<u64>,
    tiles: Vec<*mut Tile>,
    cap: usize,
    every: u64,
    next: u64,
}

static mut FILM: Option<Film> = None;

/// Takes the recording of the last embedded run. Also usable after a trap.
#[allow(static_mut_refs)]
pub fn take_film() -> Option<Film> {
    unsafe { FILM.take() }
}

impl Film {
    fn new(module: &Module, limit: usize, cap: usize) -> Film {
        // Named scalars first: they are what a reader is looking for.
        let packets = &module.global.packets;
        let mut shown: Vec<usize> = (0..packets.len()).collect();
        shown.sort_by_key(|i| packets[*i].name.contains('['));
        shown.truncate(limit);
        let tiles = shown.iter().map(|i| rt::noe_tile_at(packets[*i].c)).collect();
        Film {
            shown,
            ticks: Vec::new(),
            values: Vec::new(),
            temps: Vec::new(),
            tiles,
            cap,
            every: 1,
            next: 0,
        }
    }

    fn thin(&mut self) {
        let n = self.shown.len();
        let keep = |list: &mut Vec<_>, width: usize| {
            let mut i = 0;
            list.retain(|_| {
                i += 1;
                ((i - 1) / width) % 2 == 0
            });
        };
        keep(&mut self.ticks as &mut Vec<u64>, 1);
        let mut i = 0;
        self.values.retain(|_| {
            i += 1;
            ((i - 1) / n.max(1)) % 2 == 0
        });
        let mut i = 0;
        self.temps.retain(|_| {
            i += 1;
            ((i - 1) / n.max(1)) % 2 == 0
        });
        self.every *= 2;
    }

    fn shoot(&mut self, packets: &[Packet], tick: u64) {
        if self.ticks.len() == self.cap {
            self.thin();
        }
        self.ticks.push(tick);
        for (i, index) in self.shown.iter().enumerate() {
            self.values.push(packets[*index].v);
            self.temps.push(rt::temperature(self.tiles[i], tick));
        }
        self.next = tick + self.every;
    }
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
                let n = meta.len() as i64;
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
            Expr::Thermo { field, at, friction } => {
                let x = self.expr(at);
                self.advance();
                let tile = self.tile(*field, x);
                rt::noe_thermometer(tile, *friction, self.tick)
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
        words[0] = self.module.cfg.call_temp();
        words[1] = self.tick >> self.module.cfg.c;
        words[2] = self.serial;
        let mut frame = Frame { words, index: id };
        for (i, v) in args.iter().enumerate() {
            unsafe { (*frame.packet(i)).v = *v };
        }
        let saved = self.frame.replace(frame);
        let flow = self.ops(&func.body);
        let tile = self.frame.as_mut().unwrap().tile();
        rt::noe_vent(tile, self.tick);
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

    #[allow(static_mut_refs)]
    fn roll(&mut self) {
        if let Some(film) = unsafe { FILM.as_mut() } {
            if self.tick >= film.next {
                film.shoot(&self.packets, self.tick);
            }
        }
    }

    fn op(&mut self, op: &Op) -> Flow {
        self.roll();
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
            Op::Pump { from, to, friction } => {
                let (a, b) = (self.expr(&from.1), self.expr(&to.1));
                self.advance();
                let (a, b) = (self.tile(from.0, a), self.tile(to.0, b));
                rt::noe_pump(a, b, *friction, self.tick);
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
                        for &(field, first, count) in &s.audit {
                            match field {
                                FieldId::Global => rt::noe_audit_global(first as i64, count as i64),
                                FieldId::Frame => unsafe {
                                    let words = self.frame.as_mut().unwrap().words.as_mut_ptr();
                                    rt::noe_audit_frame(words, first as i64, count as i64);
                                },
                            }
                        }
                        if let Flow::Return(v) = self.ops(&s.body) {
                            return Flow::Return(v);
                        }
                        rt::noe_audit_settle(self.tick);
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
        }
        Flow::Next
    }
}

const FILM_PACKETS: usize = 48;
const FILM_FRAMES: usize = 600;

/// Lowers without optimizations: this executor always runs the full rule.
fn prepare(module: &Module) -> Program {
    lower(module, Options { isolate: false, symmetry: false })
}

fn execute(module: &Module, program: &Program, budget: u64, tables: &mut Field) -> u64 {
    let frame_meta = program.funcs.iter().map(|f| f.centres.clone()).collect();
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
    };
    vm.ops(&program.main);
    if let Some(film) = unsafe { (*std::ptr::addr_of_mut!(FILM)).as_mut() } {
        film.shoot(&vm.packets, vm.tick);
    }
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
    entropy: u64,
) -> u64 {
    let program = Box::leak(Box::new(prepare(module)));
    let field = Box::leak(Box::new(boot(module)));
    rt::embed(input, interactive, entropy);
    rt::start_thermo(None, samples);
    unsafe { FILM = Some(Film::new(module, FILM_PACKETS, FILM_FRAMES)) };
    let tick = execute(module, program, budget, field);
    rt::noe_finish(tick);
    tick
}
