use serde::{Deserialize, Serialize};
use super::super::types::*;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobInput {
    #[serde(flatten)] pub process: ProcessSpec,
    pub notify: bool, pub rerun: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub every: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")] pub not_before: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "lowercase", rename_all_fields = "camelCase")]
pub enum JobCheckpoint {
    Waiting { until_ms: u64, occurrence: u64 },
    Spawning { key: String, occurrence: u64 },
    Running { key: String, occurrence: u64 },
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobOutput {
    #[serde(skip_serializing_if = "Option::is_none")] pub stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub stderr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")] pub dropped_stdout: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub dropped_stderr: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")] pub exit_code: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")] pub occurrence: Option<u64>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JobResult { pub exit_code: i32, pub occurrences: u64, pub stdout: String, pub stderr: String }
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobFailureReason { Spawn, Interrupted }
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JobFailure { pub reason: JobFailureReason, pub detail: String }
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct JobAborted { pub killed: bool }
type T = Task<JobInput, JobCheckpoint>;
type Rt<'a> = dyn Runtime<(), dyn TaskTx> + 'a;
type S = Step<JobCheckpoint, JobResult, JobFailure>;

pub struct JobKind { token: KindToken, config: KindConfig, inflight: [String; 2] }
pub fn job() -> JobKind { JobKind { token: KindToken::core("pi.job"), config: KindConfig::default(), inflight: ["spawning".into(), "running".into()] } }
fn json<T: Serialize>(value: &T) -> Result<JsonValue> { Ok(to_stored(value)?.as_json().clone()) }
fn checkpoint(value: &JobCheckpoint) -> Result<Checkpoint> { Stored::from_json(json(value)?).decode() }
fn failed(reason: JobFailureReason, detail: String) -> S {
    Step::Done(Box::new(move |_, _, _| Box::pin(async move { Ok(Completion::Failed { failure: JobFailure { reason, detail } }) })))
}
fn key(task_id: Id, occurrence: u64) -> String { format!("{task_id}:{occurrence}") }
fn poll_delay(attempt: u32) -> u64 { 100_u64.saturating_mul(2_u64.pow(attempt.min(4))).min(1000) }
fn notice(text: &str, now: u64) -> Result<NewEntry> {
    Ok(NewEntry { kind: "pi.notice".into(), model: Some(vec![StoredMessage::from_json(json(&serde_json::json!({"role":"user","content":text,"timestamp":now}))?)]), data: None, head: None, edits: None })
}
impl Kind for JobKind {
    type Input = JobInput; type Checkpoint = JobCheckpoint; type Result = JobResult; type Failure = JobFailure;
    type Aborted = JobAborted; type Hooks = (); type Slot = JobOutput; type Tx = dyn TaskTx;
    fn token(&self) -> &KindToken { &self.token }
    fn turn(&self) -> bool { false }
    fn config(&self) -> &KindConfig { &self.config }
    fn inflight(&self) -> &[String] { &self.inflight }
    fn slot(&self, _: &JobInput) -> Result<JobOutput> { Ok(JobOutput::default()) }
    fn describe(&self, task: &T, slot: Option<&JobOutput>) -> Result<JsonValue> {
        let stage = match task.checkpoint { None => "starting", Some(JobCheckpoint::Waiting { .. }) => "waiting", Some(JobCheckpoint::Spawning { .. }) => "spawning", Some(JobCheckpoint::Running { .. }) => "running" };
        let mut object: JsonObject = Stored::from_json(json(&slot.cloned().unwrap_or_default())?).decode()?;
        object.insert("stage".into(), json(&stage)?); json(&object)
    }
    fn initial<'a>(&'a self, task: &'a T, rt: &'a Rt<'a>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<S>> {
        Box::pin(async move {
            if rt.process_host().is_none() { return Ok(failed(JobFailureReason::Spawn, "no process host".into())); }
            let now = rt.now(); let until_ms = task.input.not_before.unwrap_or(now);
            if until_ms > now { return Ok(Step::Next(JobCheckpoint::Waiting { until_ms, occurrence: 1 })); }
            spawn(task, 1, rt, ctx).await
        })
    }
    fn phase<'a>(&'a self, phase: &'a str, task: &'a T, rt: &'a Rt<'a>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<S>> {
        Box::pin(async move {
            let cp = task.checkpoint.as_ref().ok_or_else(|| Error::InvalidShape("job phase without checkpoint".into()))?;
            match (phase, cp) {
                ("waiting", JobCheckpoint::Waiting { until_ms, occurrence }) => { rt.sleep(*until_ms, ctx).await?; spawn(task, *occurrence, rt, ctx).await }
                ("spawning", JobCheckpoint::Spawning { key, occurrence }) | ("running", JobCheckpoint::Running { key, occurrence }) => reconcile(task, key, *occurrence, matches!(cp, JobCheckpoint::Spawning { .. }), rt, ctx).await,
                _ => Err(Error::TaskContractFault { kind: "pi.job".into(), what: format!("phase/checkpoint mismatch: {phase}") }),
            }
        })
    }
    fn abort<'a>(&'a self, task: &'a T, rt: &'a Rt<'a>, ctx: &'a Context) -> LocalBoxFuture<'a, Result<AbortClosure<JobAborted>>> {
        Box::pin(async move {
            let killed = match (&task.checkpoint, rt.process_host()) {
                (Some(JobCheckpoint::Spawning { key, .. } | JobCheckpoint::Running { key, .. }), Some(host)) => {
                    terminate(host, key, ctx, || rt.now(), |until| rt.sleep(until, ctx)).await?; true
                }
                (None | Some(JobCheckpoint::Waiting { .. }), _) | (Some(JobCheckpoint::Spawning { .. } | JobCheckpoint::Running { .. }), None) => false,
            };
            let closure: AbortClosure<JobAborted> = Box::new(move |_, _, _| Box::pin(async move { Ok(JobAborted { killed }) }));
            Ok(closure)
        })
    }
}
async fn terminate<'a>(host: &'a dyn ProcessHost, key: &'a str, ctx: &'a Context, now: impl FnOnce() -> u64,
    sleep: impl FnOnce(u64) -> LocalBoxFuture<'a, Result<()>>) -> Result<()> {
    host.kill(key, ProcessSignal::Sigterm, ctx).await?;
    sleep(now().saturating_add(5000)).await?;
    host.kill(key, ProcessSignal::Sigkill, ctx).await
}
async fn persist(cp: JobCheckpoint, rt: &Rt<'_>, ctx: &Context) -> Result<()> {
    let cp = checkpoint(&cp)?;
    rt.commit(Box::new(move |tx, _, _| Box::pin(async move { tx.checkpoint(cp)?; Ok(JsonValue::Null) })), ctx).await?; Ok(())
}
async fn spawn(task: &T, occurrence: u64, rt: &Rt<'_>, ctx: &Context) -> Result<S> {
    let mut occurrence = occurrence;
    loop {
        let key = key(task.id, occurrence);
        persist(JobCheckpoint::Spawning { key: key.clone(), occurrence }, rt, ctx).await?;
        let host = rt.process_host().ok_or_else(|| Error::InvalidShape("job process host disappeared".into()))?;
        if let Err(error) = host.start(&key, task.input.process.clone(), ctx).await {
            if ctx.is_aborted() { return Err(error); }
            let detail = error.to_string(); let notify = task.input.notify; let now = rt.now(); let id = task.id;
            return Ok(Step::Done(Box::new(move |tx, current, _| Box::pin(async move {
                if notify { tx.write(current.conversation_id, notice(&format!("job {id} failed to start: {detail}"), now)?).await?; }
                Ok(Completion::Failed { failure: JobFailure { reason: JobFailureReason::Spawn, detail } })
            }))));
        }
        persist(JobCheckpoint::Running { key: key.clone(), occurrence }, rt, ctx).await?;
        match poll(task, &key, occurrence, rt, ctx).await? {
            Poll::Step(step) => return Ok(step),
            Poll::Rerun(next) => occurrence = next,
        }
    }
}
async fn reconcile(task: &T, key: &str, occurrence: u64, spawning: bool, rt: &Rt<'_>, ctx: &Context) -> Result<S> {
    let Some(host) = rt.process_host() else { return Ok(failed(JobFailureReason::Interrupted, "no process host after restart".into())); };
    match host.status(key, ctx).await {
        Err(error) if ctx.is_aborted() => return Err(error),
        Err(error) => return Ok(failed(JobFailureReason::Interrupted, format!("host status failed: {error}"))),
        Ok(ProcessStatus::Unknown) => return if task.input.rerun { spawn(task, occurrence, rt, ctx).await } else { Ok(failed(JobFailureReason::Interrupted, "process outcome unknown".into())) },
        Ok(ProcessStatus::Running { .. } | ProcessStatus::Exited { .. }) => {}
    }
    if spawning { persist(JobCheckpoint::Running { key: key.into(), occurrence }, rt, ctx).await?; }
    match poll(task, key, occurrence, rt, ctx).await? { Poll::Step(step) => Ok(step), Poll::Rerun(occurrence) => spawn(task, occurrence, rt, ctx).await }
}
enum Poll { Step(S), Rerun(u64) }
fn unknown(rerun: bool, occurrence: u64) -> Poll {
    if rerun { Poll::Rerun(occurrence) } else { Poll::Step(failed(JobFailureReason::Interrupted, "process outcome unknown".into())) }
}
fn output(status: &ProcessStatus) -> Option<JobOutput> {
    match status {
        ProcessStatus::Running { stdout, stderr, dropped_stdout, dropped_stderr } => Some(JobOutput { stdout: Some(stdout.clone()), stderr: Some(stderr.clone()), dropped_stdout: Some(*dropped_stdout), dropped_stderr: Some(*dropped_stderr), ..JobOutput::default() }),
        ProcessStatus::Exited { exit_code, stdout, stderr, dropped_stdout, dropped_stderr } => Some(JobOutput { stdout: Some(stdout.clone()), stderr: Some(stderr.clone()), dropped_stdout: Some(*dropped_stdout), dropped_stderr: Some(*dropped_stderr), exit_code: Some(*exit_code), occurrence: None }),
        ProcessStatus::Unknown => None,
    }
}
async fn poll(task: &T, key: &str, occurrence: u64, rt: &Rt<'_>, ctx: &Context) -> Result<Poll> {
    let host = rt.process_host().ok_or_else(|| Error::InvalidShape("job lacks process host".into()))?;
    let token = rt.kind().clone(); let mut attempt: u32 = 0;
    loop {
        let status = match host.status(key, ctx).await {
            Ok(status) => status,
            Err(error) if ctx.is_aborted() => return Err(error),
            Err(error) => return Ok(Poll::Step(failed(JobFailureReason::Interrupted, format!("host status failed: {error}")))),
        };
        let Some(snapshot) = output(&status) else { return Ok(unknown(task.input.rerun, occurrence)); };
        let kind = token.clone();
        rt.commit(Box::new(move |tx, current, _| Box::pin(async move {
            let slot = tx.slot(&TaskRef { id: current.id, kind })?;
            slot.set("stdout", &json(&snapshot.stdout)?)?; slot.set("stderr", &json(&snapshot.stderr)?)?;
            slot.set("droppedStdout", &json(&snapshot.dropped_stdout)?)?; slot.set("droppedStderr", &json(&snapshot.dropped_stderr)?)?;
            if let Some(code) = snapshot.exit_code { slot.set("exitCode", &json(&code)?)?; }
            Ok(JsonValue::Null)
        })), ctx).await?;
        match status {
            ProcessStatus::Exited { exit_code, stdout, stderr, .. } => {
                let notify = task.input.notify; let id = task.id; let now = rt.now();
                let Some(every) = task.input.every else {
                    return Ok(Poll::Step(Step::Done(Box::new(move |tx, current, _| Box::pin(async move {
                        if notify { tx.write(current.conversation_id, notice(&format!("job {id} exited with code {exit_code}"), now)?).await?; }
                        Ok(Completion::Completed { result: JobResult { exit_code, occurrences: occurrence, stdout, stderr } })
                    })))));
                };
                let next = occurrence.checked_add(1).ok_or_else(|| Error::InvalidShape("job occurrence exhausted".into()))?;
                let until_ms = now.checked_add(every).ok_or_else(|| Error::InvalidShape("job recurrence deadline overflow".into()))?;
                return Ok(Poll::Step(Step::Build(Box::new(move |tx, current, _| Box::pin(async move {
                    if notify { tx.write(current.conversation_id, notice(&format!("job {id} occurrence {occurrence} exited with code {exit_code}"), now)?).await?; }
                    let slot = tx.slot(&TaskRef { id: current.id, kind: token })?;
                    slot.set("stdout", &json(&"")?)?; slot.set("stderr", &json(&"")?)?; slot.set("droppedStdout", &json(&0)?)?; slot.set("droppedStderr", &json(&0)?)?;
                    slot.delete("exitCode")?; slot.set("occurrence", &json(&next)?)?;
                    Ok(Transition::Checkpoint(JobCheckpoint::Waiting { until_ms, occurrence: next }))
                })))));
            }
            ProcessStatus::Running { .. } => { rt.sleep(rt.now().saturating_add(poll_delay(attempt)), ctx).await?; attempt = attempt.saturating_add(1); }
            ProcessStatus::Unknown => return Err(Error::InvalidShape("unknown process passed output projection".into())),
        }
    }
}

#[cfg(test)]
#[path = "job_tests.rs"]
mod tests;
