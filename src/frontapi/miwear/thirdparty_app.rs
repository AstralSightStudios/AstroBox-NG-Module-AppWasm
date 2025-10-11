#[cfg(target_arch = "wasm32")]
use corelib::device::xiaomi::components::thirdparty_app::AppInfo;
use serde_wasm_bindgen::to_value as to_js_value;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
use super::with_thirdparty_app_system;
use super::{ensure_core_initialized, with_resource_component, with_resource_system};

#[wasm_bindgen]
pub async fn thirdpartyapp_get_list(addr: String) -> Result<JsValue, JsValue> {
    ensure_core_initialized();
    let rx = with_resource_system(&addr, |sys| Ok(sys.request_quick_app_list()))
        .await
        .map_err(|err| JsValue::from_str(&err))?;
    let list = rx
        .await
        .map_err(|_| JsValue::from_str("Quick app list response not received"))?;
    to_js_value(&list).map_err(|err| JsValue::from_str(&err.to_string()))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn thirdpartyapp_send_message(
    addr: String,
    package_name: String,
    data: String,
) -> Result<(), JsValue> {
    ensure_core_initialized();
    let info = get_app_info(&addr, &package_name).await?;
    let payload = data.into_bytes();
    with_thirdparty_app_system(&addr, move |sys| {
        sys.send_phone_message(&info, payload);
        Ok(())
    })
    .await
    .map_err(|err| JsValue::from_str(&err))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn thirdpartyapp_launch(
    addr: String,
    package_name: String,
    page: String,
) -> Result<(), JsValue> {
    ensure_core_initialized();
    let info = get_app_info(&addr, &package_name).await?;
    with_thirdparty_app_system(&addr, move |sys| {
        sys.launch_app(&info, &page);
        Ok(())
    })
    .await
    .map_err(|err| JsValue::from_str(&err))
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub async fn thirdpartyapp_uninstall(addr: String, package_name: String) -> Result<(), JsValue> {
    ensure_core_initialized();
    let info = get_app_info(&addr, &package_name).await?;
    with_thirdparty_app_system(&addr, move |sys| {
        sys.uninstall_app(&info);
        Ok(())
    })
    .await
    .map_err(|err| JsValue::from_str(&err))?;

    let _ = with_resource_system(&addr, |sys| {
        let _ = sys.request_quick_app_list();
        Ok(())
    })
    .await;

    Ok(())
}

#[cfg(target_arch = "wasm32")]
async fn get_app_info(addr: &str, package_name: &str) -> Result<AppInfo, JsValue> {
    with_resource_component(addr, |comp| {
        comp.quick_apps
            .iter()
            .find(|item| item.package_name == package_name)
            .map(|item| AppInfo {
                package_name: item.package_name.clone(),
                fingerprint: item.fingerprint.clone(),
            })
            .ok_or_else(|| format!("AppInfo not found for {}", package_name))
    })
    .await
    .map_err(|err| JsValue::from_str(&err))
}
