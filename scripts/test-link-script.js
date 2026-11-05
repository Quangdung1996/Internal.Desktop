// npm test: runs LINK_SCRIPT (taken from src-tauri/src/lib.rs) against a minimal fake DOM and a
// streamed fake fetch. Checks the in-app download of S3 file links: progress notices, the saved
// name, the browser fallback, and that other window.open calls are untouched.
const assert = require("assert");
const lib = require("fs").readFileSync(require("path").join(__dirname, "../src-tauri/src/lib.rs"), "utf8");
// A made-up file host: the real one comes from AMS_FILE_HOST at build time and is not in the repo.
const FILE_HOST = "files.example.test";
const src = lib.split('const LINK_SCRIPT: &str = r#"')[1].split('"#;')[0].replace("__AMS_FILE_HOST__", FILE_HOST);
const log = []; const clicked = []; const opened = [];
const el = (tag) => ({ tag, style: {}, attrs: {}, children: [], isConnected: false, _text: "",
  setAttribute(k, v) { this.attrs[k] = v; },
  appendChild(c) { c.isConnected = true; c.parent = this; this.children.push(c); return c; },
  remove() { this.isConnected = false; if (this.parent) this.parent.children = this.parent.children.filter((x) => x !== this); },
  click() { clicked.push({ href: this.href, download: this.download }); },
  set textContent(t) { this._text = t; log.push(t); }, get textContent() { return this._text; } });
global.document = { body: el("body"), documentElement: el("html"), createElement: el, addEventListener() {} };
global.location = { href: "https://approve.agari.com.vn/leave", origin: "https://approve.agari.com.vn", assign() {} };
global.window = { open: () => "native", __TAURI_INTERNALS__: { invoke: (cmd, a) => { opened.push(a.url); return Promise.resolve(); } } };
global.Element = function () {};
URL.createObjectURL = () => "blob:fake"; URL.revokeObjectURL = () => {};
const body = "x".repeat(3000);
let mode = "ok";
global.fetch = async () => {
  if (mode === "fail") throw new TypeError("Failed to fetch");
  const enc = new TextEncoder().encode(body);
  const stream = new ReadableStream({ start(c) { for (let i = 0; i < enc.length; i += 1000) c.enqueue(enc.slice(i, i + 1000)); c.close(); } });
  return new Response(stream, { headers: { "content-length": String(enc.length), "content-type": "application/pdf" } });
};
eval(src);
const fileUrl = `https://bucket.${FILE_HOST}/ams/leave/0123456789abcdef0123456789abcdef_%C4%90%C6%A1n%20ngh%E1%BB%89.pdf?X-Amz-Signature=abc`;
(async () => {
  assert.strictEqual(window.open(fileUrl, "_blank", "noopener"), null, "file link must not open a window");
  await new Promise((r) => setTimeout(r, 50));
  console.log(log.join("\n"));
  assert.deepStrictEqual(log, ["⬇ Đang tải Đơn nghỉ.pdf…", "⬇ Đang tải Đơn nghỉ.pdf… 33%", "⬇ Đang tải Đơn nghỉ.pdf… 66%", "⬇ Đang tải Đơn nghỉ.pdf… 100%", "✓ Đã tải xong Đơn nghỉ.pdf"]);
  assert.deepStrictEqual(clicked, [{ href: "blob:fake", download: "Đơn nghỉ.pdf" }]);
  assert.strictEqual(opened.length, 0, "no browser on success");
  mode = "fail"; log.length = 0;
  window.open(fileUrl);
  await new Promise((r) => setTimeout(r, 50));
  console.log(log.join("\n"));
  assert.strictEqual(log.at(-1), "⚠ Không tải được Đơn nghỉ.pdf trong app, đang mở bằng trình duyệt…");
  assert.deepStrictEqual(opened, [fileUrl], "falls back to the browser");
  assert.strictEqual(window.open("https://meet.google.com/x"), "native", "other links unchanged");
  console.log("ALL CHECKS PASSED");
  process.exit(0);
})().catch((e) => { console.error(e); process.exit(1); });
