mod api_client;
mod app;
mod controls;
mod cube_pane;
mod layers;
mod ortho_pane;
mod viewer_canvas;
mod webgl;

fn main() {
    wasm_logger::init(wasm_logger::Config::default());
    yew::Renderer::<app::App>::new().render();
}
