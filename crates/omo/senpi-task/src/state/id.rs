use std::sync::Mutex;

const UUIDV7_TIMESTAMP_HIGH_BITS_DIVISOR: u64 = 0x10000;
const TASK_ID_SPACE_SIZE: u64 = 0x1_0000_0000;
const MAX_TASK_ID_VALUE: u32 = u32::MAX;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Invalid task id; expected st_[0-9a-f]{{8}}")]
pub struct InvalidTaskIdError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("Task id space exhausted for this process; restart before creating more task ids")]
pub struct TaskIdSpaceExhaustedError;

/// A validated `st_[0-9a-f]{8}` task id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TaskId(u32);

impl TaskId {
    pub fn value(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for TaskId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "st_{:08x}", self.0)
    }
}

pub fn parse_task_id(value: &str) -> Result<TaskId, InvalidTaskIdError> {
    let hex = value.strip_prefix("st_").ok_or(InvalidTaskIdError)?;
    let canonical = hex.len() == 8
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !canonical {
        return Err(InvalidTaskIdError);
    }
    u32::from_str_radix(hex, 16)
        .map(TaskId)
        .map_err(|_| InvalidTaskIdError)
}

pub fn bump_task_id(id: TaskId) -> Result<TaskId, TaskIdSpaceExhaustedError> {
    id.0.checked_add(1)
        .map(TaskId)
        .ok_or(TaskIdSpaceExhaustedError)
}

fn uuid_v7_timestamp_high_bits(now_ms: u64) -> u32 {
    let bucket = (now_ms / UUIDV7_TIMESTAMP_HIGH_BITS_DIVISOR) % TASK_ID_SPACE_SIZE;
    u32::try_from(bucket).unwrap_or(MAX_TASK_ID_VALUE)
}

fn next_monotonic_value(
    candidate: u32,
    previous: Option<u32>,
) -> Result<u32, TaskIdSpaceExhaustedError> {
    match previous {
        None => Ok(candidate),
        Some(previous) if candidate > previous => Ok(candidate),
        Some(previous) => previous.checked_add(1).ok_or(TaskIdSpaceExhaustedError),
    }
}

static LAST_TASK_ID_VALUE: Mutex<Option<u32>> = Mutex::new(None);

pub(crate) fn system_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Creates a sortable id from the process-wide monotonic floor (the TypeScript module global).
pub fn create_task_id(now_ms: Option<u64>) -> Result<TaskId, TaskIdSpaceExhaustedError> {
    let now_ms = now_ms.unwrap_or_else(system_now_ms);
    let mut last = LAST_TASK_ID_VALUE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let next = next_monotonic_value(uuid_v7_timestamp_high_bits(now_ms), *last)?;
    *last = Some(next);
    Ok(TaskId(next))
}

/// Raises the process-wide floor so later ids sort after `id`; never lowers it.
pub fn sync_task_id_floor(id: TaskId) {
    let mut last = LAST_TASK_ID_VALUE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    *last = Some(last.map_or(id.0, |previous| previous.max(id.0)));
}

/// An isolated id sequence with its own clock and floor (`createTaskIdFactory`).
pub struct TaskIdFactory<C: Fn() -> u64> {
    clock: C,
    last: Option<u32>,
}

impl<C: Fn() -> u64> TaskIdFactory<C> {
    pub fn next_id(&mut self) -> Result<TaskId, TaskIdSpaceExhaustedError> {
        let next = next_monotonic_value(uuid_v7_timestamp_high_bits((self.clock)()), self.last)?;
        self.last = Some(next);
        Ok(TaskId(next))
    }
}

pub fn create_task_id_factory<C: Fn() -> u64>(clock: C) -> TaskIdFactory<C> {
    TaskIdFactory { clock, last: None }
}
