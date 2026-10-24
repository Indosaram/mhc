//! senpi `modes/rpc/tool-child/admission.ts` (pin 26acd9353): host-call and job admission.
use std::collections::{HashMap, HashSet, VecDeque};

use crate::protocol::{ChildToServer, ServerToChild, TOOL_CHILD_LIMITS, ToolChildMessage};

/// The one host-call capacity both sides enforce: `HOST_CALL_ACTIVE` running plus `HOST_CALL_WAITING` queued (264).
/// The child admits against it before sending, the server admits against it on receipt; there is no second constant.
pub fn host_call_capacity() -> usize {
    TOOL_CHILD_LIMITS.host_call_active + TOOL_CHILD_LIMITS.host_call_waiting
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostCallRefusalCode {
    HostCallCapacity,
    HostCallDuplicate,
}

impl HostCallRefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::HostCallCapacity => "host_call_capacity",
            Self::HostCallDuplicate => "host_call_duplicate",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum HostCallDecision {
    Active,
    Waiting,
    /// `frame` is a `ToolChildMessage::HostCallRefused` naming the call's `call_id`.
    Refused {
        frame: Box<ToolChildMessage>,
    },
}

/// Lossless host-call admission: a call is running, queued FIFO, or refused with a frame naming its `call_id`.
#[derive(Debug, Default)]
pub struct HostCallAdmission {
    active: HashSet<String>,
    waiting: VecDeque<String>,
}

impl HostCallAdmission {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> usize {
        self.active.len()
    }

    pub fn waiting(&self) -> usize {
        self.waiting.len()
    }

    pub fn submit(&mut self, call_id: &str) -> HostCallDecision {
        if self.active.contains(call_id) || self.waiting.iter().any(|queued| queued == call_id) {
            return refuse(call_id, HostCallRefusalCode::HostCallDuplicate);
        }
        if self.active.len() < TOOL_CHILD_LIMITS.host_call_active && self.waiting.is_empty() {
            self.active.insert(call_id.to_owned());
            return HostCallDecision::Active;
        }
        if self.active.len() + self.waiting.len() >= host_call_capacity() {
            return refuse(call_id, HostCallRefusalCode::HostCallCapacity);
        }
        self.waiting.push_back(call_id.to_owned());
        HostCallDecision::Waiting
    }

    /// Ends a call (or withdraws a queued one) and returns the call promoted into the freed active slot, if any.
    pub fn settle(&mut self, call_id: &str) -> Option<String> {
        if !self.active.remove(call_id) {
            if let Some(index) = self.waiting.iter().position(|queued| queued == call_id) {
                self.waiting.remove(index);
            }
            return None;
        }
        let next = self.waiting.pop_front();
        if let Some(next) = &next {
            self.active.insert(next.clone());
        }
        next
    }
}

fn refuse(call_id: &str, code: HostCallRefusalCode) -> HostCallDecision {
    HostCallDecision::Refused {
        frame: Box::new(ToolChildMessage::Server(ServerToChild::HostCallRefused {
            call_id: call_id.to_owned(),
            code: code.as_str().to_owned(),
        })),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobRefusalCode {
    ToolChildJobsFull,
    ToolChildNestedFull,
    ToolChildParentUnknown,
    ToolChildJobDuplicate,
}

impl JobRefusalCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ToolChildJobsFull => "tool_child_jobs_full",
            Self::ToolChildNestedFull => "tool_child_nested_full",
            Self::ToolChildParentUnknown => "tool_child_parent_unknown",
            Self::ToolChildJobDuplicate => "tool_child_job_duplicate",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum JobDecision {
    Admitted,
    /// `frame` is a `ToolChildMessage::JobRefused`.
    Refused {
        frame: Box<ToolChildMessage>,
    },
}

/// Per-child job budget. Ordinary jobs are capped at `TOOL_CHILD_LIMITS.jobs`. A nested job (started on behalf of a
/// running job, e.g. a tool an eval cell calls) is admitted past that cap, so a full child cannot deadlock its own
/// callers; nested jobs arrive through host calls and are bounded by `host_call_capacity()`.
#[derive(Debug, Default)]
pub struct JobAdmission {
    ordinary: HashSet<String>,
    nested: HashMap<String, String>,
}

impl JobAdmission {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn admit(&mut self, job_id: &str, parent_job_id: Option<&str>) -> JobDecision {
        if self.ordinary.contains(job_id) || self.nested.contains_key(job_id) {
            return refuse_job(job_id, JobRefusalCode::ToolChildJobDuplicate);
        }
        let Some(parent_job_id) = parent_job_id else {
            if self.ordinary.len() >= TOOL_CHILD_LIMITS.jobs {
                return refuse_job(job_id, JobRefusalCode::ToolChildJobsFull);
            }
            self.ordinary.insert(job_id.to_owned());
            return JobDecision::Admitted;
        };
        if !self.ordinary.contains(parent_job_id) && !self.nested.contains_key(parent_job_id) {
            return refuse_job(job_id, JobRefusalCode::ToolChildParentUnknown);
        }
        if self.nested.len() >= host_call_capacity() {
            return refuse_job(job_id, JobRefusalCode::ToolChildNestedFull);
        }
        self.nested
            .insert(job_id.to_owned(), parent_job_id.to_owned());
        JobDecision::Admitted
    }

    pub fn finish(&mut self, job_id: &str) {
        self.ordinary.remove(job_id);
        self.nested.remove(job_id);
    }
}

fn refuse_job(job_id: &str, code: JobRefusalCode) -> JobDecision {
    JobDecision::Refused {
        frame: Box::new(ToolChildMessage::Child(ChildToServer::JobRefused {
            job_id: job_id.to_owned(),
            code: code.as_str().to_owned(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call_id(index: usize) -> String {
        format!("call-{index}")
    }

    struct Submitted {
        active: usize,
        waiting: usize,
        refused: Vec<String>,
    }

    fn refused_call_id(frame: &ToolChildMessage) -> String {
        match frame {
            ToolChildMessage::Server(ServerToChild::HostCallRefused { call_id, .. }) => {
                call_id.clone()
            }
            other => panic!("expected host_call_refused, got {other:?}"),
        }
    }

    fn submit_many(admission: &mut HostCallAdmission, count: usize) -> Submitted {
        let mut states = Submitted {
            active: 0,
            waiting: 0,
            refused: Vec::new(),
        };
        for index in 1..=count {
            match admission.submit(&call_id(index)) {
                HostCallDecision::Refused { frame } => states.refused.push(refused_call_id(&frame)),
                HostCallDecision::Active => states.active += 1,
                HostCallDecision::Waiting => states.waiting += 1,
            }
        }
        states
    }

    #[test]
    fn has_one_capacity_8_active_plus_256_waiting_is_264() {
        assert_eq!(host_call_capacity(), 264);
        assert_eq!(
            host_call_capacity(),
            TOOL_CHILD_LIMITS.host_call_active + TOOL_CHILD_LIMITS.host_call_waiting
        );
    }

    #[test]
    fn concurrent_264_submissions_are_8_active_256_waiting_0_refused() {
        let mut admission = HostCallAdmission::new();
        let states = submit_many(&mut admission, 264);
        assert_eq!(
            (states.active, states.waiting, states.refused.len()),
            (8, 256, 0)
        );
        assert_eq!(admission.active(), 8);
        assert_eq!(admission.waiting(), 256);
    }

    #[test]
    fn the_265th_is_refused_with_a_correlated_host_call_refused_frame() {
        let mut admission = HostCallAdmission::new();
        assert_eq!(
            submit_many(&mut admission, 265).refused,
            vec!["call-265".to_owned()]
        );
        assert_eq!(
            HostCallAdmission::new().submit("x"),
            HostCallDecision::Active
        );
        let mut full = HostCallAdmission::new();
        submit_many(&mut full, 264);
        let HostCallDecision::Refused { frame } = full.submit("call-265") else {
            panic!("expected a refusal");
        };
        assert_eq!(
            *frame,
            ToolChildMessage::Server(ServerToChild::HostCallRefused {
                call_id: "call-265".to_owned(),
                code: "host_call_capacity".to_owned()
            })
        );
    }

    #[test]
    fn promotes_waiting_calls_in_fifo_order() {
        let mut admission = HostCallAdmission::new();
        submit_many(&mut admission, 264);
        let promoted: Vec<Option<String>> = (1..=264)
            .map(|index| admission.settle(&call_id(index)))
            .collect();
        let expected: Vec<Option<String>> =
            (0..256).map(|offset| Some(call_id(offset + 9))).collect();
        assert_eq!(promoted[..256], expected[..]);
        assert!(promoted[256..].iter().all(Option::is_none));
        assert_eq!(admission.active() + admission.waiting(), 0);
    }

    #[test]
    fn child_and_server_share_the_capacity() {
        let mut child = HostCallAdmission::new();
        let mut server = HostCallAdmission::new();
        let mut refused_by_server = Vec::new();
        for index in 1..=300 {
            if matches!(
                child.submit(&call_id(index)),
                HostCallDecision::Refused { .. }
            ) {
                continue;
            }
            if let HostCallDecision::Refused { frame } = server.submit(&call_id(index)) {
                refused_by_server.push(refused_call_id(&frame));
            }
        }
        assert!(refused_by_server.is_empty());
        assert_eq!(server.active() + server.waiting(), host_call_capacity());
    }

    #[test]
    fn refuses_a_duplicate_call_id() {
        let mut admission = HostCallAdmission::new();
        admission.submit("same");
        let duplicate = admission.submit("same");
        assert_eq!(
            duplicate,
            HostCallDecision::Refused {
                frame: Box::new(ToolChildMessage::Server(ServerToChild::HostCallRefused {
                    call_id: "same".to_owned(),
                    code: "host_call_duplicate".to_owned()
                }))
            }
        );
    }

    fn job_refusal_code(decision: &JobDecision) -> Option<String> {
        match decision {
            JobDecision::Refused { frame } => match frame.as_ref() {
                ToolChildMessage::Child(ChildToServer::JobRefused { code, .. }) => {
                    Some(code.clone())
                }
                _ => None,
            },
            _ => None,
        }
    }

    #[test]
    fn refuses_the_33rd_ordinary_job_while_a_nested_job_is_still_admitted() {
        let mut jobs = JobAdmission::new();
        for index in 1..=32 {
            assert_eq!(
                jobs.admit(&format!("job-{index}"), None),
                JobDecision::Admitted
            );
        }
        assert_eq!(
            jobs.admit("job-33", None),
            JobDecision::Refused {
                frame: Box::new(ToolChildMessage::Child(ChildToServer::JobRefused {
                    job_id: "job-33".to_owned(),
                    code: "tool_child_jobs_full".to_owned()
                }))
            }
        );
        assert_eq!(jobs.admit("nested-1", Some("job-1")), JobDecision::Admitted);
        assert_eq!(
            jobs.admit("nested-2", Some("missing")),
            JobDecision::Refused {
                frame: Box::new(ToolChildMessage::Child(ChildToServer::JobRefused {
                    job_id: "nested-2".to_owned(),
                    code: "tool_child_parent_unknown".to_owned()
                }))
            }
        );
        jobs.finish("job-2");
        assert_eq!(jobs.admit("job-33", None), JobDecision::Admitted);
    }

    #[test]
    fn bounds_nested_jobs_by_the_host_call_capacity_not_by_a_second_constant() {
        let mut jobs = JobAdmission::new();
        jobs.admit("parent", None);
        for index in 1..=host_call_capacity() {
            assert_eq!(
                jobs.admit(&format!("nested-{index}"), Some("parent")),
                JobDecision::Admitted
            );
        }
        let over = jobs.admit("nested-over", Some("parent"));
        assert_eq!(
            job_refusal_code(&over).as_deref(),
            Some("tool_child_nested_full")
        );
    }
}
