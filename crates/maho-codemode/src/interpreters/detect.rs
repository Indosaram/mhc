use crate::tool::types::EvalLanguage;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InterpreterDetection {
    Detected { path: String, version: String, resolved_path: Option<std::path::PathBuf> },
    Unavailable,
}

pub struct InterpreterDetector {
    detected: Option<bool>,
    probe: InterpreterProbe,
}

pub type InterpreterProbe = std::sync::Arc<dyn Fn(String,Vec<String>,u64)->std::pin::Pin<Box<dyn std::future::Future<Output=Result<(String,String),String>>+Send>>+Send+Sync>;

impl InterpreterDetector {
    pub fn new(_node_version: String, _windows: bool) -> Self {
        Self::with_probe(std::sync::Arc::new(|command,args,timeout_ms|Box::pin(async move {
            let mut process=tokio::process::Command::new(command);
            process.args(args).kill_on_drop(true);
            let output=tokio::time::timeout(Duration::from_millis(timeout_ms),process.output()).await.map_err(|error|error.to_string())?.map_err(|error|error.to_string())?;
            if !output.status.success() {return Err("bun probe failed".into());}
            Ok((String::from_utf8_lossy(&output.stdout).into_owned(),String::from_utf8_lossy(&output.stderr).into_owned()))
        })))
    }

    pub fn with_probe(probe: InterpreterProbe) -> Self {
        Self {detected:None,probe}
    }

    pub async fn detect(&mut self, language: EvalLanguage) -> InterpreterDetection {
        if language!=EvalLanguage::Js {return InterpreterDetection::Unavailable;}
        let available=match self.detected {
            Some(available)=>available,
            None=>{
                let available=(self.probe)("bun".into(),vec!["--version".into()],3000).await.is_ok();
                self.detected=Some(available);
                available
            }
        };
        if available {InterpreterDetection::Detected {path:"bun".into(),version:String::new(),resolved_path:None}} else {InterpreterDetection::Unavailable}
    }
}
