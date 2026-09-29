//! Per-model/provider concurrency slots with FIFO waiters (`manager/concurrency.ts`).

use std::collections::{BTreeMap, HashMap, VecDeque};

pub type Grant = Box<dyn FnOnce() + Send>;

#[derive(Debug, Clone, Default)]
pub struct TaskConcurrencyConfig {
    pub default_concurrency: Option<usize>,
    pub provider_concurrency: Option<BTreeMap<String, usize>>,
    pub model_concurrency: Option<BTreeMap<String, usize>>,
}

const DEFAULT_LIMIT: usize = 5;

/// `None` is the TypeScript `Number.POSITIVE_INFINITY` (a configured 0).
type Limit = Option<usize>;

struct Waiter {
    task_id: String,
    grant: Grant,
}

#[derive(Default)]
pub struct TaskConcurrency {
    config: TaskConcurrencyConfig,
    counts: HashMap<String, usize>,
    queues: HashMap<String, VecDeque<Waiter>>,
}

fn provider_of(model: &str) -> &str {
    model.split('/').next().unwrap_or(model)
}

fn own(record: Option<&BTreeMap<String, usize>>, key: &str) -> Option<usize> {
    record.and_then(|record| record.get(key).copied())
}

fn unlimited_if_zero(limit: usize) -> Limit {
    (limit != 0).then_some(limit)
}

impl TaskConcurrency {
    pub fn new(config: TaskConcurrencyConfig) -> Self {
        Self {
            config,
            ..Self::default()
        }
    }

    /// The slot limit; `None` means unlimited.
    pub fn get_limit(&self, model: &str) -> Limit {
        if let Some(limit) = own(self.config.model_concurrency.as_ref(), model) {
            return unlimited_if_zero(limit);
        }
        if let Some(limit) = own(
            self.config.provider_concurrency.as_ref(),
            provider_of(model),
        ) {
            return unlimited_if_zero(limit);
        }
        match self.config.default_concurrency {
            Some(limit) => unlimited_if_zero(limit),
            None => Some(DEFAULT_LIMIT),
        }
    }

    pub fn get_key(&self, model: &str) -> String {
        if own(self.config.model_concurrency.as_ref(), model).is_some() {
            return model.to_string();
        }
        let provider = provider_of(model);
        if own(self.config.provider_concurrency.as_ref(), provider).is_some() {
            return provider.to_string();
        }
        model.to_string()
    }

    pub fn has_free_slot(&self, model: &str) -> bool {
        match self.get_limit(model) {
            None => true,
            Some(limit) => self.get_count(model) < limit,
        }
    }

    pub fn acquire(&mut self, model: &str, _task_id: &str) {
        if self.get_limit(model).is_none() {
            return;
        }
        *self.counts.entry(self.get_key(model)).or_default() += 1;
    }

    pub fn enqueue(&mut self, model: &str, task_id: &str, grant: Grant) -> usize {
        let queue = self.queues.entry(self.get_key(model)).or_default();
        queue.push_back(Waiter {
            task_id: task_id.to_string(),
            grant,
        });
        queue.len()
    }

    pub fn queue_position(&self, model: &str, task_id: &str) -> Option<usize> {
        self.queues
            .get(&self.get_key(model))?
            .iter()
            .position(|waiter| waiter.task_id == task_id)
            .map(|index| index + 1)
    }

    pub fn remove(&mut self, model: &str, task_id: &str) -> bool {
        let Some(queue) = self.queues.get_mut(&self.get_key(model)) else {
            return false;
        };
        match queue.iter().position(|waiter| waiter.task_id == task_id) {
            Some(index) => queue.remove(index).is_some(),
            None => false,
        }
    }

    /// Hands the slot to the next waiter, returning its grant for the caller to run outside any
    /// lock; with no waiter the count drops instead.
    pub fn release(&mut self, model: &str) -> Option<Grant> {
        let key = self.get_key(model);
        if let Some(next) = self.queues.get_mut(&key).and_then(VecDeque::pop_front) {
            return Some(next.grant);
        }
        if let Some(count) = self.counts.get_mut(&key)
            && *count > 0
        {
            *count -= 1;
        }
        None
    }

    pub fn get_count(&self, model: &str) -> usize {
        self.counts.get(&self.get_key(model)).copied().unwrap_or(0)
    }
}
