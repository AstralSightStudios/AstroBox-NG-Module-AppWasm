use async_channel::{unbounded, Receiver, Sender};
use js_sys::{JsString, Reflect, Uint8Array};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    window, Navigator, ReadableStream, ReadableStreamDefaultReader, Serial, SerialOptions,
    SerialPort, SerialPortRequestOptions, WritableStream, WritableStreamDefaultWriter,
};

#[wasm_bindgen]
pub struct XiaomiSpp {
    port: SerialPort,
    reader: Option<ReadableStreamDefaultReader>,
    writer: Option<WritableStreamDefaultWriter>,
}

#[wasm_bindgen]
impl XiaomiSpp {
    #[wasm_bindgen(constructor)]
    pub async fn new(baud_rate: Option<u32>) -> Result<XiaomiSpp, JsValue> {
        #[cfg(target_arch = "wasm32")]
        corelib::logger::wasm::init_logger();

        console_error_panic_hook::set_once();

        let nav: Navigator = window().unwrap().navigator();
        let serial: Serial = nav.serial();

        let opts = SerialPortRequestOptions::new();

        /*
        let allowed = Array::new();
        allowed.push(&JsValue::from_str(SPP_UUID));
        Reflect::set(
            &opts,
            &JsValue::from_str("allowedBluetoothServiceClassIds"),
            &allowed,
        )?;

        let filter = Object::new();
        Reflect::set(
            &filter,
            &JsValue::from_str("bluetoothServiceClassId"),
            &JsValue::from_str(SPP_UUID),
        )?;
        let filters = Array::new();
        filters.push(&filter);
        opts.set_filters(&filters);
        */

        let port_promise = serial.request_port_with_options(&opts);
        let port_val = JsFuture::from(port_promise).await?;
        let port: SerialPort = port_val.unchecked_into();

        let info = port.get_info();
        log::info!("SerialPort info: {:?}", info);

        let open_opts = SerialOptions::new(baud_rate.unwrap_or(115200));
        let open_promise = port.open(&open_opts);
        JsFuture::from(open_promise).await?;

        Ok(XiaomiSpp {
            port,
            reader: None,
            writer: None,
        })
    }

    fn ensure_writer_handle(&mut self) -> Result<WritableStreamDefaultWriter, JsValue> {
        if self.writer.is_none() {
            let writable: WritableStream = self.port.writable();
            let writer: WritableStreamDefaultWriter = writable.get_writer().unwrap();
            self.writer = Some(writer);
        }
        Ok(self.writer.as_ref().unwrap().clone())
    }

    pub async fn start(&mut self, authkey: JsString) -> Result<(), JsValue> {
        let readable: ReadableStream = self.port.readable();
        let reader: ReadableStreamDefaultReader = readable.get_reader().unchecked_into();
        self.reader = Some(reader.clone());

        let writer_handle = self.ensure_writer_handle()?;

        let (tx, rx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = unbounded();

        wasm_bindgen_futures::spawn_local(async move {
            while let Ok(data) = rx.recv().await {
                let chunk = Uint8Array::from(data.as_slice());
                let _ = JsFuture::from(writer_handle.write_with_chunk(&chunk)).await;
            }
        });

        corelib::ecs::init_runtime_default();
        let tk_rt = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        corelib::device::create_miwear_device(
            tk_rt.handle().clone(),
            "WASM_SERIAL_DEVICE".to_string(),
            "UNKNOWN".to_string(),
            authkey.as_string().unwrap(),
            2,
            corelib::device::xiaomi::r#type::ConnectType::SPP,
            false,
            {
                let tx = tx.clone();
                move |data: Vec<u8>| {
                    let tx = tx.clone();
                    async move {
                        log::info!("Send: {}", corelib::tools::to_hex_string(&data));
                        let _ = tx.send(data).await;
                        Ok(())
                    }
                }
            },
        )
        .await
        .map_err(|err| JsValue::from_str(&err.to_string()))?;

        wasm_bindgen_futures::spawn_local(async move {
            loop {
                let read_promise = reader.read();
                let read_res = JsFuture::from(read_promise).await;
                let Ok(val) = read_res else {
                    break;
                };

                let done = Reflect::get(&val, &JsValue::from_str("done"))
                    .ok()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);

                if done {
                    let _ = reader.release_lock();
                    break;
                }

                let chunk =
                    Reflect::get(&val, &JsValue::from_str("value")).unwrap_or(JsValue::UNDEFINED);

                if chunk.is_undefined() || chunk.is_null() {
                    continue;
                }

                let data: Vec<u8> = Uint8Array::new(&chunk).to_vec();

                log::info!("Recv: {}", corelib::tools::to_hex_string(&data));

                let tk_rt = tokio::runtime::Builder::new_current_thread()
                    .build()
                    .unwrap();
                corelib::device::xiaomi::packet::on_packet(
                    tk_rt.handle().clone(),
                    "WASM_SERIAL_DEVICE".to_string(),
                    data,
                );
            }
        });

        Ok(())
    }

    pub async fn write_bytes(&mut self, data: Vec<u8>) -> Result<(), JsValue> {
        if self.writer.is_none() {
            let writable: WritableStream = self.port.writable();
            let writer: WritableStreamDefaultWriter = writable.get_writer().unwrap();
            self.writer = Some(writer);
        }
        let writer = self.writer.as_ref().unwrap().clone();

        let chunk = Uint8Array::from(data.as_slice());
        let write_promise = writer.write_with_chunk(&chunk);
        JsFuture::from(write_promise).await?;
        Ok(())
    }

    pub async fn disconnect(mut self) -> Result<(), JsValue> {
        if let Some(w) = self.writer.take() {
            let _ = JsFuture::from(w.close()).await;
        }
        if let Some(r) = self.reader.take() {
            let _ = r.release_lock();
        }
        let _ = JsFuture::from(self.port.close()).await;
        Ok(())
    }
}
