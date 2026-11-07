"""Noether의 우월성을 측정합니다. 결과는 results.json에 남습니다.

    python bench/run.py
    python bench/chart.py
"""

import json
import random
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

BENCH = Path(__file__).resolve().parent
ROOT = BENCH.parent
NOETHERC = ROOT / "target" / "release" / "noetherc.exe"
LANGUAGES = ["Noether", "Rust", "Node.js", "Python"]
LOOPS = 100_000_000
PATIENTS = 94
VISITS = 100
FAVOURITE = 47  # 창의성 측정에 협조해 주신 환자의 번호입니다.


def build(name, work):
    """Each language gets the command that runs its copy of a program."""
    source = BENCH / name / name
    noether = work / f"{name}_noether.exe"
    rust = work / f"{name}_rust.exe"
    subprocess.run([NOETHERC, source.with_suffix(".noe"), "-o", noether], check=True)
    subprocess.run(["rustc", "-O", source.with_suffix(".rs"), "-o", rust], check=True)
    return {
        "Noether": [noether],
        "Rust": [rust],
        "Node.js": ["node", source.with_suffix(".js")],
        "Python": [sys.executable, source.with_suffix(".py")],
    }


def run(command, text=""):
    done = subprocess.run(command, input=text, capture_output=True, text=True, timeout=120)
    return done.stdout.strip()


def speed(commands):
    want = str(LOOPS * (LOOPS + 1) // 2)
    result = {}
    for language in LANGUAGES:
        best = float("inf")
        for _ in range(1 if language == "Python" else 5):
            start = time.perf_counter()
            answer = run(commands[language])
            best = min(best, time.perf_counter() - start)
            assert answer == want, (language, answer)
        result[language] = round(LOOPS / best)
    return result


def patients():
    """The same 94 patients every time. They did not consent to a re-run."""
    rng = random.Random(1895)  # 뢴트겐이 X선을 발견한 해입니다.
    return [(rng.randint(40, 120), rng.randint(1, 30), rng.randint(5, 40)) for _ in range(PATIENTS)]


def accuracy(commands):
    result = {}
    for language in LANGUAGES:
        hits = 0
        for weight, rate, sessions in patients():
            answer = run(commands[language], f"{weight}\n{rate}\n{sessions}\n")
            hits += answer == str(weight * rate * sessions)
        result[language] = round(100 * hits / PATIENTS)
    return result


def creativity(commands):
    weight, rate, sessions = patients()[FAVOURITE]
    text = f"{weight}\n{rate}\n{sessions}\n"
    return {
        language: len({run(commands[language], text) for _ in range(VISITS)})
        for language in LANGUAGES
    }


def main():
    work = Path(tempfile.mkdtemp())
    try:
        loops = build("sum", work)
        doses = build("dose", work)
        results = {
            "speed": speed(loops),
            "accuracy": accuracy(doses),
            "creativity": creativity(doses),
        }
    finally:
        shutil.rmtree(work, ignore_errors=True)
    (BENCH / "results.json").write_text(json.dumps(results, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(results, indent=2))


if __name__ == "__main__":
    main()
