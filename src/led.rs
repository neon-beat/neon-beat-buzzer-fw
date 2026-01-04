use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{
    gpio::interconnect::PeripheralOutput,
    rmt::{PulseCode, Rmt},
};
use esp_hal_smartled::{self as sl, SmartLedsAdapterAsync, smart_led_buffer};
use log::{error, info};
pub use smart_leds::RGB;
use smart_leds::{SmartLedsWriteAsync, brightness};
use static_cell::StaticCell;

const MAX_BRIGHTNESS_TABLE_LEN: usize = 50;
pub enum LedCmd {
    Off,
    Blink {
        color: RGB<u8>,
        duration: Duration,
        period: Duration,
        duty_cycle: u8,
    },
}

pub struct Led {
    cmd_channel: Sender<'static, NoopRawMutex, LedCmd, 1>,
}

#[derive(Copy, Clone, Default)]
struct SubPatternProperties {
    brightness: u8,
    duration: Duration,
}

struct PatternProperties {
    color: RGB<u8>,
    duration: Duration,
    brightness_table: [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN],
    brighntess_table_len: usize,
}

impl PatternProperties {
    fn new(value: &LedCmd) -> Result<Self, &'static str> {
        match *value {
            LedCmd::Blink {
                color: _,
                duration: _,
                period: _,
                duty_cycle: dc,
            } if dc > 100 => Err("Invalid duty cycle"),
            LedCmd::Blink {
                color: c,
                duration: d,
                period: p,
                duty_cycle: dc,
            } => {
                let mut table: [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN] =
                    [Default::default(); MAX_BRIGHTNESS_TABLE_LEN];
                table[0].brightness = 100;
                table[0].duration = p * dc.into() / 100;
                table[1].brightness = 0;
                table[1].duration = p - table[0].duration;
                Ok(PatternProperties {
                    color: c,
                    duration: d,
                    brightness_table: table,
                    brighntess_table_len: 2,
                })
            }
            _ => Err("Unsupported pattern"),
        }
    }
}

static LED_CMD_CHANNEL: StaticCell<Channel<NoopRawMutex, LedCmd, 1>> = StaticCell::new();
static ADAPTER_BUFFER: StaticCell<[PulseCode; 25]> = StaticCell::new();

impl Led {
    pub fn new<O>(spawner: &Spawner, rmt: Rmt<'static, esp_hal::Async>, gpio: O) -> Self
    where
        O: PeripheralOutput<'static>,
    {
        let channel: &'static mut _ = LED_CMD_CHANNEL.init(Channel::new());
        let buffer: &'static mut _ = ADAPTER_BUFFER.init(smart_led_buffer!(1));
        spawner
            .spawn(led_task(
                sl::SmartLedsAdapterAsync::new(rmt.channel0, gpio, buffer),
                channel.receiver(),
            ))
            .ok();
        Led {
            cmd_channel: channel.sender(),
        }
    }

    pub async fn set(&mut self, cmd: LedCmd) {
        self.cmd_channel.send(cmd).await
    }
}

async fn execute_off(
    controller: &mut SmartLedsAdapterAsync<'static, 25>,
    cmd_channel: &Receiver<'static, NoopRawMutex, LedCmd, 1>,
) {
    controller
        .write(brightness([RGB::new(0, 0, 0)].into_iter(), 0))
        .await
        .expect("Failed to set led off");
    cmd_channel.receive().await;
}

async fn execute_blink(
    controller: &mut SmartLedsAdapterAsync<'static, 25>,
    cmd_channel: &Receiver<'static, NoopRawMutex, LedCmd, 1>,
    pattern: PatternProperties,
) -> LedCmd {
    let mut value = pattern.brightness_table[..pattern.brighntess_table_len]
        .iter()
        .cycle();
    let start = Instant::now();

    loop {
        let subpattern = value.next().unwrap();
        controller
            .write(brightness(
                [pattern.color].into_iter(),
                subpattern.brightness,
            ))
            .await
            .expect("Failed to set led");
        match select(cmd_channel.receive(), Timer::after(subpattern.duration)).await {
            Either::First(x) => return x,
            _ => {
                if Instant::now().duration_since(start) > pattern.duration {
                    info!("Pattern expired");
                    return LedCmd::Off;
                }
            }
        }
    }
}

#[embassy_executor::task]
async fn led_task(
    mut controller: SmartLedsAdapterAsync<'static, 25>,
    cmd_channel: Receiver<'static, NoopRawMutex, LedCmd, 1>,
) {
    controller
        .write(brightness([RGB::new(0, 0, 0)].into_iter(), 0))
        .await
        .expect("Failed to set led off");
    let mut cmd = cmd_channel.receive().await;
    loop {
        match cmd {
            LedCmd::Blink { .. } => {
                let pattern = PatternProperties::new(&cmd);
                if let Err(e) = pattern {
                    error!("Received invalid blink command: {e}");
                } else {
                    info!("Start blinking");
                    cmd = execute_blink(&mut controller, &cmd_channel, pattern.unwrap()).await;
                }
            }
            LedCmd::Off => {
                info!("Received Off command");
                execute_off(&mut controller, &cmd_channel).await
            }
        }
    }
}
