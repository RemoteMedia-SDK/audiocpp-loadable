use remotemedia_plugin_sdk::types::RuntimeData;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub library_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub model: String,
    pub task: String,
    pub mode: String,
    pub backend: String,
    pub device: i32,
    pub threads: i32,
    pub family: String,
    pub config_id: String,
    pub weight_id: String,
    pub model_spec_override: String,
    pub implementation: String,
    pub load_options: BTreeMap<String, String>,
    pub session_options: BTreeMap<String, String>,
    pub request_options: BTreeMap<String, Value>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            library_dir: std::env::var_os("AUDIOCPP_LIBRARY_DIR")
                .map(PathBuf::from)
                .unwrap_or_default(),
            cache_dir: dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("remotemedia/audiocpp"),
            model: String::new(),
            task: String::new(),
            mode: "offline".into(),
            backend: "cpu".into(),
            device: 0,
            threads: 1,
            family: String::new(),
            config_id: String::new(),
            weight_id: String::new(),
            model_spec_override: String::new(),
            implementation: String::new(),
            load_options: BTreeMap::new(),
            session_options: BTreeMap::new(),
            request_options: BTreeMap::new(),
        }
    }
}
impl Config {
    pub fn validate(&self, manager: bool) -> Result<(), String> {
        if self.library_dir.as_os_str().is_empty() {
            return Err("library_dir or AUDIOCPP_LIBRARY_DIR is required".into());
        }
        if !manager && (self.model.is_empty() || self.task.is_empty()) {
            return Err("model and task are required".into());
        }
        if !["offline", "streaming"].contains(&self.mode.as_str()) {
            return Err("mode must be offline or streaming".into());
        }
        if ![
            "cpu", "cuda", "hip", "rocm", "vulkan", "metal", "webgpu", "best",
        ]
        .contains(&self.backend.as_str())
            || self.threads < 1
            || self.device < 0
        {
            return Err("invalid backend, device or threads".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Audio {
    pub samples: Vec<f32>,
    pub sample_rate: i32,
    pub channels: i32,
}
impl Audio {
    pub fn frames(&self) -> Result<usize, String> {
        if self.sample_rate <= 0
            || self.channels <= 0
            || !self.samples.len().is_multiple_of(self.channels as usize)
            || self.samples.iter().any(|s| !s.is_finite())
        {
            return Err(
                "audio requires a positive rate/channels and finite, interleaved whole frames"
                    .into(),
            );
        }
        Ok(self.samples.len() / self.channels as usize)
    }
}

pub fn input(data: RuntimeData) -> Result<Value, String> {
    match data {
        RuntimeData::Json(v) if v.is_object() => Ok(v),
        RuntimeData::Text(text) => Ok(json!({"text":text})),
        RuntimeData::Audio {
            samples,
            sample_rate,
            channels,
            ..
        } => Ok(
            json!({"audio":{"samples":samples.to_vec(),"sample_rate":sample_rate,"channels":channels}}),
        ),
        _ => Err("expected text, audio, or a structured JSON request".into()),
    }
}

pub fn emit_result(
    mut v: Value,
    emit: &mut dyn FnMut(RuntimeData) -> Result<(), String>,
) -> Result<(), String> {
    let request_id = v["request_id"].clone();
    for key in ["audio", "named_audio"] {
        // Upstream finish often returns the entire waveform again. Preserve it
        // in the final JSON result without playing it a second time.
        if v["audio_already_streamed"] == true {
            break;
        }
        let entries = match v.get_mut(key).and_then(Value::as_array_mut) {
            Some(a) => a,
            None => continue,
        };
        for entry in entries {
            let audio: Audio = serde_json::from_value(json!({"samples":entry["samples"],"sample_rate":entry["sample_rate"],"channels":entry["channels"]})).map_err(|e|e.to_string())?;
            audio.frames()?;
            emit(RuntimeData::Audio {
                samples: audio.samples.into(),
                sample_rate: audio.sample_rate as u32,
                channels: audio.channels as u32,
                // Upstream names generated TTS chunks chunk_0, chunk_1, ...;
                // those are chunk identifiers, not independent playback tracks.
                stream_id: entry["name"]
                    .as_str()
                    .filter(|name| !name.starts_with("chunk_"))
                    .map(str::to_owned),
                timestamp_us: None,
                arrival_ts_us: None,
                metadata: Some(json!({"request_id":request_id, "name":entry["name"]})),
            })?;
            entry.as_object_mut().unwrap().remove("samples");
        }
    }
    if let Some(t) = v["text"].as_str() {
        emit(RuntimeData::Text(t.into()))?;
    }
    emit(RuntimeData::Json(v))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_malformed_audio() {
        for a in [
            Audio {
                samples: vec![0.],
                sample_rate: 16000,
                channels: 2,
            },
            Audio {
                samples: vec![f32::NAN],
                sample_rate: 16000,
                channels: 1,
            },
            Audio {
                samples: vec![],
                sample_rate: 0,
                channels: 1,
            },
        ] {
            assert!(a.frames().is_err());
        }
    }
    #[test]
    fn defaults_are_explicit_and_typoes_fail() {
        assert_eq!(Config::default().backend, "cpu");
        assert!(serde_json::from_value::<Config>(json!({"backned":"cuda"})).is_err());
    }
    #[test]
    fn preserves_multimodal_outputs_and_correlation() {
        let source = json!({"request_id":"r1","final":true,"audio":[{"samples":[0.25,-0.25],"sample_rate":16000,"channels":1}],
            "named_audio":[{"name":"vocals","samples":[0.5],"sample_rate":16000,"channels":1}],"text":"hello",
            "words":[{"text":"hello","start_sample":0,"end_sample":2}],"speaker_turns":[{"speaker_id":"s1"}],
            "artifacts":[{"kind":4,"id":"midi","payload":[0,255],"metadata":{"format":"test"}}]});
        let mut out = Vec::new();
        emit_result(source.clone(), &mut |v| {
            out.push(v);
            Ok(())
        })
        .unwrap();
        assert_eq!(out.len(), 4);
        match &out[1] {
            RuntimeData::Audio {
                samples,
                stream_id,
                metadata,
                ..
            } => {
                assert_eq!(&samples[..], &[0.5]);
                assert_eq!(stream_id.as_deref(), Some("vocals"));
                assert_eq!(metadata.as_ref().unwrap()["request_id"], "r1");
            }
            _ => panic!("audio expected"),
        }
        match &out[3] {
            RuntimeData::Json(v) => {
                assert_eq!(v["artifacts"], source["artifacts"]);
                assert_eq!(v["words"], source["words"]);
                assert_eq!(v["final"], true);
                assert!(v["audio"][0].get("samples").is_none());
            }
            _ => panic!("metadata expected"),
        }
    }
    #[test]
    fn downstream_failure_stops_emission() {
        assert_eq!(
            emit_result(json!({"text":"hello"}), &mut |_| Err("closed".into())).unwrap_err(),
            "closed"
        );
    }
    #[test]
    fn final_waveform_is_preserved_without_duplicate_playback() {
        let value = json!({"audio_already_streamed":true,"audio":[{"samples":[0.5],"sample_rate":16000,"channels":1}],"final":true});
        let mut out = Vec::new();
        emit_result(value.clone(), &mut |v| {
            out.push(v);
            Ok(())
        })
        .unwrap();
        assert_eq!(out, vec![RuntimeData::Json(value)]);
    }
}
