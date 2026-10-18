use super::{model::ResourceSample, unix_ms};
use std::{
    sync::{Arc, Condvar, Mutex},
    thread::JoinHandle,
    time::Duration,
};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};
use tokio::time::Instant;

pub(super) trait Provider: Send + 'static {
    fn refresh(&mut self) -> Option<(f64, u64)>;
}
struct CurrentProcess {
    system: System,
    pid: sysinfo::Pid,
}
impl Provider for CurrentProcess {
    fn refresh(&mut self) -> Option<(f64, u64)> {
        let updated = self.system.refresh_processes_specifics(
            ProcessesToUpdate::Some(&[self.pid]),
            true,
            ProcessRefreshKind::nothing()
                .with_cpu()
                .with_memory()
                .without_tasks(),
        );
        if updated == 0 {
            return None;
        }
        self.system
            .process(self.pid)
            .map(|process| (f64::from(process.cpu_usage()), process.memory()))
    }
}
#[derive(Default)]
pub(super) struct SamplerState {
    previous_refresh: Option<Instant>,
}
impl SamplerState {
    pub fn record(
        &mut self,
        result: Option<(f64, u64)>,
        at: Instant,
        unix_ms: u64,
    ) -> ResourceSample {
        let cpu_ready = self.previous_refresh.is_some_and(|previous| {
            at.saturating_duration_since(previous) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL
        });
        self.previous_refresh = result.map(|_| at);
        ResourceSample {
            at,
            unix_ms,
            cpu_percent: result
                .and_then(|(cpu, _)| (cpu_ready && cpu.is_finite() && cpu >= 0.0).then_some(cpu)),
            rss_bytes: result.map(|(_, rss)| rss),
            available: result.is_some(),
        }
    }
}

pub(super) struct ProcessWorker {
    started_at: Instant,
    latest: Arc<Mutex<Option<ResourceSample>>>,
    cancel: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}
impl ProcessWorker {
    pub fn start() -> Result<Self, super::DashboardError> {
        let pid = sysinfo::get_current_pid()
            .map_err(|_| super::DashboardError("cannot determine dashboard process PID"))?;
        Self::with_provider(CurrentProcess {
            system: System::new(),
            pid,
        })
    }
    pub fn with_provider(mut provider: impl Provider) -> Result<Self, super::DashboardError> {
        let started_at = Instant::now();
        let latest = Arc::new(Mutex::new(None));
        let cancel = Arc::new((Mutex::new(false), Condvar::new()));
        let thread_latest = latest.clone();
        let thread_cancel = cancel.clone();
        let thread = std::thread::Builder::new()
            .name("campfire-admin-process".into())
            .spawn(move || {
                let mut state = SamplerState::default();
                loop {
                    if *thread_cancel
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                    {
                        break;
                    }
                    let raw = provider.refresh();
                    let sample = state.record(raw, Instant::now(), unix_ms());
                    if *thread_cancel
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner())
                    {
                        break;
                    }
                    *thread_latest
                        .lock()
                        .unwrap_or_else(|error| error.into_inner()) = Some(sample);
                    let cancel = thread_cancel
                        .0
                        .lock()
                        .unwrap_or_else(|error| error.into_inner());
                    let (cancel, _) = thread_cancel
                        .1
                        .wait_timeout_while(
                            cancel,
                            Duration::from_secs(1).max(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL),
                            |cancel| !*cancel,
                        )
                        .unwrap_or_else(|error| error.into_inner());
                    if *cancel {
                        break;
                    }
                }
            })
            .map_err(|_| super::DashboardError("cannot start dashboard process sampler"))?;
        Ok(Self {
            started_at,
            latest,
            cancel,
            thread: Some(thread),
        })
    }
    pub fn latest(&self) -> Arc<Mutex<Option<ResourceSample>>> {
        self.latest.clone()
    }
    pub fn started_at(&self) -> Instant {
        self.started_at
    }
    pub fn cancel(&self) {
        *self
            .cancel
            .0
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        self.cancel.1.notify_all();
    }
    pub async fn shutdown(&mut self) {
        self.cancel();
        let deadline = Instant::now() + Duration::from_secs(2);
        while self
            .thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
            && Instant::now() < deadline
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        if let Some(thread) = self.thread.take()
            && thread.is_finished()
        {
            let _ = thread.join();
        }
        // A blocked OS call is detached; no replacement worker is spawned.
    }
}
impl Drop for ProcessWorker {
    fn drop(&mut self) {
        self.cancel();
    }
}
