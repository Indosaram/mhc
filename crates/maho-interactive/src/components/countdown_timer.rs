pub struct CountdownTimer {
    remaining_seconds: i64,
    disposed: bool,
    on_tick: Box<dyn FnMut(i64)>,
    on_expire: Box<dyn FnMut()>,
    request_render: Option<Box<dyn FnMut()>>,
}

impl CountdownTimer {
    pub fn new(
        timeout_ms: u64, request_render: Option<Box<dyn FnMut()>>,
        mut on_tick: Box<dyn FnMut(i64)>, on_expire: Box<dyn FnMut()>,
    ) -> Self {
        let remaining_seconds = i64::try_from(timeout_ms.div_ceil(1000)).unwrap_or(i64::MAX);
        on_tick(remaining_seconds);
        Self { remaining_seconds, disposed: false, on_tick, on_expire, request_render }
    }

    pub fn tick(&mut self) {
        if self.disposed { return; }
        self.remaining_seconds -= 1;
        (self.on_tick)(self.remaining_seconds);
        if let Some(render) = &mut self.request_render { render(); }
        if self.remaining_seconds <= 0 {
            self.dispose();
            (self.on_expire)();
        }
    }

    pub fn remaining_seconds(&self) -> i64 { self.remaining_seconds }
    pub fn dispose(&mut self) { self.disposed = true; }
}
