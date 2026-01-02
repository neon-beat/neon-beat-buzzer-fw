#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod network;
use crate::websocket::WebsocketEvent;

use self::network::connection;
use self::network::net_task;
mod button;
use self::button::button_task;
mod websocket;
use self::websocket::Websocket;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::StackResources;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::Channel;
use embassy_time::{Duration, Timer};
use esp_hal::clock::CpuClock;
use esp_hal::rng::Rng;
use esp_hal::timer::timg::TimerGroup;
use esp_radio::Controller;
use log::info;
use static_cell::StaticCell;

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    info!("{}", info);
    loop {}
}

extern crate alloc;
//
// When you are okay with using a nightly compiler it's better to use https://docs.rs/static_cell/2.1.0/static_cell/macro.make_static.html
macro_rules! mk_static {
    ($t:ty,$val:expr) => {{
        static STATIC_CELL: static_cell::StaticCell<$t> = static_cell::StaticCell::new();
        #[deny(unused_attributes)]
        let x = STATIC_CELL.uninit().write($val);
        x
    }};
}

esp_bootloader_esp_idf::esp_app_desc!();

static WS_CHANNEL: StaticCell<Channel<NoopRawMutex, WebsocketEvent, 3>> = StaticCell::new();
static BUTTON_CHANNEL: StaticCell<Channel<NoopRawMutex, bool, 1>> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    // generator version: 1.0.0

    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[unsafe(link_section = ".dram2_uninit")] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let radio_init = &*mk_static!(
        Controller<'static>,
        esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller")
    );
    let (wifi_controller, wifi_interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");

    info!("Buzzer initialized");
    let config = embassy_net::Config::dhcpv4(Default::default());
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, runner) = embassy_net::new(
        wifi_interfaces.sta,
        config,
        mk_static!(StackResources<3>, StackResources::<3>::new()),
        seed,
    );
    spawner.spawn(connection(wifi_controller)).ok();
    spawner.spawn(net_task(runner)).ok();
    let button_channel: &'static mut _ = BUTTON_CHANNEL.init(Channel::new());
    spawner
        .spawn(button_task(
            peripherals.GPIO2.into(),
            button_channel.sender(),
        ))
        .ok();

    loop {
        if stack.is_link_up() {
            break;
        }
        Timer::after(Duration::from_millis(500)).await;
    }

    info!("Waiting to get IP address...");
    while stack.config_v4().is_none() {
        Timer::after_secs(1).await;
    }
    let address = stack.config_v4().unwrap().address;
    info!("Got IP: {address}");

    let ws_channel: &'static mut _ = WS_CHANNEL.init(Channel::new());
    let mut ws = Websocket::new(&spawner, stack, ws_channel.sender());
    let mac = esp_hal::efuse::Efuse::mac_address();

    loop {
        match select(ws_channel.receive(), button_channel.receive()).await {
            Either::First(WebsocketEvent::Connected) => {
                info!("Buzzer is now connected to NBC");
                ws.send_identify(&mac).await;
            }
            Either::First(WebsocketEvent::Disconnected) => {
                info!("Buzzer is now disconnected from NBC")
            }
            Either::Second(_) => {
                ws.send_button_pushed(&mac).await;
            }
        }
    }
}
