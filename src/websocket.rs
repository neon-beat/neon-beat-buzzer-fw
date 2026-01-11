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

// Use static allocation instead of stack allocation for buffers to save stack space:
// - Reduces peak stack usage during message processing (saves ~1KB per message)
// - Improves determinism by eliminating per-message allocations
// - Prevents stack overflow risk on resource-constrained embedded systems
// Trade-off: Fixed memory cost at startup, but much safer and more efficient at runtime
static RX_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();
static TX_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();
static CONNECT_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();
static FRAME_BUFFER: StaticCell<[u8; BUF_SIZE]> = StaticCell::new();

#[embassy_executor::task]
pub async fn websocket_task(
    stack: Stack<'static>,
    rx_channel: Sender<'static, NoopRawMutex, WebsocketEvent, 3>,
    tx_channel: Receiver<'static, NoopRawMutex, sj::Value, 3>,
) {
    // Initialize static buffers once at task startup
    // These are reused throughout the connection lifetime, eliminating repeated allocations
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
        let res = socket.connect(remote).await;
        if let Err(e) = res {
            error!("Failed to connect to TCP server: {:?}", e);
            continue;
        }
        info!("Connected to TCP server");
        while socket.state() == embassy_net::tcp::State::Established {
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
                            if !connected {
                                let res = client.client_accept(&ws_key, &connect_buffer[..count]);
                                match res {
                                    Ok(_) => {
                                        connected = true;
                                        info!("Connected to WS server");
                                        rx_channel.send(WebsocketEvent::Connected).await;
                                    }
                                    Err(e) => error!("Can not accept connection: {:?}", e),
                                }
                            } else if let Ok(x) = client.read(connect_buffer, frame_buffer) {
                                let message = str::from_utf8(&frame_buffer[..x.len_to]);
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
                                    &connect_buffer[..count]
                                );
                            }
                        }
                    },
                    Either::Second(x) => {
                        let message_str = x.to_string();
                        info!("Sending message {message_str:?}");
                        let res = client.write(
                            ws::WebSocketSendMessageType::Text,
                            true,
                            message_str.as_bytes(),
                            connect_buffer,
                        );
                        match res {
                            Ok(count) => match socket.write(&connect_buffer[..count]).await {
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
