/// Fetch a source image's raw bytes directly from its CDN.
///
/// This is the native half of Komika's image pipeline: desktop and mobile builds
/// scrape/fetch images themselves and never touch the Cloudflare Worker proxy
/// (which exists for the CORS-bound web build). Returning `tauri::ipc::Response`
/// sends the bytes over IPC efficiently and resolves to an `ArrayBuffer` in JS.
#[tauri::command]
async fn fetch_image(url: String) -> Result<tauri::ipc::Response, String> {
  let resp = reqwest::get(&url).await.map_err(|e| e.to_string())?;
  if !resp.status().is_success() {
    return Err(format!("upstream returned {}", resp.status()));
  }
  let bytes = resp.bytes().await.map_err(|e| e.to_string())?;
  Ok(tauri::ipc::Response::new(bytes.to_vec()))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .setup(|app| {
      if cfg!(debug_assertions) {
        app.handle().plugin(
          tauri_plugin_log::Builder::default()
            .level(log::LevelFilter::Info)
            .build(),
        )?;
      }
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![fetch_image])
    .run(tauri::generate_context!())
    .expect("error while running tauri application");
}
