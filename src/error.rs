use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternError {
    InvalidDutyCycle,
    MissingDetails,
    InvalidPatternType,
    WavePeriodTooShort { min_ms: u64 },
    UnsupportedCommand,
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidDutyCycle => write!(f, "invalid duty cycle"),
            Self::MissingDetails => write!(f, "missing pattern details"),
            Self::InvalidPatternType => write!(f, "invalid pattern type"),
            Self::WavePeriodTooShort { min_ms } => {
                write!(f, "wave period too short (minimum: {min_ms}ms)")
            }
            Self::UnsupportedCommand => write!(f, "unsupported command"),
        }
    }
}
