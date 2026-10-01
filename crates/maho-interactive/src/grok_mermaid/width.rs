//! Port of the `grok-mermaid` `width.ts`.
use unicode_segmentation::UnicodeSegmentation;

use super::width_data::WIDTHS;

const VS16: u32 = 0xfe0f;

pub fn code_point_width(cp: u32) -> u8 {
    let mut lo = 0usize;
    let mut hi = WIDTHS.len().saturating_sub(1);
    while lo <= hi {
        let mid = (lo + hi) / 2;
        let (run_lo, run_hi, width) = WIDTHS[mid];
        if cp < run_lo {
            if mid == 0 {
                break;
            }
            hi = mid - 1;
        } else if cp > run_hi {
            lo = mid + 1;
        } else {
            return width;
        }
    }
    1
}

fn is_regional_indicator(cp: u32) -> bool {
    (0x1f1e6..=0x1f1ff).contains(&cp)
}

pub fn cluster_width(cluster: &str) -> usize {
    let mut width = 0usize;
    let mut vs16 = false;
    let mut regional = 0usize;
    for character in cluster.chars() {
        let cp = character as u32;
        if cp == VS16 {
            vs16 = true;
        }
        if is_regional_indicator(cp) {
            regional += 1;
        }
        let character_width = usize::from(code_point_width(cp));
        if character_width > width {
            width = character_width;
        }
    }
    if vs16 || regional >= 2 { 2 } else { width }
}

pub fn clusters(text: &str) -> Vec<&str> {
    UnicodeSegmentation::graphemes(text, true).collect()
}

pub fn measured(text: &str) -> Vec<(&str, usize)> {
    clusters(text).into_iter().map(|cluster| (cluster, cluster_width(cluster))).collect()
}

pub fn string_width(text: &str) -> usize {
    clusters(text).into_iter().map(cluster_width).sum()
}
