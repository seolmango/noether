//! Runs the examples through the executor and through native compilation.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

const NOETHERC: &str = env!("CARGO_BIN_EXE_noetherc");
const HELLO: &str = "++++++++[>++++[>++>+++>+++>+<<<<-]>+>+>->>+[<]<-]>>.>---.+++++++..+++.\
                     >>.<-.<.+++.------.--------.>>+.>++.!";

const SQUARES: &str = "++++[>+++++<-]>[<+++++>-]+<+[>[>+>+<<-]++>>[<<+>>-]>>>[-]++>[-]+>>>+[[-]++++++>>>]\
                       <<<[[<++++++++<++>>-]+<.<[>----<-]<]<<[>>>>>[>>>[-]+++++++++<[>-<-]+++++++++>\
                       [-[<->-]+[<<<]]<[>+<-]>]<<-]<<-]!";

const EXAMPLES: [(&str, &str, &str); 11] = [
    ("sum", "", "55\n"),
    ("mul", "", "42\n-144\n97406784\n"),
    ("fact", "", "3628800\n1307674368000\n"),
    ("sort", "", "1\n2\n3\n4\n5\n7\n8\n9\n"),
    ("symmetry", "", "11\n44\n16\n"),
    ("condensate", "", "1000\n228\n2414\n"),
    ("thermostat", "", "1000\n온도계를 본 뒤의 온도: 2006\n뒤쪽 타일의 온도: 6940\n"),
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

const REJECTED: [(&str, &str); 18] = [
    ("fermion i\nwhile <i|~32 > 0\n  raise i\n", "energy level"),
    ("fermion i\nwhile <i|~32 > 0\n    raise i\n", "forbidden transition"),
    ("fermion a @ 0\nfermion b @ 100\n", "Pauli"),
    ("fermion a\nemit <a|~24\n", "power of two"),
    ("fermion a\nsymmetry time\n |a> <- 1\n", "symmetry time"),
    ("fermion a[4]\nfermion s\nsymmetry translate k in a\n raise s\n", "may only change"),
    ("emit <nope|~32\n", "unknown name"),
    ("field hbar=random\n", "only seed can be random"),
    ("path f(a)\n return a\nemit f(1)\n", "closed system"),
    ("fermion a[2]\n|a> <- 1\n", "single packet"),
    ("fermion a[2]\nraise a\n", "single packet"),
    ("path emit(x)\n return 1\n", "law of nature"),
    ("fermion absorb\n", "law of nature"),
    ("fermion a mass 3\n", "unknown attribute"),
    ("create boson @ 8192\n", "cannot start"),
    ("fermion a\nfermion b\nemit <a|T|b>\n", "thermometer"),
    ("fermion a\nfermion b @ 8192\nsymmetry time\n pump a b\n", "symmetry time"),
    ("fermion a\nsymmetry time\n if <a|T|a> > 0\n    raise a\n", "closed system"),
];

const TRAPS: [(&str, &str); 3] = [
    ("|(999999)> <- 1\n", "vacuum"),
    ("fermion a[2]\n|a[5]> <- 1\n", "outside lattice"),
    ("boson a @ 0\nboson b @ 5000\nfuse a b\n", "same centre"),
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
    let mut input = String::new();
    (1..=100).for_each(|n| input.push_str(&format!("{n}\n")));
    let out = feed(Command::new(NOETHERC).arg(example("guess")).arg("--run"), &input);
    assert!(String::from_utf8_lossy(&out.stdout).ends_with("번 만에 맞혔습니다.\n"));
}

#[test]
fn the_cat_is_decided_by_opening_the_box_and_stays_decided() {
    let mut alive = 0;
    for _ in 0..40 {
        let out = feed(Command::new(NOETHERC).arg(example("cat")).arg("--run"), "\n");
        let text = String::from_utf8_lossy(&out.stdout);
        alive += text.contains("살아 있습니다") as u32;
        let looks = text.split("들여다봅니다:").nth(1).unwrap().lines().next().unwrap();
        let values: Vec<&str> = looks.split_whitespace().collect();
        assert_eq!(values.len(), 3);
        assert!(values.iter().all(|v| *v == values[0]), "{text}");
    }
    assert!((8..=32).contains(&alive), "{alive} of 40 survived");
}

#[test]
fn seed_random_differs_between_runs_of_one_executable() {
    let text = "field seed=random\nfermion v\nfermion s @ 8192\n|s> <- <v|\nemit <v|~1024\n";
    build_runtime();
    let exe = dir().join("random.exe");
    let built = Command::new(NOETHERC).arg(source("random", text)).arg("-o").arg(&exe).output();
    assert!(built.unwrap().status.success());
    let runs: std::collections::HashSet<Vec<u8>> =
        (0..12).map(|_| Command::new(&exe).output().unwrap().stdout).collect();
    assert!(runs.len() > 1);
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
        let text = format!(
            "field seed={seed}\nfermion v\nfermion s @ 8192\n|v> <- 1000\n\
             |s> <- <v|\nemit <v|~1024\nemit <v|~1024\n"
        );
        let out = feed(Command::new(NOETHERC).arg(source("noise", &text)).arg("--run"), "");
        String::from_utf8(out.stdout).unwrap()
    };
    assert_eq!(run(1), run(1));
    let values: Vec<Vec<i64>> = (0..8)
        .map(|s| run(s).lines().map(|l| l.parse().unwrap()).collect())
        .collect();
    assert!(values.iter().any(|v| v[0] != values[0][0]));
    for v in &values {
        // A sharp read shakes by hbar plus a quarter of the value.
        assert!((v[0] - 1000).abs() <= 16 + 250);
        assert_eq!(v[0], v[1]);
    }
}

/// Entering a symmetric block cold must not take the fast version.
#[test]
fn symmetric_blocks_check_the_band_on_entry() {
    build_runtime();
    let run_guard = "fermion x\nfermion y @ 8192\n|x> <- 1000\nsymmetry time\n \
                     if <x|~32 == 1000\n    raise y\nemit <y|~1024\n";
    let sweep_guard = "fermion a[2] @ 0 stride 8192\n|a[0]> <- 1000\n\
                       symmetry translate k in a\n |a[k]> <- <a[k]|~32\nemit <a[0]|~1024\n";
    // The loop counter is global, so the function's own tile only cools.
    let frame_guard = "fermion c\npath f(x)\n fermion y @ 8192\n |x> <- 1000\n |c> <- 400\n \
                       while <c|~1024 > 0\n    lower c\n    lower c\n    raise c\n \
                       symmetry time\n    if <x|~32 == 1000\n         raise y\n \
                       return <y|~1024\nemit f(0)\n";
    let programs = [
        ("guard_run", run_guard, "0\n"),
        ("guard_sweep", sweep_guard, "62\n"),
        ("guard_frame", frame_guard, "0\n"),
    ];
    for (name, text, want) in programs {
        let run = feed(Command::new(NOETHERC).arg(source(name, text)).arg("--run"), "");
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{name} --run");
        let exe = dir().join(format!("{name}.exe"));
        let built = Command::new(NOETHERC).arg(source(name, text)).arg("-o").arg(&exe).output();
        assert!(built.unwrap().status.success());
        let out = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name} native");
    }
}

#[test]
fn reads_depend_on_the_temperature_band() {
    let text = "fermion x\n|x> <- 1000\nemit <x|~32\nemit <x|~1024\nemit <x|~32\n";
    let out = feed(Command::new(NOETHERC).arg(source("band", text)).arg("--run"), "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "62\n1000\n250\n");
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
    let summary = String::from_utf8_lossy(&run.stderr);
    assert!(summary.contains("noisy reads") && !summary.contains(" 0 noisy"));
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
fn brainfuck_survives_the_squares() {
    let out = feed(Command::new(NOETHERC).arg(example("bf")).arg("--run"), SQUARES);
    let want: String = (0..=100).map(|i| format!("{}\n", i * i)).collect();
    assert_eq!(String::from_utf8_lossy(&out.stdout), want);
}

#[test]
fn programs_end_when_the_input_does() {
    for name in ["calc", "guess", "bf"] {
        let out = feed(Command::new(NOETHERC).arg(example(name)).arg("--run"), "");
        assert_eq!(out.status.code(), Some(0), "{name}");
    }
}

#[test]
fn a_windows_line_end_is_one_line_end() {
    let out = feed(Command::new(NOETHERC).arg(example("name")).arg("--run"), "Hi\r\n");
    assert!(String::from_utf8_lossy(&out.stdout).contains("Hi님!"));
    let out = feed(Command::new(NOETHERC).arg(example("calc")).arg("--run"), "*\r\n6\r\n7\r\nq\r\n");
    assert!(String::from_utf8_lossy(&out.stdout).contains("결과: 42"));
}

#[test]
fn comparisons_survive_the_lexer() {
    let text = "fermion x\n|x> <- -3\nif <x|~1024 <-1\n emit 1\nif <x|x>~1024== 9\n emit 2\nif <x|x>== -1\n emit 3\nemit 4\n";
    let out = feed(Command::new(NOETHERC).arg(source("compare", text)).arg("--run"), "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "1\n2\n4\n");
}

#[test]
fn the_executor_recurses_as_deep_as_compiled_code() {
    let text = "path f(n)\n if <n|~64 == 0\n    return 0\n fermion m\n |m> <- <n|~64\n lower m\n \
                return f(<m|~64)\nemit f(1000000)\n";
    let out = feed(Command::new(NOETHERC).arg(source("deep", text)).arg("--run"), "");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "0\n");
}

const HEAT_LAWS: [(&str, &str, &str); 6] = [
    ("thermometer", "fermion x\nemit <x|T|x>\nemit <x|T|x>\n", "65\n129\n"),
    (
        "exit_charged",
        "fermion x\n|x> <- 1000\nsymmetry time\n while <x|~1024 > 0\n    lower x\nemit <x|T|x>\n",
        "78\n",
    ),
    (
        "exit_free",
        "fermion x\n|x> <- 1000\nsymmetry time\n raise x\n lower x\nemit <x|T|x>\n",
        "72\n",
    ),
    (
        "exit_frame",
        "path f(a)\n symmetry time\n    while <a|~1024 > 0\n         lower a\n return <a|T|a>\nemit f(1000)\n",
        "1286\n",
    ),
    (
        "pump",
        "fermion a @ 0\nfermion b @ 8192\n|a> <- -1\n|a> <- 0\n|a> <- -1\n|a> <- 0\n\
         emit <a|T|a>\npump a b\nemit <a|T|a>\nemit <b|T|b>\n",
        "325\n227\n309\n",
    ),
    (
        "vent",
        "field vent=1000000\nfermion v @ 0\npath f()\n fermion w\n |w> <- 1\n |w> <- <w|~1024\n \
         |w> <- <w|~1024\n |w> <- <w|~1024\n |w> <- <w|~1024\nf()\nemit <v|T|v>\n\
         emit <1000000|T|1000000>\n",
        "65\n1110\n",
    ),
];

#[test]
fn heat_is_measured_moved_and_charged_the_same_both_ways() {
    build_runtime();
    for (name, text, want) in HEAT_LAWS {
        let run = feed(Command::new(NOETHERC).arg(source(name, text)).arg("--run"), "");
        assert_eq!(String::from_utf8_lossy(&run.stdout), want, "{name} --run");
        let exe = dir().join(format!("{name}.exe"));
        let built = Command::new(NOETHERC).arg(source(name, text)).arg("-o").arg(&exe).output();
        assert!(built.unwrap().status.success());
        let out = Command::new(&exe).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{name} native");
    }
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
