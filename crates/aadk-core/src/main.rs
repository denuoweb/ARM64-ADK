use std::{
    cmp::{Ordering, Reverse},
    collections::{BinaryHeap, HashMap, HashSet, VecDeque},
    fs,
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};

use aadk_proto::aadk::v1::{
    job_service_server::{JobService, JobServiceServer},
    CancelJobRequest, CancelJobResponse, ErrorCode, ErrorDetail, GetJobRequest, GetJobResponse, Id,
    Job, JobCompleted, JobEvent, JobEventKind, JobFailed, JobFilter, JobHistoryFilter, RunId,
    JobLogAppended, JobProgress, JobProgressUpdated, JobRef, JobState, JobStateChanged, KeyValue,
    ListJobHistoryRequest, ListJobHistoryResponse, ListJobsRequest, ListJobsResponse, LogChunk,
    PageInfo, Pagination, PublishJobEventRequest, PublishJobEventResponse, Remediation,
    StartJobRequest, StartJobResponse, StreamJobEventsRequest, StreamRunEventsRequest, Timestamp,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, mpsc, Mutex, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::{info, warn};
use uuid::Uuid;

const BROADCAST_CAPACITY: usize = 1024;
const HISTORY_CAPACITY: usize = 2048;
const STATE_FILE_NAME: &str = "jobs.json";
const DEFAULT_JOB_HISTORY_RETENTION_DAYS: u64 = 30;
const DEFAULT_JOB_HISTORY_MAX: usize = 200;
const PERSIST_DEBOUNCE_MS: u64 = 250;
const RETENTION_TICK_SECS: u64 = 300;
const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 200;
const DEFAULT_RUN_STREAM_BUFFER_MAX: usize = 512;
const DEFAULT_RUN_STREAM_MAX_DELAY_MS: u64 = 1500;
const DEFAULT_RUN_STREAM_DISCOVERY_MS: u64 = 750;
const DEFAULT_RUN_STREAM_FLUSH_MS: u64 = 200;

#[derive(Clone, Copy)]
struct RetentionPolicy {
    max_jobs: usize,
    max_age: Option<Duration>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct JobStateFile {
    #[serde(default = "default_schema_version")]
    schema_version: u32,
    jobs: Vec<PersistedJob>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedJob {
    job_id: String,
    job_type: String,
    state: i32,
    created_at_unix_millis: i64,
    started_at_unix_millis: Option<i64>,
    finished_at_unix_millis: Option<i64>,
    display_name: String,
    correlation_id: String,
    run_id: String,
    project_id: Option<String>,
    target_id: Option<String>,
    toolchain_set_id: Option<String>,
    history: Vec<PersistedEvent>,
}

#[derive(Clone, Serialize, Deserialize)]
struct PersistedEvent {
    at_unix_millis: i64,
    payload: PersistedEventPayload,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
enum PersistedEventPayload {
    StateChanged { new_state: i32 },
    Progress { progress: Option<PersistedJobProgress> },
    Log { chunk: Option<PersistedLogChunk> },
    Completed { summary: String, outputs: Vec<KeyValueRecord> },
    Failed { error: Option<ErrorDetailRecord> },
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedJobProgress {
    percent: u32,
    phase: String,
    metrics: Vec<KeyValueRecord>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct PersistedLogChunk {
    stream: String,
    data: Vec<u8>,
    truncated: bool,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct KeyValueRecord {
    key: String,
    value: String,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct RemediationRecord {
    title: String,
    description: String,
    action_id: String,
    params: Vec<KeyValueRecord>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default)]
struct ErrorDetailRecord {
    code: i32,
    message: String,
    technical_details: String,
    remedies: Vec<RemediationRecord>,
    correlation_id: String,
}

fn default_schema_version() -> u32 {
    1
}

impl KeyValueRecord {
    fn from_proto(item: &KeyValue) -> Self {
        Self {
            key: item.key.clone(),
            value: item.value.clone(),
        }
    }

    fn into_proto(self) -> KeyValue {
        KeyValue {
            key: self.key,
            value: self.value,
        }
    }
}

impl RemediationRecord {
    fn from_proto(item: &Remediation) -> Self {
        Self {
            title: item.title.clone(),
            description: item.description.clone(),
            action_id: item.action_id.clone(),
            params: item.params.iter().map(KeyValueRecord::from_proto).collect(),
        }
    }

    fn into_proto(self) -> Remediation {
        Remediation {
            title: self.title,
            description: self.description,
            action_id: self.action_id,
            params: self
                .params
                .into_iter()
                .map(KeyValueRecord::into_proto)
                .collect(),
        }
    }
}

impl ErrorDetailRecord {
    fn from_proto(item: &ErrorDetail) -> Self {
        Self {
            code: item.code,
            message: item.message.clone(),
            technical_details: item.technical_details.clone(),
            remedies: item
                .remedies
                .iter()
                .map(RemediationRecord::from_proto)
                .collect(),
            correlation_id: item.correlation_id.clone(),
        }
    }

    fn into_proto(self) -> ErrorDetail {
        ErrorDetail {
            code: self.code,
            message: self.message,
            technical_details: self.technical_details,
            remedies: self
                .remedies
                .into_iter()
                .map(RemediationRecord::into_proto)
                .collect(),
            correlation_id: self.correlation_id,
        }
    }
}

impl PersistedJobProgress {
    fn from_proto(item: &JobProgress) -> Self {
        Self {
            percent: item.percent,
            phase: item.phase.clone(),
            metrics: item
                .metrics
                .iter()
                .map(KeyValueRecord::from_proto)
                .collect(),
        }
    }

    fn into_proto(self) -> JobProgress {
        JobProgress {
            percent: self.percent,
            phase: self.phase,
            metrics: self
                .metrics
                .into_iter()
                .map(KeyValueRecord::into_proto)
                .collect(),
        }
    }
}

impl PersistedLogChunk {
    fn from_proto(item: &LogChunk) -> Self {
        Self {
            stream: item.stream.clone(),
            data: item.data.clone(),
            truncated: item.truncated,
        }
    }

    fn into_proto(self) -> LogChunk {
        LogChunk {
            stream: self.stream,
            data: self.data,
            truncated: self.truncated,
        }
    }
}

impl PersistedEventPayload {
    fn from_proto(payload: &aadk_proto::aadk::v1::job_event::Payload) -> Self {
        match payload {
            aadk_proto::aadk::v1::job_event::Payload::StateChanged(state) => {
                PersistedEventPayload::StateChanged {
                    new_state: state.new_state,
                }
            }
            aadk_proto::aadk::v1::job_event::Payload::Progress(progress) => {
                PersistedEventPayload::Progress {
                    progress: progress
                        .progress
                        .as_ref()
                        .map(PersistedJobProgress::from_proto),
                }
            }
            aadk_proto::aadk::v1::job_event::Payload::Log(log) => PersistedEventPayload::Log {
                chunk: log.chunk.as_ref().map(PersistedLogChunk::from_proto),
            },
            aadk_proto::aadk::v1::job_event::Payload::Completed(done) => {
                PersistedEventPayload::Completed {
                    summary: done.summary.clone(),
                    outputs: done.outputs.iter().map(KeyValueRecord::from_proto).collect(),
                }
            }
            aadk_proto::aadk::v1::job_event::Payload::Failed(failed) => {
                PersistedEventPayload::Failed {
                    error: failed.error.as_ref().map(ErrorDetailRecord::from_proto),
                }
            }
        }
    }

    fn into_proto(self) -> aadk_proto::aadk::v1::job_event::Payload {
        match self {
            PersistedEventPayload::StateChanged { new_state } => {
                aadk_proto::aadk::v1::job_event::Payload::StateChanged(JobStateChanged {
                    new_state,
                })
            }
            PersistedEventPayload::Progress { progress } => {
                aadk_proto::aadk::v1::job_event::Payload::Progress(JobProgressUpdated {
                    progress: progress.map(PersistedJobProgress::into_proto),
                })
            }
            PersistedEventPayload::Log { chunk } => {
                aadk_proto::aadk::v1::job_event::Payload::Log(JobLogAppended {
                    chunk: chunk.map(PersistedLogChunk::into_proto),
                })
            }
            PersistedEventPayload::Completed { summary, outputs } => {
                aadk_proto::aadk::v1::job_event::Payload::Completed(JobCompleted {
                    summary,
                    outputs: outputs
                        .into_iter()
                        .map(KeyValueRecord::into_proto)
                        .collect(),
                })
            }
            PersistedEventPayload::Failed { error } => {
                aadk_proto::aadk::v1::job_event::Payload::Failed(JobFailed {
                    error: error.map(ErrorDetailRecord::into_proto),
                })
            }
        }
    }
}

impl PersistedEvent {
    fn from_proto(event: &JobEvent) -> Option<Self> {
        let payload = event.payload.as_ref()?;
        let at_unix_millis = event.at.as_ref().map(|ts| ts.unix_millis).unwrap_or_default();
        Some(Self {
            at_unix_millis,
            payload: PersistedEventPayload::from_proto(payload),
        })
    }

    fn into_proto(self, job_id: &str) -> JobEvent {
        JobEvent {
            at: Some(Timestamp {
                unix_millis: self.at_unix_millis,
            }),
            job_id: Some(Id {
                value: job_id.to_string(),
            }),
            payload: Some(self.payload.into_proto()),
        }
    }
}

impl PersistedJob {
    fn from_inner(job_id: &str, inner: &JobRecordInner) -> Self {
        let job = &inner.job;
        Self {
            job_id: job_id.to_string(),
            job_type: job.job_type.clone(),
            state: job.state,
            created_at_unix_millis: job
                .created_at
                .as_ref()
                .map(|ts| ts.unix_millis)
                .unwrap_or_default(),
            started_at_unix_millis: job.started_at.as_ref().map(|ts| ts.unix_millis),
            finished_at_unix_millis: job.finished_at.as_ref().map(|ts| ts.unix_millis),
            display_name: job.display_name.clone(),
            correlation_id: job.correlation_id.clone(),
            run_id: job
                .run_id
                .as_ref()
                .map(|id| id.value.clone())
                .unwrap_or_default(),
            project_id: job.project_id.as_ref().map(|id| id.value.clone()),
            target_id: job.target_id.as_ref().map(|id| id.value.clone()),
            toolchain_set_id: job.toolchain_set_id.as_ref().map(|id| id.value.clone()),
            history: inner
                .history
                .iter()
                .filter_map(PersistedEvent::from_proto)
                .collect(),
        }
    }

    fn into_inner(self) -> JobRecordInner {
        let PersistedJob {
            job_id,
            job_type,
            state,
            created_at_unix_millis,
            started_at_unix_millis,
            finished_at_unix_millis,
            display_name,
            correlation_id,
            run_id,
            project_id,
            target_id,
            toolchain_set_id,
            mut history,
        } = self;

        if history.len() > HISTORY_CAPACITY {
            let trim = history.len() - HISTORY_CAPACITY;
            history.drain(0..trim);
        }

        let mut deque = VecDeque::with_capacity(HISTORY_CAPACITY);
        for evt in history {
            deque.push_back(evt.into_proto(&job_id));
        }

        let (btx, _brx) = broadcast::channel::<JobEvent>(BROADCAST_CAPACITY);
        let (cancel_tx, _cancel_rx) = watch::channel(false);

        let correlation = if correlation_id.is_empty() {
            job_id.clone()
        } else {
            correlation_id
        };
        let run_ref = if run_id.trim().is_empty() {
            None
        } else {
            Some(RunId { value: run_id })
        };

        let job = Job {
            job_id: Some(Id { value: job_id.clone() }),
            job_type,
            state,
            created_at: Some(Timestamp {
                unix_millis: created_at_unix_millis,
            }),
            started_at: started_at_unix_millis.map(|ms| Timestamp { unix_millis: ms }),
            finished_at: finished_at_unix_millis.map(|ms| Timestamp { unix_millis: ms }),
            display_name,
            correlation_id: correlation,
            run_id: run_ref,
            project_id: project_id.map(|value| Id { value }),
            target_id: target_id.map(|value| Id { value }),
            toolchain_set_id: toolchain_set_id.map(|value| Id { value }),
        };

        JobRecordInner {
            job,
            broadcaster: btx,
            history: deque,
            cancel_tx,
        }
    }

    fn is_active(&self) -> bool {
        matches!(
            JobState::try_from(self.state).unwrap_or(JobState::Unspecified),
            JobState::Queued | JobState::Running
        )
    }

    fn sort_key(&self) -> i64 {
        self.finished_at_unix_millis
            .unwrap_or(self.created_at_unix_millis)
    }
}

fn is_known_job_type(job_type: &str) -> bool {
    matches!(
        job_type,
        "demo.job"
            | "workflow.pipeline"
            | "project.create"
            | "build.run"
            | "toolchain.install"
            | "toolchain.verify"
            | "targets.install"
            | "targets.launch"
            | "targets.stop"
            | "targets.cuttlefish.install"
            | "targets.cuttlefish.start"
            | "targets.cuttlefish.stop"
            | "observe.support_bundle"
            | "observe.evidence_bundle"
    )
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn now_ts() -> Timestamp {
    Timestamp {
        unix_millis: now_millis(),
    }
}

fn parse_page(page: Option<Pagination>) -> Result<(usize, usize), Status> {
    let page = page.unwrap_or_default();
    let mut page_size = if page.page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page.page_size as usize
    };
    if page_size > MAX_PAGE_SIZE {
        page_size = MAX_PAGE_SIZE;
    }
    let start = if page.page_token.trim().is_empty() {
        0
    } else {
        page.page_token
            .parse::<usize>()
            .map_err(|_| Status::invalid_argument("invalid page_token"))?
    };
    Ok((start, page_size))
}

fn millis_from_ts(ts: &Option<Timestamp>) -> Option<i64> {
    ts.as_ref().map(|ts| ts.unix_millis)
}

fn job_sort_key(job: &Job) -> i64 {
    millis_from_ts(&job.finished_at).unwrap_or_else(|| {
        millis_from_ts(&job.created_at).unwrap_or_default()
    })
}

fn job_matches_filter(job: &Job, filter: &JobFilter) -> bool {
    if !filter.job_types.is_empty()
        && !filter.job_types.iter().any(|t| t == &job.job_type)
    {
        return false;
    }
    if !filter.correlation_id.trim().is_empty()
        && job.correlation_id != filter.correlation_id
    {
        return false;
    }
    if let Some(run_id) = filter
        .run_id
        .as_ref()
        .map(|id| id.value.trim())
        .filter(|value| !value.is_empty())
    {
        let job_run_id = job.run_id.as_ref().map(|id| id.value.as_str());
        if job_run_id != Some(run_id) {
            return false;
        }
    }
    if !filter.states.is_empty() && !filter.states.contains(&job.state) {
        return false;
    }
    if let Some(after) = filter.created_after.as_ref() {
        let created = millis_from_ts(&job.created_at).unwrap_or_default();
        if created < after.unix_millis {
            return false;
        }
    }
    if let Some(before) = filter.created_before.as_ref() {
        let created = millis_from_ts(&job.created_at).unwrap_or_default();
        if created > before.unix_millis {
            return false;
        }
    }
    if let Some(after) = filter.finished_after.as_ref() {
        match millis_from_ts(&job.finished_at) {
            Some(finished) if finished >= after.unix_millis => {}
            _ => return false,
        }
    }
    if let Some(before) = filter.finished_before.as_ref() {
        match millis_from_ts(&job.finished_at) {
            Some(finished) if finished <= before.unix_millis => {}
            _ => return false,
        }
    }
    true
}

fn event_kind(event: &JobEvent) -> JobEventKind {
    match event.payload {
        Some(aadk_proto::aadk::v1::job_event::Payload::StateChanged(_)) => {
            JobEventKind::StateChanged
        }
        Some(aadk_proto::aadk::v1::job_event::Payload::Progress(_)) => JobEventKind::Progress,
        Some(aadk_proto::aadk::v1::job_event::Payload::Log(_)) => JobEventKind::Log,
        Some(aadk_proto::aadk::v1::job_event::Payload::Completed(_)) => JobEventKind::Completed,
        Some(aadk_proto::aadk::v1::job_event::Payload::Failed(_)) => JobEventKind::Failed,
        None => JobEventKind::Unspecified,
    }
}

fn event_matches_filter(event: &JobEvent, filter: &JobHistoryFilter) -> bool {
    if !filter.kinds.is_empty() {
        let kind = event_kind(event) as i32;
        if !filter.kinds.contains(&kind) {
            return false;
        }
    }
    if let Some(after) = filter.after.as_ref() {
        let at = millis_from_ts(&event.at).unwrap_or_default();
        if at < after.unix_millis {
            return false;
        }
    }
    if let Some(before) = filter.before.as_ref() {
        let at = millis_from_ts(&event.at).unwrap_or_default();
        if at > before.unix_millis {
            return false;
        }
    }
    true
}

#[derive(Clone)]
struct RunStreamConfig {
    buffer_max_events: usize,
    max_delay: Duration,
    discovery_interval: Duration,
    flush_interval: Duration,
}

#[derive(Clone)]
struct BufferedEvent {
    at_unix_millis: i64,
    seq: u64,
    event: JobEvent,
}

impl Ord for BufferedEvent {
    fn cmp(&self, other: &Self) -> Ordering {
        match self.at_unix_millis.cmp(&other.at_unix_millis) {
            Ordering::Equal => self.seq.cmp(&other.seq),
            ordering => ordering,
        }
    }
}

impl PartialOrd for BufferedEvent {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for BufferedEvent {
    fn eq(&self, other: &Self) -> bool {
        self.at_unix_millis == other.at_unix_millis && self.seq == other.seq
    }
}

impl Eq for BufferedEvent {}

fn event_timestamp(event: &JobEvent) -> i64 {
    event
        .at
        .as_ref()
        .map(|ts| ts.unix_millis)
        .unwrap_or_else(now_millis)
}

fn job_matches_run(job: &Job, run_id: &str, correlation_id: &str) -> bool {
    let run_id = run_id.trim();
    let correlation_id = correlation_id.trim();
    if !run_id.is_empty() {
        let job_run = job.run_id.as_ref().map(|id| id.value.as_str()).unwrap_or("");
        if job_run == run_id {
            return true;
        }
        if job_run.is_empty() && !correlation_id.is_empty() && job.correlation_id == correlation_id {
            return true;
        }
        return false;
    }
    if !correlation_id.is_empty() {
        return job.correlation_id == correlation_id;
    }
    false
}

fn run_stream_config(req: &StreamRunEventsRequest) -> RunStreamConfig {
    let buffer_max = if req.buffer_max_events == 0 {
        read_env_usize("AADK_RUN_STREAM_BUFFER_MAX", DEFAULT_RUN_STREAM_BUFFER_MAX)
    } else {
        req.buffer_max_events as usize
    };
    let max_delay_ms = if req.max_delay_ms == 0 {
        read_env_u64("AADK_RUN_STREAM_MAX_DELAY_MS", DEFAULT_RUN_STREAM_MAX_DELAY_MS)
    } else {
        req.max_delay_ms
    };
    let discovery_ms = if req.discovery_interval_ms == 0 {
        read_env_u64("AADK_RUN_STREAM_DISCOVERY_MS", DEFAULT_RUN_STREAM_DISCOVERY_MS)
    } else {
        req.discovery_interval_ms
    };
    let flush_ms = read_env_u64("AADK_RUN_STREAM_FLUSH_MS", DEFAULT_RUN_STREAM_FLUSH_MS);
    RunStreamConfig {
        buffer_max_events: buffer_max.max(1),
        max_delay: Duration::from_millis(max_delay_ms.max(1)),
        discovery_interval: Duration::from_millis(discovery_ms.max(1)),
        flush_interval: Duration::from_millis(flush_ms.max(1)),
    }
}

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn state_file_path() -> PathBuf {
    data_dir().join("state").join(STATE_FILE_NAME)
}

fn write_json_atomic<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("json.tmp");
    let payload = serde_json::to_vec_pretty(value)
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?;
    fs::write(&tmp, payload)?;
    fs::rename(&tmp, path)?;
    Ok(())
}

fn read_env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn read_env_usize(key: &str, default: usize) -> usize {
    std::env::var(key)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(default)
}

fn retention_policy_from_env() -> RetentionPolicy {
    let retention_days =
        read_env_u64("AADK_JOB_HISTORY_RETENTION_DAYS", DEFAULT_JOB_HISTORY_RETENTION_DAYS);
    let max_jobs = read_env_usize("AADK_JOB_HISTORY_MAX", DEFAULT_JOB_HISTORY_MAX);
    let max_age = if retention_days == 0 {
        None
    } else {
        Some(Duration::from_secs(
            retention_days.saturating_mul(24 * 60 * 60),
        ))
    };
    RetentionPolicy { max_jobs, max_age }
}

fn mk_event(job_id: &str, payload: aadk_proto::aadk::v1::job_event::Payload) -> JobEvent {
    JobEvent {
        at: Some(now_ts()),
        job_id: Some(Id { value: job_id.to_string() }),
        payload: Some(payload),
    }
}

struct JobRecordInner {
    job: Job,
    broadcaster: broadcast::Sender<JobEvent>,
    history: VecDeque<JobEvent>,
    cancel_tx: watch::Sender<bool>,
}

async fn spawn_job_stream(
    job_id: String,
    rec: Arc<Mutex<JobRecordInner>>,
    include_history: bool,
    event_tx: mpsc::Sender<JobEvent>,
) {
    let (history, mut rx) = {
        let inner = rec.lock().await;
        let hist = if include_history {
            inner.history.iter().cloned().collect::<Vec<_>>()
        } else {
            vec![]
        };
        (hist, inner.broadcaster.subscribe())
    };

    tokio::spawn(async move {
        for evt in history {
            if event_tx.send(evt).await.is_err() {
                return;
            }
        }

        loop {
            match rx.recv().await {
                Ok(evt) => {
                    if event_tx.send(evt).await.is_err() {
                        return;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(skipped)) => {
                    let notice = mk_event(
                        &job_id,
                        aadk_proto::aadk::v1::job_event::Payload::Log(JobLogAppended {
                            chunk: Some(LogChunk {
                                stream: "server".into(),
                                data: format!(
                                    "WARNING: client lagged; skipped {skipped} events\n"
                                )
                                .into_bytes(),
                                truncated: false,
                            }),
                        }),
                    );
                    let _ = event_tx.send(notice).await;
                }
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

#[derive(Clone, Default)]
struct JobStore {
    inner: Arc<Mutex<HashMap<String, Arc<Mutex<JobRecordInner>>>>>,
}

impl JobStore {
    async fn insert(&self, job_id: &str, rec: JobRecordInner) {
        self.inner
            .lock()
            .await
            .insert(job_id.to_string(), Arc::new(Mutex::new(rec)));
    }

    async fn get(&self, job_id: &str) -> Option<Arc<Mutex<JobRecordInner>>> {
        self.inner.lock().await.get(job_id).cloned()
    }

    async fn list_jobs(&self) -> Vec<Job> {
        let entries = {
            let inner = self.inner.lock().await;
            inner.values().cloned().collect::<Vec<_>>()
        };
        let mut jobs = Vec::with_capacity(entries.len());
        for rec in entries {
            let inner = rec.lock().await;
            jobs.push(inner.job.clone());
        }
        jobs
    }

    async fn snapshot(&self) -> Vec<PersistedJob> {
        let entries = {
            let inner = self.inner.lock().await;
            inner
                .iter()
                .map(|(job_id, rec)| (job_id.clone(), rec.clone()))
                .collect::<Vec<_>>()
        };

        let mut jobs = Vec::with_capacity(entries.len());
        for (job_id, rec) in entries {
            let inner = rec.lock().await;
            jobs.push(PersistedJob::from_inner(&job_id, &inner));
        }
        jobs
    }

    async fn prune_to(&self, keep_ids: &HashSet<String>) {
        let mut inner = self.inner.lock().await;
        inner.retain(|job_id, _| keep_ids.contains(job_id));
    }
}

async fn discover_run_jobs(
    store: &JobStore,
    run_id: &str,
    correlation_id: &str,
    include_history: bool,
    known_jobs: &mut HashSet<String>,
    event_tx: &mpsc::Sender<JobEvent>,
) {
    let jobs = store.list_jobs().await;
    for job in jobs {
        let job_id = job
            .job_id
            .as_ref()
            .map(|id| id.value.clone())
            .unwrap_or_default();
        if job_id.is_empty() || known_jobs.contains(&job_id) {
            continue;
        }
        if !job_matches_run(&job, run_id, correlation_id) {
            continue;
        }
        known_jobs.insert(job_id.clone());
        if let Some(rec) = store.get(&job_id).await {
            spawn_job_stream(job_id, rec, include_history, event_tx.clone()).await;
        }
    }
}

async fn flush_run_buffer(
    buffer: &mut BinaryHeap<Reverse<BufferedEvent>>,
    out_tx: &mut mpsc::Sender<Result<JobEvent, Status>>,
    config: &RunStreamConfig,
    force: bool,
) -> bool {
    let max_delay_ms = config.max_delay.as_millis() as i64;
    loop {
        let should_flush = if force {
            !buffer.is_empty()
        } else if buffer.len() > config.buffer_max_events {
            true
        } else if let Some(Reverse(evt)) = buffer.peek() {
            now_millis().saturating_sub(evt.at_unix_millis) >= max_delay_ms
        } else {
            false
        };

        if !should_flush {
            break;
        }

        let Some(Reverse(next)) = buffer.pop() else {
            break;
        };
        if out_tx.send(Ok(next.event)).await.is_err() {
            return false;
        }
    }
    true
}

fn load_state_file() -> JobStateFile {
    let path = state_file_path();
    match fs::read_to_string(&path) {
        Ok(data) => match serde_json::from_str::<JobStateFile>(&data) {
            Ok(file) => file,
            Err(err) => {
                warn!("Failed to parse {}: {}", path.display(), err);
                JobStateFile::default()
            }
        },
        Err(err) => {
            if err.kind() != io::ErrorKind::NotFound {
                warn!("Failed to read {}: {}", path.display(), err);
            }
            JobStateFile::default()
        }
    }
}

fn apply_retention(jobs: &mut Vec<PersistedJob>, policy: &RetentionPolicy) {
    let now_ms = now_millis();
    let max_age_ms = policy.max_age.map(|age| age.as_millis() as i64);
    let mut active = Vec::new();
    let mut completed = Vec::new();

    for job in jobs.drain(..) {
        if job.is_active() {
            active.push(job);
        } else {
            completed.push(job);
        }
    }

    if let Some(max_age_ms) = max_age_ms {
        completed.retain(|job| now_ms.saturating_sub(job.sort_key()) <= max_age_ms);
    }

    if policy.max_jobs != 0 {
        let max_completed = policy.max_jobs.saturating_sub(active.len());
        if completed.len() > max_completed {
            completed.sort_by(|a, b| b.sort_key().cmp(&a.sort_key()));
            completed.truncate(max_completed);
        }
    }

    let mut kept = Vec::new();
    kept.extend(active);
    kept.extend(completed);
    kept.sort_by(|a, b| b.sort_key().cmp(&a.sort_key()));
    *jobs = kept;
}

async fn load_store(policy: &RetentionPolicy) -> JobStore {
    let mut state = load_state_file();
    apply_retention(&mut state.jobs, policy);
    let count = state.jobs.len();
    let store = JobStore::default();

    for job in state.jobs {
        let job_id = job.job_id.clone();
        store.insert(&job_id, job.into_inner()).await;
    }

    if count > 0 {
        info!("Loaded {} job(s) from {}", count, state_file_path().display());
    }

    store
}

async fn persist_state(store: &JobStore, policy: RetentionPolicy) -> io::Result<()> {
    let mut jobs = store.snapshot().await;
    apply_retention(&mut jobs, &policy);
    let keep_ids: HashSet<String> = jobs.iter().map(|job| job.job_id.clone()).collect();
    store.prune_to(&keep_ids).await;

    let file = JobStateFile {
        schema_version: default_schema_version(),
        jobs,
    };
    write_json_atomic(&state_file_path(), &file)
}

fn spawn_persist_worker(store: JobStore, policy: RetentionPolicy) -> mpsc::Sender<()> {
    let (tx, mut rx) = mpsc::channel::<()>(32);
    tokio::spawn(async move {
        while rx.recv().await.is_some() {
            tokio::time::sleep(Duration::from_millis(PERSIST_DEBOUNCE_MS)).await;
            while rx.try_recv().is_ok() {}
            if let Err(err) = persist_state(&store, policy).await {
                warn!("Failed to persist job history: {}", err);
            }
        }
    });
    tx
}

fn spawn_retention_tick(tx: mpsc::Sender<()>) {
    tokio::spawn(async move {
        let mut ticker = tokio::time::interval(Duration::from_secs(RETENTION_TICK_SECS));
        loop {
            ticker.tick().await;
            let _ = tx.try_send(());
        }
    });
}

#[derive(Clone)]
struct JobSvc {
    store: JobStore,
    persist_tx: mpsc::Sender<()>,
}

impl JobSvc {
    fn new(store: JobStore, persist_tx: mpsc::Sender<()>) -> Self {
        Self { store, persist_tx }
    }

    fn schedule_persist(&self) {
        let _ = self.persist_tx.try_send(());
    }

    async fn update_state_only(&self, job_id: &str, state: JobState) {
        if let Some(rec) = self.store.get(job_id).await {
            let mut inner = rec.lock().await;
            inner.job.state = state as i32;
            match state {
                JobState::Running => inner.job.started_at = Some(now_ts()),
                JobState::Success | JobState::Failed | JobState::Cancelled => {
                    inner.job.finished_at = Some(now_ts())
                }
                _ => {}
            }
            self.schedule_persist();
        }
    }

    async fn publish(&self, job_id: &str, payload: aadk_proto::aadk::v1::job_event::Payload) {
        if let Some(rec) = self.store.get(job_id).await {
            let evt = mk_event(job_id, payload);
            {
                let mut inner = rec.lock().await;
                // Maintain bounded history.
                if inner.history.len() >= HISTORY_CAPACITY {
                    inner.history.pop_front();
                }
                inner.history.push_back(evt.clone());

                // Broadcast (ignore send errors if no listeners).
                let _ = inner.broadcaster.send(evt);
            }
            self.schedule_persist();
        }
    }

    async fn set_state(&self, job_id: &str, state: JobState) {
        if let Some(rec) = self.store.get(job_id).await {
            let mut inner = rec.lock().await;
            inner.job.state = state as i32;
            match state {
                JobState::Running => inner.job.started_at = Some(now_ts()),
                JobState::Success | JobState::Failed | JobState::Cancelled => {
                    inner.job.finished_at = Some(now_ts())
                }
                _ => {}
            }
        }
        self.publish(job_id, aadk_proto::aadk::v1::job_event::Payload::StateChanged(JobStateChanged {
            new_state: state as i32,
        }))
        .await;
    }

    async fn demo_job_runner(&self, job_id: String, mut cancel_rx: watch::Receiver<bool>) {
        self.set_state(&job_id, JobState::Queued).await;
        tokio::time::sleep(Duration::from_millis(150)).await;

        self.set_state(&job_id, JobState::Running).await;

        let total_steps = 10u32;
        for step in 1..=total_steps {
            // Cancellation check (cheap and deterministic).
            if *cancel_rx.borrow() {
                self.set_state(&job_id, JobState::Cancelled).await;
                self.publish(
                    &job_id,
                    aadk_proto::aadk::v1::job_event::Payload::Completed(JobCompleted {
                        summary: "Demo job cancelled".into(),
                        outputs: vec![],
                    }),
                )
                .await;
                return;
            }

            // Also react quickly if a change arrives.
            if cancel_rx.has_changed().unwrap_or(false) {
                let _ = cancel_rx.changed().await;
            }

            tokio::time::sleep(Duration::from_millis(250)).await;

            let pct = step * 10;
            self.publish(
                &job_id,
                aadk_proto::aadk::v1::job_event::Payload::Progress(JobProgressUpdated {
                    progress: Some(JobProgress {
                        percent: pct,
                        phase: format!("Demo phase {step}"),
                        metrics: vec![
                            KeyValue {
                                key: "step".into(),
                                value: step.to_string(),
                            },
                            KeyValue {
                                key: "total_steps".into(),
                                value: total_steps.to_string(),
                            },
                        ],
                    }),
                }),
            )
            .await;

            let line = format!("demo: step {step} complete ({pct}%)\n");
            self.publish(
                &job_id,
                aadk_proto::aadk::v1::job_event::Payload::Log(JobLogAppended {
                    chunk: Some(LogChunk {
                        stream: "stdout".into(),
                        data: line.into_bytes(),
                        truncated: false,
                    }),
                }),
            )
            .await;
        }

        self.set_state(&job_id, JobState::Success).await;
        self.publish(
            &job_id,
            aadk_proto::aadk::v1::job_event::Payload::Completed(JobCompleted {
                summary: "Demo job finished successfully".into(),
                outputs: vec![KeyValue {
                    key: "artifact".into(),
                    value: "/tmp/demo-artifact.txt".into(),
                }],
            }),
        )
        .await;
    }
}

async fn stream_run_events_task(
    store: JobStore,
    run_id: String,
    correlation_id: String,
    include_history: bool,
    config: RunStreamConfig,
    mut out_tx: mpsc::Sender<Result<JobEvent, Status>>,
) {
    let (event_tx, mut event_rx) = mpsc::channel::<JobEvent>(config.buffer_max_events * 2);
    let mut known_jobs = HashSet::new();
    let mut buffer: BinaryHeap<Reverse<BufferedEvent>> = BinaryHeap::new();
    let mut seq = 0u64;

    discover_run_jobs(
        &store,
        &run_id,
        &correlation_id,
        include_history,
        &mut known_jobs,
        &event_tx,
    )
    .await;

    let mut discovery_tick = tokio::time::interval(config.discovery_interval);
    let mut flush_tick = tokio::time::interval(config.flush_interval);

    loop {
        tokio::select! {
            _ = discovery_tick.tick() => {
                discover_run_jobs(
                    &store,
                    &run_id,
                    &correlation_id,
                    include_history,
                    &mut known_jobs,
                    &event_tx,
                )
                .await;
                if out_tx.is_closed() {
                    break;
                }
            }
            _ = flush_tick.tick() => {
                if !flush_run_buffer(&mut buffer, &mut out_tx, &config, false).await {
                    return;
                }
            }
            maybe_evt = event_rx.recv() => {
                let Some(evt) = maybe_evt else {
                    break;
                };
                let at = event_timestamp(&evt);
                buffer.push(Reverse(BufferedEvent {
                    at_unix_millis: at,
                    seq,
                    event: evt,
                }));
                seq = seq.wrapping_add(1);
                if !flush_run_buffer(&mut buffer, &mut out_tx, &config, false).await {
                    return;
                }
            }
        }
    }

    let _ = flush_run_buffer(&mut buffer, &mut out_tx, &config, true).await;
}

#[tonic::async_trait]
impl JobService for JobSvc {
    async fn start_job(
        &self,
        request: Request<StartJobRequest>,
    ) -> Result<Response<StartJobResponse>, Status> {
        let req = request.into_inner();
        let job_type = req.job_type.trim();
        if job_type.is_empty() {
            return Err(Status::invalid_argument("job_type is required"));
        }
        if !is_known_job_type(job_type) {
            return Err(Status::invalid_argument(format!(
                "unknown job_type: {job_type}"
            )));
        }
        let display_name = match job_type {
            "demo.job" => "Demo Job",
            "workflow.pipeline" => "Workflow Pipeline",
            _ => job_type,
        };

        let job_id = Uuid::new_v4().to_string();
        let (btx, _brx) = broadcast::channel::<JobEvent>(BROADCAST_CAPACITY);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let correlation_id_raw = req.correlation_id.trim();
        let correlation_id = if correlation_id_raw.is_empty() {
            job_id.clone()
        } else {
            correlation_id_raw.to_string()
        };
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let run_id = if run_id.is_empty() && !correlation_id_raw.is_empty() {
            correlation_id_raw.to_string()
        } else {
            run_id
        };
        let run_ref = if run_id.is_empty() {
            None
        } else {
            Some(RunId { value: run_id })
        };

        let job = Job {
            job_id: Some(Id { value: job_id.clone() }),
            job_type: job_type.to_string(),
            state: JobState::Queued as i32,
            created_at: Some(now_ts()),
            started_at: None,
            finished_at: None,
            display_name: display_name.into(),
            correlation_id,
            run_id: run_ref,
            project_id: req.project_id,
            target_id: req.target_id,
            toolchain_set_id: req.toolchain_set_id,
        };

        let rec = JobRecordInner {
            job,
            broadcaster: btx.clone(),
            history: VecDeque::with_capacity(HISTORY_CAPACITY),
            cancel_tx,
        };

        self.store.insert(&job_id, rec).await;
        self.schedule_persist();

        // Start known jobs.
        if job_type == "demo.job" {
            let svc = self.clone();
            let job_id_clone = job_id.clone();
            tokio::spawn(async move {
                svc.demo_job_runner(job_id_clone, cancel_rx).await;
            });
        }

        Ok(Response::new(StartJobResponse {
            job: Some(JobRef { job_id: Some(Id { value: job_id }) }),
        }))
    }

    async fn get_job(&self, request: Request<GetJobRequest>) -> Result<Response<GetJobResponse>, Status> {
        let req = request.into_inner();
        let job_id = req.job_id.map(|i| i.value).unwrap_or_default();

        let rec = self.store.get(&job_id).await.ok_or_else(|| {
            Status::not_found(format!("Job not found: {job_id}"))
        })?;

        let inner = rec.lock().await;
        Ok(Response::new(GetJobResponse { job: Some(inner.job.clone()) }))
    }

    async fn cancel_job(
        &self,
        request: Request<CancelJobRequest>,
    ) -> Result<Response<CancelJobResponse>, Status> {
        let req = request.into_inner();
        let job_id = req.job_id.map(|i| i.value).unwrap_or_default();

        let rec = match self.store.get(&job_id).await {
            Some(r) => r,
            None => return Ok(Response::new(CancelJobResponse { accepted: false })),
        };

        let inner = rec.lock().await;
        let state = JobState::try_from(inner.job.state).unwrap_or(JobState::Unspecified);
        if matches!(
            state,
            JobState::Success | JobState::Failed | JobState::Cancelled
        ) {
            return Ok(Response::new(CancelJobResponse { accepted: false }));
        }
        let _ = inner.cancel_tx.send(true);
        drop(inner);

        self.set_state(&job_id, JobState::Cancelled).await;
        Ok(Response::new(CancelJobResponse { accepted: true }))
    }

    type StreamJobEventsStream = ReceiverStream<Result<JobEvent, Status>>;
    type StreamRunEventsStream = ReceiverStream<Result<JobEvent, Status>>;

    async fn stream_job_events(
        &self,
        request: Request<StreamJobEventsRequest>,
    ) -> Result<Response<Self::StreamJobEventsStream>, Status> {
        let req = request.into_inner();
        let job_id = req.job_id.map(|i| i.value).unwrap_or_default();

        let rec = self.store.get(&job_id).await.ok_or_else(|| {
            let err = ErrorDetail {
                code: ErrorCode::JobNotFound as i32,
                message: format!("Job not found: {job_id}"),
                technical_details: "".into(),
                remedies: vec![],
                correlation_id: job_id.clone(),
            };
            Status::not_found(format!("{:?}", err))
        })?;

        // Snapshot history and subscribe to broadcast.
        let (history, mut rx) = {
            let inner = rec.lock().await;
            let hist = if req.include_history {
                inner.history.iter().cloned().collect::<Vec<_>>()
            } else {
                vec![]
            };
            (hist, inner.broadcaster.subscribe())
        };

        let (tx, out_rx) = mpsc::channel::<Result<JobEvent, Status>>(1024);

        // Producer task: replay history then forward live events.
        let job_id_clone = job_id.clone();
        tokio::spawn(async move {
            for evt in history {
                if tx.send(Ok(evt)).await.is_err() {
                    return;
                }
            }

            loop {
                match rx.recv().await {
                    Ok(evt) => {
                        if tx.send(Ok(evt)).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        // Best-effort notice to the client.
                        let notice = mk_event(
                            &job_id_clone,
                            aadk_proto::aadk::v1::job_event::Payload::Log(JobLogAppended {
                                chunk: Some(LogChunk {
                                    stream: "server".into(),
                                    data: format!("WARNING: client lagged; skipped {skipped} events\n").into_bytes(),
                                    truncated: false,
                                }),
                            }),
                        );
                        let _ = tx.send(Ok(notice)).await;
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        return;
                    }
                }
            }
        });

        Ok(Response::new(ReceiverStream::new(out_rx)))
    }

    async fn stream_run_events(
        &self,
        request: Request<StreamRunEventsRequest>,
    ) -> Result<Response<Self::StreamRunEventsStream>, Status> {
        let req = request.into_inner();
        let run_id = req
            .run_id
            .as_ref()
            .map(|id| id.value.trim().to_string())
            .filter(|value| !value.is_empty())
            .unwrap_or_default();
        let correlation_id = req.correlation_id.trim().to_string();

        if run_id.is_empty() && correlation_id.is_empty() {
            return Err(Status::invalid_argument(
                "run_id or correlation_id is required",
            ));
        }

        let config = run_stream_config(&req);
        let (tx, out_rx) = mpsc::channel::<Result<JobEvent, Status>>(1024);
        let store = self.store.clone();
        let include_history = req.include_history;

        tokio::spawn(async move {
            stream_run_events_task(
                store,
                run_id,
                correlation_id,
                include_history,
                config,
                tx,
            )
            .await;
        });

        Ok(Response::new(ReceiverStream::new(out_rx)))
    }

    async fn publish_job_event(
        &self,
        request: Request<PublishJobEventRequest>,
    ) -> Result<Response<PublishJobEventResponse>, Status> {
        let req = request.into_inner();
        let event = req.event.ok_or_else(|| Status::invalid_argument("event is required"))?;
        let job_id = event
            .job_id
            .ok_or_else(|| Status::invalid_argument("event.job_id is required"))?
            .value;

        if self.store.get(&job_id).await.is_none() {
            return Err(Status::not_found(format!("Job not found: {job_id}")));
        }

        let payload = event
            .payload
            .ok_or_else(|| Status::invalid_argument("event.payload is required"))?;

        match &payload {
            aadk_proto::aadk::v1::job_event::Payload::StateChanged(state) => {
                let new_state = JobState::try_from(state.new_state).unwrap_or(JobState::Unspecified);
                self.update_state_only(&job_id, new_state).await;
            }
            aadk_proto::aadk::v1::job_event::Payload::Completed(_) => {
                self.update_state_only(&job_id, JobState::Success).await;
            }
            aadk_proto::aadk::v1::job_event::Payload::Failed(_) => {
                self.update_state_only(&job_id, JobState::Failed).await;
            }
            _ => {}
        }

        self.publish(&job_id, payload).await;

        Ok(Response::new(PublishJobEventResponse { accepted: true }))
    }

    async fn list_jobs(
        &self,
        request: Request<ListJobsRequest>,
    ) -> Result<Response<ListJobsResponse>, Status> {
        let req = request.into_inner();
        let (start, page_size) = parse_page(req.page)?;
        let filter = req.filter.unwrap_or_default();

        let mut jobs = self.store.list_jobs().await;
        jobs.retain(|job| job_matches_filter(job, &filter));
        jobs.sort_by(|a, b| job_sort_key(b).cmp(&job_sort_key(a)));

        let total = jobs.len();
        if start >= total && total != 0 {
            return Err(Status::invalid_argument("page_token out of range"));
        }
        let end = (start + page_size).min(total);
        let items = jobs[start..end].to_vec();
        let next_token = if end < total { end.to_string() } else { String::new() };

        Ok(Response::new(ListJobsResponse {
            jobs: items,
            page_info: Some(PageInfo {
                next_page_token: next_token,
            }),
        }))
    }

    async fn list_job_history(
        &self,
        request: Request<ListJobHistoryRequest>,
    ) -> Result<Response<ListJobHistoryResponse>, Status> {
        let req = request.into_inner();
        let job_id = req.job_id.map(|id| id.value).unwrap_or_default();
        if job_id.trim().is_empty() {
            return Err(Status::invalid_argument("job_id is required"));
        }
        let rec = self.store.get(&job_id).await.ok_or_else(|| {
            Status::not_found(format!("Job not found: {job_id}"))
        })?;
        let (start, page_size) = parse_page(req.page)?;
        let filter = req.filter.unwrap_or_default();

        let mut events = {
            let inner = rec.lock().await;
            inner.history.iter().cloned().collect::<Vec<_>>()
        };
        events.retain(|event| event_matches_filter(event, &filter));

        let total = events.len();
        if start >= total && total != 0 {
            return Err(Status::invalid_argument("page_token out of range"));
        }
        let end = (start + page_size).min(total);
        let items = events[start..end].to_vec();
        let next_token = if end < total { end.to_string() } else { String::new() };

        Ok(Response::new(ListJobHistoryResponse {
            events: items,
            page_info: Some(PageInfo {
                next_page_token: next_token,
            }),
        }))
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str = std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let retention = retention_policy_from_env();
    let store = load_store(&retention).await;
    let persist_tx = spawn_persist_worker(store.clone(), retention);
    spawn_retention_tick(persist_tx.clone());
    let svc = JobSvc::new(store, persist_tx.clone());
    let _ = persist_tx.try_send(());

    info!("aadk-core (JobService) listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(JobServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
