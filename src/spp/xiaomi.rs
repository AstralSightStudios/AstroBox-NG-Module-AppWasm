use std::rc::Rc;

use async_channel::{Receiver, Sender, unbounded};
use corelib::device::xiaomi::r#type::ConnectType;
use corelib::device::{self, DeviceConnectionInfo};
use js_sys::{Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    Navigator, ReadableStream, ReadableStreamDefaultReader, Serial, SerialOptions, SerialPort,
    SerialPortInfo, SerialPortRequestOptions, WritableStream, WritableStreamDefaultWriter, window,
};

fn read_optional_string(info: &JsValue, key: &str) -> Option<String> {
    Reflect::get(info, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_string())
        .and_then(|s| if s.is_empty() { None } else { Some(s) })
}

fn read_optional_u16(info: &JsValue, key: &str) -> Option<u16> {
    Reflect::get(info, &JsValue::from_str(key))
        .ok()
        .and_then(|value| value.as_f64())
        .map(|num| num as u16)
}

pub struct XiaomiSpp {
    port: SerialPort,
    reader: Option<ReadableStreamDefaultReader>,
    writer: Option<WritableStreamDefaultWriter>,
    device_addr: String,
    device_label: Option<String>,
    runtime: Option<tokio::runtime::Runtime>,
}

impl XiaomiSpp {
    pub async fn new(baud_rate: Option<u32>) -> Result<Self, JsValue> {
        let nav: Navigator = window().unwrap().navigator();
        let serial: Serial = nav.serial();
        let opts = SerialPortRequestOptions::new();

        let port_val = JsFuture::from(serial.request_port_with_options(&opts)).await?;
        let port: SerialPort = port_val.unchecked_into();

        let info: SerialPortInfo = port.get_info();
        let info_js: JsValue = info.into();

        let serial_number = read_optional_string(&info_js, "serialNumber");
        let vendor_id = read_optional_u16(&info_js, "usbVendorId");
        let product_id = read_optional_u16(&info_js, "usbProductId");

        let device_addr = if let Some(serial_num) = serial_number.clone() {
            format!("serial:{serial_num}")
        } else if let (Some(vendor), Some(product)) = (vendor_id, product_id) {
            format!("usb:{vendor:04x}:{product:04x}")
        } else {
            format!("serial-port-{}", js_sys::Date::now() as u64)
        };

        let device_label = serial_number.or_else(|| {
            vendor_id
                .zip(product_id)
                .map(|(v, p)| format!("USB {:04x}:{:04x}", v, p))
        });

        let open_opts = SerialOptions::new(baud_rate.unwrap_or(115200));
        JsFuture::from(port.open(&open_opts)).await?;

        Ok(Self {
            port,
            reader: None,
            writer: None,
            device_addr,
            device_label,
            runtime: None,
        })
    }

    pub fn device_addr(&self) -> &str {
        &self.device_addr
    }

    pub fn device_label(&self) -> Option<&str> {
        self.device_label.as_deref()
    }

    fn ensure_writer(&mut self) -> Result<WritableStreamDefaultWriter, JsValue> {
        if self.writer.is_none() {
            let writable: WritableStream = self.port.writable();
            let writer: WritableStreamDefaultWriter = writable.get_writer().unwrap();
            self.writer = Some(writer);
        }
        Ok(self.writer.as_ref().unwrap().clone())
    }

    pub async fn start(
        &mut self,
        mut name: String,
        addr_hint: String,
        authkey: String,
        sar_version: u32,
        connect_type: ConnectType,
        disconnect_cb: Rc<dyn Fn(String)>,
    ) -> Result<DeviceConnectionInfo, JsValue> {
        let readable: ReadableStream = self.port.readable();
        let reader: ReadableStreamDefaultReader = readable.get_reader().unchecked_into();
        self.reader = Some(reader.clone());

        let writer_handle = self.ensure_writer()?;
        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = unbounded();

        wasm_bindgen_futures::spawn_local(async move {
            while let Ok(data) = rx.recv().await {
                let chunk = Uint8Array::from(data.as_slice());
                if let Err(err) = JsFuture::from(writer_handle.write_with_chunk(&chunk)).await {
                    web_sys::console::warn_1(&JsValue::from_str(&format!(
                        "[wasm] Failed to write to serial port: {:?}",
                        err
                    )));
                    break;
                }
            }
        });

        corelib::ecs::init_runtime_default();
        let runtime = corelib::asyncrt::build_runtime();
        let handle = runtime.handle().clone();

        if name.is_empty() {
            name = self
                .device_label
                .clone()
                .unwrap_or_else(|| "Bluetooth Device".to_string());
        }

        let final_addr = if addr_hint.trim().is_empty() {
            self.device_addr.clone()
        } else {
            addr_hint
        };

        let packet_handle = handle.clone();
        let disconnect_handle = disconnect_cb.clone();
        let device_id_for_loop = final_addr.clone();
        let reader_for_loop = reader.clone();

        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let read_res = JsFuture::from(reader_for_loop.read()).await;
                let Ok(val) = read_res else {
                    disconnect_handle(device_id_for_loop.clone());
                    break;
                };

                let done = Reflect::get(&val, &JsValue::from_str("done"))
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if done {
                    let _ = reader_for_loop.release_lock();
                    disconnect_handle(device_id_for_loop.clone());
                    break;
                }

                let chunk =
                    Reflect::get(&val, &JsValue::from_str("value")).unwrap_or(JsValue::UNDEFINED);

                if chunk.is_undefined() || chunk.is_null() {
                    continue;
                }

                let data: Vec<u8> = Uint8Array::new(&chunk).to_vec();
                //log::info!("[wasm] Recv: {}", corelib::tools::to_hex_string(&data));
                corelib::device::xiaomi::packet::dispatcher::on_packet(
                    packet_handle.clone(),
                    device_id_for_loop.clone(),
                    data,
                );
            }
        });

        let device_info_res = device::create_miwear_device(
            handle.clone(),
            name.clone(),
            final_addr.clone(),
            authkey,
            sar_version,
            connect_type,
            false,
            {
                let tx = tx.clone();
                move |data: Vec<u8>| {
                    let tx = tx.clone();
                    async move {
                        //log::info!("[wasm] Send: {}", corelib::tools::to_hex_string(&data));
                        let _ = tx.send(data).await;
                        Ok(())
                    }
                }
            },
        )
        .await;

        let device_info = match device_info_res {
            Ok(info) => info,
            Err(err) => {
                web_sys::console::error_1(&JsValue::from_str(&format!(
                    "[wasm] create_miwear_device failed: {}",
                    err
                )));
                let _ = reader.release_lock();
                let _ = JsFuture::from(self.port.close()).await;
                return Err(JsValue::from_str(&err.to_string()));
            }
        };

        self.runtime = Some(runtime);

        Ok(device_info)
    }

    pub async fn disconnect(mut self) -> Result<(), JsValue> {
        if let Some(writer) = self.writer.take() {
            let _ = JsFuture::from(writer.close()).await;
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.release_lock();
        }
        let _ = JsFuture::from(self.port.close()).await;
        self.runtime.take();
        Ok(())
    }
}
