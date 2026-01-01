use async_channel::unbounded;
use corelib::device::DeviceConnectionInfo;
use corelib::device::xiaomi::XiaomiDevice;
use corelib::device::xiaomi::components::info::InfoSystem;
use corelib::device::xiaomi::components::install::InstallSystem;
use corelib::device::xiaomi::components::mass::SendMassCallbackData;
use corelib::device::xiaomi::components::resource::{ResourceComponent, ResourceSystem};
use corelib::device::xiaomi::components::thirdparty_app::ThirdpartyAppSystem;
use corelib::device::xiaomi::components::watchface::WatchfaceSystem;
use corelib::device::xiaomi::packet::mass::MassDataType;
use corelib::device::xiaomi::resutils::{FileType, get_file_type};
use corelib::device::xiaomi::r#type::ConnectType;
use js_sys::{Function, Uint8Array};
use once_cell::sync::OnceCell;
use serde_wasm_bindgen::to_value as to_js_value;
use std::sync::Arc;
use std::{cell::RefCell, collections::HashMap, rc::Rc};
use tokio::sync::oneshot;
use wasm_bindgen::JsValue;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::spawn_local;

use crate::spp::xiaomi::XiaomiSpp;

pub mod thirdparty_app;
pub mod watchface;

static CORE_INIT: OnceCell<()> = OnceCell::new();

thread_local! {
    static EVENT_SINK: RefCell<Option<js_sys::Function>> = RefCell::new(None);
    static SESSIONS: RefCell<HashMap<String, XiaomiSpp>> = RefCell::new(HashMap::new());
}

pub(super) fn ensure_core_initialized() {
    CORE_INIT.get_or_init(|| {
        console_error_panic_hook::set_once();
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
            rt.component_ref::<XiaomiDevice>(&owned)
                .map(|dev| DeviceConnectionInfo {
                    name: dev.name().to_string(),
                    addr: dev.addr().to_string(),
                });
        rt.remove_device(&owned);
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

async fn disconnect_all_sessions() {
    let sessions = SESSIONS.with(|cell| {
        let mut map = cell.borrow_mut();
        map.drain().collect::<Vec<(String, XiaomiSpp)>>()
    });

    for (addr, session) in sessions {
        let _ = session.disconnect().await;
        notify_disconnected(addr).await;
    }
}

async fn handle_remote_disconnect(addr: String) {
    SESSIONS.with(|cell| {
        cell.borrow_mut().remove(&addr);
    });
    notify_disconnected(addr).await;
}

pub(super) async fn await_result_receiver<T, E>(
    rx: oneshot::Receiver<Result<T, E>>,
    missing_msg: &'static str,
) -> Result<T, JsValue>
where
    E: std::fmt::Display,
{
    let result = rx.await.map_err(|_| JsValue::from_str(missing_msg))?;
    result.map_err(|err| JsValue::from_str(&err.to_string()))
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

    disconnect_all_sessions().await;

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

    let payload =
        to_js_value(&device_info).map_err(|err| JsValue::from_str(&format!("{:?}", err)))?;
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
        rt.device_ids()
            .filter_map(|device_id| {
                rt.component_ref::<XiaomiDevice>(device_id)
                    .map(|dev| DeviceConnectionInfo {
                        name: dev.name().to_string(),
                        addr: dev.addr().to_string(),
                    })
            })
            .collect::<Vec<_>>()
    })
    .await;

    to_js_value(&devices).map_err(|err| JsValue::from_str(&format!("{:?}", err)))
}

pub(super) async fn with_info_system<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut InfoSystem) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        rt.with_device_mut(&owned, |world, entity| {
            let mut system = world
                .get_mut::<InfoSystem>(entity)
                .ok_or_else(|| "Info system not found".to_string())?;
            f(&mut system)
        })
        .ok_or_else(|| "Device not found".to_string())?
    })
    .await
}

pub(super) async fn with_install_system<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut InstallSystem) -> Result<R, String> + 'static,
    R: 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        rt.with_device_mut(&owned, |world, entity| {
            let mut system = world
                .get_mut::<InstallSystem>(entity)
                .ok_or_else(|| "Install system not found".to_string())?;
            f(&mut system)
        })
        .ok_or_else(|| "Device not found".to_string())?
    })
    .await
}

pub(super) async fn with_resource_system<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut ResourceSystem) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        rt.with_device_mut(&owned, |world, entity| {
            let mut system = world
                .get_mut::<ResourceSystem>(entity)
                .ok_or_else(|| "Resource system not found".to_string())?;
            f(&mut system)
        })
        .ok_or_else(|| "Device not found".to_string())?
    })
    .await
}

pub(super) async fn with_resource_component<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&ResourceComponent) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        rt.with_device_mut(&owned, |world, entity| {
            let component = world
                .get::<ResourceComponent>(entity)
                .ok_or_else(|| "Resource component not found".to_string())?;
            f(component)
        })
        .ok_or_else(|| "Device not found".to_string())?
    })
    .await
}

pub(super) async fn with_watchface_system<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut WatchfaceSystem) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        rt.with_device_mut(&owned, |world, entity| {
            let mut system = world
                .get_mut::<WatchfaceSystem>(entity)
                .ok_or_else(|| "Watchface system not found".to_string())?;
            f(&mut system)
        })
        .ok_or_else(|| "Device not found".to_string())?
    })
    .await
}

pub(super) async fn with_thirdparty_app_system<F, R>(addr: &str, f: F) -> Result<R, String>
where
    F: FnOnce(&mut ThirdpartyAppSystem) -> Result<R, String> + Send + 'static,
    R: Send + 'static,
{
    let owned = addr.to_string();
    corelib::ecs::with_rt_mut(move |rt| {
        rt.with_device_mut(&owned, |world, entity| {
            let mut system = world
                .get_mut::<ThirdpartyAppSystem>(entity)
                .ok_or_else(|| "Thirdparty app system not found".to_string())?;
            f(&mut system)
        })
        .ok_or_else(|| "Device not found".to_string())?
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
            let info = await_result_receiver(rx, "Device info response not received").await?;
            to_js_value(&info).map_err(|err| JsValue::from_str(&format!("{:?}", err)))
        }
        "status" => {
            let rx = with_info_system(&addr, |sys| Ok(sys.request_device_status()))
                .await
                .map_err(|err| JsValue::from_str(&err))?;
            let status = await_result_receiver(rx, "Device status response not received").await?;
            to_js_value(&status).map_err(|err| JsValue::from_str(&format!("{:?}", err)))
        }
        "storage" => {
            let rx = with_info_system(&addr, |sys| Ok(sys.request_device_storage()))
                .await
                .map_err(|err| JsValue::from_str(&err))?;
            let storage = await_result_receiver(rx, "Device storage response not received").await?;
            to_js_value(&storage).map_err(|err| JsValue::from_str(&format!("{:?}", err)))
        }
        other => Err(JsValue::from_str(&format!(
            "Unsupported data type: {other}"
        ))),
    }
}

#[wasm_bindgen]
pub async fn miwear_install(
    addr: String,
    res_type: u8,
    data: Uint8Array,
    package_name: Option<String>,
    progress_cb: Option<Function>,
) -> Result<(), JsValue> {
    ensure_core_initialized();

    let data_type = MassDataType::try_from(res_type).map_err(|err| JsValue::from_str(err))?;
    let file_data = data.to_vec();

    let (progress_tx, progress_rx) = unbounded::<SendMassCallbackData>();
    let progress_notifier = {
        let sender = progress_tx.clone();
        Arc::new(move |payload: SendMassCallbackData| {
            let _ = sender.try_send(payload);
        }) as Arc<dyn Fn(SendMassCallbackData) + Send + Sync>
    };

    let package_name_clone = package_name.clone();
    let install_future = with_install_system(&addr, move |install_sys| {
        install_sys
            .send_install_request_with_progress(
                data_type,
                file_data,
                package_name_clone.as_deref(),
                progress_notifier,
            )
            .map_err(|err| format!("{:?}", err))
    })
    .await
    .map_err(|err| JsValue::from_str(&err))?;

    if let Some(callback) = progress_cb.clone() {
        spawn_local(async move {
            let receiver = progress_rx;
            while let Ok(payload) = receiver.recv().await {
                match to_js_value(&payload) {
                    Ok(js_payload) => {
                        let _ = callback.call1(&JsValue::NULL, &js_payload);
                    }
                    Err(err) => {
                        web_sys::console::error_1(&JsValue::from_str(&format!(
                            "[wasm] miwear_install progress serialization failed: {}",
                            err
                        )));
                    }
                }
            }
        });
    } else {
        drop(progress_rx);
    }

    let result = install_future
        .await
        .map_err(|err| JsValue::from_str(&format!("{:?}", err)));

    drop(progress_tx);
    result
}

#[wasm_bindgen]
pub async fn miwear_get_file_type(file: Uint8Array, name: String) -> u8 {
    let file_type = get_file_type(&file.to_vec());
    if file_type == FileType::Zip {
        // 检查扩展名 abp
        if let Some(ext) = name.split('.').last() {
            if ext == "abp" {
                return FileType::Abp as u8;
            }
        }
    }

    file_type as u8
}
