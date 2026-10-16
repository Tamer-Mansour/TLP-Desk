//! Keeps exactly one session agent running in the target session.
//!
//! The service calls [`AgentSupervisor::set_target`] / [`AgentSupervisor::restart`] when the
//! [`SessionTracker`](super::SessionTracker) says so, and [`AgentSupervisor::tick`]
//! periodically. An agent that exits (crash, logoff tearing the session down, killed by a
//! user with admin rights) is started again with exponential backoff, so a broken agent never
//! spins the CPU; one that ran for [`RestartPolicy::healthy_after`] restarts quickly.
//!
//! Process creation is behind [`AgentLauncher`], so the logic is tested with fakes and the
//! Windows launcher (`CreateProcessAsUserW`) lives in the platform module.

use std::time::{Duration, Instant};

use crate::HostError;

/// Starts agent processes.
pub trait AgentLauncher: Send {
    /// The running process type.
    type Process: AgentProcess;

    /// Starts the agent in Windows session `session`. `seq` is unique per launch (it goes
    /// into the agent's pipe name).
    fn launch(&mut self, session: u32, seq: u32) -> Result<Self::Process, HostError>;
}

/// A running agent process.
pub trait AgentProcess: Send {
    /// Process id (the service checks that the agent pipe's server has this id).
    fn pid(&self) -> u32;
    /// The exit code if the process has exited, without blocking.
    fn try_wait(&mut self) -> Option<u32>;
    /// Ends the process (best effort; it is also told to exit over IPC first).
    fn terminate(&mut self);
}

/// Restart timing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartPolicy {
    /// Delay before the first restart after a failure.
    pub initial_backoff: Duration,
    /// Upper bound of the doubling delay.
    pub max_backoff: Duration,
    /// An agent that ran at least this long counts as healthy: its exit resets the backoff.
    pub healthy_after: Duration,
}

impl Default for RestartPolicy {
    fn default() -> Self {
        Self {
            initial_backoff: Duration::from_millis(500),
            max_backoff: Duration::from_secs(30),
            healthy_after: Duration::from_secs(10),
        }
    }
}

impl RestartPolicy {
    /// Delay after `failures` consecutive failures (1 = first failure).
    pub fn backoff(&self, failures: u32) -> Duration {
        let shift = failures.saturating_sub(1).min(16);
        self.initial_backoff
            .saturating_mul(1u32 << shift)
            .min(self.max_backoff)
    }
}

/// What happened during a supervisor call (for logging and for (re)connecting the IPC).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorEvent {
    /// An agent is running; connect to its pipe.
    Started { session: u32, pid: u32, seq: u32 },
    /// The agent exited on its own; it will be restarted after `retry_in`.
    Exited {
        session: u32,
        pid: u32,
        code: u32,
        retry_in: Duration,
    },
    /// The supervisor stopped the agent (retarget, restart or shutdown).
    Stopped { session: u32, pid: u32 },
    /// Starting the agent failed; the next attempt is in `retry_in`.
    LaunchFailed {
        session: u32,
        error: String,
        retry_in: Duration,
    },
}

struct Running<P> {
    session: u32,
    process: P,
    started: Instant,
    seq: u32,
}

/// One agent per target session, restarted with backoff.
pub struct AgentSupervisor<L: AgentLauncher> {
    launcher: L,
    policy: RestartPolicy,
    target: Option<u32>,
    running: Option<Running<L::Process>>,
    failures: u32,
    next_try: Option<Instant>,
    seq: u32,
}

impl<L: AgentLauncher> std::fmt::Debug for AgentSupervisor<L> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentSupervisor")
            .field("target", &self.target)
            .field("running", &self.running())
            .field("failures", &self.failures)
            .finish_non_exhaustive()
    }
}

impl<L: AgentLauncher> AgentSupervisor<L> {
    /// A supervisor with no target (nothing runs until [`AgentSupervisor::set_target`]).
    pub fn new(launcher: L, policy: RestartPolicy) -> Self {
        Self {
            launcher,
            policy,
            target: None,
            running: None,
            failures: 0,
            next_try: None,
            seq: 0,
        }
    }

    /// The session the agent should run in.
    pub fn target(&self) -> Option<u32> {
        self.target
    }

    /// `(session, pid, seq)` of the running agent.
    pub fn running(&self) -> Option<(u32, u32, u32)> {
        self.running
            .as_ref()
            .map(|r| (r.session, r.process.pid(), r.seq))
    }

    /// Moves the agent to `session` (`None` stops it). A change launches immediately on the
    /// next [`AgentSupervisor::tick`] (no backoff: this is not a failure).
    pub fn set_target(&mut self, session: Option<u32>, now: Instant) -> Vec<SupervisorEvent> {
        let mut events = Vec::new();
        if self.running.as_ref().map(|r| r.session) != session {
            events.extend(self.stop());
        }
        if self.target != session {
            self.failures = 0;
            self.next_try = Some(now);
        }
        self.target = session;
        events.extend(self.tick(now));
        events
    }

    /// Stops the agent and starts a fresh one in the same session right away.
    pub fn restart(&mut self, now: Instant) -> Vec<SupervisorEvent> {
        let mut events: Vec<_> = self.stop().into_iter().collect();
        self.failures = 0;
        self.next_try = Some(now);
        events.extend(self.tick(now));
        events
    }

    /// Notices an exited agent and (re)launches when due. Call about twice a second.
    pub fn tick(&mut self, now: Instant) -> Vec<SupervisorEvent> {
        let mut events = Vec::new();
        if let Some(r) = self.running.as_mut() {
            if let Some(code) = r.process.try_wait() {
                let ran = now.saturating_duration_since(r.started);
                if ran >= self.policy.healthy_after {
                    self.failures = 0;
                }
                self.failures = self.failures.saturating_add(1);
                let retry_in = self.policy.backoff(self.failures);
                self.next_try = Some(now + retry_in);
                events.push(SupervisorEvent::Exited {
                    session: r.session,
                    pid: r.process.pid(),
                    code,
                    retry_in,
                });
                self.running = None;
            }
        }
        let due = self.next_try.is_none_or(|t| now >= t);
        if let (None, Some(session), true) = (&self.running, self.target, due) {
            self.seq = self.seq.wrapping_add(1);
            match self.launcher.launch(session, self.seq) {
                Ok(process) => {
                    events.push(SupervisorEvent::Started {
                        session,
                        pid: process.pid(),
                        seq: self.seq,
                    });
                    self.running = Some(Running {
                        session,
                        process,
                        started: now,
                        seq: self.seq,
                    });
                    self.next_try = None;
                }
                Err(e) => {
                    self.failures = self.failures.saturating_add(1);
                    let retry_in = self.policy.backoff(self.failures);
                    self.next_try = Some(now + retry_in);
                    events.push(SupervisorEvent::LaunchFailed {
                        session,
                        error: e.to_string(),
                        retry_in,
                    });
                }
            }
        }
        events
    }

    /// Stops the agent and forgets the target (service stopping).
    pub fn shutdown(&mut self) -> Vec<SupervisorEvent> {
        self.target = None;
        self.stop().into_iter().collect()
    }

    fn stop(&mut self) -> Option<SupervisorEvent> {
        let mut r = self.running.take()?;
        r.process.terminate();
        Some(SupervisorEvent::Stopped {
            session: r.session,
            pid: r.process.pid(),
        })
    }
}

impl<L: AgentLauncher> Drop for AgentSupervisor<L> {
    fn drop(&mut self) {
        // Never leave an orphaned SYSTEM agent behind.
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default)]
    struct World {
        next_pid: u32,
        /// (session, seq, pid) of every launch.
        launched: Vec<(u32, u32, u32)>,
        exited: Vec<(u32, u32)>,
        terminated: Vec<u32>,
        fail_launches: u32,
    }

    #[derive(Clone, Default)]
    struct FakeLauncher(Arc<Mutex<World>>);

    struct FakeProcess {
        pid: u32,
        world: Arc<Mutex<World>>,
    }

    impl AgentLauncher for FakeLauncher {
        type Process = FakeProcess;
        fn launch(&mut self, session: u32, seq: u32) -> Result<FakeProcess, HostError> {
            let mut w = self.0.lock().unwrap();
            if w.fail_launches > 0 {
                w.fail_launches -= 1;
                return Err(HostError::Os {
                    call: "CreateProcessAsUserW",
                    code: 5,
                });
            }
            w.next_pid += 1;
            let pid = 1000 + w.next_pid;
            w.launched.push((session, seq, pid));
            Ok(FakeProcess {
                pid,
                world: self.0.clone(),
            })
        }
    }

    impl AgentProcess for FakeProcess {
        fn pid(&self) -> u32 {
            self.pid
        }
        fn try_wait(&mut self) -> Option<u32> {
            let w = self.world.lock().unwrap();
            w.exited
                .iter()
                .find(|(p, _)| *p == self.pid)
                .map(|(_, c)| *c)
        }
        fn terminate(&mut self) {
            self.world.lock().unwrap().terminated.push(self.pid);
        }
    }

    fn setup() -> (AgentSupervisor<FakeLauncher>, Arc<Mutex<World>>, Instant) {
        let l = FakeLauncher::default();
        let w = l.0.clone();
        (
            AgentSupervisor::new(l, RestartPolicy::default()),
            w,
            Instant::now(),
        )
    }

    #[test]
    fn starts_in_target_and_retargets() {
        let (mut s, w, t0) = setup();
        assert!(s.tick(t0).is_empty(), "no target, no agent");
        let ev = s.set_target(Some(1), t0);
        assert_eq!(
            ev,
            vec![SupervisorEvent::Started {
                session: 1,
                pid: 1001,
                seq: 1
            }]
        );
        assert_eq!(s.running(), Some((1, 1001, 1)));
        // Same target again: nothing happens.
        assert!(s.set_target(Some(1), t0).is_empty());
        // Fast user switching: old agent stopped, new one launched in session 2 at once.
        let ev = s.set_target(Some(2), t0);
        assert_eq!(
            ev,
            vec![
                SupervisorEvent::Stopped {
                    session: 1,
                    pid: 1001
                },
                SupervisorEvent::Started {
                    session: 2,
                    pid: 1002,
                    seq: 2
                },
            ]
        );
        assert_eq!(w.lock().unwrap().terminated, vec![1001]);
        // No console session: stop and stay stopped.
        let ev = s.set_target(None, t0);
        assert_eq!(
            ev,
            vec![SupervisorEvent::Stopped {
                session: 2,
                pid: 1002
            }]
        );
        assert!(s.tick(t0 + Duration::from_secs(60)).is_empty());
    }

    #[test]
    fn crashed_agent_restarts_with_growing_backoff() {
        let (mut s, w, t0) = setup();
        s.set_target(Some(1), t0);
        let mut now = t0;
        let mut delays = Vec::new();
        for _ in 0..4 {
            let pid = s.running().unwrap().1;
            w.lock().unwrap().exited.push((pid, 0xC000_0005));
            now += Duration::from_millis(100);
            let ev = s.tick(now);
            let SupervisorEvent::Exited { retry_in, .. } = ev[0] else {
                panic!("{ev:?}");
            };
            delays.push(retry_in);
            // Not before the backoff...
            assert!(s.tick(now + retry_in - Duration::from_millis(1)).is_empty());
            // ...but right after it.
            now += retry_in;
            assert!(matches!(
                s.tick(now)[..],
                [SupervisorEvent::Started { session: 1, .. }]
            ));
        }
        assert_eq!(
            delays,
            vec![
                Duration::from_millis(500),
                Duration::from_secs(1),
                Duration::from_secs(2),
                Duration::from_secs(4)
            ]
        );
        // A healthy run resets the backoff.
        let pid = s.running().unwrap().1;
        now += Duration::from_secs(11);
        w.lock().unwrap().exited.push((pid, 0));
        let ev = s.tick(now);
        assert!(matches!(
            ev[..],
            [SupervisorEvent::Exited { retry_in, .. }] if retry_in == Duration::from_millis(500)
        ));
    }

    #[test]
    fn backoff_is_capped() {
        let p = RestartPolicy::default();
        assert_eq!(p.backoff(0), Duration::from_millis(500));
        assert_eq!(p.backoff(7), Duration::from_secs(30));
        assert_eq!(p.backoff(u32::MAX), Duration::from_secs(30));
    }

    #[test]
    fn launch_failures_retry_with_backoff() {
        let (mut s, w, t0) = setup();
        w.lock().unwrap().fail_launches = 2;
        let ev = s.set_target(Some(1), t0);
        assert!(matches!(
            ev[..],
            [SupervisorEvent::LaunchFailed { session: 1, retry_in, .. }] if retry_in == Duration::from_millis(500)
        ));
        let ev = s.tick(t0 + Duration::from_millis(500));
        assert!(matches!(
            ev[..],
            [SupervisorEvent::LaunchFailed { retry_in, .. }] if retry_in == Duration::from_secs(1)
        ));
        let ev = s.tick(t0 + Duration::from_millis(1500));
        assert!(matches!(ev[..], [SupervisorEvent::Started { .. }]));
    }

    #[test]
    fn restart_and_shutdown_terminate_the_agent() {
        let (mut s, w, t0) = setup();
        s.set_target(Some(3), t0);
        let ev = s.restart(t0);
        assert_eq!(ev.len(), 2);
        assert!(matches!(ev[1], SupervisorEvent::Started { session: 3, .. }));
        let ev = s.shutdown();
        assert!(matches!(
            ev[..],
            [SupervisorEvent::Stopped { session: 3, .. }]
        ));
        assert_eq!(w.lock().unwrap().terminated.len(), 2);
        assert!(s.tick(t0 + Duration::from_secs(5)).is_empty());
    }

    #[test]
    fn drop_terminates_a_running_agent() {
        let (mut s, w, t0) = setup();
        s.set_target(Some(1), t0);
        drop(s);
        assert_eq!(w.lock().unwrap().terminated, vec![1001]);
    }
}
