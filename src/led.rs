use core::f64::consts::PI;
use embassy_executor::Spawner;
use embassy_futures::select::{Either, select};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::channel::{Channel, Receiver, Sender};
use embassy_time::{Duration, Instant, Ticker, Timer};
use esp_hal::{
    gpio::interconnect::PeripheralOutput,
    rmt::{PulseCode, Rmt},
};
use esp_hal_smartled::{self as sl, SmartLedsAdapterAsync, smart_led_buffer};
use libm::{cos, fabs, fmod, trunc};
use log::{error, info};
use serde_json as sj;
pub use smart_leds::RGB;
use smart_leds::{SmartLedsWriteAsync, brightness};
use static_cell::StaticCell;

const MAX_BRIGHTNESS_TABLE_LEN: usize = 50;
const MAX_BRIGHTNESS: u32 = 255;

#[derive(Debug)]
pub enum LedCmd {
    Off,
    Blink {
        color: RGB<u8>,
        duration: Duration,
        period: Duration,
        duty_cycle: u8,
    },
    Wave {
        color: RGB<u8>,
        duration: Duration,
        period: Duration,
        duty_cycle: u8,
    },
}

pub struct Led {
    cmd_channel: Sender<'static, NoopRawMutex, LedCmd, 1>,
}

#[derive(Copy, Clone, Default, Debug)]
struct SubPatternProperties {
    brightness: u8,
    duration: Duration,
}

fn hsv_to_rgb(h: f64, s: f64, v: f64) -> RGB<u8> {
    let h = match h {
        h if h < 0.0 => 360.0 + h,
        _ => fmod(h, 360.0),
    };
    let c = v * s;
    let x = c * (1.0 - fabs(fmod(h / 60.0, 2.0) - 1.0));
    let m = v - c;

    let (r_tmp, g_tmp, b_tmp) = match h {
        0.0..60.0 => (c, x, 0.0),
        60.0..120.0 => (x, c, 0.0),
        120.0..180.0 => (0.0, c, x),
        180.0..240.0 => (0.0, x, c),
        240.0..300.0 => (x, 0.0, c),
        300.0..360.0 => (c, 0.0, x),
        _ => panic!("Invalid h value !"),
    };

    RGB::new(
        ((r_tmp + m) * 255.0) as u8,
        ((g_tmp + m) * 255.0) as u8,
        ((b_tmp + m) * 255.0) as u8,
    )
}

fn parse_generic_pattern_cmd(
    pattern_type: &str,
    details: &sj::Value,
) -> Result<LedCmd, &'static str> {
    let color = match details.get("color") {
        Some(c) if c.is_object() => c,
        None => return Err("No color object in blink message"),
        _ => return Err("Invalid json type for blink message"),
    };

    let hue = match color.get("h") {
        Some(h) if h.is_f64() => h.as_f64().unwrap(),
        None => return Err("No hue in blink color"),
        _ => return Err("Invalid json type for hue in blink message"),
    };
    let saturation = match color.get("s") {
        Some(s) if s.is_f64() => s.as_f64().unwrap(),
        None => return Err("No saturation in blink color"),
        _ => return Err("Invalid json type for saturation in blink message"),
    };
    let value = match color.get("v") {
        Some(v) if v.is_f64() => v.as_f64().unwrap(),
        None => return Err("No value in blink color"),
        _ => return Err("Invalid json type for value in blink message"),
    };
    let dc = match details.get("dc") {
        Some(d) if d.is_f64() => d.as_f64().unwrap(),
        None => return Err("No duty cycle in blink message"),
        _ => return Err("Invalid json type for duty cycle in blink message"),
    };
    let period = match details.get("period_ms") {
        Some(p) if p.is_u64() => p.as_u64().unwrap(),
        None => return Err("No period in blink message"),
        _ => return Err("Invalid json type for period in blink message"),
    };
    let duration = match details.get("duration_ms") {
        Some(d) if d.is_u64() => d.as_u64().unwrap(),
        None => return Err("No duration in blink message"),
        _ => return Err("Invalid json type for duration in blink message"),
    };

    match pattern_type {
        "blink" => Ok(LedCmd::Blink {
            color: hsv_to_rgb(hue, saturation, value),
            duration: Duration::from_millis(duration),
            period: Duration::from_millis(period),
            duty_cycle: (dc * 100.0) as u8,
        }),
        "wave" => Ok(LedCmd::Wave {
            color: hsv_to_rgb(hue, saturation, value),
            duration: Duration::from_millis(duration),
            period: Duration::from_millis(period),
            duty_cycle: (dc * 100.0) as u8,
        }),
        _ => panic!("Invalid internal pattern type"),
    }
}

impl TryFrom<sj::Value> for LedCmd {
    type Error = &'static str;

    fn try_from(value: sj::Value) -> Result<Self, Self::Error> {
        let pattern = match value.get("pattern") {
            Some(p) if p.is_object() => p,
            None => return Err("No pattern object in led message"),
            _ => return Err("Invalid json type for pattern"),
        };
        let pattern_type = match pattern.get("type") {
            Some(t) if t.is_string() => t.as_str().unwrap(),
            None => return Err("No type object in led message"),
            _ => return Err("Invalid json type for type"),
        };
        let details = match pattern.get("details") {
            Some(d) if d.is_object() => d,
            None => return Err("No details object in led message"),
            _ => return Err("Invalid json type for details"),
        };
        match pattern_type {
            "blink" | "wave" => parse_generic_pattern_cmd(pattern_type, details),
            _ => Err("Unknown pattern"),
        }
    }
}

#[derive(Debug)]
struct PatternProperties {
    color: RGB<u8>,
    duration: Duration,
    brightness_table: [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN],
    brighntess_table_len: usize,
}

fn compute_wave_table(
    _period: Duration,
    _duty_cycle: u8,
) -> [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN] {
    let mut result: [SubPatternProperties; MAX_BRIGHTNESS_TABLE_LEN] =
        [Default::default(); MAX_BRIGHTNESS_TABLE_LEN];

    for (index, subpattern) in result.iter_mut().enumerate().take(MAX_BRIGHTNESS_TABLE_LEN) {
        let value: f64 = MAX_BRIGHTNESS as f64 / 2.0
            * (1.0
                + cos(PI * (2.0 * index as f64 - MAX_BRIGHTNESS_TABLE_LEN as f64)
                    / MAX_BRIGHTNESS_TABLE_LEN as f64));
        subpattern.brightness = trunc(value) as u8;
        subpattern.duration = Duration::from_millis(50);
    }

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
                    brighntess_table_len: 2,
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
                let table = compute_wave_table(p, dc);
                Ok(PatternProperties {
                    color: c,
                    duration: d,
                    brightness_table: table,
                    brighntess_table_len: MAX_BRIGHTNESS_TABLE_LEN,
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

async fn execute_wave(
    controller: &mut SmartLedsAdapterAsync<'static, 25>,
    cmd_channel: &Receiver<'static, NoopRawMutex, LedCmd, 1>,
    pattern: PatternProperties,
) -> LedCmd {
    let mut value = pattern.brightness_table[..pattern.brighntess_table_len]
        .iter()
        .cycle();
    let start = Instant::now();
    let mut ticker = Ticker::every(pattern.brightness_table[0].duration);

    loop {
        let subpattern = value.next().unwrap();
        controller
            .write(brightness(
                [pattern.color].into_iter(),
                subpattern.brightness,
            ))
            .await
            .expect("Failed to set led");
        match select(cmd_channel.receive(), ticker.next()).await {
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
                    info!("Starting blink pattern");
                    cmd = execute_blink(&mut controller, &cmd_channel, pattern.unwrap()).await;
                }
            }
            LedCmd::Wave { .. } => {
                let pattern = PatternProperties::new(&cmd);
                if let Err(e) = pattern {
                    error!("Received invalid wave command: {e}");
                } else {
                    info!("Starting wave pattern");
                    cmd = execute_wave(&mut controller, &cmd_channel, pattern.unwrap()).await;
                }
            }
            LedCmd::Off => {
                info!("Shutting led off");
                execute_off(&mut controller, &cmd_channel).await
            }
        }
    }
}
