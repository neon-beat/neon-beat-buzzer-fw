use embassy_time::Duration;
use libm::{fabsf, fmodf};
use log::warn;
use serde::Deserialize;
use smart_leds::RGB;

use crate::error::PatternError;

#[derive(Deserialize, Debug)]
struct MessageLedColor {
    h: f32,
    s: f32,
    v: f32,
}

#[derive(Deserialize, Debug)]
struct MessageLedDetails {
    duration_ms: u32,
    period_ms: u32,
    dc: f32,
    color: MessageLedColor,
}

#[derive(Deserialize, Debug)]
struct MessageLedType<'a> {
    r#type: &'a str,
    details: Option<MessageLedDetails>,
}

#[derive(Deserialize, Debug)]
pub struct MessageLedPattern<'a> {
    #[serde(borrow)]
    pattern: MessageLedType<'a>,
}

#[derive(Debug, Copy, Clone)]
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

fn hsv_to_rgb(h: f32, s: f32, v: f32) -> RGB<u8> {
    let h = match h {
        h if h < 0.0 => 360.0 + h,
        _ => fmodf(h, 360.0),
    };
    let c = v * s;
    let x = c * (1.0 - fabsf(fmodf(h / 60.0, 2.0) - 1.0));
    let m = v - c;

    let (r_tmp, g_tmp, b_tmp) = match h {
        0.0..60.0 => (c, x, 0.0),
        60.0..120.0 => (x, c, 0.0),
        120.0..180.0 => (0.0, c, x),
        180.0..240.0 => (0.0, x, c),
        240.0..300.0 => (x, 0.0, c),
        300.0..360.0 => (c, 0.0, x),
        _ => {
            warn!("Invalid h value!");
            (0.0, 0.0, 0.0)
        }
    };

    RGB::new(
        ((r_tmp + m) * 255.0) as u8,
        ((g_tmp + m) * 255.0) as u8,
        ((b_tmp + m) * 255.0) as u8,
    )
}

impl TryFrom<MessageLedPattern<'_>> for LedCmd {
    type Error = PatternError;
    fn try_from(value: MessageLedPattern<'_>) -> Result<Self, Self::Error> {
        if value.pattern.r#type == "off" {
            return Ok(LedCmd::Off);
        }
        let details = value.pattern.details.ok_or(PatternError::MissingDetails)?;
        if !(0.0..=1.0).contains(&details.dc) {
            return Err(PatternError::InvalidDutyCycle);
        }
        let rgb = hsv_to_rgb(details.color.h, details.color.s, details.color.v);
        match value.pattern.r#type {
            "blink" => Ok(LedCmd::Blink {
                color: rgb,
                duration: Duration::from_millis(details.duration_ms.into()),
                period: Duration::from_millis(details.period_ms.into()),
                duty_cycle: (details.dc * 100.0) as u8,
            }),
            "wave" => Ok(LedCmd::Wave {
                color: rgb,
                duration: Duration::from_millis(details.duration_ms.into()),
                period: Duration::from_millis(details.period_ms.into()),
                duty_cycle: (details.dc * 100.0) as u8,
            }),
            _ => Err(PatternError::InvalidPatternType),
        }
    }
}
