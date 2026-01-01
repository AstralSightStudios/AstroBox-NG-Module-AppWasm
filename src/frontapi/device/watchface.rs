use serde_wasm_bindgen::to_value as to_js_value;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;

use super::{
    await_result_receiver, ensure_core_initialized, with_resource_system, with_watchface_system,
};

#[wasm_bindgen]
pub async fn watchface_get_list(addr: String) -> Result<JsValue, JsValue> {
    ensure_core_initialized();
    let rx = with_resource_system(&addr, |sys| Ok(sys.request_watchface_list()))
        .await
        .map_err(|err| JsValue::from_str(&err))?;
    let list = await_result_receiver(rx, "Watchface list response not received").await?;
    to_js_value(&list).map_err(|err| JsValue::from_str(&format!("{:?}", err)))
}

#[wasm_bindgen]
pub async fn watchface_set_current(addr: String, watchface_id: String) -> Result<(), JsValue> {
    ensure_core_initialized();
    let id = watchface_id.clone();
    with_watchface_system(&addr, move |sys| {
        sys.set_watchface(&id);
        Ok(())
    })
    .await
    .map_err(|err| JsValue::from_str(&err))
}

#[wasm_bindgen]
pub async fn watchface_uninstall(addr: String, watchface_id: String) -> Result<(), JsValue> {
    ensure_core_initialized();
    let id = watchface_id.clone();
    with_watchface_system(&addr, move |sys| {
        sys.uninstall_watchface(&id);
        Ok(())
    })
    .await
    .map_err(|err| JsValue::from_str(&err))
}
