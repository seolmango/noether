const $ = (id) => document.getElementById(id);
const MAX_ROWS = 16;
const LABEL_WIDTH = 92;
const AXIS_HEIGHT = 22;

const code = $("code");
const stdinField = $("stdin");
const screen = $("screen");
const runButton = $("run");
const stopButton = $("stop");
const verdict = $("verdict");

let worker;
let examples = [];
let last = null; // the most recent result, for the chart and CSV
let grid = null; // what the chart is showing, for hover
let checkId = 0;
let checkTimer;
let running = false;
// Programs are deterministic, so "continuing" after input means running again
// from the start with everything typed so far.
let preset = ""; // input the example supplies by itself
let stdin = "";
let eof = false;

// ---------------------------------------------------------------- worker

function startWorker() {
  worker = new Worker("worker.js");
  worker.onmessage = ({ data }) => {
    if (data.kind === "examples") showExamples(data.value);
    if (data.kind === "check" && data.id === checkId) showCheck(data.value);
    if (data.kind === "run") finishRun(data.value);
  };
}

function say(text) {
  verdict.textContent = text;
  verdict.hidden = !text;
}

function run() {
  stdin = preset;
  eof = false;
  execute();
}

function execute() {
  running = true;
  runButton.hidden = true;
  stopButton.hidden = false;
  stdinField.hidden = true;
  say("");
  $("state").textContent = "실행 중";
  worker.postMessage({ kind: "run", source: code.value, input: stdin, interactive: !eof });
}

function idle() {
  running = false;
  runButton.hidden = false;
  stopButton.hidden = true;
}

function stop() {
  worker.terminate();
  startWorker();
  idle();
  $("state").textContent = "중지됨";
}

function finishRun(r) {
  idle();
  if (r.status === "compile") {
    showCheck({ ok: false, line: r.line, message: r.message });
    return;
  }
  last = r;
  say("");
  $("output").textContent = r.output;
  $("fault").textContent = {
    ok: "",
    input: "",
    trap: `\n실행 오류: ${r.message}\n`,
    budget: "\n실행 단계 한도를 넘어 멈췄습니다. 반복 횟수를 줄여 보세요.\n",
  }[r.status];
  $("state").textContent = {
    ok: "종료", input: "입력 대기", trap: "오류로 멈춤", budget: "오류로 멈춤",
  }[r.status];
  stdinField.hidden = r.status !== "input";
  if (r.status === "input") stdinField.focus({ preventScroll: true });
  screen.scrollTop = screen.scrollHeight;
  const n = (v) => Number(v).toLocaleString("ko-KR");
  $("summary").textContent =
    `최고 온도 ${n(r.peak)}, 임계 온도 ${n(r.critical)}, 흔들린 읽기 ${n(r.noisy)}회`;
  draw();
}

// ---------------------------------------------------------------- editor

function showExamples(list) {
  examples = list;
  const select = $("examples");
  list.forEach((e, i) => select.add(new Option(e.title, i)));
  load(0);
  runButton.disabled = false;
  run();
}

function load(i) {
  code.value = examples[i].source;
  preset = examples[i].input;
}

function showCheck(r) {
  if (running) return;
  say(r.ok ? "" : (r.line ? `${r.line}번째 줄: ` : "") + r.message);
}

code.addEventListener("input", () => {
  clearTimeout(checkTimer);
  checkTimer = setTimeout(() => {
    checkId += 1;
    worker.postMessage({ kind: "check", id: checkId, source: code.value });
  }, 180);
});

// Tab moves to the next legal energy level instead of leaving the editor.
code.addEventListener("keydown", (e) => {
  if (e.key !== "Tab" || e.shiftKey) return;
  const start = code.selectionStart;
  const lineStart = code.value.lastIndexOf("\n", start - 1) + 1;
  const column = start - lineStart;
  let n = 0;
  while (n * n <= column) n += 1;
  e.preventDefault();
  code.setRangeText(" ".repeat(n * n - column), start, start, "end");
  code.dispatchEvent(new Event("input"));
});

$("examples").addEventListener("change", (e) => {
  load(Number(e.target.value));
  run();
});
runButton.addEventListener("click", run);

stdinField.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    stdin += stdinField.value + "\n";
    stdinField.value = "";
    execute();
  } else if (e.key === "d" && e.ctrlKey) {
    e.preventDefault();
    stdin += stdinField.value;
    stdinField.value = "";
    eof = true;
    execute();
  }
});

screen.addEventListener("click", () => {
  if (!stdinField.hidden && !getSelection().toString()) stdinField.focus();
});
stopButton.addEventListener("click", stop);

// ---------------------------------------------------------------- thermogram

function token(name) {
  return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
}

function rgb(hex) {
  const v = parseInt(hex.slice(1), 16);
  return [v >> 16, (v >> 8) & 255, v & 255];
}

function ramp() {
  const stops = ["--heat-0", "--heat-1", "--heat-2"].map((t) => rgb(token(t)));
  return (f) => {
    const x = Math.min(1, Math.max(0, f)) * 2;
    const [a, b] = x < 1 ? [stops[0], stops[1]] : [stops[1], stops[2]];
    const t = x < 1 ? x : x - 1;
    return `rgb(${a.map((v, i) => Math.round(v + (b[i] - v) * t)).join(" ")})`;
  };
}

// Between samples a tile only cools, so the gaps can be filled in exactly.
function cooled(temp, fromTick, toTick, r) {
  const halvings = Math.floor(toTick / r.tau) - Math.floor(fromTick / r.tau);
  return Math.max(r.tmin, temp / 2 ** Math.min(halvings, 62));
}

function build(r, columns) {
  const peaks = new Array(r.tiles.length).fill(0);
  for (let i = 0; i < r.samples.length; i += 3) {
    peaks[r.samples[i + 1]] = Math.max(peaks[r.samples[i + 1]], r.samples[i + 2]);
  }
  const order = peaks.map((p, i) => i).sort((a, b) => peaks[b] - peaks[a]).slice(0, MAX_ROWS);
  const rowOf = new Map(order.map((tile, row) => [tile, row]));
  const span = Math.max(r.ticks, 1);
  const cells = order.map(() => new Float64Array(columns));
  const state = order.map(() => null);
  const at = (tick) => Math.min(columns - 1, Math.floor((tick / span) * columns));

  const flood = (row, upto) => {
    const s = state[row];
    if (!s) return;
    for (let c = s.column + 1; c < upto; c += 1) {
      const tick = (c / columns) * span;
      cells[row][c] = Math.max(cells[row][c], cooled(s.temp, s.tick, tick, r));
    }
  };
  for (let i = 0; i < r.samples.length; i += 3) {
    const row = rowOf.get(r.samples[i + 1]);
    if (row === undefined) continue;
    const [tick, temp] = [r.samples[i], r.samples[i + 2]];
    const column = at(tick);
    flood(row, column);
    cells[row][column] = Math.max(cells[row][column], temp);
    state[row] = { tick, temp, column };
  }
  order.forEach((_, row) => flood(row, columns));
  return {
    cells,
    columns,
    span,
    labels: order.map((t) => r.tiles[t]),
    hidden: r.tiles.length - order.length,
  };
}

function draw() {
  const canvas = $("heat");
  const box = canvas.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;
  canvas.width = box.width * dpr;
  canvas.height = box.height * dpr;
  const g = canvas.getContext("2d");
  g.scale(dpr, dpr);
  g.clearRect(0, 0, box.width, box.height);
  const ink = token("--soft");
  g.font = '12px "IBM Plex Sans KR", sans-serif';
  g.textBaseline = "middle";
  grid = null;
  if (!last || last.samples.length === 0) {
    g.fillStyle = ink;
    g.fillText("아직 메모리에 열이 나지 않았습니다.", 0, box.height / 2);
    return;
  }

  const width = Math.max(40, box.width - LABEL_WIDTH);
  const columns = Math.max(1, Math.min(Math.floor(width / 3), 320));
  const data = build(last, columns);
  const rows = data.cells.length;
  const height = box.height - AXIS_HEIGHT;
  const rowH = height / rows;
  const colW = width / columns;
  const color = ramp();
  const over = token("--heat-over");
  const top = Math.log2(last.critical);

  for (let row = 0; row < rows; row += 1) {
    const y = row * rowH;
    g.fillStyle = ink;
    const label = data.labels[row];
    g.fillText(label.length > 12 ? label.slice(0, 11) + "…" : label, 0, y + rowH / 2);
    for (let c = 0; c < columns; c += 1) {
      const t = data.cells[row][c];
      const x = LABEL_WIDTH + c * colW;
      // A 1px surface gap keeps rows distinct.
      if (t >= last.critical) {
        g.fillStyle = color(1);
        g.fillRect(x, y, colW + .5, rowH - 1);
        g.fillStyle = over;
        for (let k = (c % 2) * 3; k < rowH - 1; k += 6) g.fillRect(x, y + k, colW + .5, 2);
      } else {
        g.fillStyle = color(t > 0 ? Math.log2(Math.max(t, 1)) / top : 0);
        g.fillRect(x, y, colW + .5, rowH - 1);
      }
    }
  }
  const bottom = rows * rowH + 14;
  g.fillStyle = ink;
  g.textAlign = "left";
  g.fillText("틱 0", LABEL_WIDTH, bottom);
  g.textAlign = "right";
  g.fillText(`틱 ${data.span.toLocaleString("ko-KR")}`, LABEL_WIDTH + width, bottom);
  g.textAlign = "left";
  if (data.hidden > 0) g.fillText(`타일 ${data.hidden}개 생략`, 0, bottom);
  grid = { data, rowH, colW, width };
}

function drawLegend() {
  const canvas = $("legend");
  const g = canvas.getContext("2d");
  const color = ramp();
  for (let x = 0; x < canvas.width; x += 1) {
    g.fillStyle = color(x / (canvas.width - 1));
    g.fillRect(x, 0, 1, canvas.height);
  }
}

$("plot").addEventListener("pointermove", (e) => {
  const tip = $("tip");
  const box = $("heat").getBoundingClientRect();
  const x = e.clientX - box.left - LABEL_WIDTH;
  const y = e.clientY - box.top;
  if (!grid || x < 0 || x >= grid.width || y >= grid.rowH * grid.data.cells.length) {
    tip.hidden = true;
    return;
  }
  const row = Math.floor(y / grid.rowH);
  const column = Math.floor(x / grid.colW);
  const temp = Math.round(grid.data.cells[row][column]);
  const tick = Math.round((column / grid.data.columns) * grid.data.span);
  tip.textContent = "";
  const name = document.createElement("strong");
  name.textContent = grid.data.labels[row];
  tip.append(name, document.createElement("br"),
             `틱 ${tick.toLocaleString("ko-KR")}, 온도 ${temp.toLocaleString("ko-KR")}`);
  if (temp >= last.critical) tip.append(document.createElement("br"), "임계 초과: 값이 흔들립니다");
  tip.hidden = false;
  const left = Math.min(x + LABEL_WIDTH + 12, box.width - tip.offsetWidth);
  tip.style.left = `${Math.max(0, left)}px`;
  tip.style.top = `${y + 14}px`;
});
$("plot").addEventListener("pointerleave", () => { $("tip").hidden = true; });

new ResizeObserver(draw).observe($("heat"));
matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
  drawLegend();
  draw();
});

// On GitHub Pages the repository can be worked out from the address.
const pages = location.hostname.match(/^(.+)\.github\.io$/);
if (pages) {
  const repo = location.pathname.split("/")[1];
  const base = `https://github.com/${pages[1]}/${repo}`;
  $("link-repo").href = base;
  $("link-syntax").href = `${base}/blob/main/docs/syntax.md`;
  $("links").hidden = false;
}

drawLegend();
startWorker();
worker.postMessage({ kind: "examples" });
