/// A thinking level the host session options accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SenpiThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl SenpiThinkingLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "off" => Some(Self::Off),
            "minimal" => Some(Self::Minimal),
            "low" => Some(Self::Low),
            "medium" => Some(Self::Medium),
            "high" => Some(Self::High),
            "xhigh" => Some(Self::Xhigh),
            "max" => Some(Self::Max),
            _ => None,
        }
    }
}

/// omo.json reasoning spells the disabled level `none` where senpi spells it `off`. Unknown tokens
/// (and `auto`) are dropped so a child keeps the harness default.
pub fn as_senpi_thinking_level(reasoning: Option<&str>) -> Option<SenpiThinkingLevel> {
    let reasoning = reasoning?;
    let normalized = if reasoning == "none" {
        "off"
    } else {
        reasoning
    };
    SenpiThinkingLevel::parse(normalized)
}
