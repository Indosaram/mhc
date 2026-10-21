//! Port of tool-execution-cache.ts memory accounting. The component owns its render cache.
use std::sync::{Arc, LazyLock, Mutex};

#[derive(Clone, Copy, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TuiRenderCacheTotals {
    pub components: usize,
    pub cached_lines: usize,
    pub images: usize,
    pub finished_cards: usize,
    pub cached_lines_bytes: usize,
    pub result_bytes: usize,
}

#[derive(Default)]
struct Totals {
    reported: bool,
    figures: TuiRenderCacheTotals,
}

static TOTALS: LazyLock<Arc<Mutex<Totals>>> =
    LazyLock::new(|| Arc::new(Mutex::new(Totals::default())));

pub fn tui_render_cache_totals() -> Option<TuiRenderCacheTotals> {
    let totals = TOTALS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    totals.reported.then_some(totals.figures)
}

pub struct ToolExecutionCacheMemory {
    totals: Arc<Mutex<Totals>>,
    figures: TuiRenderCacheTotals,
    disposed: bool,
}

impl Default for ToolExecutionCacheMemory {
    fn default() -> Self {
        Self::new(Arc::clone(&TOTALS))
    }
}

impl ToolExecutionCacheMemory {
    fn new(totals: Arc<Mutex<Totals>>) -> Self {
        {
            let mut state = totals
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state.reported = true;
            state.figures.components += 1;
        }
        Self {
            totals,
            figures: TuiRenderCacheTotals {
                components: 1,
                ..Default::default()
            },
            disposed: false,
        }
    }

    pub fn store_lines(&mut self, lines: &[String]) {
        self.drop_lines();
        let bytes = line_array_bytes(lines);
        let mut totals = self
            .totals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        totals.figures.cached_lines += lines.len();
        totals.figures.cached_lines_bytes += bytes;
        self.figures.cached_lines = lines.len();
        self.figures.cached_lines_bytes = bytes;
    }

    pub fn drop_lines(&mut self) {
        let mut totals = self
            .totals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        totals.figures.cached_lines -= self.figures.cached_lines;
        totals.figures.cached_lines_bytes -= self.figures.cached_lines_bytes;
        self.figures.cached_lines = 0;
        self.figures.cached_lines_bytes = 0;
    }

    pub fn set_images(&mut self, count: usize) {
        let mut totals = self
            .totals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        totals.figures.images = totals.figures.images - self.figures.images + count;
        self.figures.images = count;
    }

    pub fn finalize_result(&mut self, bytes: usize) {
        let mut totals = self
            .totals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        totals.figures.finished_cards += usize::from(self.figures.finished_cards == 0);
        totals.figures.result_bytes =
            totals.figures.result_bytes - self.figures.result_bytes + bytes;
        self.figures.finished_cards = 1;
        self.figures.result_bytes = bytes;
    }

    pub fn dispose(&mut self) {
        if self.disposed {
            return;
        }
        self.drop_lines();
        self.set_images(0);
        let mut totals = self
            .totals
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        totals.figures.components -= 1;
        totals.figures.finished_cards -= self.figures.finished_cards;
        totals.figures.result_bytes -= self.figures.result_bytes;
        self.disposed = true;
    }
}

impl Drop for ToolExecutionCacheMemory {
    fn drop(&mut self) {
        self.dispose();
    }
}

/// Keep the upstream UTF-16 estimate so frame and card figures remain comparable.
pub fn line_array_bytes(lines: &[String]) -> usize {
    lines.len() * 8
        + lines
            .iter()
            .map(|line| line.encode_utf16().count() * 2)
            .sum::<usize>()
}

pub fn serialized_tool_result_bytes(
    result: &super::tool_execution_types::ToolExecutionResult,
) -> usize {
    let content = match serde_json::to_string(&result.content) {
        Ok(content) => content,
        Err(error) => {
            eprintln!("Memory report tool result serialization failed: {error}");
            return 0;
        }
    };
    let details = result.details.as_ref().map(|value| value.to_string());
    content.encode_utf16().count()
        + details
            .as_deref()
            .map_or(0, |value| value.encode_utf16().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_totals_replace_lines_and_finalized_results() {
        // Given
        let totals = Arc::new(Mutex::new(Totals::default()));
        let mut card = ToolExecutionCacheMemory::new(Arc::clone(&totals));
        card.store_lines(&["old".into()]);
        card.finalize_result(20);
        // When
        card.store_lines(&["a".into(), "\u{1f600}".into()]);
        card.finalize_result(7);
        card.set_images(2);
        // Then
        let figures = totals.lock().expect("totals").figures;
        assert_eq!(figures.components, 1);
        assert_eq!(figures.cached_lines, 2);
        assert_eq!(figures.cached_lines_bytes, 22);
        assert_eq!(figures.finished_cards, 1);
        assert_eq!(figures.result_bytes, 7);
        assert_eq!(figures.images, 2);
    }

    #[test]
    fn disposing_then_dropping_card_releases_only_its_totals() {
        // Given
        let totals = Arc::new(Mutex::new(Totals::default()));
        let mut first = ToolExecutionCacheMemory::new(Arc::clone(&totals));
        let mut second = ToolExecutionCacheMemory::new(Arc::clone(&totals));
        first.store_lines(&["first".into()]);
        first.finalize_result(99);
        first.set_images(3);
        second.store_lines(&["x".into()]);
        // When
        first.dispose();
        drop(first);
        // Then
        let figures = totals.lock().expect("totals").figures;
        assert_eq!(figures.components, 1);
        assert_eq!(figures.cached_lines, 1);
        assert_eq!(figures.cached_lines_bytes, 10);
        assert_eq!(figures.finished_cards, 0);
        assert_eq!(figures.result_bytes, 0);
        assert_eq!(figures.images, 0);
    }
}
