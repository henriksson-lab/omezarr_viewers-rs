use gloo_net::http::Request;
use omezarr_viewer_common::DatasetInfo;

/// Derive the API base URL from the current browser location.
fn get_host_url() -> String {
    let window = web_sys::window().expect("no window");
    let location = window.location();
    let protocol = location.protocol().expect("no protocol");
    let host = location.host().expect("no host");
    format!("{}//{}", protocol, host)
}

/// Fetch dataset metadata from the server.
pub async fn fetch_info() -> Result<DatasetInfo, String> {
    let url = format!("{}/api/info", get_host_url());
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch info: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch info: status {}", resp.status()));
    }
    resp.json::<DatasetInfo>()
        .await
        .map_err(|e| format!("parse info: {}", e))
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

/// Open a dataset by name on the server, returning its metadata.
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

/// Fetch a rectangular tile region as float32 pixel data.
pub async fn fetch_tile(
    level: usize,
    t: u64,
    c: u64,
    z: u64,
    y: u64,
    x: u64,
    h: u64,
    w: u64,
) -> Result<Vec<f32>, String> {
    let url = format!(
        "{}/api/tile?level={}&t={}&c={}&z={}&y={}&x={}&h={}&w={}",
        get_host_url(),
        level,
        t,
        c,
        z,
        y,
        x,
        h,
        w
    );
    let resp = Request::get(&url)
        .send()
        .await
        .map_err(|e| format!("fetch tile: {}", e))?;
    if !resp.ok() {
        return Err(format!("fetch tile: status {}", resp.status()));
    }

    let bytes = resp
        .binary()
        .await
        .map_err(|e| format!("read tile bytes: {}", e))?;

    // Convert raw bytes back to f32
    let floats: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    Ok(floats)
}
