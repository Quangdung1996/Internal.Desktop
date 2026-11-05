// npm test: runs src-tauri/src/links/link.js (the app's LINK_SCRIPT) against a minimal fake DOM and a
// streamed fake fetch. Checks the in-app download of S3 file links: progress notices, the saved
// name, the browser fallback, and that other window.open calls are untouched.
const assert = require("assert");
const script = require("fs").readFileSync(require("path").join(__dirname, "../src-tauri/src/links/link.js"), "utf8");
// A made-up file host: the real one comes from AMS_FILE_HOST at build time and is not in the repo.
const FILE_HOST = "files.example.test";
const src = script.replace("__AMS_FILE_HOST__", FILE_HOST);
const log = []; const clicked = []; const opened = [];
const el = (tag) => ({ tag, style: {}, attrs: {}, children: [], isConnected: false, _text: "",
  setAttribute(k, v) { this.attrs[k] = v; },
  appendChild(c) { c.isConnected = true; c.parent = this; this.children.push(c); return c; },
  remove() { this.isConnected = false; if (this.parent) this.parent.children = this.parent.children.filter((x) => x !== this); },
  click() { clicked.push({ href: this.href, download: this.download }); },
  set textContent(t) { this._text = t; log.push(t); }, get textContent() { return this._text; } });
const handlers = {};
global.document = { body: el("body"), documentElement: el("html"), createElement: el, addEventListener: (type, fn) => { handlers[type] = fn; } };
global.location = { href: "https://approve.agari.com.vn/leave", origin: "https://approve.agari.com.vn", assign() {} };
global.window = { open: () => "native", __TAURI_INTERNALS__: { invoke: (cmd, a) => { opened.push(a.url); return Promise.resolve(); } } };
global.Element = function () {};
// A clicked <a target="_blank">: attributes on the anchor, plus one descendant with a title.
const anchor = (href, attrs = {}, childTitle) => {
  const a = Object.assign(Object.create(Element.prototype), {
    href, getAttribute: (k) => attrs[k] ?? null,
    querySelector: () => (childTitle ? { getAttribute: () => childTitle } : null),
  });
  a.closest = () => a;
  return a;
};
const click = (a) => handlers.click({ button: 0, target: a, preventDefault() {} });
const savedName = async (fn) => { clicked.length = 0; fn(); await new Promise((r) => setTimeout(r, 30)); return clicked.at(-1)?.download; };
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

  // File names: the S3 key is not the original name (see fileName in src-tauri/src/links/link.js).
  mode = "ok";
  const hex = "0123456789abcdef0123456789abcdef", hex2 = "fedcba9876543210fedcba9876543210";
  const key = (k, q = "") => `https://bucket.${FILE_HOST}/ams/chat/${k}${q}`;
  // FileController + AwsS3Service: <hex>_<name without spaces>_<hex>.<ext> -> both ids dropped
  assert.strictEqual(await savedName(() => window.open(key(`${hex}_Đơnnghỉphép1_${hex2}.pdf`))), "Đơnnghỉphép1.pdf");
  // the name the page shows wins (chat file card: <span title="…">), same extension only
  assert.strictEqual(await savedName(() => click(anchor(key(`${hex}_Đơnnghỉphép1_${hex2}.pdf`), {}, "Đơn nghỉ phép (1).pdf"))), "Đơn nghỉ phép (1).pdf");
  assert.strictEqual(await savedName(() => click(anchor(key(`${hex}_report_${hex2}.xlsx`), { title: "Mở tệp" }))), "report.xlsx", "a title that is not a file name is ignored");
  assert.strictEqual(await savedName(() => click(anchor(key(`${hex}_a_${hex2}.pdf`), { download: "Bảng lương T10.pdf" }))), "Bảng lương T10.pdf");
  // response-content-disposition in the presigned URL, if the backend adds it
  assert.strictEqual(await savedName(() => window.open(key(`${hex}_x_${hex2}.docx`, "?response-content-disposition=" + encodeURIComponent("attachment; filename*=UTF-8''" + encodeURIComponent("Hợp đồng số 5.docx"))))), "Hợp đồng số 5.docx");
  console.log("ALL CHECKS PASSED");
  process.exit(0);
})().catch((e) => { console.error(e); process.exit(1); });
