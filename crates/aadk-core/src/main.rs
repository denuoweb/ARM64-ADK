use std::{collections::{HashMap, VecDeque}, net::SocketAddr, sync::Arc, time::Duration};

use aadk_proto::aadk::v1::{
    job_service_server::{JobService, JobServiceServer},
    CancelJobRequest, CancelJobResponse, GetJobRequest, GetJobResponse, Id, Job, JobCompleted,
    JobEvent, JobProgress, JobProgressUpdated, JobRef, JobState, JobStateChanged,
    JobLogAppended, KeyValue, LogChunk, PublishJobEventRequest, PublishJobEventResponse,
    StartJobRequest, StartJobResponse, StreamJobEventsRequest, Timestamp, ErrorDetail, ErrorCode,
};
use tokio::sync::{broadcast, mpsc, Mutex, watch};
use tokio_stream::wrappers::ReceiverStream;
use tonic::{Request, Response, Status};
use tracing::info;
use uuid::Uuid;

const BROADCAST_CAPACITY: usize = 1024;
const HISTORY_CAPACITY: usize = 2048;

fn is_known_job_type(job_type: &str) -> bool {
    matches!(
        job_type,
        "demo.job"
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

fn now_ts() -> Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64;
    Timestamp { unix_millis: ms }
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
}

#[derive(Clone)]
struct JobSvc {
    store: JobStore,
}

impl JobSvc {
    fn new(store: JobStore) -> Self {
        Self { store }
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
        }
    }

    async fn publish(&self, job_id: &str, payload: aadk_proto::aadk::v1::job_event::Payload) {
        if let Some(rec) = self.store.get(job_id).await {
            let mut inner = rec.lock().await;
            let evt = mk_event(job_id, payload);

            // Maintain bounded history.
            if inner.history.len() >= HISTORY_CAPACITY {
                inner.history.pop_front();
            }
            inner.history.push_back(evt.clone());

            // Broadcast (ignore send errors if no listeners).
            let _ = inner.broadcaster.send(evt);
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

        for step in 1..=10u32 {
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
                        metrics: vec![KeyValue {
                            key: "step".into(),
                            value: step.to_string(),
                        }],
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
        let display_name = if job_type == "demo.job" { "Demo Job" } else { job_type };

        let job_id = Uuid::new_v4().to_string();
        let (btx, _brx) = broadcast::channel::<JobEvent>(BROADCAST_CAPACITY);
        let (cancel_tx, cancel_rx) = watch::channel(false);

        let job = Job {
            job_id: Some(Id { value: job_id.clone() }),
            job_type: job_type.to_string(),
            state: JobState::Queued as i32,
            created_at: Some(now_ts()),
            started_at: None,
            finished_at: None,
            display_name: display_name.into(),
            correlation_id: job_id.clone(),
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
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let addr_str = std::env::var("AADK_JOB_ADDR").unwrap_or_else(|_| "127.0.0.1:50051".to_string());
    let addr: SocketAddr = addr_str.parse()?;

    let store = JobStore::default();
    let svc = JobSvc::new(store);

    info!("aadk-core (JobService) listening on {}", addr);

    tonic::transport::Server::builder()
        .add_service(JobServiceServer::new(svc))
        .serve(addr)
        .await?;

    Ok(())
}
