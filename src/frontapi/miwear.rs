use std::{cell::RefCell, collections::HashMap, rc::Rc};

use corelib::device::DeviceConnectionInfo;
use corelib::device::xiaomi::XiaomiDevice;
use corelib::device::xiaomi::components::info::{InfoComponent, InfoSystem};
use corelib::device::xiaomi::components::resource::{ResourceComponent, ResourceSystem};
use corelib::device::xiaomi::r#type::ConnectType;
use corelib::ecs::entity::EntityExt;
use corelib::ecs::logic_component::LogicComponent;
use once_cell::sync::OnceCell;
use serde_wasm_bindgen::to_value as to_js_value;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::spp::xiaomi::XiaomiSpp;

static CORE_INIT: OnceCell<()> = OnceCell::new();

thread_local! {
    static EVENT_SINK: RefCell<Option<js_sys::Function>> = RefCell::new(None);
    static SESSIONS: RefCell<HashMap<String, XiaomiSpp>> = RefCell::new(HashMap::new());
}

fn ensure_core_initialized() {
    CORE_INIT.get_or_init(|| {
        console_error_panic_hook::set_once();
        #[cfg(target_arch = "wasm32")]
        corelib::logger::wasm::init_logger();
        corelib::ecs::init_runtime_default();
    });
}

fn emit_event(event: &str, payload: &JsValue) {
    EVENT_SINK.with(|cell| {
        if let Some(ref sink) = *cell.borrow() {
            if let Err(err) = sink.call2(&JsValue::NULL, &JsValue::from_str(event), payload) {
                web_sys::console::error_2(&JsValue::from_str("emit_event failed"), &err);
            }
        }
    });
}

fn connect_type_from_str(value: &str) -> ConnectType {
    match value.to_ascii_uppercase().as_str() {
        "BLE" => ConnectType::BLE,
        _ => ConnectType::SPP,
    }
}

async fn remove_device_and_get_info(addr: &str) -> Option<DeviceConnectionInfo> {
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        let info =
            rt.find_entity_by_id_mut::<XiaomiDevice>(&owned)
                .map(|dev| DeviceConnectionInfo {
                    name: dev.name.clone(),
                    addr: dev.addr.clone(),
                });
        rt.remove_entity_by_id(&owned);
        info
    })
    .await
}

async fn notify_disconnected(addr: String) {
    let info = remove_device_and_get_info(&addr)
        .await
        .unwrap_or(DeviceConnectionInfo {
            name: String::new(),
            addr: addr.clone(),
        });
    if let Ok(payload) = to_js_value(&info) {
        emit_event("device-disconnected", &payload);
    }
}

async fn handle_remote_disconnect(addr: String) {
    SESSIONS.with(|cell| {
        cell.borrow_mut().remove(&addr);
    });
    notify_disconnected(addr).await;
}

#[wasm_bindgen]
pub fn register_event_sink(callback: js_sys::Function) {
    EVENT_SINK.with(|cell| {
        *cell.borrow_mut() = Some(callback);
    });
}

#[wasm_bindgen]
pub async fn miwear_connect(
    name: String,
    addr: String,
    authkey: String,
    sar_version: u32,
    connect_type: String,
) -> Result<JsValue, JsValue> {
    ensure_core_initialized();

    let mut session = XiaomiSpp::new(None).await?;
    let ct = connect_type_from_str(&connect_type);
    let disconnect_cb: Rc<dyn Fn(String)> = Rc::new(|target| {
        spawn_local(async move {
            handle_remote_disconnect(target).await;
        });
    });

    let device_info = session
        .start(name, addr, authkey, sar_version, ct, disconnect_cb)
        .await?;

    SESSIONS.with(|cell| {
        cell.borrow_mut().insert(device_info.addr.clone(), session);
    });

    let payload = to_js_value(&device_info).map_err(|err| JsValue::from_str(&err.to_string()))?;
    emit_event("device-connected", &payload);
    Ok(payload)
}

#[wasm_bindgen]
pub async fn miwear_disconnect(addr: String) -> Result<(), JsValue> {
    ensure_core_initialized();
    let removed = SESSIONS.with(|cell| cell.borrow_mut().remove(&addr));
    if let Some(session) = removed {
        let _ = session.disconnect().await;
    }
    notify_disconnected(addr).await;
    Ok(())
}

#[wasm_bindgen]
pub async fn miwear_get_connected_devices() -> Result<JsValue, JsValue> {
    ensure_core_initialized();
    let devices = corelib::ecs::with_rt_mut(|rt| {
        rt.entities
            .values()
            .filter_map(|entity| entity.as_any().downcast_ref::<XiaomiDevice>())
            .map(|dev| DeviceConnectionInfo {
                name: dev.name.clone(),
                addr: dev.addr.clone(),
            })
            .collect::<Vec<_>>()
    })
    .await;

    to_js_value(&devices).map_err(|err| JsValue::from_str(&err.to_string()))
}

async fn with_info_system<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut InfoSystem) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        let device = rt
            .find_entity_by_id_mut::<XiaomiDevice>(&owned)
            .ok_or_else(|| "Device not found".to_string())?;
        let component = device
            .get_component_as_mut::<InfoComponent>(InfoComponent::ID)
            .map_err(|err| format!("{:?}", err))?;
        let system = component
            .system_mut()
            .as_any_mut()
            .downcast_mut::<InfoSystem>()
            .ok_or_else(|| "Info system not found".to_string())?;
        f(system)
    })
    .await
}

async fn with_resource_system<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut ResourceSystem) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        let device = rt
            .find_entity_by_id_mut::<XiaomiDevice>(&owned)
            .ok_or_else(|| "Device not found".to_string())?;
        let component = device
            .get_component_as_mut::<ResourceComponent>(ResourceComponent::ID)
            .map_err(|err| format!("{:?}", err))?;
        let system = component
            .system_mut()
            .as_any_mut()
            .downcast_mut::<ResourceSystem>()
            .ok_or_else(|| "Resource system not found".to_string())?;
        f(system)
    })
    .await
}

#[wasm_bindgen]
pub async fn miwear_get_data(addr: String, data_type: String) -> Result<JsValue, JsValue> {
    ensure_core_initialized();
    let lower = data_type.to_ascii_lowercase();
    match lower.as_str() {
        "info" => {
            let rx = with_info_system(&addr, |sys| Ok(sys.request_device_info()))
                .await
                .map_err(|err| JsValue::from_str(&err))?;
            let info = rx
                .await
                .map_err(|_| JsValue::from_str("Device info response not received"))?;
            to_js_value(&info).map_err(|err| JsValue::from_str(&err.to_string()))
        }
        "status" => {
            let rx = with_info_system(&addr, |sys| Ok(sys.request_device_status()))
                .await
                .map_err(|err| JsValue::from_str(&err))?;
            let status = rx
                .await
                .map_err(|_| JsValue::from_str("Device status response not received"))?;
            to_js_value(&status).map_err(|err| JsValue::from_str(&err.to_string()))
        }
        "storage" => {
            let rx = with_info_system(&addr, |sys| Ok(sys.request_device_storage()))
                .await
                .map_err(|err| JsValue::from_str(&err))?;
            let storage = rx
                .await
                .map_err(|_| JsValue::from_str("Device storage response not received"))?;
            to_js_value(&storage).map_err(|err| JsValue::from_str(&err.to_string()))
        }
        other => Err(JsValue::from_str(&format!(
            "Unsupported data type: {other}"
        ))),
    }
}

#[wasm_bindgen]
pub async fn miwear_get_watchfaces(addr: String) -> Result<JsValue, JsValue> {
    ensure_core_initialized();
    let rx = with_resource_system(&addr, |sys| Ok(sys.request_watchface_list()))
        .await
        .map_err(|err| JsValue::from_str(&err))?;
    let list = rx
        .await
        .map_err(|_| JsValue::from_str("Watchface list response not received"))?;
    to_js_value(&list).map_err(|err| JsValue::from_str(&err.to_string()))
}

#[wasm_bindgen]
pub async fn miwear_get_quick_apps(addr: String) -> Result<JsValue, JsValue> {
    ensure_core_initialized();
    let rx = with_resource_system(&addr, |sys| Ok(sys.request_quick_app_list()))
        .await
        .map_err(|err| JsValue::from_str(&err))?;
    let list = rx
        .await
        .map_err(|_| JsValue::from_str("Quick app list response not received"))?;
    to_js_value(&list).map_err(|err| JsValue::from_str(&err.to_string()))
}

#[wasm_bindgen]
pub async fn miwear_install(
    _addr: String,
    _res_type: String,
    _file_path: String,
    _package_name: Option<String>,
) -> Result<(), JsValue> {
    Err(JsValue::from_str(
        "miwear_install is not supported in wasm runtime",
    ))
}

#[wasm_bindgen]
pub fn app_get_config() -> JsValue {
    to_js_value(&serde_json::json!({ "disable_auto_clean": false })).unwrap_or(JsValue::NULL)
}
