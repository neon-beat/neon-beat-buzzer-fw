use alloc::{format, string::ToString};
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{Ipv4Address, Stack, tcp::TcpSocket};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embedded_websocket as ws;
use esp_hal::rng::Rng;
use log::{error, info};
use serde_json as sj;
use static_cell::StaticCell;

const BUF_SIZE: usize = 512;

pub struct Websocket {
    tx_channel: Sender<'static, NoopRawMutex, sj::Value, 3>,
}

pub enum WebsocketEvent {
    Connected,
    Disconnected,
    Command(sj::Value),
}

static CHANNEL: StaticCell<Channel<NoopRawMutex, sj::Value, 3>> = StaticCell::new();

impl Websocket {
    pub fn new(
        spawner: &Spawner,
        stack: Stack<'static>,
        rx_channel: Sender<'static, NoopRawMutex, WebsocketEvent, 3>,
    ) -> Self {
        let tx_channel: &'static mut _ = CHANNEL.init(Channel::new());
        let res = Websocket {
            tx_channel: tx_channel.sender(),
        };
        let result = spawner.spawn(websocket_task(stack, rx_channel, tx_channel.receiver()));
        if let Err(e) = result {
            error!("Failed to spawn websocket task: {:?}", e);
        }
        res
    }

    pub async fn send_identify(&mut self, mac: &[u8; 6]) {
        let data = sj::json!({
            "type": "identification",
            "id": format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])
        });
        self.tx_channel.send(data).await;
    }
    pub async fn send_button_pushed(&mut self, mac: &[u8; 6]) {
        let data = sj::json!({
            "type": "buzz",
            "id": format!("{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
                mac[0], mac[1], mac[2], mac[3], mac[4], mac[5])
        });
        self.tx_channel.send(data).await;
    }
}

#[embassy_executor::task]
pub async fn websocket_task(
    stack: Stack<'static>,
    rx_channel: Sender<'static, NoopRawMutex, WebsocketEvent, 3>,
    tx_channel: Receiver<'static, NoopRawMutex, sj::Value, 3>,
) {
    let mut rx_buffer: [u8; BUF_SIZE] = [0; BUF_SIZE];
    let mut tx_buffer: [u8; BUF_SIZE] = [0; BUF_SIZE];
    let mut socket = TcpSocket::new(stack, &mut rx_buffer, &mut tx_buffer);
    let mut client = ws::WebSocketClient::new_client(Rng::new());
    let mut connected: bool = false;
    let mut ws_key: ws::WebSocketKey;

    info!("Starting websocket task");
    loop {
        let remote = (Ipv4Address::new(192, 168, 66, 1), 8080);
        let res = socket.connect(remote).await;
        if let Err(e) = res {
            error!("Failed to connect to TCP server: {:?}", e);
            continue;
        }
        info!("Connected to TCP server");
        while socket.state() == embassy_net::tcp::State::Established {
            let mut buffer: [u8; BUF_SIZE] = [0; BUF_SIZE];
            let websocket_options = ws::WebSocketOptions {
                path: "/ws",
                host: "192.168.66.1",
                origin: "http://localhost:1337",
                sub_protocols: None,
                additional_headers: None,
            };
            let res = client.client_connect(&websocket_options, &mut buffer);
            match res {
                Err(e) => {
                    error!("Failed to generate connect message: {:?}", e);
                    continue;
                }
                Ok((count, key)) => {
                    let res = socket.write(&buffer[..count]).await;
                    match res {
                        Err(e) => {
                            error!("Failed to connect to websocket server: {:?}", e);
                            continue;
                        }
                        Ok(_) => ws_key = key,
                    }
                }
            }
            loop {
                match select(socket.read(&mut buffer), tx_channel.receive()).await {
                    Either::First(x) => match x {
                        Ok(0) => {
                            info!("Socket is closed");
                            let res = client.close(
                                ws::WebSocketCloseStatusCode::NormalClosure,
                                None,
                                &mut buffer,
                            );
                            if let Err(e) = res {
                                error!("Failed to close client after TCP disconnect: {:?}", e);
                            }
                            socket.abort();
                            connected = false;
                            rx_channel.send(WebsocketEvent::Disconnected).await;
                            break;
                        }
                        Err(e) => {
                            error!("Can not read socket: {:?}", e);
                            continue;
                        }
                        Ok(count) => {
                            if !connected {
                                let res = client.client_accept(&ws_key, &buffer[..count]);
                                match res {
                                    Ok(_) => {
                                        connected = true;
                                        info!("Connected to WS server");
                                        rx_channel.send(WebsocketEvent::Connected).await;
                                    }
                                    Err(e) => error!("Can not accept connection: {:?}", e),
                                }
                            } else {
                                let mut frame: [u8; BUF_SIZE] = [0; BUF_SIZE];
                                if let Ok(x) = client.read(&buffer, &mut frame) {
                                    let message = str::from_utf8(&frame[..x.len_to]);
                                    match message {
                                        Err(e) => error!("invalid message ({e})"),
                                        Ok(m) => {
                                            info!("Received new message: {m}");
                                            if let Ok(v) = sj::from_str(m) {
                                                rx_channel.send(WebsocketEvent::Command(v)).await;
                                            } else {
                                                error!("Failed to parse message as json");
                                            }
                                        }
                                    }
                                } else {
                                    error!(
                                        "Failed to decode receveid message {:?}",
                                        &buffer[..count]
                                    );
                                }
                            }
                        }
                    },
                    Either::Second(x) => {
                        info!("Sending message {:?}", x.to_string());
                        let res = client.write(
                            ws::WebSocketSendMessageType::Text,
                            true,
                            x.to_string().as_bytes(),
                            &mut buffer,
                        );
                        match res {
                            Ok(count) => match socket.write(&buffer[..count]).await {
                                Err(e) => error!("Failed to send message: {:?}", e),
                                _ => info!("Message sent"),
                            },
                            Err(e) => error!("Failed to send message: {:?}", e),
                        }
                    }
                }
            }
        }
    }
}
