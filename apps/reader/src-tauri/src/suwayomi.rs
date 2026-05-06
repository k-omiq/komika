//! Embedded Suwayomi-Server sidecar supervisor (native desktop plan §3.2/§3.3, item N1.1).
//!
//! This module owns the lifecycle of an embedded Suwayomi-Server JVM child process:
//! it brokers a loopback port, writes the authoritative `server.conf`, launches the
//! jar headless, gates on a GraphQL readiness probe, supervises crashes with capped
//! backoff, and exposes three `#[tauri::command]`s to the JS side.
//!
//! Security posture (deliberate, and different from `fetch_image`'s SSRF guard):
//! the engine binds **127.0.0.1 only**, and JS never fetches the loopback port
//! directly — it calls `suwayomi_gql`, which proxies over IPC. Loopback is therefore
//! *allowed* here (the opposite of the image path), so this module uses its own plain
//! `reqwest::Client`, NOT `image_client()`.
//!
//! Nothing here runs unless the app spawns it; it does not touch `fetch_image` or any
//! existing behavior. The boot recipe follows `suwayomi/SPIKE-FINDINGS.md` exactly
//! (pin: Suwayomi-Server v2.3.2243).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Manager, State};
use tokio::io::AsyncBufReadExt;

/// Cold-start budget for the readiness gate (spike: ~5 s warm; recipe allows ~30 s).
const READY_TIMEOUT: Duration = Duration::from_secs(30);
/// How often to poll `aboutServer` while waiting for readiness.
const POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Per-request timeout for the readiness poll and the `suwayomi_gql` proxy.
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap on an engine image body. Kept in lockstep with lib.rs's
/// `MAX_IMAGE_BYTES` (32 MiB) so `suwayomi_image` and `fetch_image` bound bytes
/// identically; `read_capped` lives in lib.rs and is private, so we mirror its
/// value + streaming logic here rather than reach across modules.
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024; // 32 MiB
/// Restart-storm cap: more than this many restarts inside 60 s trips a long backoff.
const MAX_RESTARTS_PER_MIN: usize = 5;
/// Grace period to let a killed JVM reap before we force it again.
const GRACE: Duration = Duration::from_secs(3);

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

/// JSON shape returned by `suwayomi_status`.
#[derive(Serialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Status {
  pub state: String,
  pub version: Option<String>,
  pub last_error: Option<String>,
}

/// Resolved launch inputs. Kept AppHandle-free so the boot path is unit/integration
/// testable without a running Tauri app.
#[derive(Clone, Debug)]
struct EngineConfig {
  java: PathBuf,
  jar: PathBuf,
  data_dir: PathBuf,
}

/// Mutable state behind a single lock. The child process itself lives inside the
/// supervision task (so it can be `select!`ed on `.wait()`); it is NOT stored here.
struct Inner {
  port: Option<u16>,
  state: EngineState,
  version: Option<String>,
  last_error: Option<String>,
}

/// Best-effort single-instance lockfile guard; removes the file on drop.
struct LockGuard {
  path: PathBuf,
}

impl Drop for LockGuard {
  fn drop(&mut self) {
    let _ = std::fs::remove_file(&self.path);
  }
}

/// Owns the embedded engine's shared state, HTTP client, and shutdown signal.
pub struct SuwayomiSupervisor {
  inner: Mutex<Inner>,
  client: reqwest::Client,
  /// Woken to interrupt boot/backoff/monitor and stop the supervision loop.
  notify: tokio::sync::Notify,
  /// Set once shutdown has been requested; the loop checks it and exits.
  stopping: AtomicBool,
  /// Held for the app's lifetime; dropping removes the lockfile.
  lock: Mutex<Option<LockGuard>>,
}

impl SuwayomiSupervisor {
  pub fn new() -> Self {
    let client = reqwest::Client::builder()
      .timeout(HTTP_TIMEOUT)
      // No redirect policy / SSRF host guard here on purpose: loopback is allowed.
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
      notify: tokio::sync::Notify::new(),
      stopping: AtomicBool::new(false),
      lock: Mutex::new(None),
    }
  }

  fn set_ready(&self, port: u16, version: String) {
    let mut g = self.inner.lock().unwrap();
    g.port = Some(port);
    g.state = EngineState::Ready;
    g.version = Some(version);
    g.last_error = None;
  }

  /// Move to a non-ready state, clearing the port (base_url must be None off-ready).
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

  /// The `(port, client)` needed to proxy a GraphQL call, or `None` if not ready.
  fn ready_endpoint(&self) -> Option<(u16, reqwest::Client)> {
    let g = self.inner.lock().unwrap();
    if g.state == EngineState::Ready {
      g.port.map(|p| (p, self.client.clone()))
    } else {
      None
    }
  }

  /// Signal shutdown and wait (bounded) for the supervision loop to report `Stopped`.
  /// Safe to call from the Tauri run-loop thread (not a tokio worker).
  pub async fn stop_and_wait(&self) {
    self.stopping.store(true, Ordering::SeqCst);
    self.notify.notify_waiters();
    let deadline = Instant::now() + Duration::from_secs(6);
    while Instant::now() < deadline {
      if self.inner.lock().unwrap().state == EngineState::Stopped {
        break;
      }
      tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // Backstop: reflect Stopped even if no task was running (e.g. lock refused).
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

/// Render the authoritative HOCON `server.conf`. The persisted conf is authoritative
/// (JVM `-Dserver.*` sysprops do NOT reliably override Suwayomi's reactive config), so
/// this is rewritten before every launch. `kcefEnabled = false` is CRITICAL (else the
/// server downloads 226 MB of Chromium and SIGTRAPs headless).
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

/// Write `server.conf` into the data dir (creating it), and best-effort remove any
/// previously downloaded kcef payload so a stale copy can't be re-used.
fn prepare_data_dir(data_dir: &Path, port: u16) -> Result<(), String> {
  std::fs::create_dir_all(data_dir).map_err(|e| format!("create data dir: {e}"))?;
  std::fs::write(data_dir.join("server.conf"), render_server_conf(port))
    .map_err(|e| format!("write server.conf: {e}"))?;
  let kcef = data_dir.join("bin").join("kcef");
  let _ = std::fs::remove_dir_all(&kcef);
  let _ = std::fs::remove_file(&kcef);
  Ok(())
}

/// Spawn the JVM per the proven launch line, piping stdout/stderr into the `suwayomi`
/// log target. Port is delivered via `server.conf`, not argv.
fn spawn_engine(cfg: &EngineConfig) -> Result<tokio::process::Child, String> {
  let mut cmd = tokio::process::Command::new(&cfg.java);
  cmd
    .arg("-Djava.awt.headless=true")
    .arg("-Xmx512m")
    .arg(format!(
      "-Dsuwayomi.tachidesk.config.server.rootDir={}",
      cfg.data_dir.display()
    ))
    .arg("-jar")
    .arg(&cfg.jar)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    // Backstop: if this Child is ever dropped (task cancelled / runtime torn down),
    // the OS kills the JVM so we never orphan it.
    .kill_on_drop(true);

  let mut child = cmd
    .spawn()
    .map_err(|e| format!("spawn java ({}) failed: {e}", cfg.java.display()))?;

  if let Some(out) = child.stdout.take() {
    tokio::spawn(async move {
      let mut lines = tokio::io::BufReader::new(out).lines();
      while let Ok(Some(line)) = lines.next_line().await {
        log::info!(target: "suwayomi", "{line}");
      }
    });
  }
  if let Some(err) = child.stderr.take() {
    tokio::spawn(async move {
      let mut lines = tokio::io::BufReader::new(err).lines();
      while let Ok(Some(line)) = lines.next_line().await {
        log::warn!(target: "suwayomi", "{line}");
      }
    });
  }
  Ok(child)
}

/// Poll `POST /api/graphql { aboutServer { version } }` until a version comes back or
/// the timeout elapses.
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

/// Full core boot path: broker port, write conf, spawn, poll readiness. On readiness
/// failure the just-spawned child is killed before returning the error (no orphan).
/// Returns the live child, the port, and the reported version.
async fn boot_engine(
  cfg: &EngineConfig,
  client: &reqwest::Client,
  ready_timeout: Duration,
) -> Result<(tokio::process::Child, u16, String), String> {
  let port = broker_port().map_err(|e| format!("port broker: {e}"))?;
  prepare_data_dir(&cfg.data_dir, port)?;
  log::info!(target: "suwayomi", "launching engine on 127.0.0.1:{port}");
  let mut child = spawn_engine(cfg)?;
  match poll_ready(client, port, ready_timeout).await {
    Ok(version) => Ok((child, port, version)),
    Err(e) => {
      let _ = child.start_kill();
      let _ = child.wait().await;
      Err(e)
    }
  }
}

/// Terminate a live child: SIGKILL (spike-proven to leave no orphan), confirm within a
/// grace window, and force again if it somehow lingers.
async fn stop_child(child: &mut tokio::process::Child) {
  let _ = child.start_kill();
  if tokio::time::timeout(GRACE, child.wait()).await.is_err() {
    let _ = child.start_kill();
    let _ = child.wait().await;
  }
}

/// The supervision loop: (re)boot the engine, monitor for unexpected exit, restart with
/// capped backoff, and exit cleanly on shutdown. Runs on the Tauri async runtime.
async fn run_supervisor(sup: Arc<SuwayomiSupervisor>, cfg: EngineConfig) {
  let mut restarts: VecDeque<Instant> = VecDeque::new();
  while !sup.stopping.load(Ordering::SeqCst) {
    sup.transition(EngineState::Starting, None);

    // Prefer a shutdown signal over completing a boot. If notify fires mid-boot the
    // boot future is dropped, and kill_on_drop reaps any child it had spawned.
    let boot = tokio::select! {
      biased;
      _ = sup.notify.notified() => break,
      r = boot_engine(&cfg, &sup.client, READY_TIMEOUT) => r,
    };

    match boot {
      Ok((mut child, port, version)) => {
        log::info!(target: "suwayomi", "engine ready ({version}) on port {port}");
        sup.set_ready(port, version);
        tokio::select! {
          _ = sup.notify.notified() => {
            stop_child(&mut child).await;
            break;
          }
          status = child.wait() => {
            log::warn!(target: "suwayomi", "engine exited unexpectedly: {status:?}");
            sup.transition(EngineState::Degraded, Some("engine exited unexpectedly".into()));
          }
        }
      }
      Err(e) => {
        log::error!(target: "suwayomi", "engine boot failed: {e}");
        sup.transition(EngineState::Degraded, Some(e));
      }
    }

    if sup.stopping.load(Ordering::SeqCst) {
      break;
    }

    // Restart accounting: count restarts inside the trailing 60 s window.
    let now = Instant::now();
    restarts.push_back(now);
    while matches!(restarts.front(), Some(&t) if now.duration_since(t) > Duration::from_secs(60)) {
      restarts.pop_front();
    }
    let storm = restarts.len() > MAX_RESTARTS_PER_MIN;
    let backoff = if storm {
      sup.transition(
        EngineState::Degraded,
        Some("restart budget exceeded; backing off".into()),
      );
      Duration::from_secs(60)
    } else {
      // 2s, 4s, 6s ... capped at 30s.
      Duration::from_secs((2 * restarts.len() as u64).min(30))
    };

    tokio::select! {
      _ = sup.notify.notified() => break,
      _ = tokio::time::sleep(backoff) => {}
    }
    if storm {
      restarts.clear();
    }
  }
  sup.transition(EngineState::Stopped, None);
  log::info!(target: "suwayomi", "supervisor stopped");
}

/// On Windows the JVM launcher is `java.exe`.
fn java_exe() -> &'static str {
  if cfg!(windows) {
    "java.exe"
  } else {
    "java"
  }
}

/// Best-effort bundle sub-path for a bundled JRE. N1.2 finalizes the real resource
/// layout; until then this is only probed (absent in dev), so the exact slug is not
/// load-bearing.
fn target_slug() -> String {
  format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

/// Resolve the `java` binary: env override first (dev/test), then a bundled JRE.
fn resolve_java(app: &AppHandle) -> Result<PathBuf, String> {
  if let Ok(p) = std::env::var("KOMIKA_JAVA_BIN") {
    if !p.is_empty() {
      return Ok(PathBuf::from(p));
    }
  }
  if let Ok(res) = app.path().resource_dir() {
    let cand = res
      .join("jre")
      .join(target_slug())
      .join("bin")
      .join(java_exe());
    if cand.exists() {
      return Ok(cand);
    }
  }
  Err("no java runtime found: set KOMIKA_JAVA_BIN or bundle a JRE".into())
}

/// Resolve the Suwayomi-Server jar: env override first (dev/test), then bundled resource.
fn resolve_jar(app: &AppHandle) -> Result<PathBuf, String> {
  if let Ok(p) = std::env::var("KOMIKA_SUWAYOMI_JAR") {
    if !p.is_empty() {
      return Ok(PathBuf::from(p));
    }
  }
  if let Ok(res) = app.path().resource_dir() {
    let cand = res.join("suwayomi").join("Suwayomi-Server.jar");
    if cand.exists() {
      return Ok(cand);
    }
  }
  Err("no Suwayomi-Server.jar found: set KOMIKA_SUWAYOMI_JAR or bundle the jar".into())
}

fn resolve_config(app: &AppHandle) -> Result<EngineConfig, String> {
  let data_dir = app
    .path()
    .app_data_dir()
    .map_err(|e| format!("resolve app_data_dir: {e}"))?
    .join("suwayomi");
  let java = resolve_java(app)?;
  let jar = resolve_jar(app)?;
  Ok(EngineConfig {
    java,
    jar,
    data_dir,
  })
}

/// Best-effort single-instance lock: a lockfile in the data dir keeps a second app
/// instance from double-spawning the engine. PARTIAL: a hard crash leaves a stale
/// lockfile that must be cleared manually (we cannot verify the holder's liveness
/// without a platform syscall / extra crate). Cleared on clean shutdown via Drop.
fn acquire_lock(data_dir: &Path) -> Result<LockGuard, String> {
  std::fs::create_dir_all(data_dir).map_err(|e| format!("create data dir: {e}"))?;
  let path = data_dir.join("komika-suwayomi.lock");
  match std::fs::OpenOptions::new()
    .write(true)
    .create_new(true)
    .open(&path)
  {
    Ok(mut f) => {
      use std::io::Write;
      let _ = write!(f, "{}", std::process::id());
      Ok(LockGuard { path })
    }
    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
      Err("another Komika instance holds the engine lock".into())
    }
    Err(e) => Err(format!("acquire engine lock: {e}")),
  }
}

/// Initialize and launch the supervisor. Non-fatal by design: a resolution or lock
/// failure logs, sets `Degraded`, and returns — it never aborts app startup. The
/// supervisor is always started; the JS-side flag gates *use*, and an unused engine is
/// harmless (nothing calls it).
pub fn start(app: AppHandle) {
  let sup = match app.try_state::<Arc<SuwayomiSupervisor>>() {
    Some(s) => s.inner().clone(),
    None => {
      log::error!(target: "suwayomi", "supervisor state not managed; skipping start");
      return;
    }
  };

  let cfg = match resolve_config(&app) {
    Ok(c) => c,
    Err(e) => {
      log::warn!(target: "suwayomi", "engine unavailable: {e}");
      sup.transition(EngineState::Degraded, Some(e));
      return;
    }
  };

  match acquire_lock(&cfg.data_dir) {
    Ok(guard) => *sup.lock.lock().unwrap() = Some(guard),
    Err(e) => {
      log::warn!(target: "suwayomi", "not starting engine: {e}");
      sup.transition(EngineState::Degraded, Some(e));
      return;
    }
  }

  tauri::async_runtime::spawn(run_supervisor(sup, cfg));
}

// ---- JS-facing commands ----------------------------------------------------

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

/// IPC-proxy transport: POST a GraphQL query to the loopback engine and return the
/// parsed JSON. This is the transport the JS `LocalSuwayomiBackend` calls by name — the
/// port stays hidden and CSP stays tight (`connect-src 'self' ipc:`).
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

/// Guard for `suwayomi_image`'s `path`: our narrow, deliberate loopback exemption
/// (plan §13). The engine's image/proxy routes all live under `/api/`, so we require
/// `path` to be exactly that shape — this is the SSRF containment that keeps the
/// command from ever being turned into an arbitrary-URL fetch on 127.0.0.1. Reject
/// anything that isn't a rooted `/api/...` path, and reject any `..` traversal outright.
fn validate_image_path(path: &str) -> Result<(), String> {
  // Must be a rooted path beginning with `/api/`. This rejects empty strings,
  // absolute URLs (`http://…`), scheme-relative (`//evil`), backslashes, and any
  // other route (`/etc/passwd`, `/foo`). Byte-level `starts_with` is exact — a
  // leading `//` fails because the second char must be `a`.
  if !path.starts_with("/api/") {
    return Err(format!("suwayomi_image: path must start with /api/, got {path:?}"));
  }
  // Belt-and-braces: no path traversal, even though it can't escape `/api/` on the
  // engine — a `/api/../secret` must never be forwarded.
  if path.contains("..") {
    return Err(format!("suwayomi_image: path must not contain '..', got {path:?}"));
  }
  // No backslashes (a Windows-y separator that could confuse downstream parsing).
  if path.contains('\\') {
    return Err(format!("suwayomi_image: path must not contain '\\', got {path:?}"));
  }
  Ok(())
}

/// IPC-proxy transport for engine image bytes: stream a page/proxy image from the
/// LOOPBACK engine's own `/api/` routes over IPC and hand the raw bytes to JS. This is
/// the image counterpart to `suwayomi_gql` — the native `NativeImageProvider` passes a
/// relative Suwayomi path (e.g. `/api/v1/manga/3/chapter/1/page/0`) and gets back an
/// `ArrayBuffer`, so the engine (not the JS side) applies the extension's request
/// context, the port stays hidden, and CSP stays `connect-src 'self' ipc:`.
///
/// Loopback is INTENTIONAL here (the inverse of `fetch_image`, whose host guard blocks
/// 127.0.0.1). The SSRF containment is the `/api/` path guard in `validate_image_path`:
/// this command can only ever hit the engine's own routes, never an arbitrary URL. The
/// body is size-capped exactly like `fetch_image`'s `read_capped` (`MAX_IMAGE_BYTES`).
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
  // Mirror lib.rs's `read_capped`: fail fast on an oversized Content-Length, then
  // enforce the cap again while streaming (a missing/understated header can't OOM us).
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

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn server_conf_has_required_lines_and_kcef_off() {
    let conf = render_server_conf(54321);
    assert!(conf.contains("server.ip = \"127.0.0.1\""));
    assert!(conf.contains("server.port = 54321"));
    assert!(conf.contains("server.webUIEnabled = false"));
    assert!(conf.contains("server.initialOpenInBrowserEnabled = false"));
    assert!(conf.contains("server.systemTrayEnabled = false"));
    // The two lines that keep the JVM from crashing / phoning out.
    assert!(conf.contains("server.kcefEnabled = false"));
    assert!(conf.contains("server.flareSolverrEnabled = false"));
  }

  #[test]
  fn image_path_guard_accepts_engine_api_routes() {
    // The real relative path shape the engine returns (GQL-SCHEMA-FINDINGS.md §C).
    assert!(validate_image_path("/api/v1/manga/3/chapter/1/page/0").is_ok());
    assert!(validate_image_path("/api/v1/manga/3/thumbnail").is_ok());
  }

  #[test]
  fn image_path_guard_rejects_non_api_and_traversal() {
    for bad in [
      "",                        // empty
      "/foo",                    // wrong route
      "/etc/passwd",             // absolute-looking non-api
      "http://evil",             // absolute URL
      "//evil",                  // scheme-relative host
      "/api/../secret",          // traversal out of /api/
      "../x",                    // relative traversal
      "/api/\\windows",          // backslash separator
      "api/v1/manga",            // not rooted
    ] {
      assert!(
        validate_image_path(bad).is_err(),
        "{bad:?} must be rejected"
      );
    }
  }

  #[test]
  fn broker_port_returns_usable_port() {
    let port = broker_port().expect("broker a port");
    assert!(port > 0);
    // Freed by the broker, so we can bind it ourselves right after.
    let again = std::net::TcpListener::bind(("127.0.0.1", port));
    assert!(again.is_ok(), "brokered port should be immediately bindable");
  }

  #[test]
  fn state_transitions_drive_base_url_and_status() {
    let sup = SuwayomiSupervisor::new();
    assert_eq!(sup.status().state, "starting");
    assert!(sup.base_url().is_none());

    sup.set_ready(4567, "v2.3.2243".into());
    assert_eq!(sup.status().state, "ready");
    assert_eq!(sup.status().version.as_deref(), Some("v2.3.2243"));
    assert_eq!(sup.base_url().as_deref(), Some("http://127.0.0.1:4567"));

    sup.transition(EngineState::Degraded, Some("boom".into()));
    assert_eq!(sup.status().state, "degraded");
    assert!(sup.base_url().is_none(), "no base_url when not ready");
    assert_eq!(sup.status().last_error.as_deref(), Some("boom"));

    sup.transition(EngineState::Stopped, None);
    assert_eq!(sup.status().state, "stopped");
    // last_error is sticky across a None-error transition.
    assert_eq!(sup.status().last_error.as_deref(), Some("boom"));
  }

  #[test]
  fn lockfile_is_exclusive_then_released() {
    let dir = std::env::temp_dir().join(format!("komika-lock-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let guard = acquire_lock(&dir).expect("first lock acquires");
    assert!(acquire_lock(&dir).is_err(), "second lock is refused");
    drop(guard);
    assert!(acquire_lock(&dir).is_ok(), "lock re-acquirable after release");
    let _ = std::fs::remove_dir_all(&dir);
  }

  /// Live boot against a REAL Suwayomi-Server jar. Skipped unless BOTH
  /// `KOMIKA_JAVA_BIN` and `KOMIKA_SUWAYOMI_JAR` are set; kept `#[ignore]` so the
  /// normal `cargo test` stays fast and offline. Run with:
  ///   KOMIKA_JAVA_BIN=... KOMIKA_SUWAYOMI_JAR=... cargo test -- --ignored sidecar_boots
  #[tokio::test]
  #[ignore]
  async fn sidecar_boots_and_reports_about_server() {
    let java = match std::env::var("KOMIKA_JAVA_BIN") {
      Ok(v) if !v.is_empty() => v,
      _ => return, // skip: env not set
    };
    let jar = match std::env::var("KOMIKA_SUWAYOMI_JAR") {
      Ok(v) if !v.is_empty() => v,
      _ => return, // skip: env not set
    };

    let data_dir =
      std::env::temp_dir().join(format!("komika-suwayomi-it-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data_dir);
    let cfg = EngineConfig {
      java: PathBuf::from(java),
      jar: PathBuf::from(jar),
      data_dir: data_dir.clone(),
    };
    let client = reqwest::Client::builder()
      .timeout(HTTP_TIMEOUT)
      .build()
      .unwrap();

    // Boot: broker port, write conf, spawn, poll aboutServer.
    let (mut child, port, version) = boot_engine(&cfg, &client, Duration::from_secs(60))
      .await
      .expect("engine should boot and report aboutServer");
    assert!(port > 0);
    assert!(!version.is_empty(), "aboutServer returned a version");

    // The conf we wrote must have kcef disabled (else the JVM would SIGTRAP).
    let conf = std::fs::read_to_string(data_dir.join("server.conf")).unwrap();
    assert!(conf.contains("server.kcefEnabled = false"));

    // Shut down and assert no orphan: the child must have exited.
    stop_child(&mut child).await;
    assert!(
      child.try_wait().unwrap().is_some(),
      "child JVM must be gone after stop_child (no orphan)"
    );

    let _ = std::fs::remove_dir_all(&data_dir);
  }
}
