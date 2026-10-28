//! wasm-bindgen bindings for the Vditor markdown editor (loaded from
//! `/vditor/dist/index.min.js` as a classic script, exposing `window.Vditor`).
//! Only the surface the app uses is bound.

use wasm_bindgen::prelude::*;

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(js_name = Vditor)]
    pub type VditorEditor;

    #[wasm_bindgen(constructor, js_class = "Vditor")]
    pub fn new(el: &web_sys::Element, options: JsValue) -> VditorEditor;

    #[wasm_bindgen(method, js_name = getValue)]
    pub fn get_value(this: &VditorEditor) -> String;

    #[wasm_bindgen(method, js_name = setValue)]
    pub fn set_value(this: &VditorEditor, value: &str);

    #[wasm_bindgen(method, js_name = insertValue)]
    pub fn insert_value(this: &VditorEditor, value: &str, render: bool);

    #[wasm_bindgen(method, js_name = deleteValue)]
    pub fn delete_value(this: &VditorEditor);

    #[wasm_bindgen(method, js_name = setTheme)]
    pub fn set_theme(this: &VditorEditor, theme: &str);

    #[wasm_bindgen(method, js_name = focus)]
    pub fn focus(this: &VditorEditor);

    #[wasm_bindgen(method, js_name = destroy)]
    pub fn destroy(this: &VditorEditor);
}
