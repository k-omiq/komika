//! In-process embedded Suwayomi engine for REAL iOS DEVICES (N4.2).
//!
//! iOS has no child-process model and forbids app-loaded dylibs, so unlike the
//! desktop supervisor (`suwayomi.rs`, a `java` child-process sidecar) this module
//! boots the stock `Suwayomi-Server.jar` INSIDE the app process: it calls
//! `JNI_CreateJavaVM` against the interpreter-only (Zero, NO-JIT) OpenJDK that
//! N4.1 built as static libs (`jre-ios/aarch64-ios`, statically linked into the
//! app binary — see `build.rs` and docs/plans/n4.1-ios-jvm-findings.md §6), then
//! invokes `suwayomi.tachidesk.MainKt.main(String[])` (the jar's manifest
//! Main-Class) on a dedicated native thread.
//!
//! Runtime layout (why the app bundle carries a root-level `lib/` folder): the
//! statically-linked HotSpot derives `java.home` ITSELF as `<exe dir>/lib` — on
//! iOS `os::init_system_properties_values()` takes the dladdr path of the app
//! executable, strips the binary name, and appends `/lib` because
//! `is_vm_statically_linked()` is true (verified against the pinned
//! openjdk/mobile sources, os_bsd.cpp `__IOS__` branch). `-Djava.home` CANNOT
//! relocate the boot jimage: `set_boot_path()` (=> `<java.home>/lib/modules`)
//! runs during property init, BEFORE `-D` options are applied. The bundle must
//! therefore contain:
//!   Komika.app/lib/lib/modules            <- jimage (all 66 modules, ~149 MB)
//!   Komika.app/lib/lib/{tzdb.dat,security,jfr,jvm.cfg}
//!   Komika.app/lib/{conf,release}
//! staged into the Xcode project by scripts/stage-ios-jvm-runtime.sh (a folder
//! reference in gen/apple/project.yml). The jar itself IS a normal tauri
//! resource (`tauri.ios.conf.json` bundles `suwayomi/`, resolved via
//! `resource_dir()` => Komika.app/assets/suwayomi/Suwayomi-Server.jar).
//!
//! Same public surface as desktop: `SuwayomiSupervisor` (managed state),
//! `start(app)`, and the four `#[tauri::command]`s with identical names and
//! JSON shapes, so the JS `LocalSuwayomiBackend` works unchanged. Security
//! posture also unchanged: the engine binds 127.0.0.1 only; JS reaches it only
//! through the `suwayomi_gql` / `suwayomi_image` IPC proxies.
//!
//! Lifecycle limits (deliberate for this gate, N4.8 refines):
//!   * ONE boot per process. A JVM cannot be re-created after DestroyJavaVM in
//!     the same process, so there is no crash/restart supervision loop — a dead
//!     engine transitions to `degraded` and stays there until app relaunch.
//!   * `stop_and_wait` does NOT call DestroyJavaVM: it would block until the
//!     engine's non-daemon server threads exit (i.e. forever), and on iOS the
//!     process is about to be torn down by the OS anyway. It just flips the
//!     state to `stopped`; the JVM dies with the process. Best-effort by design.

use std::ffi::{c_char, c_void, CString};
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use jni_sys::{
  jint, jvalue, JavaVM, JavaVMInitArgs, JavaVMOption, JNIEnv, JNI_CreateJavaVM, JNI_OK,
  JNI_TRUE, JNI_VERSION_1_8,
};
use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};

/// Cold-start budget for the readiness gate. The desktop JIT JVM needs ~5 s; the
/// Zero interpreter on a phone is an order of magnitude slower and unmeasured
/// before this spike (OQ1), so the budget is deliberately generous.
const READY_TIMEOUT: Duration = Duration::from_secs(300);
/// How often to poll `aboutServer` while waiting for readiness.
const POLL_INTERVAL: Duration = Duration::from_millis(500);
/// Per-request timeout for the readiness poll and the `suwayomi_gql` proxy.
const HTTP_TIMEOUT: Duration = Duration::from_secs(60);
/// Hard cap on an engine image body (kept in lockstep with lib.rs / desktop).
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024; // 32 MiB
/// Native stack for the thread that owns the VM and runs Java `main` — the Zero
/// interpreter burns C stack per Java frame, so give it far more than `-Xss`.
const JVM_THREAD_STACK: usize = 16 * 1024 * 1024;

/// Lifecycle state, surfaced to JS as a lowercase string via `suwayomi_status`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum EngineState {
  Starting,
  Ready,
  Degraded,
  Stopped,
}

impl EngineState {
  fn as_str(self) -> &'static str {
    match self {
      EngineState::Starting => "starting",
      EngineState::Ready => "ready",
      EngineState::Degraded => "degraded",
      EngineState::Stopped => "stopped",
    }
  }
}

/// JSON shape returned by `suwayomi_status` — identical to desktop's.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Status {
  pub state: String,
  pub version: Option<String>,
  pub last_error: Option<String>,
}

struct Inner {
  port: Option<u16>,
  state: EngineState,
  version: Option<String>,
  last_error: Option<String>,
}

/// Owns the embedded engine's shared state and HTTP client. Unlike desktop there
/// is no child process, no restart loop, and no lockfile — the VM lives and dies
/// with this process.
pub struct SuwayomiSupervisor {
  inner: Mutex<Inner>,
  client: reqwest::Client,
  /// One JVM per process, ever (see module docs).
  boot_attempted: AtomicBool,
}

impl SuwayomiSupervisor {
  pub fn new() -> Self {
    let client = reqwest::Client::builder()
      .timeout(HTTP_TIMEOUT)
      // Loopback is allowed here (engine transport), same posture as desktop.
      .build()
      .expect("build suwayomi http client");
    SuwayomiSupervisor {
      inner: Mutex::new(Inner {
        port: None,
        state: EngineState::Starting,
        version: None,
        last_error: None,
      }),
      client,
      boot_attempted: AtomicBool::new(false),
    }
  }

  fn set_ready(&self, port: u16, version: String) {
    let mut g = self.inner.lock().unwrap();
    g.port = Some(port);
    g.state = EngineState::Ready;
    g.version = Some(version);
    g.last_error = None;
  }

  fn transition(&self, state: EngineState, err: Option<String>) {
    let mut g = self.inner.lock().unwrap();
    g.state = state;
    if state != EngineState::Ready {
      g.port = None;
    }
    if let Some(e) = err {
      g.last_error = Some(e);
    }
  }

  pub fn status(&self) -> Status {
    let g = self.inner.lock().unwrap();
    Status {
      state: g.state.as_str().to_string(),
      version: g.version.clone(),
      last_error: g.last_error.clone(),
    }
  }

  pub fn base_url(&self) -> Option<String> {
    let g = self.inner.lock().unwrap();
    if g.state == EngineState::Ready {
      g.port.map(|p| format!("http://127.0.0.1:{p}"))
    } else {
      None
    }
  }

  fn ready_endpoint(&self) -> Option<(u16, reqwest::Client)> {
    let g = self.inner.lock().unwrap();
    if g.state == EngineState::Ready {
      g.port.map(|p| (p, self.client.clone()))
    } else {
      None
    }
  }

  /// Best-effort shutdown (see module docs): the in-process JVM is NOT destroyed
  /// (DestroyJavaVM would block on the engine's non-daemon threads); we only
  /// reflect `Stopped` so the JS side stops routing to the engine.
  pub async fn stop_and_wait(&self) {
    self.transition(EngineState::Stopped, None);
  }
}

impl Default for SuwayomiSupervisor {
  fn default() -> Self {
    Self::new()
  }
}

/// Broker a free loopback port: bind :0, read the assignment, drop the listener.
fn broker_port() -> std::io::Result<u16> {
  let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
  let port = listener.local_addr()?.port();
  drop(listener);
  Ok(port)
}

/// Render the authoritative HOCON `server.conf` — VERBATIM the desktop template
/// (suwayomi.rs): kcefEnabled=false is critical there and harmless-but-correct
/// here (no Chromium download attempts on a phone).
fn render_server_conf(port: u16) -> String {
  format!(
    "server.ip = \"127.0.0.1\"\n\
     server.port = {port}\n\
     server.webUIEnabled = false\n\
     server.initialOpenInBrowserEnabled = false\n\
     server.systemTrayEnabled = false\n\
     server.kcefEnabled = false\n\
     server.flareSolverrEnabled = false\n"
  )
}

/// Write `server.conf` into the data dir (creating it), mirroring desktop.
fn prepare_data_dir(data_dir: &Path, port: u16) -> Result<(), String> {
  std::fs::create_dir_all(data_dir).map_err(|e| format!("create data dir: {e}"))?;
  std::fs::write(data_dir.join("server.conf"), render_server_conf(port))
    .map_err(|e| format!("write server.conf: {e}"))?;
  let kcef = data_dir.join("bin").join("kcef");
  let _ = std::fs::remove_dir_all(&kcef);
  let _ = std::fs::remove_file(&kcef);
  Ok(())
}

/// Poll `POST /api/graphql { aboutServer { version } }` until a version comes
/// back or the timeout elapses (same shape as desktop's readiness gate).
async fn poll_ready(
  client: &reqwest::Client,
  port: u16,
  timeout: Duration,
) -> Result<String, String> {
  let url = format!("http://127.0.0.1:{port}/api/graphql");
  let body = json!({ "query": "{ aboutServer { version } }" }).to_string();
  let deadline = Instant::now() + timeout;
  loop {
    if let Ok(resp) = client
      .post(&url)
      .header(reqwest::header::CONTENT_TYPE, "application/json")
      .body(body.clone())
      .send()
      .await
    {
      if let Ok(bytes) = resp.bytes().await {
        if let Ok(val) = serde_json::from_slice::<Value>(&bytes) {
          if let Some(ver) = val
            .pointer("/data/aboutServer/version")
            .and_then(|v| v.as_str())
          {
            return Ok(ver.to_string());
          }
        }
      }
    }
    if Instant::now() >= deadline {
      return Err(format!(
        "engine did not become ready within {}s",
        timeout.as_secs()
      ));
    }
    tokio::time::sleep(POLL_INTERVAL).await;
  }
}

/// Describe + clear any pending Java exception (stack trace goes to stderr,
/// which devicectl's console capture streams).
unsafe fn describe_exception(env: *mut JNIEnv) {
  if let Some(check) = (**env).ExceptionCheck {
    if check(env) == JNI_TRUE {
      if let Some(describe) = (**env).ExceptionDescribe {
        describe(env);
      }
      if let Some(clear) = (**env).ExceptionClear {
        clear(env);
      }
    }
  }
}

/// Create the in-process JVM and run the engine's `main` on THIS thread. Blocks
/// for the lifetime of the engine (i.e. of the app) on success; returns `Err`
/// only when the boot itself failed.
///
/// `-Xmx256m` is the jetsam-informed cap from the spike plan (§2 Q1); `-Xss1m`
/// bounds per-Java-thread stacks. NO `-Djava.home`: the statically-linked VM
/// derives it from the executable path (see module docs) and the bundle layout
/// satisfies it.
fn run_jvm(jar: &Path, data_dir: &Path) -> Result<(), String> {
  let opt_strings: Vec<CString> = [
    format!("-Djava.class.path={}", jar.display()),
    format!(
      "-Dsuwayomi.tachidesk.config.server.rootDir={}",
      data_dir.display()
    ),
    "-Djava.awt.headless=true".to_string(),
    "-Xmx256m".to_string(),
    "-Xss1m".to_string(),
  ]
  .into_iter()
  .map(|s| CString::new(s).expect("jvm option contains NUL"))
  .collect();

  let mut options: Vec<JavaVMOption> = opt_strings
    .iter()
    .map(|s| JavaVMOption {
      optionString: s.as_ptr() as *mut c_char,
      extraInfo: ptr::null_mut(),
    })
    .collect();

  let mut init_args = JavaVMInitArgs {
    version: JNI_VERSION_1_8,
    nOptions: options.len() as jint,
    options: options.as_mut_ptr(),
    ignoreUnrecognized: JNI_TRUE,
  };

  let mut vm: *mut JavaVM = ptr::null_mut();
  let mut env_ptr: *mut c_void = ptr::null_mut();
  let t0 = Instant::now();
  let rc = unsafe {
    JNI_CreateJavaVM(
      &mut vm,
      &mut env_ptr,
      &mut init_args as *mut JavaVMInitArgs as *mut c_void,
    )
  };
  if rc != JNI_OK || vm.is_null() || env_ptr.is_null() {
    return Err(format!("JNI_CreateJavaVM failed: rc={rc}"));
  }
  log::info!(
    target: "suwayomi",
    "in-process Zero JVM created in {} ms",
    t0.elapsed().as_millis()
  );

  let env = env_ptr as *mut JNIEnv;
  unsafe {
    // Manifest Main-Class of Suwayomi-Server.jar v2.3.2243 (verified from
    // META-INF/MANIFEST.MF): suwayomi.tachidesk.MainKt.
    let main_cls_name = CString::new("suwayomi/tachidesk/MainKt").unwrap();
    let main_cls = ((**env).FindClass.unwrap())(env, main_cls_name.as_ptr());
    if main_cls.is_null() {
      describe_exception(env);
      return Err("FindClass suwayomi/tachidesk/MainKt failed (classpath?)".into());
    }

    let name = CString::new("main").unwrap();
    let sig = CString::new("([Ljava/lang/String;)V").unwrap();
    let mid = ((**env).GetStaticMethodID.unwrap())(env, main_cls, name.as_ptr(), sig.as_ptr());
    if mid.is_null() {
      describe_exception(env);
      return Err("GetStaticMethodID main([Ljava/lang/String;)V failed".into());
    }

    let string_cls_name = CString::new("java/lang/String").unwrap();
    let string_cls = ((**env).FindClass.unwrap())(env, string_cls_name.as_ptr());
    if string_cls.is_null() {
      describe_exception(env);
      return Err("FindClass java/lang/String failed".into());
    }
    let argv = ((**env).NewObjectArray.unwrap())(env, 0, string_cls, ptr::null_mut());
    if argv.is_null() {
      describe_exception(env);
      return Err("NewObjectArray(String[0]) failed".into());
    }

    log::info!(target: "suwayomi", "invoking suwayomi.tachidesk.MainKt.main");
    let args = [jvalue { l: argv }];
    ((**env).CallStaticVoidMethodA.unwrap())(env, main_cls, mid, args.as_ptr());
    if let Some(check) = (**env).ExceptionCheck {
      if check(env) == JNI_TRUE {
        describe_exception(env);
        return Err("Suwayomi main threw".into());
      }
    }
  }

  // main() returned; the engine's non-daemon server threads keep the VM alive.
  // DestroyJavaVM parks this thread until they exit — effectively for the app's
  // lifetime. That is intentional (this thread owns the VM).
  log::info!(target: "suwayomi", "engine main returned; parking VM thread in DestroyJavaVM");
  unsafe {
    ((**vm).DestroyJavaVM.unwrap())(vm);
  }
  log::warn!(target: "suwayomi", "in-process JVM destroyed (all non-daemon threads exited)");
  Ok(())
}

/// Resolve the bundled jar via tauri's resource resolver
/// (=> Komika.app/assets/suwayomi/Suwayomi-Server.jar on iOS).
fn resolve_jar(app: &AppHandle) -> Result<PathBuf, String> {
  let res = app
    .path()
    .resource_dir()
    .map_err(|e| format!("resolve resource_dir: {e}"))?;
  let cand = res.join("suwayomi").join("Suwayomi-Server.jar");
  if cand.exists() {
    Ok(cand)
  } else {
    Err(format!(
      "Suwayomi-Server.jar not bundled at {} (tauri.ios.conf.json resources)",
      cand.display()
    ))
  }
}

/// Sanity-check the JVM runtime the app bundle must carry at <exe dir>/lib (the
/// java.home the statically-linked VM derives — see module docs). Catching this
/// here yields a clean `degraded` + actionable error instead of a VM abort.
fn check_bundled_runtime() -> Result<(), String> {
  let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
  let app_dir = exe
    .parent()
    .ok_or_else(|| "executable has no parent dir".to_string())?;
  let jimage = app_dir.join("lib").join("lib").join("modules");
  if jimage.exists() {
    Ok(())
  } else {
    Err(format!(
      "bundled JVM runtime missing: {} not found (stage it with \
       scripts/stage-ios-jvm-runtime.sh and rebuild)",
      jimage.display()
    ))
  }
}

/// Initialize and launch the in-process engine. Non-fatal by design: any
/// resolution failure logs, sets `degraded`, and returns — app startup never
/// aborts, and the JS fallback ladder routes to the hosted backend.
pub fn start(app: AppHandle) {
  let sup = match app.try_state::<Arc<SuwayomiSupervisor>>() {
    Some(s) => s.inner().clone(),
    None => {
      log::error!(target: "suwayomi", "supervisor state not managed; skipping start");
      return;
    }
  };

  if sup.boot_attempted.swap(true, Ordering::SeqCst) {
    log::warn!(target: "suwayomi", "in-process engine already booted once; not booting again");
    return;
  }

  let boot = || -> Result<(PathBuf, PathBuf, u16), String> {
    check_bundled_runtime()?;
    let jar = resolve_jar(&app)?;
    let data_dir = app
      .path()
      .app_data_dir()
      .map_err(|e| format!("resolve app_data_dir: {e}"))?
      .join("suwayomi");
    let port = broker_port().map_err(|e| format!("port broker: {e}"))?;
    prepare_data_dir(&data_dir, port)?;
    Ok((jar, data_dir, port))
  };

  let (jar, data_dir, port) = match boot() {
    Ok(v) => v,
    Err(e) => {
      log::warn!(target: "suwayomi", "engine unavailable: {e}");
      sup.transition(EngineState::Degraded, Some(e));
      return;
    }
  };

  log::info!(
    target: "suwayomi",
    "launching in-process engine on 127.0.0.1:{port} (jar: {}, rootDir: {})",
    jar.display(),
    data_dir.display()
  );
  let boot_started = Instant::now();

  // The VM owner thread: creates the JVM, runs Java main, then parks for the
  // app's lifetime. A boot error transitions to degraded immediately (the poll
  // below would otherwise only report the generic timeout).
  let sup_jvm = sup.clone();
  let spawned = std::thread::Builder::new()
    .name("suwayomi-jvm".into())
    .stack_size(JVM_THREAD_STACK)
    .spawn(move || {
      if let Err(e) = run_jvm(&jar, &data_dir) {
        log::error!(target: "suwayomi", "engine boot failed: {e}");
        sup_jvm.transition(EngineState::Degraded, Some(e));
      }
    });
  if let Err(e) = spawned {
    let msg = format!("spawn suwayomi-jvm thread: {e}");
    log::error!(target: "suwayomi", "{msg}");
    sup.transition(EngineState::Degraded, Some(msg));
    return;
  }

  // Readiness gate on the async runtime, mirroring desktop's Status transitions.
  tauri::async_runtime::spawn(async move {
    match poll_ready(&sup.client.clone(), port, READY_TIMEOUT).await {
      Ok(version) => {
        log::info!(
          target: "suwayomi",
          "engine ready ({version}) on port {port} — cold start {} ms",
          boot_started.elapsed().as_millis()
        );
        sup.set_ready(port, version);
      }
      Err(e) => {
        // Don't clobber a more precise error from the JVM thread.
        let already_degraded = sup.status().state == "degraded";
        log::error!(target: "suwayomi", "engine readiness failed: {e}");
        if !already_degraded {
          sup.transition(EngineState::Degraded, Some(e));
        }
      }
    }
  });
}

// ---- JS-facing commands (identical signatures/shapes to desktop) ------------

/// `http://127.0.0.1:<port>` when the engine is ready, else `None`.
#[tauri::command]
pub fn suwayomi_base_url(state: State<'_, Arc<SuwayomiSupervisor>>) -> Option<String> {
  state.base_url()
}

/// `{ state, version, lastError }`.
#[tauri::command]
pub fn suwayomi_status(state: State<'_, Arc<SuwayomiSupervisor>>) -> Status {
  state.status()
}

/// IPC-proxy transport: POST a GraphQL query to the loopback engine and return
/// the parsed JSON (verbatim desktop behavior — HTTP over loopback).
#[tauri::command]
pub async fn suwayomi_gql(
  state: State<'_, Arc<SuwayomiSupervisor>>,
  query: String,
  variables: Option<Value>,
) -> Result<Value, String> {
  let (port, client) = state
    .ready_endpoint()
    .ok_or_else(|| "suwayomi engine not ready".to_string())?;
  let body = match variables {
    Some(vars) => json!({ "query": query, "variables": vars }),
    None => json!({ "query": query }),
  };
  let resp = client
    .post(format!("http://127.0.0.1:{port}/api/graphql"))
    .header(reqwest::header::CONTENT_TYPE, "application/json")
    .body(body.to_string())
    .send()
    .await
    .map_err(|e| e.to_string())?;
  let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
  serde_json::from_slice::<Value>(&bytes).map_err(|e| e.to_string())
}

/// Guard for `suwayomi_image`'s `path` — verbatim desktop's SSRF containment:
/// only rooted `/api/...` engine routes, no traversal, no backslashes.
fn validate_image_path(path: &str) -> Result<(), String> {
  if !path.starts_with("/api/") {
    return Err(format!(
      "suwayomi_image: path must start with /api/, got {path:?}"
    ));
  }
  if path.contains("..") {
    return Err(format!(
      "suwayomi_image: path must not contain '..', got {path:?}"
    ));
  }
  if path.contains('\\') {
    return Err(format!(
      "suwayomi_image: path must not contain '\\', got {path:?}"
    ));
  }
  Ok(())
}

/// IPC-proxy transport for engine image bytes (verbatim desktop behavior).
#[tauri::command]
pub async fn suwayomi_image(
  state: State<'_, Arc<SuwayomiSupervisor>>,
  path: String,
) -> Result<tauri::ipc::Response, String> {
  validate_image_path(&path)?;
  let (port, client) = state
    .ready_endpoint()
    .ok_or_else(|| "suwayomi engine not ready".to_string())?;
  let resp = client
    .get(format!("http://127.0.0.1:{port}{path}"))
    .send()
    .await
    .map_err(|e| e.to_string())?;
  let status = resp.status();
  if !status.is_success() {
    return Err(format!("engine returned {status}"));
  }
  if let Some(len) = resp.content_length() {
    if len > MAX_IMAGE_BYTES as u64 {
      return Err(format!("image too large: {len} bytes"));
    }
  }
  let mut buf: Vec<u8> = Vec::new();
  let mut resp = resp;
  while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
    if buf.len() + chunk.len() > MAX_IMAGE_BYTES {
      return Err("image exceeds size cap".to_string());
    }
    buf.extend_from_slice(&chunk);
  }
  Ok(tauri::ipc::Response::new(buf))
}

// ---- Tests (compile only when testing the ios-device target) ----------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn server_conf_matches_desktop_template() {
    let conf = render_server_conf(54321);
    assert!(conf.contains("server.ip = \"127.0.0.1\""));
    assert!(conf.contains("server.port = 54321"));
    assert!(conf.contains("server.kcefEnabled = false"));
    assert!(conf.contains("server.flareSolverrEnabled = false"));
  }

  #[test]
  fn image_path_guard_matches_desktop() {
    assert!(validate_image_path("/api/v1/manga/3/thumbnail").is_ok());
    for bad in ["", "/foo", "http://evil", "//evil", "/api/../s", "/api/\\x"] {
      assert!(validate_image_path(bad).is_err(), "{bad:?} must be rejected");
    }
  }
}
