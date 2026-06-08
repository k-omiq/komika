use std::net::IpAddr;
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::Duration;

// The embedded engine has three per-platform implementations under the SAME
// `suwayomi` module name, so the rest of `run()` (manage/start/invoke_handler/exit)
// stays platform-agnostic:
//   * desktop            -> suwayomi.rs        (child-process JVM supervisor + the
//                           WebView Cloudflare shim `cloudflare.rs`, both desktop-only)
//   * iOS REAL DEVICE    -> suwayomi_ios.rs    (in-process JNI boot of the statically
//                           linked Zero JVM, N4.2 — device only: the vendored .a files
//                           have no simulator slice, so the sim keeps the stub)
//   * other mobile       -> suwayomi_mobile.rs ("engine unavailable" stubs; Android
//                           and the iOS simulator)
#[cfg(desktop)]
mod cloudflare;
#[cfg(desktop)]
mod suwayomi;
#[cfg(all(target_os = "ios", not(target_abi = "sim")))]
#[path = "suwayomi_ios.rs"]
mod suwayomi;
#[cfg(all(
  not(desktop),
  not(all(target_os = "ios", not(target_abi = "sim")))
))]
#[path = "suwayomi_mobile.rs"]
mod suwayomi;

/// Total budget for a single native image fetch (connect + transfer).
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
/// Hard cap on the decoded body — a missing/lying Content-Length can't OOM us.
const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024; // 32 MiB
/// Redirect hops we'll follow manually (each one re-validated).
const MAX_REDIRECTS: usize = 5;
/// Max concurrent native image fetches. Bounds peak native heap at roughly
/// N × MAX_IMAGE_BYTES so a burst of page prefetches (or a malicious source
/// serving large images) can't OOM/jetsam the app — critical on iOS, where the
/// in-process JVM already runs under `-Xmx256m` in a jetsam-limited process.
const MAX_CONCURRENT_IMAGE_FETCHES: usize = 6;

/// Global permit pool gating every native image command (`fetch_image` here and
/// the loopback `suwayomi_image` proxies in the `suwayomi` module). Shared so the
/// cap is a true process-wide bound on concurrent 32 MiB buffers, not per-command.
pub(crate) fn image_fetch_semaphore() -> &'static tokio::sync::Semaphore {
  static SEM: OnceLock<tokio::sync::Semaphore> = OnceLock::new();
  SEM.get_or_init(|| tokio::sync::Semaphore::new(MAX_CONCURRENT_IMAGE_FETCHES))
}

/// Base config for a native image-fetch client: a request timeout and NO automatic
/// redirect following — we follow manually so every hop's host is re-validated
/// (a 3xx to `http://169.254.169.254/…` must not slip past the host guard).
///
/// Callers add a per-host `.resolve(host, addr)` pin before `.build()`, so we hand
/// back a fresh builder rather than a shared client: the connection then dials
/// exactly the IP that passed `is_blocked_ip`, closing the DNS-rebinding TOCTOU
/// where reqwest would otherwise re-resolve to a second, unchecked address.
fn image_client_builder() -> reqwest::ClientBuilder {
  reqwest::Client::builder()
    .timeout(FETCH_TIMEOUT)
    .redirect(reqwest::redirect::Policy::none())
    // Some CDNs (MangaDex among them) gate on a UA/Referer and 403 requests
    // without them; the Worker sets both, so the native path must match to avoid
    // a web/native divergence on exactly those hosts (I4).
    .user_agent("Komika/0.1 (+https://komika.app)")
}

/// Same-origin Referer for the target (mirrors the Worker's `scheme://host[:port]/`).
fn referer_for(url: &reqwest::Url) -> String {
  let mut r = format!("{}://{}", url.scheme(), url.host_str().unwrap_or(""));
  if let Some(port) = url.port() {
    r.push_str(&format!(":{port}"));
  }
  r.push('/');
  r
}

/// Fetch a source image's raw bytes directly from its CDN.
///
/// This is the native half of Komika's image pipeline: desktop and mobile builds
/// scrape/fetch images themselves and never touch the Cloudflare Worker proxy
/// (which exists for the CORS-bound web build). Returning `tauri::ipc::Response`
/// sends the bytes over IPC efficiently and resolves to an `ArrayBuffer` in JS.
///
/// The `url` comes from third-party source data, so the fetch is hardened against
/// client-side SSRF (I2): http(s) only, every hop's host must resolve to a public
/// address (no loopback/LAN/link-local/metadata targets), redirects are followed
/// manually with re-validation, and the body is bounded by a timeout + size cap.
#[tauri::command]
async fn fetch_image(url: String) -> Result<tauri::ipc::Response, String> {
  // Gate concurrency: one permit, held across the whole redirect chain, so the
  // in-flight 32 MiB buffers can't stack up past MAX_CONCURRENT_IMAGE_FETCHES.
  let _permit = image_fetch_semaphore()
    .acquire()
    .await
    .map_err(|_| "image fetch pool closed".to_string())?;
  let mut next = url;
  for _ in 0..=MAX_REDIRECTS {
    let (target, addr) = validate_image_url(&next).await?;
    // Pin the connection to the validated IP. Without this, reqwest resolves DNS
    // a SECOND time at connect, so a short-TTL/round-robin rebind could swap in a
    // blocked address (LAN / 169.254.169.254) after the check passed. `.resolve`
    // dials exactly the vetted IP while host/SNI/cert stay the original hostname.
    let host = target.host_str().unwrap_or("").to_string();
    let client = image_client_builder()
      .resolve(&host, addr)
      .build()
      .map_err(|e| e.to_string())?;
    let resp = client
      .get(target.clone())
      .header(reqwest::header::REFERER, referer_for(&target))
      .send()
      .await
      .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_redirection() {
      let location = resp
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .ok_or_else(|| "redirect without a Location header".to_string())?;
      // Resolve relative redirects against the current URL; the next loop turn
      // re-runs the host guard on the resolved target.
      next = target
        .join(location)
        .map_err(|e| format!("invalid redirect target: {e}"))?
        .to_string();
      continue;
    }
    if !status.is_success() {
      return Err(format!("upstream returned {status}"));
    }
    return read_capped(resp).await.map(tauri::ipc::Response::new);
  }
  Err("too many redirects".to_string())
}

/// Parse `raw`, require http(s), and reject any host that resolves to a
/// non-public address — the pre-fetch SSRF guard. Returns the parsed URL together
/// with the vetted `SocketAddr` the caller must pin the connection to (so reqwest
/// cannot re-resolve to a different, unchecked address).
async fn validate_image_url(raw: &str) -> Result<(reqwest::Url, std::net::SocketAddr), String> {
  let url = reqwest::Url::parse(raw).map_err(|e| format!("invalid url: {e}"))?;
  match url.scheme() {
    "http" | "https" => {}
    other => return Err(format!("unsupported scheme: {other}")),
  }
  let host = url.host_str().ok_or_else(|| "url has no host".to_string())?;
  let port = url.port_or_known_default().unwrap_or(80);
  // `lookup_host` handles both IP literals (no DNS) and names; require EVERY
  // resolved address to be public so a name pointing at the LAN is blocked too.
  let addrs: Vec<_> = tokio::net::lookup_host((host, port))
    .await
    .map_err(|e| format!("dns lookup failed: {e}"))?
    .collect();
  if addrs.is_empty() {
    return Err("host did not resolve".to_string());
  }
  if let Some(addr) = addrs.iter().find(|a| is_blocked_ip(&a.ip())) {
    return Err(format!("host resolves to a non-public address: {}", addr.ip()));
  }
  // Every resolved address passed the guard; pin the connection to the first so
  // the dialed IP is exactly one we validated (defeats the DNS-rebinding TOCTOU).
  Ok((url, addrs[0]))
}

/// True for addresses a source image must never point at: loopback, private,
/// link-local (incl. the cloud metadata endpoint 169.254.169.254), CGNAT,
/// unspecified, broadcast, and their IPv6 equivalents / v4-mapped forms.
fn is_blocked_ip(ip: &IpAddr) -> bool {
  match ip {
    IpAddr::V4(v4) => {
      let o = v4.octets();
      v4.is_loopback()
        || v4.is_private()
        || v4.is_link_local()
        || v4.is_broadcast()
        || v4.is_documentation()
        || v4.is_unspecified()
        || o[0] == 0
        // Carrier-grade NAT 100.64.0.0/10.
        || (o[0] == 100 && (o[1] & 0xc0) == 0x40)
        // Class E / reserved 240.0.0.0/4 (255.255.255.255 is is_broadcast).
        || o[0] >= 240
        // Benchmarking 198.18.0.0/15.
        || (o[0] == 198 && (o[1] & 0xfe) == 18)
        // IETF protocol assignments 192.0.0.0/24.
        || (o[0] == 192 && o[1] == 0 && o[2] == 0)
    }
    IpAddr::V6(v6) => {
      if let Some(mapped) = v6.to_ipv4_mapped() {
        return is_blocked_ip(&IpAddr::V4(mapped));
      }
      let seg = v6.segments();
      // Deprecated IPv4-compatible address ::a.b.c.d (::/96), excluding :: and
      // ::1 (handled below): fold to the embedded v4 so ::169.254.169.254 etc.
      // can't tunnel past the guard the way only ::ffff: forms did before.
      if seg[..6].iter().all(|&s| s == 0) && !(seg[6] == 0 && seg[7] <= 1) {
        let v4 = std::net::Ipv4Addr::new(
          (seg[6] >> 8) as u8,
          seg[6] as u8,
          (seg[7] >> 8) as u8,
          seg[7] as u8,
        );
        return is_blocked_ip(&IpAddr::V4(v4));
      }
      v6.is_loopback()
        || v6.is_unspecified()
        // Unique-local fc00::/7.
        || (seg[0] & 0xfe00) == 0xfc00
        // Link-local fe80::/10.
        || (seg[0] & 0xffc0) == 0xfe80
        // Documentation 2001:db8::/32.
        || (seg[0] == 0x2001 && seg[1] == 0x0db8)
    }
  }
}

/// Read the body, enforcing `MAX_IMAGE_BYTES` from Content-Length (fail fast) and
/// while streaming (guards against a missing/understated header).
async fn read_capped(mut resp: reqwest::Response) -> Result<Vec<u8>, String> {
  if let Some(len) = resp.content_length() {
    if len > MAX_IMAGE_BYTES as u64 {
      return Err(format!("image too large: {len} bytes"));
    }
  }
  let mut buf: Vec<u8> = Vec::new();
  while let Some(chunk) = resp.chunk().await.map_err(|e| e.to_string())? {
    if buf.len() + chunk.len() > MAX_IMAGE_BYTES {
      return Err("image exceeds size cap".to_string());
    }
    buf.extend_from_slice(&chunk);
  }
  Ok(buf)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  let app = tauri::Builder::default()
    .setup(|app| {
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }
      // Embedded Suwayomi sidecar (N1.1). On desktop this starts the JVM supervisor; a
      // start failure only logs + sets a degraded state and never aborts app startup. The
      // engine binds loopback only and JS reaches it via the `suwayomi_gql` IPC proxy —
      // the JS-side flag gates USE, and an unused engine is harmless. On mobile
      // (`suwayomi_mobile.rs`) both `new()` and `start()` are inert no-ops: the engine
      // reports "unavailable" and JS routes content to the hosted backend.
      use tauri::Manager;
      app.manage(Arc::new(suwayomi::SuwayomiSupervisor::new()));
      suwayomi::start(app.handle().clone());
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      fetch_image,
      suwayomi::suwayomi_gql,
      suwayomi::suwayomi_image,
      suwayomi::suwayomi_status,
      suwayomi::suwayomi_base_url
    ])
    .build(tauri::generate_context!())
    .expect("error while building tauri application");

  // Graceful shutdown: on exit, terminate the JVM child (Drop's kill_on_drop is a
  // backstop). block_on is safe here — the run callback is on the main thread, not a
  // tokio worker.
  app.run(|handle, event| {
    if let tauri::RunEvent::Exit = event {
      use tauri::Manager;
      if let Some(sup) = handle.try_state::<Arc<suwayomi::SuwayomiSupervisor>>() {
        let sup = sup.inner().clone();
        tauri::async_runtime::block_on(async move { sup.stop_and_wait().await });
      }
    }
  });
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn blocks_non_public_ipv4() {
    for ip in [
      "127.0.0.1",
      "10.0.0.5",
      "172.16.0.1",
      "192.168.1.1",
      "169.254.169.254", // cloud metadata
      "100.64.0.1",      // CGNAT
      "0.0.0.0",
      "255.255.255.255",
      "240.0.0.1",       // class E / reserved 240.0.0.0/4
      "198.18.0.1",      // benchmarking 198.18.0.0/15
      "198.19.255.255",  // benchmarking (upper half)
      "192.0.0.1",       // IETF protocol assignments 192.0.0.0/24
    ] {
      assert!(is_blocked_ip(&ip.parse().unwrap()), "{ip} must be blocked");
    }
  }

  #[test]
  fn allows_public_ipv4() {
    for ip in ["1.1.1.1", "8.8.8.8", "104.16.0.1"] {
      assert!(!is_blocked_ip(&ip.parse().unwrap()), "{ip} must be allowed");
    }
  }

  #[test]
  fn blocks_non_public_ipv6() {
    for ip in [
      "::1",
      "::",
      "fe80::1",
      "fc00::1",
      "::ffff:127.0.0.1",
      "2001:db8::1",     // documentation 2001:db8::/32
      "::192.168.1.1",   // deprecated IPv4-compatible folding to a private v4
      "::169.254.169.254", // IPv4-compatible folding to link-local metadata
    ] {
      assert!(is_blocked_ip(&ip.parse().unwrap()), "{ip} must be blocked");
    }
  }

  #[test]
  fn allows_public_ipv6() {
    assert!(!is_blocked_ip(&"2606:4700:4700::1111".parse().unwrap()));
  }

  #[test]
  fn referer_is_origin_with_trailing_slash() {
    let u = reqwest::Url::parse("https://uploads.mangadex.org/covers/x/y.jpg").unwrap();
    assert_eq!(referer_for(&u), "https://uploads.mangadex.org/");
    let p = reqwest::Url::parse("http://example.com:8080/a/b").unwrap();
    assert_eq!(referer_for(&p), "http://example.com:8080/");
  }

  #[tokio::test]
  async fn rejects_non_http_schemes() {
    assert!(validate_image_url("file:///etc/passwd").await.is_err());
    assert!(validate_image_url("ftp://example.com/x.png").await.is_err());
    assert!(validate_image_url("not a url").await.is_err());
  }

  #[tokio::test]
  async fn rejects_loopback_and_lan_literals() {
    assert!(validate_image_url("http://127.0.0.1/x.png").await.is_err());
    assert!(validate_image_url("http://[::1]/x.png").await.is_err());
    assert!(validate_image_url("http://192.168.0.1/x.png").await.is_err());
    assert!(validate_image_url("http://169.254.169.254/latest/meta-data")
      .await
      .is_err());
  }
}
