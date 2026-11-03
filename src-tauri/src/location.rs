//! macOS location for the page's `navigator.geolocation`. wry does not answer WKWebView's geolocation
//! permission request on macOS, so WebKit denies every call ("Định vị đang bị chặn"). GEO_SCRIPT routes
//! the page's calls here instead; CoreLocation asks the user itself ("AMS would like to use your
//! location"), using NSLocationUsageDescription from Info.plist. Windows (WebView2) handles geolocation
//! natively, so this is macOS only.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Sender};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{define_class, msg_send, DefinedClass, MainThreadMarker, MainThreadOnly};
use objc2_core_location::{
  kCLLocationAccuracyBest, CLAuthorizationStatus, CLLocation, CLLocationManager, CLLocationManagerDelegate,
};
use objc2_foundation::{NSArray, NSError, NSObject, NSObjectProtocol};
use serde::Serialize;
use tauri::AppHandle;

/// Replaces `navigator.geolocation` and the "geolocation" answer of `navigator.permissions.query`
/// (the attendance page reads it to show "blocked" and to fetch a fix on its own once granted).
pub const GEO_SCRIPT: &str = r#"(() => {
  const ipc = () => window.__TAURI_INTERNALS__;
  if (!navigator.geolocation) return;
  const CODES = { PERMISSION_DENIED: 1, POSITION_UNAVAILABLE: 2, TIMEOUT: 3 };
  let state = null;
  const statuses = new Set();
  class GeoPermissionStatus extends EventTarget {
    constructor() { super(); this.name = "geolocation"; this.onchange = null; }
    get state() { return state || "prompt"; }
  }
  const setState = (next) => {
    if (next === state) return;
    state = next;
    for (const status of statuses) {
      const event = new Event("change");
      status.dispatchEvent(event);
      if (typeof status.onchange === "function") status.onchange(event);
    }
  };
  const getCurrentPosition = (success, error, options) => {
    const t = options && options.timeout;
    const timeoutMs = Number.isFinite(t) ? Math.max(0, Math.round(t)) : null;
    ipc().invoke("get_location", { timeoutMs }).then((fix) => {
      setState("granted");
      success({
        coords: {
          latitude: fix.latitude, longitude: fix.longitude, accuracy: fix.accuracy,
          altitude: null, altitudeAccuracy: null, heading: null, speed: null,
        },
        timestamp: fix.timestamp,
      });
    }, (reason) => {
      const message = String(reason);
      const code = message === "denied" ? 1 : message === "timeout" ? 3 : 2;
      if (code === 1) setState("denied");
      if (typeof error === "function") error({ code, message, ...CODES });
    });
  };
  let nextWatch = 0;
  const watches = new Map();
  const geolocation = {
    getCurrentPosition,
    watchPosition(success, error, options) {
      const id = ++nextWatch;
      getCurrentPosition(success, error, options);
      watches.set(id, setInterval(() => getCurrentPosition(success, error, options), 15000));
      return id;
    },
    clearWatch(id) { clearInterval(watches.get(id)); watches.delete(id); },
  };
  Object.defineProperty(navigator, "geolocation", { configurable: true, get: () => geolocation });
  const permissions = navigator.permissions;
  if (permissions && typeof permissions.query === "function") {
    const query = permissions.query.bind(permissions);
    permissions.query = (descriptor) => {
      if (!descriptor || descriptor.name !== "geolocation") return query(descriptor);
      return ipc().invoke("location_permission").then((current) => {
        state = current;
        const status = new GeoPermissionStatus();
        statuses.add(status);
        return status;
      });
    };
  }
})();"#;

/// The page's `GeolocationPosition` (timestamp in ms since the epoch).
#[derive(Serialize)]
pub struct Fix {
  latitude: f64,
  longitude: f64,
  accuracy: f64,
  timestamp: u64,
}

type Reply = Sender<Result<Fix, String>>;

struct Ivars {
  reply: RefCell<Option<Reply>>,
  requested: Cell<bool>,
}

define_class!(
  // SAFETY: NSObject has no subclassing requirements; Delegate has no Drop impl.
  #[unsafe(super(NSObject))]
  #[thread_kind = MainThreadOnly]
  #[name = "AMSLocationDelegate"]
  #[ivars = Ivars]
  struct Delegate;

  unsafe impl NSObjectProtocol for Delegate {}

  unsafe impl CLLocationManagerDelegate for Delegate {
    /// Also called once right after the delegate is set (macOS 11+): the single entry point.
    #[unsafe(method(locationManagerDidChangeAuthorization:))]
    fn did_change_authorization(&self, manager: &CLLocationManager) {
      self.on_authorization(manager);
    }

    #[unsafe(method(locationManager:didUpdateLocations:))]
    fn did_update_locations(&self, _manager: &CLLocationManager, locations: &NSArray<CLLocation>) {
      if let Some(location) = locations.lastObject() {
        self.finish(Ok(fix_of(&location)));
      }
    }

    #[unsafe(method(locationManager:didFailWithError:))]
    fn did_fail(&self, _manager: &CLLocationManager, error: &NSError) {
      match error.code() {
        // kCLErrorLocationUnknown: transient, CoreLocation keeps trying until the timeout.
        0 => {}
        // kCLErrorDenied
        1 => self.finish(Err("denied".into())),
        _ => self.finish(Err(format!("unavailable: {}", error.localizedDescription()))),
      }
    }
  }
);

impl Delegate {
  fn new(mtm: MainThreadMarker, reply: Reply) -> Retained<Self> {
    let this = Self::alloc(mtm).set_ivars(Ivars { reply: RefCell::new(Some(reply)), requested: Cell::new(false) });
    // SAFETY: NSObject's init on a freshly allocated instance.
    unsafe { msg_send![super(this), init] }
  }

  fn on_authorization(&self, manager: &CLLocationManager) {
    let status = unsafe { manager.authorizationStatus() };
    if status == CLAuthorizationStatus::NotDetermined {
      unsafe { manager.requestWhenInUseAuthorization() };
    } else if status == CLAuthorizationStatus::Denied || status == CLAuthorizationStatus::Restricted {
      self.finish(Err("denied".into()));
    } else if !self.ivars().requested.replace(true) {
      unsafe { manager.requestLocation() };
    }
  }

  fn finish(&self, result: Result<Fix, String>) {
    if let Some(reply) = self.ivars().reply.take() {
      let _ = reply.send(result);
    }
  }
}

fn fix_of(location: &CLLocation) -> Fix {
  let coordinate = unsafe { location.coordinate() };
  let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
  Fix {
    latitude: coordinate.latitude,
    longitude: coordinate.longitude,
    accuracy: unsafe { location.horizontalAccuracy() },
    timestamp,
  }
}

thread_local! {
  /// Requests in flight: the manager and its delegate must outlive the async callbacks.
  static ACTIVE: RefCell<HashMap<u32, (Retained<CLLocationManager>, Retained<Delegate>)>> = RefCell::new(HashMap::new());
}
static NEXT_REQUEST: AtomicU32 = AtomicU32::new(0);

fn permission_of(status: CLAuthorizationStatus) -> &'static str {
  if status == CLAuthorizationStatus::NotDetermined {
    "prompt"
  } else if status == CLAuthorizationStatus::Denied || status == CLAuthorizationStatus::Restricted {
    "denied"
  } else {
    "granted"
  }
}

/// Main thread only (CoreLocation delivers to the run loop of the thread that made the manager).
/// Returns whether the user is about to be asked, so the caller allows time for the dialog.
fn start(id: u32, reply: Reply) -> bool {
  let Some(mtm) = MainThreadMarker::new() else {
    let _ = reply.send(Err("unavailable: not on the main thread".into()));
    return false;
  };
  let delegate = Delegate::new(mtm, reply);
  let manager = unsafe { CLLocationManager::new() };
  let prompting = unsafe { manager.authorizationStatus() } == CLAuthorizationStatus::NotDetermined;
  unsafe {
    manager.setDesiredAccuracy(kCLLocationAccuracyBest);
    manager.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
  }
  ACTIVE.with(|active| active.borrow_mut().insert(id, (manager, delegate)));
  prompting
}

fn stop(id: u32) {
  ACTIVE.with(|active| {
    if let Some((manager, _delegate)) = active.borrow_mut().remove(&id) {
      unsafe {
        manager.stopUpdatingLocation();
        manager.setDelegate(None);
      }
    }
  });
}

/// The page's `navigator.geolocation.getCurrentPosition`. `timeout_ms` is the page's own timeout;
/// the first-time permission dialog does not count against it (as in browsers).
#[tauri::command]
pub async fn get_location(app: AppHandle, timeout_ms: Option<u64>) -> Result<Fix, String> {
  let id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
  let (reply, result) = mpsc::channel();
  let (started, prompting) = mpsc::channel();
  app
    .run_on_main_thread(move || {
      let _ = started.send(start(id, reply));
    })
    .map_err(|e| format!("unavailable: {e}"))?;
  let page_timeout = Duration::from_millis(timeout_ms.unwrap_or(15_000).clamp(1_000, 60_000));
  let outcome = tauri::async_runtime::spawn_blocking(move || {
    let prompting = prompting.recv_timeout(Duration::from_secs(5)).unwrap_or(false);
    let wait = if prompting { page_timeout + Duration::from_secs(30) } else { page_timeout };
    result.recv_timeout(wait)
  })
  .await
  .map_err(|e| format!("unavailable: {e}"))?;
  let _ = app.run_on_main_thread(move || stop(id));
  outcome.unwrap_or_else(|_| Err("timeout".into()))
}

/// The page's `navigator.permissions.query({ name: "geolocation" })`: "granted", "denied" or "prompt".
#[tauri::command]
pub async fn location_permission(app: AppHandle) -> Result<String, String> {
  let (tx, rx) = mpsc::channel();
  app
    .run_on_main_thread(move || {
      let status = unsafe { CLLocationManager::new().authorizationStatus() };
      let _ = tx.send(permission_of(status));
    })
    .map_err(|e| e.to_string())?;
  tauri::async_runtime::spawn_blocking(move || rx.recv_timeout(Duration::from_secs(5)))
    .await
    .map_err(|e| e.to_string())?
    .map(str::to_string)
    .map_err(|_| "prompt".to_string())
}
