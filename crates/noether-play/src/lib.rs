//! WebAssembly exports for the browser playground.
//!
//! A trap aborts the wasm call, but linear memory survives, so the page calls
//! `finish` afterwards either way and reads the result as JSON.

#![allow(static_mut_refs, clippy::missing_safety_doc)]

use noether_rt as rt;
use noether_sema::Module;
use std::collections::BTreeMap;
use std::fmt::Write;

const SAMPLES: usize = 40_000;

const EXAMPLES: [(&str, &str, &str); 6] = [
    ("슈뢰딩거의 고양이", include_str!("../../../examples/cat.noe"), ""),
    ("숫자 맞히기", include_str!("../../../examples/guess.noe"), ""),
    ("온도 구간", include_str!("../../../examples/overheat.noe"), ""),
    ("온도 조절기", include_str!("../../../examples/thermostat.noe"), ""),
    ("식으면 섞이는 변수", include_str!("../../../examples/condensate.noe"), ""),
    (
        "Brainfuck 인터프리터",
        include_str!("../../../examples/bf.noe"),
        "++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.>>.<-.<.+++.------.--------.>>+.>++.!",
    ),
];

static mut RESULT: Vec<u8> = Vec::new();
static mut ERROR: Option<(usize, String)> = None;
static mut MODULE: Option<Module> = None;
static mut TICK: Option<u64> = None;

fn quote(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn set_result(s: String) {
    unsafe { RESULT = s.into_bytes() };
}

unsafe fn text(p: *const u8, n: usize) -> String {
    String::from_utf8_lossy(std::slice::from_raw_parts(p, n)).into_owned()
}

fn analyze(src: &str) -> Result<Module, noether_syntax::Error> {
    noether_sema::analyze(noether_syntax::parse(src)?)
}

#[no_mangle]
pub extern "C" fn alloc(n: usize) -> *mut u8 {
    let mut v = Vec::<u8>::with_capacity(n.max(1));
    let p = v.as_mut_ptr();
    std::mem::forget(v);
    p
}

#[no_mangle]
pub extern "C" fn result_ptr() -> *const u8 {
    unsafe { RESULT.as_ptr() }
}

#[no_mangle]
pub extern "C" fn result_len() -> usize {
    unsafe { RESULT.len() }
}

#[no_mangle]
pub extern "C" fn examples() {
    let mut out = String::from("[");
    for (i, (title, source, input)) in EXAMPLES.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str("{\"title\":");
        quote(&mut out, title);
        out.push_str(",\"source\":");
        quote(&mut out, source);
        out.push_str(",\"input\":");
        quote(&mut out, input);
        out.push('}');
    }
    out.push(']');
    set_result(out);
}

/// Compile-time check for live feedback in the editor.
#[no_mangle]
pub unsafe extern "C" fn check(src: *const u8, n: usize) {
    let mut out = String::new();
    match analyze(&text(src, n)) {
        Ok(_) => out.push_str("{\"ok\":true}"),
        Err(e) => {
            let _ = write!(out, "{{\"ok\":false,\"line\":{},\"message\":", e.line);
            quote(&mut out, &e.msg);
            out.push('}');
        }
    }
    set_result(out);
}

#[no_mangle]
pub unsafe extern "C" fn run(
    src: *const u8,
    n: usize,
    input: *const u8,
    m: usize,
    budget: f64,
    interactive: i32,
    entropy: f64,
) {
    let source = text(src, n);
    let stdin = std::slice::from_raw_parts(input, m).to_vec();
    match analyze(&source) {
        Err(e) => ERROR = Some((e.line, e.msg)),
        Ok(module) => {
            MODULE = Some(module);
            let module = MODULE.as_ref().unwrap();
            let tick = noether_vm::run_embedded(
                module,
                budget as u64,
                stdin,
                SAMPLES,
                interactive != 0,
                entropy as u64,
            );
            TICK = Some(tick);
        }
    }
}

/// Names of the variables in each tile, for chart labels.
fn tile_labels(module: &Module) -> BTreeMap<i64, Vec<String>> {
    let b = module.cfg.b;
    let mut labels: BTreeMap<i64, Vec<String>> = BTreeMap::new();
    for p in &module.global.packets {
        labels.entry((p.c >> b) << b).or_default().push(p.name.clone());
    }
    labels
}

#[no_mangle]
pub unsafe extern "C" fn finish() {
    let mut out = String::new();
    if let Some((line, msg)) = ERROR.take() {
        let _ = write!(out, "{{\"status\":\"compile\",\"line\":{line},\"message\":");
        quote(&mut out, &msg);
        out.push('}');
        set_result(out);
        return;
    }
    let s = rt::summary();
    let thermo = s.thermo.as_ref();
    let module = MODULE.as_ref().unwrap();
    let (status, message) = match &s.trap {
        Some(m) if m.contains("step budget") => ("budget", m.clone()),
        Some(m) if m == rt::WAITING => ("input", String::new()),
        Some(m) => ("trap", m.clone()),
        None => ("ok", String::new()),
    };
    let tick = TICK.or(thermo.map(|t| t.last_tick)).unwrap_or(0);
    let _ = write!(out, "{{\"status\":\"{status}\",\"message\":");
    quote(&mut out, &message);
    out.push_str(",\"output\":");
    quote(&mut out, &String::from_utf8_lossy(&s.output));
    let cfg = &module.cfg;
    let _ = write!(
        out,
        ",\"ticks\":{tick},\"critical\":{},\"noisy\":{},\"peak\":{},\"erased\":{},\
         \"tau\":{},\"tmin\":{},\"tile\":{}",
        s.critical,
        s.noisy,
        thermo.map_or(0, |t| t.peak),
        thermo.map_or(0, |t| t.erased),
        1u64 << cfg.c,
        cfg.tmin,
        1u64 << cfg.b,
    );
    // Tiles are numbered in order of first appearance in the samples.
    let labels = tile_labels(module);
    let mut ids: Vec<i64> = Vec::new();
    let mut index: BTreeMap<i64, usize> = BTreeMap::new();
    out.push_str(",\"samples\":[");
    if let Some(t) = thermo {
        for (i, (tick, tile, temp)) in t.rows.iter().enumerate() {
            let n = *index.entry(*tile).or_insert_with(|| {
                ids.push(*tile);
                ids.len() - 1
            });
            if i > 0 {
                out.push(',');
            }
            let _ = write!(out, "{tick},{n},{temp}");
        }
    }
    out.push_str("],\"tiles\":[");
    for (i, id) in ids.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        let label = if *id == rt::FRAME_TILE {
            "path 호출".to_string()
        } else {
            match labels.get(id) {
                Some(names) if names.len() > 3 => {
                    format!("{}, {} 외 {}개", names[0], names[1], names.len() - 2)
                }
                Some(names) => names.join(", "),
                None => format!("주소 {id}"),
            }
        };
        quote(&mut out, &label);
    }
    out.push(']');
    write_film(&mut out, module);
    out.push('}');
    set_result(out);
}

fn write_film(out: &mut String, module: &Module) {
    let Some(film) = noether_vm::take_film() else { return };
    let cfg = &module.cfg;
    let _ = write!(out, ",\"lambda\":{},\"hbar\":{}", 1u64 << cfg.l, 1u64 << cfg.h);
    out.push_str(",\"film\":{\"names\":[");
    for (i, p) in film.shown.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        quote(out, &module.global.packets[*p].name);
    }
    let join = |out: &mut String, key: &str, items: Vec<String>| {
        let _ = write!(out, "],\"{key}\":[{}", items.join(","));
    };
    let packets = &module.global.packets;
    join(out, "centres", film.shown.iter().map(|p| packets[*p].c.to_string()).collect());
    join(out, "ticks", film.ticks.iter().map(u64::to_string).collect());
    join(out, "values", film.values.iter().map(i64::to_string).collect());
    join(out, "temps", film.temps.iter().map(u64::to_string).collect());
    out.push_str("]}");
}
