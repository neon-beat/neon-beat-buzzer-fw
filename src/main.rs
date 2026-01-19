#![no_std]
#![no_main]
#![deny(
    clippy::mem_forget,
    reason = "mem::forget is generally not safe to do with esp_hal types, especially those \
    holding buffers for the duration of a data transfer."
)]

mod button;
mod led_cmd;
mod led_driver;
mod network;
mod websocket;

use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_net::StackResources;
use embassy_sync::{blocking_mutex::raw::NoopRawMutex, channel::Channel};
use esp_hal::{clock::CpuClock, rmt::Rmt, rng::Rng, time::Rate, timer::timg::TimerGroup};
use esp_radio::Controller;
use log::info;
use static_cell::StaticCell;

use crate::{
    button::button_task,
    led_driver::Led,
    network::{connection, net_task},
    websocket::{Websocket, WebsocketEvent},
};

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    info!("{}", info);
    loop {}
}

extern crate alloc;

esp_bootloader_esp_idf::esp_app_desc!();

static WS_CHANNEL: StaticCell<Channel<NoopRawMutex, WebsocketEvent, 3>> = StaticCell::new();
static BUTTON_CHANNEL: StaticCell<Channel<NoopRawMutex, bool, 1>> = StaticCell::new();
static RADIO_CELL: StaticCell<Controller<'static>> = StaticCell::new();
static RESOURCES_CELL: StaticCell<StackResources<3>> = StaticCell::new();

#[esp_rtos::main]
async fn main(spawner: Spawner) -> ! {
    esp_println::logger::init_logger_from_env();
    let config = esp_hal::Config::default().with_cpu_clock(CpuClock::max());
    let peripherals = esp_hal::init(config);

    esp_alloc::heap_allocator!(#[unsafe(link_section = ".dram2_uninit")] size: 66320);

    let timg0 = TimerGroup::new(peripherals.TIMG0);
    let sw_interrupt =
        esp_hal::interrupt::software::SoftwareInterruptControl::new(peripherals.SW_INTERRUPT);
    esp_rtos::start(timg0.timer0, sw_interrupt.software_interrupt0);

    let radio_init =
        RADIO_CELL.init(esp_radio::init().expect("Failed to initialize Wi-Fi/BLE controller"));
    let resources = RESOURCES_CELL.init(StackResources::<3>::new());
    let button_channel: &'static mut _ = BUTTON_CHANNEL.init(Channel::new());
    let ws_channel: &'static mut _ = WS_CHANNEL.init(Channel::new());

    let (wifi_controller, wifi_interfaces) =
        esp_radio::wifi::new(radio_init, peripherals.WIFI, Default::default())
            .expect("Failed to initialize Wi-Fi controller");
    info!("Buzzer initialized");
    let config = embassy_net::Config::dhcpv4(Default::default());
    let rng = Rng::new();
    let seed = (rng.random() as u64) << 32 | rng.random() as u64;
    let (stack, runner) = embassy_net::new(wifi_interfaces.sta, config, resources, seed);
    let rmt =
        Rmt::new(peripherals.RMT, Rate::from_mhz(80)).expect("Failed to initialize RMT controller");
    let mut led = Led::new(&spawner, rmt.into_async(), peripherals.GPIO3);
    spawner
        .spawn(connection(wifi_controller))
        .expect("Failed to spawn connection task");
    spawner
        .spawn(net_task(runner))
        .expect("Failed to spawn net_task");

    spawner
        .spawn(button_task(
            peripherals.GPIO2.into(),
            button_channel.sender(),
        ))
        .expect("Failed to spawn button_task");

    let mut ws = Websocket::new(&spawner, stack, ws_channel.sender());

    loop {
        match select(ws_channel.receive(), button_channel.receive()).await {
            Either::First(WebsocketEvent::Connected) => {
                info!("Buzzer is now connected to NBC");
                ws.send_identify().await;
            }
            Either::First(WebsocketEvent::Disconnected) => {
                info!("Buzzer is now disconnected from NBC")
            }
            Either::First(WebsocketEvent::Command(cmd)) => {
                led.set(cmd).await;
            }
            Either::Second(_) => {
                ws.send_button_pushed().await;
            }
        }
    }
}
