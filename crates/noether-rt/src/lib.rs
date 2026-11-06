//! Noether runtime: the slow path of the field, dynamic packets and I/O.
//!
//! Compiled code keeps static packets and tiles in its own data sections and
//! handles the fast path inline. Anything that needs the full read rule of
//! docs/spec.md chapter 7 comes here. Programs are single-threaded.

#![allow(static_mut_refs, clippy::missing_safety_doc)]

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Read, Write};

const K_ADDR: u64 = 0x9E37_79B9_7F4A_7C15;
const K_CLOCK: u64 = 0xC2B2_AE3D_27D4_EB4F;
const K_CALL: u64 = 0x1656_67B1_9E37_79F9;
const T_MAX: u64 = 1 << 62;

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Packet {
    pub v: i64,
    pub clock: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Tile {
    pub t: u64,
    pub epoch: u64,
}

/// Tile id used for every frame in temperature samples.
pub const FRAME_TILE: i64 = i64::MIN;

/// Temperature recording. Rows are thinned evenly once `cap` is reached, so a
/// long run keeps a sample of its whole history. The peak stays exact.
pub struct Thermo {
    pub rows: Vec<(u64, i64, u64)>,
    cap: usize,
    every: u64,
    seen: u64,
    pub peak: u64,
    pub peak_tile: i64,
    pub last_tick: u64,
    pub erased: u64,
    path: Option<String>,
}

#[derive(Clone, Copy)]
struct Static {
    c: i64,
    mass: i64,
    fermion: bool,
    index: u32,
    tile: *mut Tile,
}

struct Dynamic {
    packet: *mut Packet,
    fermion: bool,
}

struct Rt {
    h: i64,
    l: i64,
    c: u32,
    b: u32,
    tmin: u64,
    seed: u64,
    spacing: i64,
    vacuum_base: i64,
    packets: *mut Packet,
    /// Static packets sorted by centre, ties in declaration order.
    sorted: Vec<Static>,
    /// Tile of each static packet, in declaration order.
    by_index: Vec<*mut Tile>,
    tiles: HashMap<i64, *mut Tile>,
    vacuum: BTreeMap<i64, Vec<Dynamic>>,
    deferred: Vec<(*mut Tile, u64)>,
    out: Box<dyn Write>,
    input: Box<dyn BufRead>,
    /// Embedded in a host: traps unwind to it instead of ending the process.
    embedded: bool,
    noisy: std::cell::Cell<u64>,
    thermo: Option<Thermo>,
    tile_ids: HashMap<*mut Tile, i64>,
}

/// Program output when embedded.
static mut CAPTURED: Vec<u8> = Vec::new();
static mut TRAP: Option<String> = None;
const CAPTURE_LIMIT: usize = 1 << 20;

struct Capture;

impl Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        unsafe {
            let room = CAPTURE_LIMIT.saturating_sub(CAPTURED.len());
            CAPTURED.extend_from_slice(&buf[..buf.len().min(room)]);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A read's running sum. Packets read at their exact centre have weight one
/// and need no fixed point, so only the off-centre ones touch 128-bit maths.
/// floor((whole * 2^q + part) / 2^q) = whole + floor(part / 2^q), so the single
/// rounding of the spec is preserved.
#[derive(Default)]
struct Acc {
    whole: i64,
    part: i128,
}

impl Acc {
    #[inline(always)]
    fn finish(&self, q: i64) -> i64 {
        if self.part == 0 {
            self.whole
        } else {
            self.whole.wrapping_add((self.part >> q) as i64)
        }
    }
}

static mut RT: Option<Rt> = None;

#[inline(always)]
fn rt() -> &'static mut Rt {
    unsafe { RT.as_mut().unwrap_unchecked() }
}

fn die(msg: String) -> ! {
    let r = rt();
    let _ = r.out.flush();
    if r.embedded {
        unsafe { TRAP = Some(msg) };
        panic!("noether trap");
    }
    eprintln!("noether: trap: {msg}");
    let last = r.thermo.as_ref().map_or(0, |t| t.last_tick);
    report(r, last);
    std::process::exit(3);
}

/// Writes the temperature file and a one-line summary, if recording.
fn report(r: &mut Rt, tick: u64) {
    let noisy = r.noisy.get();
    let critical = critical_temperature(r);
    let Some(t) = &r.thermo else { return };
    let Some(path) = &t.path else { return };
    let mut csv = String::from("tick,tile,temperature\n");
    for (tick, tile, temp) in &t.rows {
        match *tile {
            FRAME_TILE => csv.push_str(&format!("{tick},frame,{temp}\n")),
            id => csv.push_str(&format!("{tick},{id},{temp}\n")),
        }
    }
    if let Err(e) = std::fs::write(path, csv) {
        eprintln!("noether: thermo: {path}: {e}");
    }
    eprintln!(
        "thermo: {tick} ticks, peak T {} (critical {critical}), {noisy} noisy reads, \
         {} bits erased -> {path}",
        t.peak, t.erased
    );
}

fn critical_temperature(r: &Rt) -> u64 {
    1u64 << (2 * (r.l - r.h)).clamp(0, 62)
}

#[inline(always)]
fn mix(mut z: u64) -> u64 {
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

impl Rt {
    #[inline(always)]
    fn temp(&self, tile: *mut Tile, tick: u64) -> u64 {
        let t = unsafe { &mut *tile };
        let now = tick >> self.c;
        if now > t.epoch {
            t.t = (t.t >> (now - t.epoch).min(63)).max(self.tmin);
            t.epoch = now;
        }
        t.t
    }

    /// One packet's share of a read: spec chapter 7, step 3.
    #[allow(clippy::too_many_arguments)]
    #[inline(always)]
    fn absorb_packet(
        &self,
        acc: &mut Acc,
        p: *mut Packet,
        c: i64,
        mass: i64,
        tile: *mut Tile,
        call_id: u64,
        x: i64,
        k: i64,
        q: i64,
        tick: u64,
    ) {
        let t = 63 - self.temp(tile, tick).leading_zeros() as i64;
        let s = (self.l - (t + mass) / 2).max(0);
        let r = k.max(s);
        let d = x.abs_diff(c);
        if d >= 1u64 << r {
            return;
        }
        let p = unsafe { &mut *p };
        let e = self.h - k.min(s);
        if e >= 0 {
            let dv = 1u128 << e;
            let h = mix(
                self.seed
                    ^ (c as u64).wrapping_mul(K_ADDR)
                    ^ p.clock.wrapping_mul(K_CLOCK)
                    ^ call_id.wrapping_mul(K_CALL),
            );
            let n = ((((h >> 32) as u128) * (2 * dv + 1)) >> 32) as i64 - dv as i64;
            p.v = p.v.wrapping_add(n);
            p.clock += 1;
            self.noisy.set(self.noisy.get() + 1);
        }
        if d == 0 {
            acc.whole = acc.whole.wrapping_add(p.v);
        } else {
            acc.part += ((p.v as i128) * ((1i128 << r) - d as i128)) << (q - r);
        }
    }

    fn tile_at(&mut self, x: i64) -> *mut Tile {
        let key = x >> self.b;
        let tmin = self.tmin;
        if let Some(t) = self.tiles.get(&key) {
            return *t;
        }
        let tile = Box::into_raw(Box::new(Tile { t: tmin, epoch: 0 }));
        self.tiles.insert(key, tile);
        self.tile_ids.insert(tile, key << self.b);
        tile
    }

    fn sample(&mut self, tile: *mut Tile, tick: u64, bits: u64) {
        if self.thermo.is_none() {
            return;
        }
        let id = self.tile_ids.get(&tile).copied().unwrap_or(FRAME_TILE);
        let Some(t) = &mut self.thermo else { return };
        let temp = unsafe { (*tile).t };
        t.erased += bits;
        t.last_tick = t.last_tick.max(tick);
        if temp > t.peak {
            t.peak = temp;
            t.peak_tile = id;
        }
        t.seen += 1;
        if t.seen % t.every == 0 {
            if t.rows.len() == t.cap {
                let mut keep = false;
                t.rows.retain(|_| {
                    keep = !keep;
                    keep
                });
                t.every *= 2;
            }
            t.rows.push((tick, id, temp));
        }
    }

    fn first_static(&self, c: i64) -> usize {
        self.sorted.partition_point(|p| p.c < c)
    }
}

/// `desc` is [h, l, c, b, tmin, seed, spacing, vacuum_base, n_packets, n_tiles].
/// `meta` holds four i64 per packet: centre, mass, fermion, tile slot.
#[no_mangle]
pub unsafe extern "C" fn noe_init(
    desc: *const i64,
    packets: *mut Packet,
    meta: *const i64,
    tiles: *mut Tile,
    tile_keys: *const i64,
) {
    let d = std::slice::from_raw_parts(desc, 10);
    let (n_packets, n_tiles) = (d[8] as usize, d[9] as usize);
    let tmin = d[4] as u64;
    let mut table = HashMap::with_capacity(n_tiles);
    let mut tile_ids = HashMap::with_capacity(n_tiles);
    for i in 0..n_tiles {
        let t = tiles.add(i);
        (*t).t = tmin;
        table.insert(*tile_keys.add(i), t);
        tile_ids.insert(t, *tile_keys.add(i) << d[3]);
    }
    let m = std::slice::from_raw_parts(meta, n_packets * 4);
    let mut sorted: Vec<Static> = (0..n_packets)
        .map(|i| Static {
            c: m[i * 4],
            mass: m[i * 4 + 1],
            fermion: m[i * 4 + 2] != 0,
            index: i as u32,
            tile: tiles.add(m[i * 4 + 3] as usize),
        })
        .collect();
    let by_index = sorted.iter().map(|p| p.tile).collect();
    sorted.sort_by_key(|p| (p.c, p.index));
    RT = Some(Rt {
        h: d[0],
        l: d[1],
        c: d[2] as u32,
        b: d[3] as u32,
        tmin,
        seed: d[5] as u64,
        spacing: d[6],
        vacuum_base: d[7],
        packets,
        sorted,
        by_index,
        tiles: table,
        vacuum: BTreeMap::new(),
        deferred: Vec::new(),
        out: Box::new(std::io::BufWriter::with_capacity(1 << 16, std::io::stdout())),
        input: Box::new(std::io::BufReader::new(std::io::stdin())),
        embedded: false,
        noisy: std::cell::Cell::new(0),
        thermo: None,
        tile_ids,
    });
}

#[no_mangle]
pub extern "C" fn noe_finish(tick: u64) {
    let r = rt();
    let _ = r.out.flush();
    report(r, tick);
}

/// Starts temperature recording into a file named by `path`.
#[no_mangle]
pub unsafe extern "C" fn noe_thermo_start(path: *const u8, n: i64) {
    let bytes = std::slice::from_raw_parts(path, n as usize);
    start_thermo(Some(String::from_utf8_lossy(bytes).into_owned()), 1 << 20);
}

pub fn start_thermo(path: Option<String>, cap: usize) {
    rt().thermo = Some(Thermo {
        rows: Vec::new(),
        cap,
        every: 1,
        seen: 0,
        peak: 0,
        peak_tile: 0,
        last_tick: 0,
        erased: 0,
        path,
    });
}

/// Records a tile right after heat went in. `bits` is the information erased.
#[no_mangle]
pub extern "C" fn noe_thermo(tile: *mut Tile, tick: u64, bits: u64) {
    rt().sample(tile, tick, bits);
}

/// Counts erased bits whose heat is still deferred.
#[no_mangle]
pub extern "C" fn noe_erased(bits: u64) {
    if let Some(t) = &mut rt().thermo {
        t.erased += bits;
    }
}

/// Adds heat to a tile, for hosts that do not inline this.
pub fn heat(tile: *mut Tile, q: u64, tick: u64) {
    let r = rt();
    let t = r.temp(tile, tick);
    unsafe { (*tile).t = (t + q).min(T_MAX) };
}

/// Switches I/O to memory and makes traps panic. Call right after `noe_init`.
pub fn embed(input: Vec<u8>) {
    let r = rt();
    r.out = Box::new(Capture);
    r.input = Box::new(std::io::Cursor::new(input));
    r.embedded = true;
    unsafe {
        CAPTURED.clear();
        TRAP = None;
    }
}

pub struct Summary {
    pub output: Vec<u8>,
    pub trap: Option<String>,
    pub noisy: u64,
    pub critical: u64,
    pub thermo: Option<Thermo>,
}

/// What an embedded run left behind. Also usable after a trap.
pub fn summary() -> Summary {
    let r = rt();
    Summary {
        output: unsafe { std::mem::take(&mut CAPTURED) },
        trap: unsafe { TRAP.take() },
        noisy: r.noisy.get(),
        critical: critical_temperature(r),
        thermo: r.thermo.take(),
    }
}

#[no_mangle]
pub extern "C" fn noe_tile_at(x: i64) -> *mut Tile {
    rt().tile_at(x)
}

/// The full read rule on the global field. Friction is already applied.
#[no_mangle]
pub extern "C" fn noe_read_global(x: i64, k: i64, tick: u64) -> i64 {
    let r = rt();
    let q = k.max(r.l);
    let reach = (1i64 << q) - 1;
    let lo = x.saturating_sub(reach);
    let hi = x.saturating_add(reach);
    let mut acc = Acc::default();
    let start = r.first_static(lo);
    for i in start..r.sorted.len() {
        let p = r.sorted[i];
        if p.c > hi {
            break;
        }
        let ptr = unsafe { r.packets.add(p.index as usize) };
        r.absorb_packet(&mut acc, ptr, p.c, p.mass, p.tile, 0, x, k, q, tick);
    }
    if !r.vacuum.is_empty() {
        let hits: Vec<(i64, *mut Packet)> = r
            .vacuum
            .range(lo..=hi)
            .flat_map(|(c, list)| list.iter().map(move |d| (*c, d.packet)))
            .collect();
        for (c, ptr) in hits {
            let tile = r.tile_at(c);
            r.absorb_packet(&mut acc, ptr, c, 0, tile, 0, x, k, q, tick);
        }
    }
    acc.finish(q)
}

/// The full read rule on a frame. `frame` starts with T, epoch, call_id, pad,
/// then the packets. `meta` holds centre and mass per packet.
#[no_mangle]
pub unsafe extern "C" fn noe_read_frame(
    frame: *mut u64,
    meta: *const i64,
    n: i64,
    x: i64,
    k: i64,
    tick: u64,
) -> i64 {
    let r = rt();
    let q = k.max(r.l);
    let tile = frame as *mut Tile;
    let call_id = *frame.add(2);
    let packets = frame.add(4) as *mut Packet;
    let mut acc = Acc::default();
    for i in 0..n as usize {
        let (c, mass) = (*meta.add(i * 2), *meta.add(i * 2 + 1));
        r.absorb_packet(&mut acc, packets.add(i), c, mass, tile, call_id, x, k, q, tick);
    }
    acc.finish(q)
}

/// Packet behind `|(x)>`: the earliest one whose centre is exactly x.
#[no_mangle]
pub extern "C" fn noe_resolve(x: i64) -> *mut Packet {
    let r = rt();
    let i = r.first_static(x);
    if let Some(p) = r.sorted.get(i) {
        if p.c == x {
            return unsafe { r.packets.add(p.index as usize) };
        }
    }
    match r.vacuum.get(&x).and_then(|l| l.first()) {
        Some(d) => d.packet,
        None => die(format!("write into vacuum at {x}")),
    }
}

#[no_mangle]
pub extern "C" fn noe_create(x: i64, fermion: i64, friction: u64, tick: u64) {
    let r = rt();
    if x < r.vacuum_base {
        die(format!("create at {x} is below the vacuum base {}", r.vacuum_base));
    }
    if fermion != 0 {
        let lo = x.saturating_sub(r.spacing - 1);
        let hi = x.saturating_add(r.spacing - 1);
        let clash = r.vacuum.range(lo..=hi).any(|(_, l)| l.iter().any(|d| d.fermion))
            || r.sorted[r.first_static(lo)..].iter().take_while(|p| p.c <= hi).any(|p| p.fermion);
        if clash {
            die(format!("Pauli violation at {x}"));
        }
    }
    let tile = r.tile_at(x);
    let t = r.temp(tile, tick);
    unsafe { (*tile).t = (t + friction).min(T_MAX) };
    r.sample(tile, tick, 0);
    // Never freed: a statement may still hold this packet while it evaluates.
    let packet = Box::into_raw(Box::new(Packet { v: 0, clock: 0 }));
    r.vacuum.entry(x).or_default().push(Dynamic { packet, fermion: fermion != 0 });
}

#[no_mangle]
pub extern "C" fn noe_annihilate(x: i64, friction: u64, tick: u64) {
    let r = rt();
    let found = if x < r.vacuum_base { None } else { r.vacuum.get_mut(&x) };
    let Some(list) = found.filter(|l| !l.is_empty()) else {
        die(format!("nothing to annihilate at {x}"));
    };
    let d = list.remove(0);
    let empty = list.is_empty();
    if empty {
        r.vacuum.remove(&x);
    }
    let bits = unsafe { (*d.packet).v.count_ones() } as u64;
    let tile = r.tile_at(x);
    let t = r.temp(tile, tick);
    unsafe { (*tile).t = (t + friction + bits).min(T_MAX) };
    r.sample(tile, tick, bits);
}

/// True when every packet of a static run is below its critical temperature.
#[no_mangle]
pub extern "C" fn noe_guard(first: i64, count: i64, limit: u64, tick: u64) -> i64 {
    let r = rt();
    let run = &r.by_index[first as usize..(first + count) as usize];
    run.iter().all(|tile| r.temp(*tile, tick) < limit) as i64
}

#[no_mangle]
pub extern "C" fn noe_defer(tile: *mut Tile, q: u64) {
    rt().deferred.push((tile, q));
}

#[no_mangle]
pub extern "C" fn noe_flush(tick: u64) {
    let r = rt();
    let pending = std::mem::take(&mut r.deferred);
    for (tile, q) in pending {
        let t = r.temp(tile, tick);
        unsafe { (*tile).t = (t + q).min(T_MAX) };
        r.sample(tile, tick, 0);
    }
}

#[no_mangle]
pub extern "C" fn noe_emit(v: i64) {
    let _ = writeln!(rt().out, "{v}");
}

#[no_mangle]
pub extern "C" fn noe_emitc(v: i64) {
    let _ = rt().out.write_all(&[v as u8]);
}

#[no_mangle]
pub unsafe extern "C" fn noe_emits(p: *const u8, n: i64) {
    let _ = rt().out.write_all(std::slice::from_raw_parts(p, n as usize));
}

#[no_mangle]
pub extern "C" fn noe_absorb() -> i64 {
    let r = rt();
    let _ = r.out.flush();
    let mut line = Vec::new();
    let _ = r.input.read_until(b'\n', &mut line);
    let text = String::from_utf8_lossy(&line);
    match text.trim().parse::<i128>() {
        Ok(v) => v as i64,
        Err(_) => 0,
    }
}

#[no_mangle]
pub extern "C" fn noe_absorbc() -> i64 {
    let r = rt();
    let _ = r.out.flush();
    let mut byte = [0u8];
    match r.input.read(&mut byte) {
        Ok(1) => byte[0] as i64,
        _ => -1,
    }
}

/// Kinds: 0 lattice used as a scalar, 1 index out of range, 2 fuse apart.
#[no_mangle]
pub extern "C" fn noe_trap(kind: i64, a: i64) -> ! {
    die(match kind {
        0 => "a lattice used as a single packet".to_string(),
        1 => format!("index {a} is outside lattice"),
        2 => "fuse needs two packets at the same centre".to_string(),
        3 => "step budget exhausted".to_string(),
        _ => format!("trap {kind}"),
    })
}

#[no_mangle]
pub unsafe extern "C" fn noe_zero(p: *mut u8, n: i64) {
    std::ptr::write_bytes(p, 0, n as usize);
}
