(() => {
  if (window.desktopApp) return;
  // fileSaved / showFile: a save fires "ams:file-saved" on window (every app window), to check again.
  const invoke = (cmd, args) => window.__TAURI_INTERNALS__?.invoke(cmd, args).catch(() => {});
  Object.defineProperty(window, "desktopApp", {
    value: Object.freeze({
      /** Dock (macOS) / tray tooltip count; 0 clears it. */
      setBadge: (count) => invoke("set_badge", { count: Math.max(0, Math.floor(Number(count) || 0)) }),
      /** Whether this file link (its presigned URL) was saved by the app and is still on disk. */
      fileSaved: (url) => invoke("file_saved", { url: String(url) }).then(Boolean),
      /** Opens the folder of that saved file, with it selected; false if it is not on disk (any more). */
      showFile: (url) => invoke("show_saved_file", { url: String(url) }).then(Boolean),
    }),
  });
  // Replaces the notification plugin's window.Notification (its script runs before this one): the FE's chat popups
  // (`new Notification(title, { body, tag })`) go to `notify`, which adds the sound, the de-duplication across the
  // main and chat windows, and the Dock bounce / taskbar flash. Desktop apps need no permission: always granted.
  // A click on the OS notification runs the page's own onclick (the FE opens the room): `notify` passes clickId
  // along, Rust calls __amsNotificationClick(clickId) in this window. The last 50 are kept.
  const shown = new Map();
  let nextClickId = 0;
  function DesktopNotification(title, options) {
    const o = options || {};
    this.title = String(title ?? "");
    this.body = String(o.body ?? "");
    this.tag = o.tag == null ? "" : String(o.tag);
    this.onclick = null;
    this.onclose = null;
    const clickId = ++nextClickId;
    shown.set(clickId, this);
    if (shown.size > 50) shown.delete(shown.keys().next().value);
    // data.kind "call" (FE desktop-notify): a click brings up the window that rings, see on_notification_click.
    const kind = o.data && typeof o.data.kind === "string" ? o.data.kind : null;
    invoke("notify", { title: this.title, body: this.body, tag: this.tag, clickId, kind });
  }
  Object.defineProperty(window, "__amsNotificationClick", {
    value: (clickId) => {
      const n = shown.get(clickId);
      shown.delete(clickId);
      if (n && typeof n.onclick === "function") n.onclick.call(n, new Event("click"));
      // The page reloaded since (instance gone): at least open the chat, as the FE's own click would.
      else if (!n && !location.pathname.startsWith("/chat")) window.open("/chat", "ams-chat");
    },
  });
  DesktopNotification.prototype.close = function () {};
  Object.defineProperty(DesktopNotification, "permission", { get: () => "granted" });
  DesktopNotification.requestPermission = (callback) => {
    if (typeof callback === "function") callback("granted");
    return Promise.resolve("granted");
  };
  window.Notification = DesktopNotification;
  // Lost connection on a loaded page: keep the page (like a PWA), dim it with a warning until it is back.
  // `offline` only sees the network going down; a failed fetch also catches "Wi-Fi but no internet",
  // confirmed by a probe of this site (a CORS error or another server down is a TypeError too).
  // The bundled start/offline page warns on its own.
  if (/^https?:$/.test(location.protocol) && location.hostname !== "amssettings.localhost") {
    let overlay = null;
    let probe = 0;
    const setOffline = (down) => {
      if (down === !!overlay) return;
      if (down) {
        // Dims and blurs the page (kept as is, nothing navigates) and holds clicks until it is back.
        overlay = document.createElement("div");
        overlay.setAttribute("role", "alert");
        overlay.style.cssText = "position:fixed;inset:0;z-index:2147483647;display:flex;justify-content:center;" +
          "align-items:flex-start;padding-top:24px;background:rgba(255,255,255,.45);" +
          "backdrop-filter:blur(3px) grayscale(.6);-webkit-backdrop-filter:blur(3px) grayscale(.6)";
        const note = document.createElement("div");
        note.textContent = "⚠ Mất kết nối internet. AMS tự kết nối lại khi có mạng.";
        note.style.cssText = "padding:10px 16px;border-radius:8px;background:#b45309;color:#fff;" +
          "font:600 14px -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;box-shadow:0 2px 8px rgba(0,0,0,.25)";
        overlay.appendChild(note);
        (document.body || document.documentElement).appendChild(overlay);
        probe = setInterval(() => navigator.onLine && reachable().then((ok) => ok && setOffline(false)), 5000);
      } else {
        overlay.remove();
        overlay = null;
        clearInterval(probe);
      }
    };
    const fetchNative = window.fetch.bind(window);
    // no-cors: any answer counts, even an opaque one or a redirect to the login.
    const reachable = () => fetchNative(location.origin + "/favicon.ico", { method: "HEAD", mode: "no-cors", cache: "no-store" })
      .then(() => true, () => false);
    window.fetch = (...args) => fetchNative(...args).then(
      (response) => (setOffline(false), response),
      (error) => {
        // TypeError = network failure (or CORS); AbortError (cancelled request) says nothing.
        if (error instanceof TypeError && !overlay) reachable().then((ok) => ok || setOffline(true));
        throw error;
      },
    );
    addEventListener("offline", () => setOffline(true));
    addEventListener("online", () => setOffline(false));
    addEventListener("DOMContentLoaded", () => setOffline(!navigator.onLine));
  }
  // The offline watch (offline/mod.rs watch_load): this document loaded; leaving it starts a new wait.
  // pageshow too: a page restored from the back/forward cache runs no init script.
  invoke("page_alive", { url: location.href });
  addEventListener("pageshow", () => invoke("page_alive", { url: location.href }));
  addEventListener("pagehide", () => invoke("page_leaving", { url: location.href }));
})();
