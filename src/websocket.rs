use core::num::ParseIntError;

use crate::led_cmd::{LedCmd, MessageLedPattern};
use alloc::format;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::{HardwareAddress, Stack, tcp::TcpSocket};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Channel, Receiver, Sender},
};
use embassy_time::Duration;
use embedded_websocket as ws;
use esp_hal::rng::Rng;
use log::{debug, error, info, warn};
use serde::Serialize;
use serde_json_core as sj;
use static_cell::StaticCell;

const BUF_SIZE: usize = 512;
const WEBSOCKET_SERVER_PORT: Result<u16, ParseIntError> =
    u16::from_str_radix(env!("NBC_BACKEND_PORT"), 10);

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

async fn websocket_handshake<'a>(
    client: &mut ws::WebSocketClient<Rng>,
    socket: &mut TcpSocket<'a>,
    buffer: &mut [u8],
) -> Result<ws::WebSocketKey, &'static str> {
    let websocket_options = ws::WebSocketOptions {
        path: "/ws",
        host: "",
        origin: "http://localhost:1337",
        sub_protocols: None,
        additional_headers: None,
    };
    let (count, key) = client
        .client_connect(&websocket_options, buffer)
        .map_err(|_| "failed to generate connect message")?;
    socket
        .write(&buffer[..count])
        .await
        .map_err(|_| "failed to write handshake")?;
    Ok(key)
}

async fn send_status_message<'a>(
    client: &mut ws::WebSocketClient<Rng>,
    socket: &mut TcpSocket<'a>,
    buffer: &mut [u8],
    status: StatusMessage,
    mac: &[u8],
) {
    let mut msg_buf = [0u8; 128];
    let prepare_result: Result<usize, &'static str> = (|| {
        let len =
            format_status_message(&mut msg_buf, status, mac).map_err(|_| "failed to serialize")?;
        debug!(
            "Sending new websocket message: {} ({} bytes)",
            core::str::from_utf8(&msg_buf[..len]).unwrap_or("<invalid utf8>"),
            len
        );
        let count = client
            .write(
                ws::WebSocketSendMessageType::Text,
                true,
                &msg_buf[..len],
                buffer,
            )
            .map_err(|_| "failed to encode websocket frame")?;
        Ok(count)
    })();

    match prepare_result {
        Ok(count) => {
            if let Err(e) = socket.write(&buffer[..count]).await {
                error!("Failed to send message: {:?}", e);
            } else {
                debug!("Message sent");
            }
        }
        Err(e) => error!("Failed to prepare message: {e}"),
    }
}

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
    socket.set_timeout(Some(Duration::from_secs(8)));
    socket.set_keep_alive(Some(Duration::from_secs(5)));

    info!("Starting websocket task");
    loop {
        if !stack.is_config_up() {
            info!("Waiting for network configuration...");
            stack.wait_config_up().await;
        }
        info!("Network configuration done");
        let Some(config) = stack.config_v4() else {
            error!("Missing network configuration after wait_config_up");
            continue;
        };
        let Some(server_address) = config.gateway else {
            error!("Missing gateway address in network configuration");
            continue;
        };
        let Ok(port) = WEBSOCKET_SERVER_PORT else {
            error!("Invalid server port configuration");
            continue;
        };
        let remote = (server_address, port);
        info!("Connecting to NBC TCP server...");
        let res = socket.connect(remote).await;
        if let Err(e) = res {
            error!("Failed to connect to TCP server: {:?}", e);
            continue;
        }
        info!("Connected to NBC TCP server");
        while socket.state() == embassy_net::tcp::State::Established {
            info!("Connecting to NBC websocket server...");
            let ws_key = match websocket_handshake(&mut client, &mut socket, connect_buffer).await {
                Ok(key) => key,
                Err(e) => {
                    error!("WebSocket handshake failed: {e}");
                    continue;
                }
            };
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
                            socket.close();
                            connected = false;
                            if let Err(e) = client.close(
                                ws::WebSocketCloseStatusCode::EndpointUnavailable,
                                None,
                                connect_buffer,
                            ) {
                                warn!("Failed to close websocket client: {:?}", e);
                            }
                            rx_channel.send(WebsocketEvent::Disconnected).await;
                            break;
                        }
                        Ok(count) => {
                            debug!(
                                "New TCP data: {:?} ({} bytes)",
                                str::from_utf8(&connect_buffer[..count])
                                    .unwrap_or("invalid_string"),
                                count
                            );
                            if !connected {
                                match client.client_accept(&ws_key, &connect_buffer[..count]) {
                                    Ok(_) => {
                                        connected = true;
                                        info!("Connected to NBC websocket server");
                                        rx_channel.send(WebsocketEvent::Connected).await;
                                    }
                                    Err(e) => error!("Can not accept connection: {:?}", e),
                                }
                            } else if let Ok(ws_frame) = client.read(connect_buffer, frame_buffer) {
                                debug!("WS message parsing status: {:?}", ws_frame);
                                debug!(
                                    "Received websocket message {:?} ({} bytes)",
                                    str::from_utf8(&frame_buffer[..ws_frame.len_to])
                                        .unwrap_or("invalid text"),
                                    ws_frame.len_to
                                );
                                let cmd = serde_json_core::from_slice::<MessageLedPattern>(
                                    &frame_buffer[..ws_frame.len_to],
                                )
                                .map_err(|e| warn!("Failed to decode JSON message: {e}"))
                                .and_then(|(data, _)| {
                                    data.try_into()
                                        .map_err(|e| warn!("Failed to decode command: {e}"))
                                });
                                if let Ok(cmd) = cmd {
                                    rx_channel.send(WebsocketEvent::Command(cmd)).await;
                                }
                            } else {
                                error!(
                                    "Failed to decode received message {:?}",
                                    &connect_buffer[..count]
                                );
                            }
                        }
                    },
                    Either::Second(status) => {
                        send_status_message(
                            &mut client,
                            &mut socket,
                            connect_buffer,
                            status,
                            mac.as_bytes(),
                        )
                        .await;
                    }
                }
            }
        }
    }
}
