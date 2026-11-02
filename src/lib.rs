mod native;
mod wire;
mod worker;
use abi_stable::{
    sabi_trait::TD_Opaque,
    std_types::{RErr, ROk, RResult, RString},
};
use remotemedia_plugin_sdk::{
    adapter::StreamingNodeFfiAdapter,
    traits::streaming::AsyncStreamingNode,
    types::{Error, RuntimeData},
    FfiNodeBox, FfiNodeFactory, FfiNode_TO,
};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use wire::Config;
use worker::{Command, Worker};

pub struct AudioCppNode {
    config: Config,
    manager: bool,
    worker: Arc<Worker>,
    prefix: String,
    next: AtomicU64,
}
impl AudioCppNode {
    pub fn create(config: serde_json::Value, manager: bool) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let config: Config = serde_json::from_value(config).map_err(|e| e.to_string())?;
        config.validate(manager)?;
        let worker = Worker::shared(&config)?;
        Ok(Self {
            config,
            manager,
            worker,
            prefix: format!("node{}:", NEXT.fetch_add(1, Ordering::Relaxed)),
            next: AtomicU64::new(1),
        })
    }
    async fn run<F>(
        &self,
        data: RuntimeData,
        session: Option<String>,
        mut emit: F,
    ) -> Result<usize, Error>
    where
        F: FnMut(RuntimeData) -> Result<(), Error> + Send,
    {
        let mut input = wire::input(data).map_err(Error::Execution)?;
        let session = match input.get("session_id") {
            Some(serde_json::Value::String(s)) => Some(s.clone()),
            Some(_) => return Err(Error::Execution("session_id must be a string".into())),
            None => session,
        };
        if !self.manager && input.get("request_id").is_none() {
            input["request_id"] = serde_json::json!(self.next.fetch_add(1, Ordering::Relaxed));
        }
        let (output, mut receiver) = tokio::sync::mpsc::channel(8);
        self.worker
            .send(Command {
                config: self.config.clone(),
                session: format!(
                    "{}{}",
                    self.prefix,
                    session.unwrap_or_else(|| "default".into())
                ),
                manager: self.manager,
                input,
                output,
            })
            .await
            .map_err(Error::Execution)?;
        let mut count = 0;
        while let Some(result) = receiver.recv().await {
            let value = result.map_err(Error::Execution)?;
            if self.manager {
                emit(RuntimeData::Json(value))?;
                count += 1;
            } else {
                wire::emit_result(value, &mut |out| {
                    emit(out).map_err(|e| e.to_string())?;
                    count += 1;
                    Ok(())
                })
                .map_err(Error::Execution)?;
            }
        }
        Ok(count)
    }
}
impl Drop for AudioCppNode {
    fn drop(&mut self) {
        self.worker.release(self.prefix.clone());
    }
}
#[async_trait::async_trait]
impl AsyncStreamingNode for AudioCppNode {
    fn node_type(&self) -> &str {
        if self.manager {
            "AudioCppModelManagerNode"
        } else {
            "AudioCppNode"
        }
    }
    async fn process(&self, data: RuntimeData) -> Result<RuntimeData, Error> {
        let mut outputs = Vec::new();
        self.run(data, None, |v| {
            outputs.push(v);
            Ok(())
        })
        .await?;
        // Single-output callers receive all emissions losslessly in one envelope.
        if outputs.len() == 1 {
            Ok(outputs.pop().unwrap())
        } else {
            Ok(RuntimeData::Json(serde_json::json!({"outputs":outputs})))
        }
    }
    async fn process_streaming<F>(
        &self,
        data: RuntimeData,
        session: Option<String>,
        callback: F,
    ) -> Result<usize, Error>
    where
        F: FnMut(RuntimeData) -> Result<(), Error> + Send,
    {
        self.run(data, session, callback).await
    }
}
#[derive(Default)]
pub struct AudioCppFactory;
#[derive(Default)]
pub struct AudioCppModelManagerFactory;
fn factory(params: RString, manager: bool) -> RResult<FfiNodeBox, RString> {
    let result = serde_json::from_str(params.as_str())
        .map_err(|e| e.to_string())
        .and_then(|v| AudioCppNode::create(v, manager));
    match result {
        Ok(node) => ROk(FfiNode_TO::from_value(
            StreamingNodeFfiAdapter::new(node),
            TD_Opaque,
        )),
        Err(e) => RErr(e.into()),
    }
}
impl FfiNodeFactory for AudioCppFactory {
    fn node_type(&self) -> RString {
        "AudioCppNode".into()
    }
    fn create(&self, p: RString) -> RResult<FfiNodeBox, RString> {
        factory(p, false)
    }
}
impl FfiNodeFactory for AudioCppModelManagerFactory {
    fn node_type(&self) -> RString {
        "AudioCppModelManagerNode".into()
    }
    fn create(&self, p: RString) -> RResult<FfiNodeBox, RString> {
        factory(p, true)
    }
}
remotemedia_plugin_sdk::plugin_export!(AudioCppFactory, AudioCppModelManagerFactory);
