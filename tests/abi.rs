use abi_stable::{library::RootModule, std_types::RVec};
use remotemedia_plugin_sdk::{types::RuntimeData, FfiNodeBox, NodePluginRef};
use serde_json::{json, Value};
struct Capture(std::sync::Arc<std::sync::Mutex<Vec<RuntimeData>>>);
impl remotemedia_plugin_sdk::OutputSink for Capture {
    fn push(
        &self,
        bytes: abi_stable::std_types::RVec<u8>,
    ) -> abi_stable::std_types::RResult<(), abi_stable::std_types::RString> {
        match remotemedia_plugin_sdk::rmp_serde::from_slice(&bytes) {
            Ok(value) => {
                self.0.lock().unwrap().push(value);
                abi_stable::std_types::ROk(())
            }
            Err(e) => abi_stable::std_types::RErr(e.to_string().into()),
        }
    }
}
async fn stream(n: &FfiNodeBox, value: Value) -> Vec<RuntimeData> {
    let captured = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = remotemedia_plugin_sdk::OutputSink_TO::from_value(
        Capture(captured.clone()),
        abi_stable::sabi_trait::TD_Opaque,
    );
    let input = remotemedia_plugin_sdk::rmp_serde::to_vec_named(&RuntimeData::Json(value)).unwrap();
    let count = n
        .process_streaming(input.into(), sink)
        .await
        .into_result()
        .unwrap();
    let outputs = captured.lock().unwrap().clone();
    assert_eq!(count, outputs.len());
    outputs
}
fn setup() -> (NodePluginRef, tempfile::TempDir, Value) {
    let dir = std::env::var("AUDIOCPP_NATIVE_DIR").expect("set AUDIOCPP_NATIVE_DIR");
    let dll = std::env::var("AUDIOCPP_PLUGIN").unwrap_or_else(|_| {
        format!(
            "{}/target/debug/{}",
            env!("CARGO_MANIFEST_DIR"),
            if cfg!(windows) {
                "audiocpp_loadable.dll"
            } else if cfg!(target_os = "macos") {
                "libaudiocpp_loadable.dylib"
            } else {
                "libaudiocpp_loadable.so"
            }
        )
    });
    let plugin = NodePluginRef::load_from_file(std::path::Path::new(&dll)).unwrap();
    let cache = tempfile::tempdir().unwrap();
    let cfg = json!({"library_dir":dir,"cache_dir":cache.path()});
    (plugin, cache, cfg)
}
fn node(plugin: NodePluginRef, cfg: Value, kind: &str) -> FfiNodeBox {
    (plugin.list_factories())()
        .iter()
        .find(|f| f.node_type().as_str() == kind)
        .unwrap()
        .create(cfg.to_string().into())
        .into_result()
        .unwrap()
}
async fn call(n: &FfiNodeBox, v: Value) -> Value {
    let bytes = remotemedia_plugin_sdk::rmp_serde::to_vec_named(&RuntimeData::Json(v)).unwrap();
    let bytes = n.process(RVec::from(bytes)).await.into_result().unwrap();
    match remotemedia_plugin_sdk::rmp_serde::from_slice::<RuntimeData>(&bytes).unwrap() {
        RuntimeData::Json(v) => v,
        v => panic!("unexpected {v:?}"),
    }
}
async fn rpc(n: &FfiNodeBox, method: &str, params: Value) -> Value {
    call(
        n,
        json!({"jsonrpc":"2.0","id":42,"method":method,"params":params}),
    )
    .await
}

#[tokio::test]
#[ignore = "requires built native libraries and plugin DLL"]
async fn native_abi_catalog_import_persistence_and_errors() {
    let (plugin, _cache, cfg) = setup();
    assert_eq!((plugin.list_factories())().len(), 2);
    let n = node(plugin, cfg.clone(), "AudioCppModelManagerNode");
    assert!(
        rpc(&n, "schema", json!({})).await["result"]["config"]["properties"]
            .get("model")
            .is_some()
    );
    let v = rpc(&n, "capabilities", json!({})).await;
    assert_eq!(v["id"], 42);
    assert!(v["result"]["tasks"]
        .as_array()
        .unwrap()
        .contains(&json!("tts")));
    assert!(
        rpc(&n, "catalog", json!({})).await["result"]["inventory"]
            .as_array()
            .unwrap()
            .len()
            > 20
    );
    assert_eq!(
        rpc(&n, "installed", json!({})).await["result"]["inventory"],
        json!([])
    );
    let local = tempfile::NamedTempFile::new().unwrap();
    assert!(rpc(
        &n,
        "import",
        json!({"model":"local:test","path":local.path()})
    )
    .await
    .get("error")
    .is_none());
    assert!(rpc(
        &n,
        "import",
        json!({"model":"local:test","path":local.path()})
    )
    .await
    .get("error")
    .is_some());
    assert!(rpc(&n, "bogus", json!({})).await.get("error").is_some());
    assert!(rpc(&n, "cancel", json!({"job_id":"missing"}))
        .await
        .get("error")
        .is_some());
    assert!(rpc(&n, "load", json!({"model":"local:test"}))
        .await
        .get("error")
        .is_some());
    drop(n);
    let n = node(plugin, cfg, "AudioCppModelManagerNode");
    assert!(rpc(&n, "installed", json!({})).await["result"]["imports"]
        .get("local:test")
        .is_some());
    assert!(rpc(&n, "remove", json!({"model":"local:test"}))
        .await
        .get("error")
        .is_none());
    assert!(local.path().exists());
}

#[tokio::test]
#[ignore = "downloads PulseVAD weights; requires native libraries and network"]
async fn native_download_load_infer_unload_remove() {
    let (plugin, _cache, cfg) = setup();
    let n = node(plugin, cfg.clone(), "AudioCppModelManagerNode");
    let id = "pulsevad_2_1k_f32";
    let job = rpc(&n, "download", json!({"model":id})).await;
    assert_eq!(job["result"]["job_id"], id);
    assert_eq!(
        rpc(&n, "download", json!({"model":id})).await["result"]["job_id"],
        id
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(180);
    loop {
        let status = rpc(&n, "status", json!({"job_id":id})).await;
        if status["result"]["state"] == "completed" {
            break;
        }
        assert_eq!(status["result"]["state"], "running", "{status}");
        assert!(std::time::Instant::now() < deadline, "download timeout");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let loaded = rpc(&n, "load", json!({"model":id,"family":"pulsevad"})).await;
    assert!(loaded.get("error").is_none(), "{loaded}");
    assert!(rpc(&n, "remove", json!({"model":id}))
        .await
        .get("error")
        .is_some());
    let mut inference = cfg.clone();
    inference["model"] = json!(id);
    inference["task"] = json!("vad");
    inference["family"] = json!("pulsevad");
    let vad = node(plugin, inference, "AudioCppNode");
    let output=call(&vad,json!({"request_id":"silence","audio":{"samples":vec![0.;16000],"sample_rate":16000,"channels":1}})).await;
    assert_eq!(output["request_id"], "silence");
    assert_eq!(output["final"], true);
    drop(vad);
    assert!(rpc(&n, "unload", json!({"model":id,"family":"pulsevad"}))
        .await
        .get("error")
        .is_none());
    assert!(rpc(&n, "remove", json!({"model":id}))
        .await
        .get("error")
        .is_none());
    assert_eq!(
        rpc(&n, "installed", json!({})).await["result"]["inventory"],
        json!([])
    );
}

#[tokio::test]
#[ignore = "requires native libraries; cancels a native download job"]
async fn native_download_cancellation() {
    let (plugin, _cache, cfg) = setup();
    let n = node(plugin, cfg, "AudioCppModelManagerNode");
    let id = "pocket_tts_english_q8_0";
    assert!(rpc(&n, "download", json!({"model":id}))
        .await
        .get("error")
        .is_none());
    assert!(rpc(&n, "cancel", json!({"job_id":id}))
        .await
        .get("error")
        .is_none());
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        let status = rpc(&n, "status", json!({"job_id":id})).await;
        if status["result"]["state"] != "running" {
            assert_eq!(status["result"]["state"], "cancelled", "{status}");
            break;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(
        rpc(&n, "installed", json!({})).await["result"]["inventory"],
        json!([])
    );
}

async fn install(n: &FfiNodeBox, id: &str) {
    let job = rpc(n, "download", json!({"model":id})).await;
    assert!(job.get("error").is_none(), "{job}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(600);
    loop {
        let status = rpc(n, "status", json!({"job_id":id})).await;
        if status["result"]["state"] == "completed" {
            break;
        }
        assert_eq!(status["result"]["state"], "running", "{status}");
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

#[tokio::test]
#[ignore = "downloads PocketTTS and Moonshine; CPU/CUDA speech smoke"]
async fn native_speech_streaming_and_transcription() {
    let (plugin, _cache, mut cfg) = setup();
    cfg["threads"] = json!(4);
    cfg["backend"] = json!(std::env::var("AUDIOCPP_TEST_BACKEND").unwrap_or_else(|_| "cpu".into()));
    let n = node(plugin, cfg.clone(), "AudioCppModelManagerNode");
    install(&n, "pocket_tts_english_q8_0").await;
    install(&n, "moonshine_streaming_tiny_q8_0").await;
    let mut tts_cfg = cfg.clone();
    tts_cfg["model"] = json!("pocket_tts_english_q8_0");
    tts_cfg["family"] = json!("pocket_tts");
    tts_cfg["task"] = json!("tts");
    tts_cfg["mode"] = json!("streaming");
    let tts = node(plugin, tts_cfg, "AudioCppNode");
    let outputs=stream(&tts,json!({"request_id":"speech","voice_id":"alba","text":"Hello from Remote Media. This is a test of local speech.","options":{"seed":42,"max_frames":180}})).await;
    assert!(outputs
        .iter()
        .any(|v| matches!(v,RuntimeData::Json(v) if v.get("stream_policy").is_some())));
    let (mut samples, mut rate) = (Vec::new(), 0);
    for output in &outputs {
        if let RuntimeData::Audio {
            samples: s,
            sample_rate,
            channels,
            ..
        } = output
        {
            assert_eq!(*channels, 1);
            samples.extend_from_slice(s);
            rate = *sample_rate;
        }
    }
    assert!(samples.len() > rate as usize / 4);
    assert!(samples.iter().all(|s| s.is_finite()));
    assert!(samples.iter().any(|s| s.abs() > 0.001));
    let final_result = outputs
        .iter()
        .rev()
        .find_map(|o| {
            if let RuntimeData::Json(v) = o {
                Some(v)
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(final_result["final"], true);
    assert_eq!(final_result["audio_already_streamed"], true);
    assert_eq!(
        final_result["audio"][0]["samples"]
            .as_array()
            .unwrap()
            .len(),
        samples.len(),
        "no duplicate playback of final waveform"
    );
    let mut asr_cfg = cfg.clone();
    asr_cfg["model"] = json!("moonshine_streaming_tiny_q8_0");
    asr_cfg["family"] = json!("moonshine_asr");
    asr_cfg["task"] = json!("asr");
    let asr = node(plugin, asr_cfg.clone(), "AudioCppNode");
    let out = call(
        &asr,
        json!({"request_id":"asr","audio":{"samples":samples,"sample_rate":rate,"channels":1}}),
    )
    .await;
    let outputs: Vec<RuntimeData> =
        serde_json::from_value(out["outputs"].clone()).expect("text and metadata");
    let transcript = outputs
        .iter()
        .find_map(|o| {
            if let RuntimeData::Text(t) = o {
                Some(t)
            } else {
                None
            }
        })
        .expect("transcript");
    assert!(!transcript.trim().is_empty());
    eprintln!("Smoke transcript: {transcript}");
    asr_cfg["mode"] = json!("streaming");
    let streaming = node(plugin, asr_cfg.clone(), "AudioCppNode");
    call(
        &streaming,
        json!({"operation":"start","request_id":"stream-asr"}),
    )
    .await;
    call(
        &streaming,
        json!({"operation":"start","session_id":"other","request_id":"other"}),
    )
    .await;
    let busy = rpc(
        &n,
        "unload",
        json!({"model":"moonshine_streaming_tiny_q8_0","family":"moonshine_asr"}),
    )
    .await;
    assert!(
        busy.get("error").is_some(),
        "active sessions must hold their model"
    );
    for chunk in samples.chunks(rate as usize) {
        call(
            &streaming,
            json!({"operation":"push","audio":{"samples":chunk,"sample_rate":rate,"channels":1}}),
        )
        .await;
    }
    let finished = call(&streaming, json!({"operation":"finish"})).await;
    assert!(finished.to_string().contains("stream-asr"));
    call(&streaming, json!({"operation":"start"})).await;
    call(&streaming, json!({"operation":"reset"})).await;
    assert!(rpc(
        &n,
        "unload",
        json!({"model":"moonshine_streaming_tiny_q8_0","family":"moonshine_asr"})
    )
    .await
    .get("error")
    .is_some());
    call(
        &streaming,
        json!({"operation":"reset","session_id":"other"}),
    )
    .await;
    assert!(rpc(
        &n,
        "unload",
        json!({"model":"moonshine_streaming_tiny_q8_0","family":"moonshine_asr"})
    )
    .await
    .get("error")
    .is_none());
}
