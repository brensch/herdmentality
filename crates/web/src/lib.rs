mod api;
mod app;
mod names;
mod render;
mod state;
mod views;

use wasm_bindgen::prelude::*;

#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    console_error_panic_hook::set_once();
    yew::Renderer::<app::App>::new().render();
    Ok(())
}
