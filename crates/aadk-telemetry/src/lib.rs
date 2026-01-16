use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const EVENT_QUEUE_CAPACITY: usize = 256;
const MAX_EVENT_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct TelemetryOptions {
    pub app_name: &'static str,
    pub app_version: &'static str,
    pub usage_enabled: bool,
    pub crash_enabled: bool,
    pub install_id: Option<String>,
}

pub struct Telemetry {
    app_name: String,
    app_version: String,
    session_id: String,
    install_id: Mutex<Option<String>>,
    usage_enabled: AtomicBool,
    crash_enabled: AtomicBool,
    sender: SyncSender<TelemetryEvent>,
}

#[derive(Serialize)]
struct TelemetryEvent {
    event_type: String,
    at_unix_millis: i64,
    app: String,
    version: String,
    session_id: String,
    install_id: Option<String>,
    properties: BTreeMap<String, String>,
}

#[derive(Serialize)]
struct CrashReport {
    at_unix_millis: i64,
    app: String,
    version: String,
    session_id: String,
    install_id: Option<String>,
    message: String,
    location: Option<String>,
    backtrace: Option<String>,
}

static TELEMETRY: OnceLock<Arc<Telemetry>> = OnceLock::new();

pub fn init(options: TelemetryOptions) -> Arc<Telemetry> {
    if let Some(existing) = TELEMETRY.get() {
        existing.update_from_options(&options);
        return Arc::clone(existing);
    }

    let (sender, receiver) = sync_channel(EVENT_QUEUE_CAPACITY);
    let telemetry = Arc::new(Telemetry {
        app_name: options.app_name.to_string(),
        app_version: options.app_version.to_string(),
        session_id: new_session_id(),
        install_id: Mutex::new(options.install_id),
        usage_enabled: AtomicBool::new(options.usage_enabled),
        crash_enabled: AtomicBool::new(options.crash_enabled),
        sender,
    });

    start_writer_thread(Arc::clone(&telemetry), receiver);
    install_panic_hook(Arc::clone(&telemetry));

    let _ = TELEMETRY.set(Arc::clone(&telemetry));
    telemetry
}

pub fn init_with_env(app_name: &'static str, app_version: &'static str) -> Arc<Telemetry> {
    let usage_enabled = env_flag("AADK_TELEMETRY");
    let crash_enabled = env_flag("AADK_TELEMETRY_CRASH");
    let install_id = std::env::var("AADK_TELEMETRY_INSTALL_ID").ok();
    init(TelemetryOptions {
        app_name,
        app_version,
        usage_enabled,
        crash_enabled,
        install_id,
    })
}

pub fn global() -> Option<Arc<Telemetry>> {
    TELEMETRY.get().map(Arc::clone)
}

pub fn set_usage_enabled(enabled: bool) {
    if let Some(telemetry) = TELEMETRY.get() {
        telemetry.usage_enabled.store(enabled, Ordering::Relaxed);
    }
}

pub fn set_crash_enabled(enabled: bool) {
    if let Some(telemetry) = TELEMETRY.get() {
        telemetry.crash_enabled.store(enabled, Ordering::Relaxed);
    }
}

pub fn set_install_id(install_id: Option<String>) {
    if let Some(telemetry) = TELEMETRY.get() {
        let mut guard = telemetry.install_id.lock().unwrap();
        *guard = install_id;
    }
}

pub fn event(event_type: &str, properties: &[(&str, &str)]) {
    if let Some(telemetry) = TELEMETRY.get() {
        telemetry.event(event_type, properties);
    }
}

pub fn generate_install_id() -> String {
    let now = now_millis();
    let pid = std::process::id();
    format!("{now:x}-{pid:x}")
}

impl Telemetry {
    fn update_from_options(&self, options: &TelemetryOptions) {
        self.usage_enabled
            .store(options.usage_enabled, Ordering::Relaxed);
        self.crash_enabled
            .store(options.crash_enabled, Ordering::Relaxed);
        if options.install_id.is_some() {
            let mut guard = self.install_id.lock().unwrap();
            *guard = options.install_id.clone();
        }
    }

    fn event(&self, event_type: &str, properties: &[(&str, &str)]) {
        if !self.usage_enabled.load(Ordering::Relaxed) {
            return;
        }
        let mut map = BTreeMap::new();
        for (key, value) in properties {
            if !key.trim().is_empty() {
                map.insert((*key).to_string(), (*value).to_string());
            }
        }
        let event = TelemetryEvent {
            event_type: event_type.to_string(),
            at_unix_millis: now_millis(),
            app: self.app_name.clone(),
            version: self.app_version.clone(),
            session_id: self.session_id.clone(),
            install_id: self.install_id.lock().unwrap().clone(),
            properties: map,
        };
        let _ = self.sender.try_send(event);
    }

    fn crash_report(&self, message: String, location: Option<String>, backtrace: Option<String>) {
        if !self.crash_enabled.load(Ordering::Relaxed) {
            return;
        }
        let report = CrashReport {
            at_unix_millis: now_millis(),
            app: self.app_name.clone(),
            version: self.app_version.clone(),
            session_id: self.session_id.clone(),
            install_id: self.install_id.lock().unwrap().clone(),
            message,
            location,
            backtrace,
        };
        write_crash_report(&self.app_name, &report);
        let event = TelemetryEvent {
            event_type: "crash".to_string(),
            at_unix_millis: report.at_unix_millis,
            app: report.app,
            version: report.version,
            session_id: report.session_id,
            install_id: report.install_id,
            properties: BTreeMap::new(),
        };
        let _ = self.sender.try_send(event);
    }
}

fn start_writer_thread(telemetry: Arc<Telemetry>, receiver: Receiver<TelemetryEvent>) {
    std::thread::spawn(move || {
        while let Ok(event) = receiver.recv() {
            write_event(&telemetry.app_name, &event);
        }
    });
}

fn install_panic_hook(telemetry: Arc<Telemetry>) {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = if let Some(msg) = info.payload().downcast_ref::<&str>() {
            (*msg).to_string()
        } else if let Some(msg) = info.payload().downcast_ref::<String>() {
            msg.clone()
        } else {
            "panic".to_string()
        };
        let location = info
            .location()
            .map(|loc| format!("{}:{}", loc.file(), loc.line()));
        let backtrace = Some(format!("{:?}", std::backtrace::Backtrace::capture()));
        telemetry.crash_report(message, location, backtrace);
        default_hook(info);
    }));
}

fn write_event(app_name: &str, event: &TelemetryEvent) {
    let dir = data_dir().join("telemetry").join(app_name);
    if let Err(err) = fs::create_dir_all(&dir) {
        eprintln!("telemetry: failed to create {}: {err}", dir.display());
        return;
    }

    let path = dir.join("events.jsonl");
    if rotate_if_needed(&path).is_err() {
        return;
    }

    let mut file = match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => file,
        Err(err) => {
            eprintln!("telemetry: failed to open {}: {err}", path.display());
            return;
        }
    };
    if let Ok(line) = serde_json::to_string(event) {
        let _ = writeln!(file, "{line}");
    }
}

fn rotate_if_needed(path: &PathBuf) -> std::io::Result<()> {
    if let Ok(meta) = fs::metadata(path) {
        if meta.len() >= MAX_EVENT_BYTES {
            let rotated = path.with_extension("jsonl.1");
            let _ = fs::remove_file(&rotated);
            fs::rename(path, rotated)?;
        }
    }
    Ok(())
}

fn write_crash_report(app_name: &str, report: &CrashReport) {
    let dir = data_dir().join("telemetry").join(app_name).join("crashes");
    if let Err(err) = fs::create_dir_all(&dir) {
        eprintln!("telemetry: failed to create {}: {err}", dir.display());
        return;
    }
    let filename = format!(
        "crash-{}-{}.json",
        report.at_unix_millis,
        std::process::id()
    );
    let path = dir.join(filename);
    if let Ok(file) = OpenOptions::new().create(true).write(true).open(&path) {
        let _ = serde_json::to_writer_pretty(file, report);
    }
}

fn data_dir() -> PathBuf {
    if let Ok(home) = std::env::var("HOME") {
        PathBuf::from(home).join(".local/share/aadk")
    } else {
        PathBuf::from("/tmp/aadk")
    }
}

fn env_flag(name: &str) -> bool {
    match std::env::var(name) {
        Ok(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn new_session_id() -> String {
    let now = now_millis();
    let pid = std::process::id();
    format!("{now:x}-{pid:x}")
}
