// Runs Noether off the main thread, so a long program can be stopped.
const BUDGET = 60_000_000;
const encoder = new TextEncoder();
const decoder = new TextDecoder();

const compiled = WebAssembly.compileStreaming(fetch("noether.wasm"));

async function instance() {
  return (await WebAssembly.instantiate(await compiled, {})).exports;
}

function put(wasm, text) {
  const bytes = encoder.encode(text);
  const ptr = wasm.alloc(bytes.length);
  new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
  return [ptr, bytes.length];
}

function result(wasm) {
  const bytes = new Uint8Array(wasm.memory.buffer, wasm.result_ptr(), wasm.result_len());
  return JSON.parse(decoder.decode(bytes));
}

self.onmessage = async ({ data }) => {
  // A trap leaves the instance unusable for another run, so each call gets a new one.
  const wasm = await instance();
  if (data.kind === "examples") {
    wasm.examples();
    self.postMessage({ kind: "examples", value: result(wasm) });
  } else if (data.kind === "check") {
    wasm.check(...put(wasm, data.source));
    self.postMessage({ kind: "check", id: data.id, value: result(wasm) });
  } else {
    try {
      wasm.run(...put(wasm, data.source), ...put(wasm, data.input), BUDGET,
               data.interactive ? 1 : 0, data.entropy);
    } catch {
      // A trap or a stack overflow. `finish` reports what happened.
    }
    wasm.finish();
    self.postMessage({ kind: "run", value: result(wasm) });
  }
};
