pub fn format_duration(ms: f64) -> String {
    if ms < 1000.0 { format!("{ms}ms") } else if ms < 60000.0 { format!("{:.1}s", ms / 1000.0) } else if ms < 3_600_000.0 { format!("{}m {}s", (ms / 60000.0).floor(), ((ms % 60000.0) / 1000.0).floor()) } else if ms < 86_400_000.0 { format!("{}h {}m", (ms / 3_600_000.0).floor(), ((ms % 3_600_000.0) / 60000.0).floor()) } else { format!("{}d {}h", (ms / 86_400_000.0).floor(), ((ms % 86_400_000.0) / 3_600_000.0).floor()) }
}
