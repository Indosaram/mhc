//! Which ledgered POSIX process groups a tool child's teardown may signal (senpi#3566, plan todo 4); port of
//! `group-ownership.ts`.
//!
//! The rule is senpi#3020's: a group the child created stays owned after its leader exits, for as long as it has
//! members, because orphaned descendants keep the group id. It is released, never signalled, when the leader pid is
//! alive but is no longer the recorded leader (the pid was reused), when the group is empty, or when it is the server's
//! own group. A `detached` entry is never swept; the command explicitly opted out of the child's lifetime.
//!
//! The few `utils/process-identity.ts` functions this file imports (`processStartTimeMs`, `sameProcessStartMs`,
//! `readProcessStartTime`, `processIsLive`) are reimplemented below, local to this module.

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;

const PROCESS_START_TOLERANCE_MS: f64 = 3_000.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LedgeredGroup {
    pub launch_id: String,
    pub pgid: i64,
    /// The group leader's start identity as recorded at launch (`read_process_start_time` form).
    pub leader_start: Option<String>,
    pub lifetime: GroupLifetime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupLifetime {
    Owned,
    Detached,
}

impl From<crate::protocol::LaunchLifetime> for GroupLifetime {
    fn from(lifetime: crate::protocol::LaunchLifetime) -> Self {
        match lifetime {
            crate::protocol::LaunchLifetime::Owned => GroupLifetime::Owned,
            crate::protocol::LaunchLifetime::Detached => GroupLifetime::Detached,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupVerdict {
    Sweep,
    Detached,
    Empty,
    OwnGroup,
    LeaderReused,
}

impl GroupVerdict {
    /// The upstream string form (`"leader-reused"`, ...), used in the teardown log.
    pub fn as_str(self) -> &'static str {
        match self {
            GroupVerdict::Sweep => "sweep",
            GroupVerdict::Detached => "detached",
            GroupVerdict::Empty => "empty",
            GroupVerdict::OwnGroup => "own-group",
            GroupVerdict::LeaderReused => "leader-reused",
        }
    }
}

/// The signals a sweep sends (`NodeJS.Signals` upstream; only these two are ever used).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupSignal {
    Term,
    Kill,
}

#[derive(Clone)]
pub struct GroupProbes {
    pub group_has_members: Arc<dyn Fn(i64) -> bool + Send + Sync>,
    pub is_alive: Arc<dyn Fn(i64) -> bool + Send + Sync>,
    pub start_of: Arc<dyn Fn(i64) -> BoxFuture<'static, Option<String>> + Send + Sync>,
    pub own_group: Arc<dyn Fn() -> BoxFuture<'static, Option<i64>> + Send + Sync>,
    pub signal_group: Arc<dyn Fn(i64, GroupSignal) + Send + Sync>,
    pub wait: Arc<dyn Fn(u64) -> BoxFuture<'static, ()> + Send + Sync>,
}

pub async fn classify_group(
    group: &LedgeredGroup,
    own_group: Option<i64>,
    probes: &GroupProbes,
) -> GroupVerdict {
    if group.lifetime == GroupLifetime::Detached {
        return GroupVerdict::Detached;
    }
    // Without the server's own group nothing can be ruled out, so nothing is signalled.
    match own_group {
        None => return GroupVerdict::OwnGroup,
        Some(own) if group.pgid == own || group.pgid <= 1 => return GroupVerdict::OwnGroup,
        Some(_) => {}
    }
    if !(probes.group_has_members)(group.pgid) {
        return GroupVerdict::Empty;
    }
    if !(probes.is_alive)(group.pgid) {
        return GroupVerdict::Sweep;
    }
    let observed = (probes.start_of)(group.pgid).await;
    let recorded = group
        .leader_start
        .as_deref()
        .and_then(process_start_time_ms);
    if same_process_start_ms(
        recorded,
        observed.as_deref().and_then(process_start_time_ms),
    ) {
        GroupVerdict::Sweep
    } else {
        GroupVerdict::LeaderReused
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SweepOutcome {
    pub launch_id: String,
    pub pgid: i64,
    pub verdict: GroupVerdict,
    pub survived: Option<bool>,
}

#[derive(Debug, Clone, Copy)]
pub struct SweepOptions {
    pub grace_ms: u64,
    pub kill_wait_ms: u64,
    pub poll_ms: Option<u64>,
}

pub async fn sweep_groups(
    groups: &[LedgeredGroup],
    options: SweepOptions,
    probes: &GroupProbes,
) -> Vec<SweepOutcome> {
    let own_group = (probes.own_group)().await;
    let verdicts: Vec<(&LedgeredGroup, GroupVerdict)> = futures::future::join_all(
        groups
            .iter()
            .map(|group| async move { (group, classify_group(group, own_group, probes).await) }),
    )
    .await;
    let targets: Vec<i64> = verdicts
        .iter()
        .filter(|(_, verdict)| *verdict == GroupVerdict::Sweep)
        .map(|(group, _)| group.pgid)
        .collect();
    let poll_ms = options.poll_ms.unwrap_or(25);
    for pgid in &targets {
        (probes.signal_group)(*pgid, GroupSignal::Term);
    }
    let mut remaining = drain(&targets, options.grace_ms, poll_ms, probes).await;
    for pgid in &remaining {
        (probes.signal_group)(*pgid, GroupSignal::Kill);
    }
    remaining = drain(&remaining, options.kill_wait_ms, poll_ms, probes).await;
    verdicts
        .into_iter()
        .map(|(group, verdict)| SweepOutcome {
            launch_id: group.launch_id.clone(),
            pgid: group.pgid,
            verdict,
            survived: (verdict == GroupVerdict::Sweep).then(|| remaining.contains(&group.pgid)),
        })
        .collect()
}

async fn drain(pgids: &[i64], budget_ms: u64, poll_ms: u64, probes: &GroupProbes) -> Vec<i64> {
    let mut remaining: Vec<i64> = pgids
        .iter()
        .copied()
        .filter(|pgid| (probes.group_has_members)(*pgid))
        .collect();
    let mut waited = 0;
    while !remaining.is_empty() && waited < budget_ms {
        (probes.wait)(poll_ms).await;
        remaining.retain(|pgid| (probes.group_has_members)(*pgid));
        waited += poll_ms.max(1);
    }
    remaining
}

#[cfg(unix)]
async fn process_group_of_self() -> Option<i64> {
    // The upstream shells out to `ps -o pgid= -p <pid>`; `getpgrp` answers the same question for this process.
    Some(i64::from(rustix::process::getpgrp().as_raw_nonzero().get()))
}

#[cfg(not(unix))]
async fn process_group_of_self() -> Option<i64> {
    None
}

#[cfg(unix)]
fn group_has_members(pgid: i64) -> bool {
    let Some(pid) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    match rustix::process::test_kill_process_group(pid) {
        Ok(()) => true,
        Err(error) => error == rustix::io::Errno::PERM,
    }
}

#[cfg(not(unix))]
fn group_has_members(_pgid: i64) -> bool {
    false
}

#[cfg(unix)]
fn signal_group(pgid: i64, signal: GroupSignal) {
    let Some(pid) = i32::try_from(pgid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return;
    };
    let signal = match signal {
        GroupSignal::Term => rustix::process::Signal::TERM,
        GroupSignal::Kill => rustix::process::Signal::KILL,
    };
    // The group emptied between the membership check and the signal: the error is the answer.
    let _ = rustix::process::kill_process_group(pid, signal);
}

#[cfg(not(unix))]
fn signal_group(_pgid: i64, _signal: GroupSignal) {}

pub fn default_group_probes() -> GroupProbes {
    GroupProbes {
        group_has_members: Arc::new(group_has_members),
        is_alive: Arc::new(|pid| u32::try_from(pid).is_ok_and(process_is_live)),
        start_of: Arc::new(|pid| {
            Box::pin(async move {
                match u32::try_from(pid) {
                    Ok(pid) => read_process_start_time(pid).await,
                    Err(_) => None,
                }
            })
        }),
        own_group: Arc::new(|| Box::pin(process_group_of_self())),
        signal_group: Arc::new(signal_group),
        wait: Arc::new(|ms| Box::pin(tokio::time::sleep(Duration::from_millis(ms)))),
    }
}

// ---- utils/process-identity.ts, the parts this module needs ----

/// Local copy of `processIsLive`: signal 0 reaches the pid; EPERM still means alive.
#[cfg(unix)]
pub fn process_is_live(pid: u32) -> bool {
    let Some(pid) = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
    else {
        return false;
    };
    match rustix::process::test_kill_process(pid) {
        Ok(()) => true,
        Err(error) => error == rustix::io::Errno::PERM,
    }
}

#[cfg(not(unix))]
pub fn process_is_live(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .split_whitespace()
                .any(|word| word == pid.to_string())
        })
        .unwrap_or(false)
}

/// Local copy of `readProcessStartTime`: Linux reads the `starttime` field of `/proc/<pid>/stat` (clock ticks since
/// boot), Windows asks CIM for the FILETIME, other unixes use `ps -o lstart=`. A probe failure or an absent pid is
/// `None` (the caller treats both as "no identity").
#[cfg(target_os = "linux")]
pub async fn read_process_start_time(pid: u32) -> Option<String> {
    let stat = tokio::fs::read_to_string(format!("/proc/{pid}/stat"))
        .await
        .ok()?;
    stat.rsplit_once(") ")?
        .1
        .split_whitespace()
        .nth(19)
        .map(str::to_owned)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub async fn read_process_start_time(pid: u32) -> Option<String> {
    let output = tokio::process::Command::new("ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .env("LC_ALL", "C")
        .env("LANG", "C")
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(2), output)
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}

#[cfg(windows)]
pub async fn read_process_start_time(pid: u32) -> Option<String> {
    let command = format!(
        "$process = Get-CimInstance Win32_Process -Filter \"ProcessId={pid}\" -ErrorAction Stop; if ($null -eq $process) {{ Write-Output '__SENPI_ABSENT__'; exit 0 }}; $process.CreationDate.ToFileTimeUtc().ToString(\"D\", [Globalization.CultureInfo]::InvariantCulture)"
    );
    let output = tokio::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &command])
        .kill_on_drop(true)
        .output();
    let output = tokio::time::timeout(Duration::from_secs(1), output)
        .await
        .ok()?
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit()))
        .then_some(text)
}

/// Local copy of `processStartTimeMs`, for the identity forms this crate records: an all-digit identity is clock
/// ticks since boot on Linux and a FILETIME on Windows; `<sec>.<usec>` is a darwin epoch pair; anything else is a
/// C-locale `ps lstart` (`Sat Oct 18 05:00:00 2026`), read as one fixed zone so two readings compare consistently
/// (only the difference between recordings is ever used). Localized (Korean/Japanese) `lstart` forms are not parsed.
pub fn process_start_time_ms(identity: &str) -> Option<f64> {
    let identity = identity.trim();
    if !identity.is_empty() && identity.bytes().all(|b| b.is_ascii_digit()) {
        let value: u128 = identity.parse().ok()?;
        #[cfg(windows)]
        {
            const WINDOWS_FILETIME_EPOCH: u128 = 116_444_736_000_000_000;
            return Some((value as f64 - WINDOWS_FILETIME_EPOCH as f64) / 10_000.0);
        }
        #[cfg(target_os = "linux")]
        {
            let boot = std::fs::read_to_string("/proc/stat")
                .ok()?
                .lines()
                .find_map(|line| {
                    line.strip_prefix("btime ")
                        .and_then(|v| v.trim().parse::<u64>().ok())
                })?;
            return Some(boot as f64 * 1000.0 + value as f64 * 10.0);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        return Some(value as f64);
    }
    if let Some((seconds, micros)) = identity.split_once('.')
        && let (Ok(seconds), true) = (
            seconds.parse::<u64>(),
            !micros.is_empty() && micros.bytes().all(|b| b.is_ascii_digit()),
        )
    {
        return Some(seconds as f64 * 1000.0 + micros.parse::<f64>().ok()? / 1000.0);
    }
    parse_lstart_ms(identity)
}

fn parse_lstart_ms(text: &str) -> Option<f64> {
    // "Sat Oct 18 05:00:00 2026"
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [_weekday, month, day, clock, year] = parts.as_slice() else {
        return None;
    };
    let month = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ]
    .iter()
    .position(|name| name == month)? as i64
        + 1;
    let day: i64 = day.parse().ok()?;
    let year: i64 = year.parse().ok()?;
    let mut clock = clock.split(':');
    let hour: i64 = clock.next()?.parse().ok()?;
    let minute: i64 = clock.next()?.parse().ok()?;
    let second: i64 = clock.next()?.parse().ok()?;
    if clock.next().is_some() || !(1..=31).contains(&day) || hour > 23 || minute > 59 || second > 60
    {
        return None;
    }
    // Days from civil (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 86_400 + hour * 3_600 + minute * 60 + second) * 1_000) as f64)
}

/// Local copy of `sameProcessStartMs`.
pub fn same_process_start_ms(recorded: Option<f64>, observed: Option<f64>) -> bool {
    match (recorded, observed) {
        (Some(recorded), Some(observed)) if recorded.is_finite() && observed.is_finite() => {
            (recorded - observed).abs() <= PROCESS_START_TOLERANCE_MS
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};
    use std::sync::Mutex;

    const START: &str = "Sat Oct 18 05:00:00 2026";

    fn owned(pgid: i64, leader_start: Option<&str>) -> LedgeredGroup {
        LedgeredGroup {
            launch_id: format!("l{pgid}"),
            pgid,
            leader_start: leader_start.map(str::to_owned),
            lifetime: GroupLifetime::Owned,
        }
    }

    struct State {
        members: Mutex<HashSet<i64>>,
        alive: HashSet<i64>,
        starts: HashMap<i64, String>,
        signals: Mutex<Vec<(i64, GroupSignal)>>,
    }

    fn fake_probes(state: Arc<State>) -> GroupProbes {
        let members = Arc::clone(&state);
        let alive = Arc::clone(&state);
        let starts = Arc::clone(&state);
        let signal = Arc::clone(&state);
        GroupProbes {
            group_has_members: Arc::new(move |pgid| {
                members.members.lock().is_ok_and(|m| m.contains(&pgid))
            }),
            is_alive: Arc::new(move |pid| alive.alive.contains(&pid)),
            start_of: Arc::new(move |pid| {
                let found = starts.starts.get(&pid).cloned();
                Box::pin(async move { found })
            }),
            own_group: Arc::new(|| Box::pin(async { Some(1000) })),
            signal_group: Arc::new(move |pgid, sig| {
                if let Ok(mut signals) = signal.signals.lock() {
                    signals.push((pgid, sig));
                }
                if (sig == GroupSignal::Kill || pgid != 77)
                    && let Ok(mut members) = signal.members.lock()
                {
                    members.remove(&pgid);
                }
            }),
            wait: Arc::new(|_| Box::pin(async {})),
        }
    }

    fn state(members: &[i64], alive: &[i64], starts: &[(i64, &str)]) -> Arc<State> {
        Arc::new(State {
            members: Mutex::new(members.iter().copied().collect()),
            alive: alive.iter().copied().collect(),
            starts: starts.iter().map(|(p, s)| (*p, (*s).to_owned())).collect(),
            signals: Mutex::new(Vec::new()),
        })
    }

    #[tokio::test]
    async fn classify_group_table() {
        let st = state(
            &[10, 11, 12, 1000],
            &[10, 12],
            &[(10, START), (12, "Sat Oct 18 06:30:00 2026")],
        );
        let probes = fake_probes(st);
        let detached = LedgeredGroup {
            lifetime: GroupLifetime::Detached,
            ..owned(10, Some(START))
        };
        let cases = [
            (
                "a live leader with the recorded start",
                owned(10, Some(START)),
                GroupVerdict::Sweep,
            ),
            (
                "an exited leader whose group still has members",
                owned(11, Some(START)),
                GroupVerdict::Sweep,
            ),
            (
                "a leader pid reused by another process",
                owned(12, Some(START)),
                GroupVerdict::LeaderReused,
            ),
            (
                "a group with no members",
                owned(13, Some(START)),
                GroupVerdict::Empty,
            ),
            (
                "the server's own group",
                owned(1000, Some(START)),
                GroupVerdict::OwnGroup,
            ),
            ("a detached entry", detached, GroupVerdict::Detached),
        ];
        for (label, group, verdict) in cases {
            assert_eq!(
                classify_group(&group, Some(1000), &probes).await,
                verdict,
                "{label}"
            );
        }
    }

    #[tokio::test]
    async fn signals_nothing_when_the_servers_own_group_is_unknown() {
        let st = state(&[10], &[10], &[(10, START)]);
        let probes = fake_probes(st);
        assert_eq!(
            classify_group(&owned(10, Some(START)), None, &probes).await,
            GroupVerdict::OwnGroup
        );
    }

    #[tokio::test]
    async fn terms_swept_groups_kills_the_ones_that_ignore_term_and_never_signals_released_or_detached_groups()
     {
        let st = state(
            &[10, 12, 77],
            &[10, 12, 77],
            &[(10, START), (12, "x"), (77, START)],
        );
        let probes = fake_probes(Arc::clone(&st));
        let detached = LedgeredGroup {
            launch_id: "d".to_owned(),
            lifetime: GroupLifetime::Detached,
            ..owned(10, Some(START))
        };
        let outcomes = sweep_groups(
            &[
                owned(10, Some(START)),
                owned(12, Some(START)),
                owned(77, Some(START)),
                detached,
            ],
            SweepOptions {
                grace_ms: 50,
                kill_wait_ms: 50,
                poll_ms: Some(10),
            },
            &probes,
        )
        .await;
        assert_eq!(
            *st.signals.lock().unwrap(),
            vec![
                (10, GroupSignal::Term),
                (77, GroupSignal::Term),
                (77, GroupSignal::Kill),
            ]
        );
        let shape: Vec<(&str, GroupVerdict, Option<bool>)> = outcomes
            .iter()
            .map(|o| (o.launch_id.as_str(), o.verdict, o.survived))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("l10", GroupVerdict::Sweep, Some(false)),
                ("l12", GroupVerdict::LeaderReused, None),
                ("l77", GroupVerdict::Sweep, Some(false)),
                ("d", GroupVerdict::Detached, None),
            ]
        );
    }

    #[test]
    fn start_time_parsing_and_tolerance() {
        let a = process_start_time_ms(START);
        let b = process_start_time_ms("Sat Oct 18 05:00:02 2026");
        let c = process_start_time_ms("Sat Oct 18 06:30:00 2026");
        assert!(same_process_start_ms(a, b));
        assert!(!same_process_start_ms(a, c));
        assert!(!same_process_start_ms(None, a));
        assert_eq!(process_start_time_ms("x"), None);
    }

    #[cfg(unix)]
    mod real_groups {
        use super::*;
        use std::os::unix::process::CommandExt;
        use std::process::{Command, Stdio};

        /// The leader backgrounds a sleep (same group) and then blocks on stdin until the test lets it exit.
        async fn group_with_exited_leader() -> (i64, Option<String>) {
            let mut leader = Command::new("sh")
                .args(["-c", "sleep 300 & read _"])
                .process_group(0)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let pid = leader.id();
            let pgid = i64::from(pid);
            let leader_start = read_process_start_time(pid).await;
            drop(leader.stdin.take());
            tokio::task::spawn_blocking(move || leader.wait())
                .await
                .unwrap()
                .unwrap();
            (pgid, leader_start)
        }

        #[tokio::test]
        async fn kills_an_owned_group_whose_leader_already_exited() {
            let probes = default_group_probes();
            let (pgid, leader_start) = group_with_exited_leader().await;
            assert!((probes.group_has_members)(pgid));
            let group = LedgeredGroup {
                launch_id: "g".to_owned(),
                pgid,
                leader_start,
                lifetime: GroupLifetime::Owned,
            };
            let outcomes = sweep_groups(
                &[group],
                SweepOptions {
                    grace_ms: 3_000,
                    kill_wait_ms: 3_000,
                    poll_ms: None,
                },
                &probes,
            )
            .await;
            assert_eq!(outcomes[0].verdict, GroupVerdict::Sweep);
            assert_eq!(outcomes[0].survived, Some(false));
            assert!(!(probes.group_has_members)(pgid));
        }

        #[tokio::test]
        async fn leaves_a_detached_group_running() {
            let probes = default_group_probes();
            let (pgid, leader_start) = group_with_exited_leader().await;
            let group = LedgeredGroup {
                launch_id: "g".to_owned(),
                pgid,
                leader_start,
                lifetime: GroupLifetime::Detached,
            };
            let outcomes = sweep_groups(
                &[group],
                SweepOptions {
                    grace_ms: 100,
                    kill_wait_ms: 100,
                    poll_ms: None,
                },
                &probes,
            )
            .await;
            let still_there = (probes.group_has_members)(pgid);
            (probes.signal_group)(pgid, GroupSignal::Kill);
            assert_eq!(outcomes[0].verdict, GroupVerdict::Detached);
            assert!(still_there);
        }
    }
}
