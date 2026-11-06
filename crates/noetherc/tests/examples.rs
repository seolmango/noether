//! Runs the examples through the executor and through native compilation.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const NOETHERC: &str = env!("CARGO_BIN_EXE_noetherc");
const HELLO: &str = "++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.\
                     >>.<-.<.+++.------.--------.>>+.>++.!";

const EXAMPLES: [(&str, &str, &str); 10] = [
    ("sum", "", "55\n"),
    ("mul", "", "42\n-144\n97406784\n"),
    ("fact", "", "3628800\n1307674368000\n"),
    ("sort", "", "1\n2\n3\n4\n5\n7\n8\n9\n"),
    ("symmetry", "", "11\n44\n16\n"),
    ("condensate", "", "1000\n2414\n"),
    ("bf", HELLO, "Hello World!\n"),
    (
        "gugudan",
        "7\n",
        "몇 단을 볼까요? (2~9, 0이면 전부): 7 x 1 = 7\n7 x 2 = 14\n7 x 3 = 21\n7 x 4 = 28\n\
         7 x 5 = 35\n7 x 6 = 42\n7 x 7 = 49\n7 x 8 = 56\n7 x 9 = 63\n",
    ),
    (
        "calc",
        "*\n6\n7\n/\n100\n7\n-\n3\n10\n+\n3\n10\nq\n",
        "연산자 (+ - * /, q는 끝): 왼쪽 수: 오른쪽 수: 결과: 42\n\
         연산자 (+ - * /, q는 끝): 왼쪽 수: 오른쪽 수: 결과: 14\n\
         연산자 (+ - * /, q는 끝): 왼쪽 수: 오른쪽 수: 결과: -7\n\
         연산자 (+ - * /, q는 끝): 왼쪽 수: 오른쪽 수: 결과: 13\n\
         연산자 (+ - * /, q는 끝): 끝\n",
    ),
    (
        "name",
        "Hi\n",
        "이름을 입력하세요: 안녕하세요, Hi님!\n입력은 2바이트입니다: 72 105\n",
    ),
];

const REJECTED: [(&str, &str); 8] = [
    ("fermion i\nwhile <i|~32 > 0\n  raise i\n", "energy level"),
    ("fermion i\nwhile <i|~32 > 0\n    raise i\n", "forbidden transition"),
    ("fermion a @ 0\nfermion b @ 100\n", "Pauli"),
    ("fermion a\nemit <a|~24\n", "power of two"),
    ("fermion a\nsymmetry time\n |a> <- 1\n", "symmetry time"),
    ("fermion a[4]\nfermion s\nsymmetry translate k in a\n raise s\n", "may only change"),
    ("emit <nope|~32\n", "unknown name"),
    ("path f(a)\n return a\nemit f(1)\n", "closed system"),
];

const TRAPS: [(&str, &str); 4] = [
    ("|(999999)> <- 1\n", "vacuum"),
    ("fermion a[2]\n|a[5]> <- 1\n", "outside lattice"),
    ("boson a @ 0\nboson b @ 5000\nfuse a b\n", "same centre"),
    ("fermion a @ 0\ncreate boson @ 10\n", "vacuum base"),
];

/// `cargo test` does not build the static runtime that native linking needs.
fn build_runtime() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let status = Command::new(env!("CARGO"))
            .args(["build", "-p", "noether-rt"])
            .status()
            .unwrap();
        assert!(status.success());
    });
}

fn dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("noether-test-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn feed(cmd: &mut Command, input: &str) -> Output {
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
}

fn example(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples").join(format!("{name}.noe"))
}

fn source(name: &str, text: &str) -> PathBuf {
    let path = dir().join(format!("{name}.noe"));
    std::fs::write(&path, text).unwrap();
    path
}

#[test]
fn examples_run() {
    for (name, input, want) in EXAMPLES {
        let out = feed(Command::new(NOETHERC).arg(example(name)).arg("--run"), input);
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name}");
    }
}

#[test]
fn guessing_every_number_finds_the_secret() {
    let mut input = String::from("3\n");
    (1..=100).for_each(|n| input.push_str(&format!("{n}\n")));
    let out = feed(Command::new(NOETHERC).arg(example("guess")).arg("--run"), &input);
    assert!(String::from_utf8_lossy(&out.stdout).ends_with("번 만에 맞혔습니다.\n"));
}

#[test]
fn the_cat_survives_a_cold_box_and_shaking_decides_a_hot_one() {
    let fate = |shakes: &str| {
        let input = format!("{shakes}\n");
        let out = feed(Command::new(NOETHERC).arg(example("cat")).arg("--run"), &input);
        String::from_utf8_lossy(&out.stdout).contains("살아 있습니다")
    };
    assert!(fate("5"));
    assert!(!fate("50"));
    assert!(fate("100"));
}

#[test]
fn native_matches_executor_in_every_mode() {
    build_runtime();
    for (name, input, want) in EXAMPLES {
        for flags in [&[][..], &["--no-symmetry"], &["--no-isolate"]] {
            let exe = dir().join(format!("{name}{}.exe", flags.len()));
            let built = Command::new(NOETHERC)
                .arg(example(name))
                .arg("-o")
                .arg(&exe)
                .args(flags)
                .output()
                .unwrap();
            assert!(built.status.success(), "{}", String::from_utf8_lossy(&built.stderr));
            let out = feed(&mut Command::new(&exe), input);
            assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name} {flags:?}");
        }
    }
}

#[test]
fn noise_is_seeded_bounded_and_persistent() {
    let run = |seed: u32| {
        let text = format!("field seed={seed}\nfermion v\n|v> <- 1000\nemit <v|\nemit <v|\nemit <v|~32\n");
        let out = feed(Command::new(NOETHERC).arg(source("noise", &text)).arg("--run"), "");
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(run(1), run(1));
    assert_ne!(run(1), run(2));
    let v: Vec<i64> = run(1).lines().map(|l| l.parse().unwrap()).collect();
    assert!((v[0] - 1000).abs() <= 16 && (v[1] - v[0]).abs() <= 16);
    assert_eq!(v[1], v[2]);
}

#[test]
fn thermo_records_the_same_history_both_ways() {
    build_runtime();
    let (a, b) = (dir().join("va.exe"), dir().join("na.exe"));
    let run = Command::new(NOETHERC)
        .args([example("overheat").as_os_str(), "--run".as_ref(), "--thermo".as_ref(), "-o".as_ref()])
        .arg(&a)
        .output()
        .unwrap();
    assert!(String::from_utf8_lossy(&run.stderr).contains("12 noisy reads"));
    let built = Command::new(NOETHERC)
        .args([example("overheat").as_os_str(), "--thermo".as_ref(), "-o".as_ref()])
        .arg(&b)
        .output()
        .unwrap();
    assert!(built.status.success());
    Command::new(&b).output().unwrap();
    let csv = |p: &PathBuf| std::fs::read_to_string(p.with_extension("thermo.csv")).unwrap();
    assert_eq!(csv(&a), csv(&b));
}

#[test]
fn bad_programs_are_rejected() {
    for (i, (text, needle)) in REJECTED.iter().enumerate() {
        let out = feed(Command::new(NOETHERC).arg(source(&format!("bad{i}"), text)).arg("--run"), "");
        assert_eq!(out.status.code(), Some(2), "{text}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(needle), "{text}");
    }
}

#[test]
fn traps_end_with_code_three() {
    for (i, (text, needle)) in TRAPS.iter().enumerate() {
        let out = feed(Command::new(NOETHERC).arg(source(&format!("trap{i}"), text)).arg("--run"), "");
        assert_eq!(out.status.code(), Some(3), "{text}");
        assert!(String::from_utf8_lossy(&out.stderr).contains(needle), "{text}");
    }
}
