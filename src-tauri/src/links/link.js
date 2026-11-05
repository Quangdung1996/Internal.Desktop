(() => {
  const isChat = (u) => u.origin === location.origin && (u.pathname === "/chat" || u.pathname.startsWith("/chat/"));
  // The S3 endpoint (AwsS3Settings.Endpoint in Internal.Backend), filled in from AMS_FILE_HOST at
  // build time (build.rs); <bucket>.<endpoint> too (virtual-hosted URLs). Empty: no file host.
  const FILE_HOST = "__AMS_FILE_HOST__";
  const isFile = (u) => FILE_HOST !== "" && (u.hostname === FILE_HOST || u.hostname.endsWith("." + FILE_HOST));
  const openLink = (href) => {
    const ipc = window.__TAURI_INTERNALS__;
    if (!ipc) { location.assign(href); return; }
    ipc.invoke("open_link", { url: href }).catch(() => location.assign(href));
  };
  // The saved name. The S3 key is not the original: AwsS3Service prefixes "<32 hex>_", and
  // FileController.GenerateUniqueFileName drops spaces/punctuation and appends "_<32 hex>". So, in order:
  // 1. the name the page shows for this link (`hint`: a download="…" or title="…" ending in the same
  //    extension, e.g. the chat file card), 2. response-content-disposition in the presigned URL (if
  //    the backend adds it), 3. the key without both hex ids.
  const extOf = (n) => (/\.[^./]+$/.exec(n) || [""])[0].toLowerCase();
  const fileName = (u, hint) => {
    const key = decodeURIComponent(u.pathname.split("/").pop() || "file");
    if (hint && extOf(hint) && extOf(hint) === extOf(key)) return hint;
    const cd = u.searchParams.get("response-content-disposition") || "";
    const m = /filename\*=UTF-8''([^;]+)/i.exec(cd) || /filename="?([^";]+)"?/i.exec(cd);
    if (m) try { return decodeURIComponent(m[1]); } catch {}
    return key.replace(/^[0-9a-f]{32}_/i, "").replace(/_[0-9a-f]{32}(?=\.[^.]+$|$)/i, "") || key;
  };
  // What the page shows as this link's file name (an attribute, never free text).
  const hintOf = (a) => [a.getAttribute("download"), a.getAttribute("title"), a.querySelector("[title]")?.getAttribute("title")]
    .map((v) => (v || "").trim()).find((v) => extOf(v)) || "";
  // One line per download, bottom right: progress, then done / failed. Clicks pass through.
  let tray = null;
  const notice = () => {
    if (!tray || !tray.isConnected) {
      tray = document.createElement("div");
      tray.setAttribute("role", "status");
      tray.setAttribute("aria-live", "polite");
      tray.style.cssText = "position:fixed;right:16px;bottom:16px;z-index:2147483646;display:flex;" +
        "flex-direction:column;gap:8px;max-width:min(360px,calc(100vw - 32px));pointer-events:none";
      (document.body || document.documentElement).appendChild(tray);
    }
    const line = document.createElement("div");
    line.style.cssText = "padding:10px 14px;border-radius:8px;background:#067179;color:#fff;" +
      "font:500 13px/1.4 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;" +
      "box-shadow:0 2px 8px rgba(0,0,0,.25);overflow-wrap:anywhere";
    tray.appendChild(line);
    return (text, end) => {
      line.textContent = text;
      if (end) {
        if (end === "error") line.style.background = '#b45309';
        setTimeout(() => line.remove(), 5000);
      }
    };
  };
  const download = async (url, hint) => {
    const name = fileName(url, hint);
    const show = notice();
    show(`⬇ Đang tải ${name}…`);
    try {
      const res = await fetch(url.href);
      if (!res.ok || !res.body) throw new Error(String(res.status));
      // Content-Length is readable cross-origin (CORS-safelisted): a percentage when the server sends it.
      const total = Number(res.headers.get("content-length")) || 0;
      const reader = res.body.getReader();
      const chunks = [];
      let received = 0;
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        chunks.push(value);
        received += value.length;
        show(`⬇ Đang tải ${name}… ${total ? Math.floor((received * 100) / total) + "%" : Math.round(received / 1024) + " KB"}`);
      }
      const blobUrl = URL.createObjectURL(new Blob(chunks, { type: res.headers.get("content-type") || "" }));
      const a = document.createElement("a");
      a.href = blobUrl;
      a.download = name;
      document.body.appendChild(a);
      a.click();
      a.remove();
      setTimeout(() => URL.revokeObjectURL(blobUrl), 60000);
      show(`✓ Đã tải xong ${name}`, "done");
    } catch {
      show(`⚠ Không tải được ${name} trong app, đang mở bằng trình duyệt…`, "error");
      openLink(url.href); // CORS / network / expired link: the browser, as before
    }
  };
  document.addEventListener("click", (e) => {
    if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
    const a = e.target instanceof Element ? e.target.closest('a[target="_blank"]') : null;
    if (!a || !a.href) return;
    let url;
    try { url = new URL(a.href, location.href); } catch { return; }
    if (isFile(url)) { e.preventDefault(); download(url, hintOf(a)); return; }
    if (a.hasAttribute("download")) return;
    if (url.protocol !== "http:" && url.protocol !== "https:") return; // blob: stays with on_new_window
    e.preventDefault();
    openLink(url.href);
  }, true);
  const nativeOpen = window.open;
  window.open = function (href, ...rest) {
    try {
      const url = new URL(String(href ?? ""), location.href);
      if (isFile(url)) { download(url); return null; }
      if (isChat(url)) {
        if (isChat(new URL(location.href))) { location.assign(url.href); return window; }
        openLink(url.href);
        return null;
      }
    } catch {}
    return nativeOpen.call(window, href, ...rest);
  };
})();
