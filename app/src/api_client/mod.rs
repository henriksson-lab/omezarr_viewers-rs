use gloo_net::http::Request;
use omezarr_viewer_common::{DatasetInfo, SessionInfo};
use wasm_bindgen::JsCast;

mod annotations;
mod objects;
mod tiles;

pub use annotations::*;
pub use objects::*;
pub use tiles::*;

thread_local! {
    /// The API base, worked out once from the page's own location.
    ///
    /// Once rather than on every request: "there is a window" is an assumption
    /// worth stating in one place, not thirty.
    static HOST: String = host_url();
}

/// The API base URL: `scheme://host` of the page this was served from.
pub(crate) fn get_host_url() -> String {
    HOST.with(Clone::clone)
}

/// Empty when there is no browser window to ask.
///
/// That cannot happen in the running app, but an empty base leaves every URL
/// relative — which still reaches the same server — rather than aborting.
fn host_url() -> String {
    let Some(window) = web_sys::window() else {
        return String::new();
    };
    let location = window.location();
    match (location.protocol(), location.host()) {
        (Ok(protocol), Ok(host)) => format!("{protocol}//{host}"),
        _ => String::new(),
    }
}

/// Fetch the open session: every layer, in draw order.
pub async fn fetch_session() -> Result<SessionInfo, String> {
    let url = format!("{}/api/session", get_host_url());
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch session: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch session: status {}", resp.status()));
    }
    resp.json::<SessionInfo>()
        .await
        .map_err(|e| format!("parse session: {}", e))
}

/// Fetch the list of available datasets from the server.
pub async fn fetch_datasets() -> Result<Vec<String>, String> {
    let url = format!("{}/api/datasets", get_host_url());
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch datasets: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch datasets: status {}", resp.status()));
    }
    resp.json::<Vec<String>>()
        .await
        .map_err(|e| format!("parse datasets: {}", e))
}

/// Open a dataset by name on the server, replacing the session.
pub async fn open_dataset(name: &str) -> Result<DatasetInfo, String> {
    let url = format!("{}/api/open?dataset={}", get_host_url(), name);
    let resp = Request::post(&url)
        .send()
        .await
        .map_err(|e| format!("open dataset: {}", e))?;
    if !resp.ok() {
        return Err(format!("open dataset: status {}", resp.status()));
    }
    resp.json::<DatasetInfo>()
        .await
        .map_err(|e| format!("parse dataset info: {}", e))
}

/// Add a layer from a source URI, returning the new session.
pub async fn add_layer(source: &str, role: Option<&str>) -> Result<SessionInfo, String> {
    let url = format!("{}/api/layers", get_host_url());
    let body = serde_json::json!({ "source": source, "role": role });
    let resp = Request::post(&url)
        .json(&body)
        .map_err(|e| format!("add layer body: {}", e))?
        .send()
        .await
        .map_err(|e| format!("add layer: {}", e))?;
    if !resp.ok() {
        let detail = resp.text().await.unwrap_or_default();
        return Err(format!("add layer: {}", detail));
    }
    resp.json::<SessionInfo>()
        .await
        .map_err(|e| format!("parse session: {}", e))
}

/// Remove a layer by id, returning the new session.
pub async fn remove_layer(id: &str) -> Result<SessionInfo, String> {
    let url = format!("{}/api/layers/{}", get_host_url(), id);
    let resp = Request::delete(&url)
        .send()
        .await
        .map_err(|e| format!("remove layer: {}", e))?;
    if !resp.ok() {
        return Err(format!("remove layer: status {}", resp.status()));
    }
    resp.json::<SessionInfo>()
        .await
        .map_err(|e| format!("parse session: {}", e))
}

/// Fetch the session as a project file and hand it to the browser to save.
///
/// The server returns JSON; turning it into a file is the browser's job, and
/// doing it here keeps the server from writing anywhere on a click.
pub async fn download_project() -> Result<(), String> {
    let url = format!("{}/api/project", get_host_url());
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch project: {}", e))?;
    let text = resp
        .text()
        .await
        .map_err(|e| format!("read project: {}", e))?;

    let document = web_sys::window()
        .ok_or("no window")?
        .document()
        .ok_or("no document")?;
    let parts = js_sys::Array::new();
    parts.push(&wasm_bindgen::JsValue::from_str(&text));
    let blob =
        web_sys::Blob::new_with_str_sequence(&parts).map_err(|e| format!("blob: {:?}", e))?;
    let href = web_sys::Url::create_object_url_with_blob(&blob)
        .map_err(|e| format!("object url: {:?}", e))?;

    let anchor = document
        .create_element("a")
        .map_err(|e| format!("anchor: {:?}", e))?
        .dyn_into::<web_sys::HtmlAnchorElement>()
        .map_err(|_| "not an anchor")?;
    anchor.set_href(&href);
    anchor.set_download("view.json");
    anchor.click();
    let _ = web_sys::Url::revoke_object_url(&href);
    Ok(())
}

/// Is the viewer running inside the desktop shell?
///
/// The desktop build exposes Tauri's IPC on the page; the browser build does
/// not. Nothing else in the frontend differs, so this is the only place that
/// asks.
pub fn is_desktop() -> bool {
    web_sys::window()
        .and_then(|window| js_sys::Reflect::get(&window, &"__TAURI__".into()).ok())
        .map(|tauri| !tauri.is_undefined() && !tauri.is_null())
        .unwrap_or(false)
}

/// Ask the desktop shell for a path — `pick_folder` or `pick_file`.
///
/// Returns `None` when the dialog was cancelled, or when there is no shell to
/// ask, which is what the browser build always gets.
pub async fn pick_path(command: &str) -> Option<String> {
    let window = web_sys::window()?;
    let tauri = js_sys::Reflect::get(&window, &"__TAURI__".into()).ok()?;
    let core = js_sys::Reflect::get(&tauri, &"core".into()).ok()?;
    let invoke = js_sys::Reflect::get(&core, &"invoke".into())
        .ok()?
        .dyn_into::<js_sys::Function>()
        .ok()?;
    let promise = invoke
        .call1(&core, &wasm_bindgen::JsValue::from_str(command))
        .ok()?
        .dyn_into::<js_sys::Promise>()
        .ok()?;
    let value = wasm_bindgen_futures::JsFuture::from(promise).await.ok()?;
    value.as_string()
}

/// One region's share of an object layer.
#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
pub struct RegionCount {
    pub id: u64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub acronym: Option<String>,
    pub count: u64,
}

/// How many objects fall in each region of a label layer.
pub async fn fetch_regions(
    labels: &str,
    objects: &str,
    limit: usize,
) -> Result<Vec<RegionCount>, String> {
    let url = format!(
        "{}/api/regions?labels={}&objects={}&limit={}",
        get_host_url(),
        labels,
        objects,
        limit
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch regions: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch regions: status {}", resp.status()));
    }
    resp.json::<Vec<RegionCount>>()
        .await
        .map_err(|e| format!("parse regions: {}", e))
}

// ---------------------------------------------------------------------------
// Annotations — the only part of this client that writes.
// ---------------------------------------------------------------------------
