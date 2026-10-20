pub const EXECUTABLE_RESERVE: usize = 4096;

pub fn platform() -> &'static str {
    #[cfg(windows)]
    {
        "win32"
    }
    #[cfg(target_os = "linux")]
    {
        "linux"
    }
    #[cfg(all(not(windows), not(target_os = "linux")))]
    {
        "darwin"
    }
}

pub fn argv_budget(platform: &str) -> usize {
    if platform == "win32" {
        32_000
    } else {
        256 * 1024
    }
}

pub fn argv_cost(argument: &str, platform: &str) -> usize {
    let length = argument.encode_utf16().count();
    if platform != "win32" {
        return length + 1;
    }
    let mut quotes = 0;
    let mut doubled = 0;
    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quotes += 1;
                doubled += backslashes;
                backslashes = 0;
            }
            _ => backslashes = 0,
        }
    }
    length + 3 + quotes + doubled + backslashes
}

pub fn command_line_overflow(args: &[String], platform: &str) -> Option<String> {
    let budget = argv_budget(platform);
    let length = EXECUTABLE_RESERVE
        + args
            .iter()
            .map(|arg| argv_cost(arg, platform))
            .sum::<usize>();
    if length > budget {
        return Some(format!(
            "the ripgrep command line would be {length} characters, over the {budget}-character limit on {platform}"
        ));
    }
    if platform == "linux" {
        let longest = args.iter().map(String::len).max().unwrap_or(0);
        if longest >= 131_072 {
            return Some(format!(
                "one ripgrep argument is {longest} bytes, over Linux's 131071-byte per-argument limit"
            ));
        }
    }
    None
}

pub fn room_after(fixed: &[String], budget: usize, platform: &str) -> usize {
    budget.saturating_sub(
        EXECUTABLE_RESERVE
            + fixed
                .iter()
                .map(|arg| argv_cost(arg, platform))
                .sum::<usize>(),
    )
}

pub fn pack_argv<T>(
    items: impl IntoIterator<Item = T>,
    cost_of: impl Fn(&T) -> usize,
    room: usize,
) -> Vec<Vec<T>> {
    let mut batches: Vec<Vec<T>> = Vec::new();
    let mut used = 0usize;
    for item in items {
        let cost = cost_of(&item);
        if let Some(batch) = batches.last_mut()
            && used.saturating_add(cost) <= room
        {
            batch.push(item);
            used += cost;
        } else {
            batches.push(vec![item]);
            used = cost;
        }
    }
    batches
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_cost_includes_quoting_and_utf16() {
        let argument = "a\\\"b\\";
        let cost = argv_cost(argument, "win32");
        assert_eq!(cost, 11);
        assert_eq!(argv_cost("😀", "win32"), 5);
    }

    #[test]
    fn linux_rejects_single_argument_at_byte_limit() {
        let args = vec!["é".repeat(65_536)];
        let overflow = command_line_overflow(&args, "linux");
        assert!(overflow.is_some());
        assert!(command_line_overflow(&args, "darwin").is_none());
    }

    #[test]
    fn packing_keeps_order_and_oversized_items() {
        let items = [2, 3, 6, 1];
        let batches = pack_argv(items, |cost| *cost, 5);
        assert_eq!(batches, vec![vec![2, 3], vec![6], vec![1]]);
    }
}
