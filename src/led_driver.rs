use core::f64::consts::PI;

use crate::led_cmd::LedCmd;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::{
    blocking_mutex::raw::NoopRawMutex,
    channel::{Channel, Receiver, Sender},
};
use embassy_time::{Duration, Instant, Timer};
use esp_hal::{
    gpio::interconnect::PeripheralOutput,
    rmt::{PulseCode, Rmt},
};
use esp_hal_smartled::{self as sl, SmartLedsAdapterAsync, smart_led_buffer};
use libm::{cos, trunc};
use log::{error, info};
use smart_leds::{RGB, SmartLedsWriteAsync, brightness};
use static_cell::StaticCell;

const MAX_BRIGHTNESS_TABLE_LEN: usize = 70;
const WAVE_TICK_PERIOD_MS: u64 = 30;
const MIN_WAVE_PERIOD_MS: u64 = MAX_BRIGHTNESS_TABLE_LEN as u64 * WAVE_TICK_PERIOD_MS;
const MAX_BRIGHTNESS: u32 = 255;

pub struct Led {
    cmd_channel: Sender<'static, NoopRawMutex, LedCmd, 1>,
}

#[derive(Copy, Clone, Default, Debug)]
struct SubPatternProperties {
    brightness: u8,
    duration: Duration,
}

#[derive(Debug)]
struct PatternProperties {
    color: RGB<u8>,
    duration: Duration,
    brightness_table: [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN],
    brightness_table_len: usize,
}

fn compute_wave_table(period: Duration) -> [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN] {
    let mut result: [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN] =
        [Default::default(); MAX_BRIGHTNESS_TABLE_LEN];

    for (index, subpattern) in result.iter_mut().enumerate().take(MAX_BRIGHTNESS_TABLE_LEN) {
        let value: f64 = MAX_BRIGHTNESS as f64 / 2.0
            * (1.0
                + cos(PI * (2.0 * index as f64 - MAX_BRIGHTNESS_TABLE_LEN as f64)
                    / MAX_BRIGHTNESS_TABLE_LEN as f64));
        subpattern.brightness = trunc(value) as u8;
        subpattern.duration = Duration::from_millis(WAVE_TICK_PERIOD_MS);
    }

    /* Make sure that the last value is 0, and make it last long enough so that it match the target
     * period
     */
    result[MAX_BRIGHTNESS_TABLE_LEN - 1].brightness = 0;
    result[MAX_BRIGHTNESS_TABLE_LEN - 1].duration =
        period - Duration::from_millis(MAX_BRIGHTNESS_TABLE_LEN as u64 * WAVE_TICK_PERIOD_MS);

    result
}

impl PatternProperties {
    fn new(value: &LedCmd) -> Result<Self, &'static str> {
        match *value {
            LedCmd::Blink {
                color: c,
                duration: d,
                period: p,
                duty_cycle: dc,
            } => {
                if dc > 100 {
                    return Err("Invalid duty cycle");
                }
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
                    brightness_table_len: 2,
                })
            }
            LedCmd::Wave {
                color: c,
                duration: d,
                period: p,
                duty_cycle: dc,
            } => {
                if dc > 100 {
                    return Err("Invalid duty cycle");
                }
                if p < Duration::from_millis(MIN_WAVE_PERIOD_MS) {
                    return Err(
                        "Driver does not support wave period less than {MIN_WAVE_PERIOD_MS}",
                    );
                }
                let table = compute_wave_table(p);
                Ok(PatternProperties {
                    color: c,
                    duration: d,
                    brightness_table: table,
                    brightness_table_len: MAX_BRIGHTNESS_TABLE_LEN,
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
            .expect("Failed to start led task");
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
) -> LedCmd {
    if let Err(e) = controller
        .write(brightness([RGB::new(0, 0, 0)].into_iter(), 0))
        .await
    {
        error!("Failed to set led off: {:?}", e);
    }
    cmd_channel.receive().await
}

async fn execute_pattern(
    controller: &mut SmartLedsAdapterAsync<'static, 25>,
    cmd_channel: &Receiver<'static, NoopRawMutex, LedCmd, 1>,
    pattern: PatternProperties,
) -> LedCmd {
    let mut value = pattern.brightness_table[..pattern.brightness_table_len]
        .iter()
        .cycle();
    let start = Instant::now();

    loop {
        let subpattern = value
            .next()
            .expect("brightness_table_len > 0 guarantees cycle never ends");
        if let Err(e) = controller
            .write(brightness(
                [pattern.color].into_iter(),
                subpattern.brightness,
            ))
            .await
        {
            error!("Failed to set led: {:?}", e);
        }
        match select(cmd_channel.receive(), Timer::after(subpattern.duration)).await {
            Either::First(x) => return x,
            _ => {
                if pattern.duration.as_millis() > 0
                    && Instant::now().duration_since(start) > pattern.duration
                {
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
    if let Err(e) = controller
        .write(brightness([RGB::new(0, 0, 0)].into_iter(), 0))
        .await
    {
        error!("Failed to initialize led to off state: {:?}", e);
    }
    let mut cmd = cmd_channel.receive().await;
    loop {
        match cmd {
            LedCmd::Blink { .. } => match PatternProperties::new(&cmd) {
                Ok(pattern) => {
                    info!("Starting blink pattern");
                    cmd = execute_pattern(&mut controller, &cmd_channel, pattern).await;
                }
                Err(e) => error!("Received invalid blink command: {e}"),
            },
            LedCmd::Wave { .. } => match PatternProperties::new(&cmd) {
                Ok(pattern) => {
                    info!("Starting wave pattern");
                    cmd = execute_pattern(&mut controller, &cmd_channel, pattern).await;
                }
                Err(e) => error!("Received invalid wave command: {e}"),
            },
            LedCmd::Off => {
                info!("Shutting led off");
                cmd = execute_off(&mut controller, &cmd_channel).await
            }
        }
    }
}
