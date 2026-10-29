use std::sync::Arc;
use std::sync::Mutex;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::Value;
use telemetry_core::TelemetryCaptureMessage;
use telemetry_core::TelemetryCpuInfo;
use telemetry_core::TelemetryDiagnosticEvent;
use telemetry_core::TelemetryDiagnosticInput;
use telemetry_core::TelemetryDiagnostics;
use telemetry_core::TelemetryEnv;
use telemetry_core::TelemetryError;
use telemetry_core::TelemetryOsProvider;
use telemetry_core::TelemetryTransport;
use telemetry_core::TelemetryTransportFactory;
use telemetry_core::TelemetryTransportOptions;

pub fn env(pairs: &[(&str, &str)]) -> TelemetryEnv {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

pub fn props(value: Value) -> serde_json::Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("expected object, got {other}"),
    }
}

#[derive(Clone, Copy, Default)]
pub struct RecorderBehavior {
    pub capture_fails: bool,
    pub flush_hangs: bool,
}

#[derive(Clone, Default)]
pub struct Recorder {
    pub messages: Arc<Mutex<Vec<TelemetryCaptureMessage>>>,
    pub options: Arc<Mutex<Vec<TelemetryTransportOptions>>>,
    pub flush_calls: Arc<Mutex<usize>>,
    pub created: Arc<Mutex<usize>>,
    behavior: RecorderBehavior,
}

struct RecordingTransport {
    recorder: Recorder,
}

impl TelemetryTransport for RecordingTransport {
    fn capture(&self, message: &TelemetryCaptureMessage) -> Result<(), TelemetryError> {
        if self.recorder.behavior.capture_fails {
            return Err(TelemetryError::new("capture failed"));
        }
        self.recorder.messages.lock().unwrap().push(message.clone());
        Ok(())
    }

    fn flush(&self) -> Option<BoxFuture<'_, Result<(), TelemetryError>>> {
        if self.recorder.behavior.flush_hangs {
            return Some(futures::future::pending().boxed());
        }
        *self.recorder.flush_calls.lock().unwrap() += 1;
        Some(futures::future::ready(Ok(())).boxed())
    }

    fn shutdown(&self) -> BoxFuture<'_, Result<(), TelemetryError>> {
        futures::future::ready(Ok(())).boxed()
    }
}

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(behavior: RecorderBehavior) -> Self {
        Self {
            behavior,
            ..Self::default()
        }
    }

    pub fn factory(&self) -> TelemetryTransportFactory {
        let recorder = self.clone();
        Arc::new(move |_api_key, options| {
            recorder.options.lock().unwrap().push(options.clone());
            *recorder.created.lock().unwrap() += 1;
            Ok(Box::new(RecordingTransport {
                recorder: recorder.clone(),
            }))
        })
    }

    pub fn messages(&self) -> Vec<TelemetryCaptureMessage> {
        self.messages.lock().unwrap().clone()
    }

    pub fn options(&self) -> Vec<TelemetryTransportOptions> {
        self.options.lock().unwrap().clone()
    }

    pub fn created(&self) -> usize {
        *self.created.lock().unwrap()
    }
}

#[derive(Clone, Default)]
pub struct DiagnosticsSink {
    pub inputs: Arc<Mutex<Vec<TelemetryDiagnosticInput>>>,
}

impl DiagnosticsSink {
    pub fn sink(&self) -> TelemetryDiagnostics {
        let inputs = Arc::clone(&self.inputs);
        Arc::new(move |input| inputs.lock().unwrap().push(input.clone()))
    }

    pub fn events(&self) -> Vec<TelemetryDiagnosticEvent> {
        self.inputs
            .lock()
            .unwrap()
            .iter()
            .map(|input| input.event)
            .collect()
    }
}

pub struct FakeOs;

impl TelemetryOsProvider for FakeOs {
    fn arch(&self) -> String {
        "arm64".to_string()
    }

    fn cpus(&self) -> Result<Vec<TelemetryCpuInfo>, TelemetryError> {
        Ok(vec![TelemetryCpuInfo {
            model: "Apple M-test".to_string(),
        }])
    }

    fn hostname(&self) -> String {
        "test-host".to_string()
    }

    fn platform(&self) -> String {
        "darwin".to_string()
    }

    fn release(&self) -> String {
        "26.0.0".to_string()
    }

    fn totalmem(&self) -> u64 {
        17_179_869_184
    }

    fn os_type(&self) -> String {
        "Darwin".to_string()
    }
}
