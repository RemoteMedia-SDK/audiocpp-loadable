//! Minimal real ABI host: no audio.cpp linkage and no remotemedia-core dependency.
use abi_stable::{
    library::RootModule,
    std_types::{RString, RVec},
};
use remotemedia_plugin_sdk::{types::RuntimeData, NodePluginRef};
use serde_json::{json, Value};
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
fn parse(value: &str) -> Result<Value> {
    Ok(if let Some(path) = value.strip_prefix('@') {
        serde_json::from_slice(&std::fs::read(path)?)?
    } else {
        serde_json::from_str(value)?
    })
}
async fn invoke(node: &remotemedia_plugin_sdk::FfiNodeBox, value: Value) -> Result<RuntimeData> {
    let bytes = remotemedia_plugin_sdk::rmp_serde::to_vec_named(&RuntimeData::Json(value))?;
    let output = node
        .process(RVec::from(bytes))
        .await
        .into_result()
        .map_err(|e| e.to_string())?;
    Ok(remotemedia_plugin_sdk::rmp_serde::from_slice(&output)?)
}
#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().collect();
    if args.len() < 3 {
        return Err("usage: host PLUGIN_DLL LIBRARY_DIR [METHOD [PARAMS_JSON]]; or host PLUGIN_DLL LIBRARY_DIR infer CONFIG_JSON REQUEST_JSON".into());
    }
    let plugin = NodePluginRef::load_from_file(std::path::Path::new(&args[1]))?;
    let factories = (plugin.list_factories())();
    eprintln!(
        "Factories: {:?}",
        factories
            .iter()
            .map(|f| f.node_type().to_string())
            .collect::<Vec<_>>()
    );
    let method = args.get(3).map(String::as_str).unwrap_or("capabilities");
    let manager = method != "infer";
    let mut config = if manager {
        json!({})
    } else {
        parse(args.get(4).ok_or("missing CONFIG_JSON")?)?
    };
    config["library_dir"] = json!(args[2]);
    if let Some(cache) = std::env::var_os("AUDIOCPP_CACHE_DIR") {
        config["cache_dir"] = json!(std::path::PathBuf::from(cache));
    }
    let factory = factories
        .iter()
        .find(|f| {
            f.node_type().as_str()
                == if manager {
                    "AudioCppModelManagerNode"
                } else {
                    "AudioCppNode"
                }
        })
        .ok_or("factory missing")?;
    let node = factory
        .create(RString::from(config.to_string()))
        .into_result()
        .map_err(|e| e.to_string())?;
    let value = if manager {
        json!({"jsonrpc":"2.0","id":1,"method":method,"params":args.get(4).map(|s|parse(s)).transpose()?.unwrap_or(json!({}))})
    } else {
        parse(args.get(5).ok_or("missing REQUEST_JSON")?)?
    };
    // `infer` with an array runs each request in turn on the same node, so
    // they share its offline session the way a pipeline's requests do.
    if let (false, Value::Array(requests)) = (manager, &value) {
        let mut outputs = Vec::new();
        for request in requests {
            outputs.push(invoke(&node, request.clone()).await?);
        }
        println!("{}", serde_json::to_string_pretty(&outputs)?);
        return Ok(());
    }
    let mut output = invoke(&node, value).await?;
    // Keep this short-lived CLI alive until its download job finishes. Pipeline
    // callers still receive the asynchronous job ID immediately.
    if method == "download" {
        let id = match &output {
            RuntimeData::Json(v) => v["result"]["job_id"].as_str().map(str::to_owned),
            _ => None,
        };
        if let Some(id) = id {
            loop {
                output = invoke(
                    &node,
                    json!({"jsonrpc":"2.0","id":2,"method":"status","params":{"job_id":id}}),
                )
                .await?;
                if let RuntimeData::Json(v) = &output {
                    if v.get("error").is_some() {
                        break;
                    }
                    match v["result"]["state"].as_str() {
                        Some("completed") => break,
                        Some("failed" | "cancelled") => {
                            eprintln!("{v}");
                            return Err("download did not complete".into());
                        }
                        _ => {}
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        }
    }
    println!("{}", serde_json::to_string_pretty(&output)?);
    if let RuntimeData::Json(v) = output {
        if v.get("error").is_some() {
            return Err("model RPC failed".into());
        }
    }
    Ok(())
}
