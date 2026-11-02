use crate::{native::Engine, wire::Config};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
    thread::JoinHandle,
};
use tokio::sync::mpsc;

pub struct Command {
    pub config: Config,
    pub session: String,
    pub manager: bool,
    pub input: Value,
    pub output: mpsc::Sender<Result<Value, String>>,
}
pub struct Worker {
    sender: Option<mpsc::Sender<Command>>,
    join: Option<JoinHandle<()>>,
    cleanup: Arc<Mutex<Vec<String>>>,
}
impl Worker {
    pub fn shared(config: &Config) -> Result<Arc<Self>, String> {
        static POOL: OnceLock<Mutex<HashMap<String, Weak<Worker>>>> = OnceLock::new();
        std::fs::create_dir_all(&config.cache_dir).map_err(|e| e.to_string())?;
        let key = format!(
            "{}|{}",
            config
                .library_dir
                .canonicalize()
                .map_err(|e| e.to_string())?
                .display(),
            config
                .cache_dir
                .canonicalize()
                .map_err(|e| e.to_string())?
                .display()
        );
        let mut pool = POOL
            .get_or_init(Default::default)
            .lock()
            .map_err(|e| e.to_string())?;
        if let Some(worker) = pool.get(&key).and_then(Weak::upgrade) {
            return Ok(worker);
        }
        let (sender, mut receiver) = mpsc::channel::<Command>(16);
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let config = config.clone();
        let cleanup = Arc::new(Mutex::new(Vec::<String>::new()));
        let clean = cleanup.clone();
        let join=std::thread::Builder::new().name("audiocpp-worker".into()).spawn(move || {
            let mut engine=match Engine::open(&config){Ok(e)=>{let _=ready_tx.send(Ok(()));e},Err(e)=>{let _=ready_tx.send(Err(e));return;}};
            while let Some(command)=receiver.blocking_recv(){
                for prefix in clean.lock().unwrap().drain(..){engine.close_node(&prefix);}
                if command.output.is_closed(){continue;}
                let result=if command.manager {
                    let method=command.input["method"].as_str().unwrap_or("");
                    let params=command.input.get("params").unwrap_or(&Value::Null);
                    let id=command.input["id"].clone();
                    let result=engine.manage(method,params,&command.config);
                    let response=match result {Ok(result)=>serde_json::json!({"jsonrpc":"2.0","id":id,"result":result}),Err(e)=>serde_json::json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":e}})};
                    command.output.blocking_send(Ok(response)).map_err(|_|"receiver closed".into())
                } else {
                    engine.infer(&command.config,&command.session,&command.input,&mut |v|command.output.blocking_send(Ok(v)).map_err(|_|"receiver closed".into()))
                };
                if let Err(e)=result {let _=command.output.blocking_send(Err(e));}
            }
        }).map_err(|e|e.to_string())?;
        ready_rx.recv().map_err(|e| e.to_string())??;
        let worker = Arc::new(Self {
            sender: Some(sender),
            join: Some(join),
            cleanup,
        });
        pool.insert(key, Arc::downgrade(&worker));
        Ok(worker)
    }
    pub async fn send(&self, command: Command) -> Result<(), String> {
        self.sender
            .as_ref()
            .unwrap()
            .send(command)
            .await
            .map_err(|_| "audio.cpp worker stopped".into())
    }
    pub fn release(&self, prefix: String) {
        if let Ok(mut list) = self.cleanup.lock() {
            list.push(prefix);
        }
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}
