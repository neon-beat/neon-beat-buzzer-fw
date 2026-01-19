use crate::led_cmd::{LedCmd, MessageLedPattern};
use alloc::format;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{HardwareAddress, Ipv4Address, Stack, tcp::TcpSocket};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Channel, Receiver, Sender},
};
use embedded_websocket as ws;
use esp_hal::rng::Rng;
use log::{debug, error, info, warn};
use serde::Serialize;
use serde_json_core as sj;
use static_cell::StaticCell;

const BUF_SIZE: usize = 512;

pub enum StatusMessage {
    Identification,
    Buzz,
}

impl From<StatusMessage> for &str {
    fn from(value: StatusMessage) -> Self {
        match value {
            StatusMessage::Identification => "identification",
            StatusMessage::Buzz => "buzz",
        }
    }
}

pub struct Websocket {
    tx_channel: Sender<'static, NoopRawMutex, StatusMessage, 3>,
}

#[derive(Serialize)]
struct StatusMessageData<'a, 'b> {
    r#type: &'a str,
    id: &'b str,
}

pub enum WebsocketEvent {
    Connected,
    Disconnected,
    Command(LedCmd),
}

static CHANNEL: StaticCell<Channel<NoopRawMutex, StatusMessage, 3>> = StaticCell::new();

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
        let result = spawner.spawn(websocket_task(
            stack,
            rx_channel,
            tx_channel.receiver(),
            stack.hardware_address(),
        ));
        if let Err(e) = result {
            error!("Failed to spawn websocket task: {:?}", e);
        }
        res
    }

    pub async fn send_identify(&mut self) {
        info!("Sending identify message");
        self.tx_channel.send(StatusMessage::Identification).await;
    }
    pub async fn send_button_pushed(&mut self) {
        info!("Sending buzz message");
        self.tx_channel.send(StatusMessage::Buzz).await;
    }
}

fn format_status_message(
    buf: &mut [u8],
    status: StatusMessage,
    mac: &[u8],
) -> sj::ser::Result<usize> {
    let ident = StatusMessageData {
        r#type: status.into(),
        id: &format!(
            "{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
            mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
        ),
    };
    sj::to_slice(&ident, buf)
}

static RX_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();
static TX_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();
static CONNECT_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();
static FRAME_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();

#[embassy_executor::task]
pub async fn websocket_task(
    stack: Stack<'static>,
    rx_channel: Sender<'static, NoopRawMutex, WebsocketEvent, 3>,
    tx_channel: Receiver<'static, NoopRawMutex, StatusMessage, 3>,
    mac: HardwareAddress,
) {
    let rx_buffer = RX_BUFFER.init([0u8; BUF_SIZE]);
    let tx_buffer = TX_BUFFER.init([0u8; BUF_SIZE]);
    let connect_buffer = CONNECT_BUFFER.init([0u8; BUF_SIZE]);
    let frame_buffer = FRAME_BUFFER.init([0u8; BUF_SIZE]);

    let mut socket = TcpSocket::new(stack, rx_buffer, tx_buffer);
    let mut client = ws::WebSocketClient::new_client(Rng::new());
    let mut connected: bool = false;

    info!("Starting websocket task");
    loop {
        let remote = (Ipv4Address::new(192, 168, 66, 1), 8080);
        info!("Connecting to NBC TCP server...");
        let res = socket.connect(remote).await;
        if let Err(e) = res {
            error!("Failed to connect to TCP server: {:?}", e);
            continue;
        }
        info!("Connected to NBC TCP server");
        while socket.state() == embassy_net::tcp::State::Established {
            info!("Connecting to NBC websocket server...");
            let websocket_options = ws::WebSocketOptions {
                path: "/ws",
                host: "192.168.66.1",
                origin: "http://localhost:1337",
                sub_protocols: None,
                additional_headers: None,
            };
            let res = client.client_connect(&websocket_options, connect_buffer);
            let ws_key: ws::WebSocketKey;
            match res {
                Err(e) => {
                    error!("Failed to generate connect message: {:?}", e);
                    continue;
                }
                Ok((count, key)) => {
                    let res = socket.write(&connect_buffer[..count]).await;
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
                match select(socket.read(connect_buffer), tx_channel.receive()).await {
                    Either::First(x) => match x {
                        Ok(0) => {
                            info!("Socket is closed");
                            let res = client.close(
                                ws::WebSocketCloseStatusCode::NormalClosure,
                                None,
                                connect_buffer,
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
                            debug!(
                                "New TCP data: {:?} ({} bytes)",
                                str::from_utf8(&connect_buffer[..count])
                                    .unwrap_or("invalid_string"),
                                count
                            );
                            if !connected {
                                let res = client.client_accept(&ws_key, &connect_buffer[..count]);
                                match res {
                                    Ok(_) => {
                                        connected = true;
                                        info!("Connected to NBC websocket server");
                                        rx_channel.send(WebsocketEvent::Connected).await;
                                    }
                                    Err(e) => error!("Can not accept connection: {:?}", e),
                                }
                            } else if let Ok(x) = client.read(connect_buffer, frame_buffer) {
                                debug!("WS message parsing status: {:?}", x);
                                debug!(
                                    "Received websocket message {:?} ({} bytes)",
                                    str::from_utf8(&frame_buffer[..x.len_to])
                                        .unwrap_or("invalid text"),
                                    x.len_to
                                );
                                let ser = serde_json_core::from_slice::<MessageLedPattern>(
                                    &frame_buffer[..x.len_to],
                                );
                                match ser {
                                    Ok((data, _)) => match data.try_into() {
                                        Ok(cmd) => {
                                            rx_channel.send(WebsocketEvent::Command(cmd)).await
                                        }
                                        Err(e) => warn!("Failed to decode received command: {e}"),
                                    },
                                    Err(e) => {
                                        warn!("Failed to decode received json message: {e}")
                                    }
                                }
                            } else {
                                error!(
                                    "Failed to decode received message {:?}",
                                    &connect_buffer[..count]
                                );
                            }
                        }
                    },
                    Either::Second(x) => {
                        let mut buf = [0; 128];
                        match format_status_message(&mut buf, x, mac.as_bytes()) {
                            Ok(x) => {
                                let res = client.write(
                                    ws::WebSocketSendMessageType::Text,
                                    true,
                                    &buf[..x],
                                    connect_buffer,
                                );
                                debug!(
                                    "Sending new websocket message: {} ({} bytes)",
                                    str::from_utf8(&buf[..x]).expect("invalid string"),
                                    x
                                );
                                match res {
                                    Ok(count) => match socket.write(&connect_buffer[..count]).await
                                    {
                                        Err(e) => error!("Failed to send message: {:?}", e),
                                        _ => debug!("Message sent"),
                                    },
                                    Err(e) => error!("Failed to send message: {:?}", e),
                                }
                            }
                            Err(e) => error!("Failed to serialize message: {e}"),
                        }
                    }
                }
            }
        }
    }
}
