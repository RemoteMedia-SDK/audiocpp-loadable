//! All raw handles stay on the owning worker thread. No unsafe Send/Sync implementations.
#![allow(
    non_camel_case_types,
    non_snake_case,
    non_upper_case_globals,
    dead_code
)]
use crate::wire::{Audio, Config};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    ffi::{CStr, CString},
    path::{Path, PathBuf},
    ptr,
};
mod audio {
    #![allow(clippy::too_many_arguments)] // Generated signatures mirror the upstream C ABI.
    include!(concat!(env!("OUT_DIR"), "/AudioApi.rs"));
}
mod manager {
    include!(concat!(env!("OUT_DIR"), "/ManagerApi.rs"));
}
use audio::*;
use manager::*;
pub type Result<T> = std::result::Result<T, String>;
fn c(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| "embedded NUL in native string".into())
}
fn optional(s: &CString) -> *const std::ffi::c_char {
    if s.as_bytes().is_empty() {
        ptr::null()
    } else {
        s.as_ptr()
    }
}
fn native_path(path: &Path) -> String {
    let value = path.to_string_lossy();
    // Upstream joins catalog entries containing '/', which Windows verbatim paths reject.
    if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else {
        value.strip_prefix(r"\\?\").unwrap_or(&value).to_owned()
    }
}
unsafe fn text(p: *const std::ffi::c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        CStr::from_ptr(p).to_string_lossy().into_owned()
    }
}
fn library(dir: &Path, name: &str) -> PathBuf {
    dir.join(if cfg!(windows) {
        format!("{name}.dll")
    } else if cfg!(target_os = "macos") {
        format!("lib{name}.dylib")
    } else {
        format!("lib{name}.so")
    })
}
unsafe fn open_library(path: PathBuf) -> Result<libloading::Library> {
    let path = path
        .canonicalize()
        .map_err(|e| format!("{}: {e}", path.display()))?;
    // Upstream ggml/OpenMP keep process-global state. Pin the DLLs across Engine
    // recreation; all per-model/session/job resources are still released normally.
    static PINNED: std::sync::OnceLock<std::sync::Mutex<HashMap<PathBuf, libloading::Library>>> =
        std::sync::OnceLock::new();
    let mut pinned = PINNED
        .get_or_init(Default::default)
        .lock()
        .map_err(|e| e.to_string())?;
    if !pinned.contains_key(&path) {
        pinned.insert(path.clone(), load_library(&path)?);
    }
    load_library(&path)
}
unsafe fn load_library(path: &Path) -> Result<libloading::Library> {
    #[cfg(windows)]
    {
        libloading::os::windows::Library::load_with_flags(path, 0x00000100 | 0x00001000)
            .map(Into::into)
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        libloading::Library::new(path).map_err(|e| e.to_string())
    }
}
struct Session {
    handle: *mut audiocpp_session,
    model: String,
    request_id: Value,
    offset: i64,
    emitted_audio: bool,
}
pub struct Engine {
    api: AudioApi,
    manager_api: ManagerApi,
    registry: *mut audiocpp_registry,
    manager: *mut rm_ac_manager,
    models: HashMap<String, *mut audiocpp_model>,
    sessions: HashMap<String, Session>,
    jobs: HashMap<String, *mut rm_ac_job>,
    imports: BTreeMap<String, PathBuf>,
    cache: PathBuf,
    _cache_lock: std::fs::File,
}
impl Engine {
    pub fn open(config: &Config) -> Result<Self> {
        unsafe {
            let api =
                AudioApi::from_library(open_library(library(&config.library_dir, "audiocpp"))?)
                    .map_err(|e| e.to_string())?;
            let version = api.audiocpp_abi_version();
            if version >> 16 != 0 || (version >> 8) & 255 < 2 {
                return Err(format!(
                    "incompatible audio.cpp ABI {version:#x}; need 0.2.x"
                ));
            }
            let manager_api = ManagerApi::from_library(open_library(library(
                &config.library_dir,
                "audiocpp_manager_bridge",
            ))?)
            .map_err(|e| e.to_string())?;
            std::fs::create_dir_all(&config.cache_dir).map_err(|e| e.to_string())?;
            let cache = config.cache_dir.canonicalize().map_err(|e| e.to_string())?;
            let cache_lock = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(cache.join("plugin.lock"))
                .map_err(|e| e.to_string())?;
            fs2::FileExt::try_lock_exclusive(&cache_lock).map_err(|_|"model cache is already owned by another plugin runtime; choose a separate cache_dir")?;
            let imports_path = cache.join("imports.json");
            let imports = if imports_path.exists() {
                serde_json::from_slice(&std::fs::read(imports_path).map_err(|e| e.to_string())?)
                    .map_err(|e| e.to_string())?
            } else {
                BTreeMap::new()
            };
            let manager = manager_api.rm_ac_create(c(&native_path(&cache))?.as_ptr());
            if manager.is_null() {
                return Err(text(manager_api.rm_ac_error()));
            }
            let mut registry = ptr::null_mut();
            if api.audiocpp_registry_create(ptr::null(), &mut registry) != 0 {
                let e = text(api.audiocpp_last_error());
                manager_api.rm_ac_destroy(manager);
                return Err(e);
            }
            Ok(Self {
                api,
                manager_api,
                registry,
                manager,
                models: HashMap::new(),
                sessions: HashMap::new(),
                jobs: HashMap::new(),
                imports,
                cache,
                _cache_lock: cache_lock,
            })
        }
    }
    unsafe fn check(&self, code: audiocpp_status) -> Result<()> {
        if code == 0 {
            Ok(())
        } else {
            Err(text(self.api.audiocpp_last_error()))
        }
    }
    unsafe fn manager_string(&self, p: *mut std::ffi::c_char) -> Result<String> {
        if p.is_null() {
            return Err(text(self.manager_api.rm_ac_error()));
        }
        let result = text(p);
        self.manager_api.rm_ac_string_free(p);
        Ok(result)
    }
    fn path(&self, model: &str) -> Result<PathBuf> {
        if let Some(p) = self.imports.get(model) {
            return Ok(p.clone());
        }
        let path = Path::new(model);
        if path.exists() {
            return path.canonicalize().map_err(|e| e.to_string());
        }
        unsafe {
            if let Some(job) = self.jobs.get(model) {
                let (mut d, mut t) = (0, 0);
                if self.manager_api.rm_ac_job_status(*job, &mut d, &mut t) == 0 {
                    return Err("model download is still active".into());
                }
            }
            self.manager_string(
                self.manager_api
                    .rm_ac_resolve(self.manager, c(model)?.as_ptr()),
            )
            .map(PathBuf::from)
        }
    }
    fn key(&self, config: &Config) -> Result<String> {
        let path = self
            .path(&config.model)?
            .canonicalize()
            .map_err(|e| e.to_string())?;
        Ok(json!([
            path,
            config.family,
            config.config_id,
            config.weight_id,
            config.model_spec_override,
            config.load_options
        ])
        .to_string())
    }
    unsafe fn options(&self, values: &BTreeMap<String, String>) -> Result<*mut audiocpp_options> {
        let p = self.api.audiocpp_options_create();
        if p.is_null() {
            return Err("option allocation failed".into());
        }
        let result = (|| {
            for (k, v) in values {
                self.check(
                    self.api
                        .audiocpp_options_set(p, c(k)?.as_ptr(), c(v)?.as_ptr()),
                )?;
            }
            Ok(p)
        })();
        if result.is_err() {
            self.api.audiocpp_options_free(p);
        }
        result
    }
    fn load(&mut self, config: &Config) -> Result<String> {
        unsafe {
            let key = self.key(config)?;
            if self.models.contains_key(&key) {
                return Ok(key);
            }
            let path = c(&native_path(&self.path(&config.model)?))?;
            let family = c(&config.family)?;
            let cid = c(&config.config_id)?;
            let weight = c(&config.weight_id)?;
            let spec_override = c(&config.model_spec_override)?;
            let conf = audiocpp_model_config {
                family_hint: optional(&family),
                config_id: optional(&cid),
                weight_id: optional(&weight),
                model_spec_override: optional(&spec_override),
            };
            let opts = self.options(&config.load_options)?;
            let mut p = ptr::null_mut();
            let status =
                self.api
                    .audiocpp_model_load(self.registry, path.as_ptr(), &conf, opts, &mut p);
            self.api.audiocpp_options_free(opts);
            self.check(status)?;
            self.models.insert(key.clone(), p);
            Ok(key)
        }
    }
    pub fn close_node(&mut self, prefix: &str) {
        unsafe {
            let keys: Vec<_> = self
                .sessions
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect();
            for key in keys {
                if let Some(s) = self.sessions.remove(&key) {
                    self.api.audiocpp_session_free(s.handle);
                }
            }
        }
    }
    fn close_session(&mut self, sid: &str) {
        if let Some(s) = self.sessions.remove(sid) {
            unsafe {
                self.api.audiocpp_session_free(s.handle);
            }
        }
    }
    pub fn manage(&mut self, method: &str, params: &Value, config: &Config) -> Result<Value> {
        unsafe {
            let id = params["model"].as_str().unwrap_or(&config.model);
            match method {
                "schema" => Ok(
                    json!({"config":schemars::schema_for!(Config),"audio":schemars::schema_for!(Audio),
                    "methods":["schema","capabilities","catalog","inspect","installed","import","download","status","cancel","load","model_info","loaded","unload","remove"],
                    "transport":"JSON-RPC 2.0 on the node data port", "stream_operations":["start","push","poll","finish","reset","generate"]}),
                ),
                "capabilities" => {
                    let tasks: Vec<_> = (0..self.api.audiocpp_task_count())
                        .map(|i| text(self.api.audiocpp_task_name(i)))
                        .collect();
                    let mut families = Vec::new();
                    for i in 0..self.api.audiocpp_registry_family_count(self.registry) {
                        let mut p = ptr::null();
                        self.check(self.api.audiocpp_registry_family(self.registry, i, &mut p))?;
                        families.push(text(p));
                    }
                    Ok(
                        json!({"version":text(self.api.audiocpp_build_version()),"tasks":tasks,"families":families}),
                    )
                }
                "catalog" | "inspect" | "installed" => {
                    let filter = if method == "inspect" { id } else { "" };
                    let inventory = self.manager_string(
                        self.manager_api
                            .rm_ac_inventory(self.manager, c(filter)?.as_ptr()),
                    )?;
                    let mut rows: Vec<Value> =
                        serde_json::from_str(&inventory).map_err(|e| e.to_string())?;
                    for row in &mut rows {
                        if self.path(row["id"].as_str().unwrap_or("")).is_err() {
                            row["installed"] = json!(false);
                        }
                    }
                    if method == "installed" {
                        rows.retain(|r| r["installed"] == true);
                    }
                    Ok(json!({"inventory":rows,"imports":self.imports}))
                }
                "import" => {
                    if id.is_empty() || !id.starts_with("local:") {
                        return Err("import IDs must start with local:".into());
                    }
                    if self.imports.contains_key(id) {
                        return Err("import ID already exists; remove it first".into());
                    }
                    let path = PathBuf::from(params["path"].as_str().ok_or("path is required")?)
                        .canonicalize()
                        .map_err(|e| e.to_string())?;
                    self.imports.insert(id.into(), path);
                    if let Err(e) = self.save_imports() {
                        self.imports.remove(id);
                        return Err(e);
                    }
                    Ok(json!({"model":id}))
                }
                "download" => {
                    if id.is_empty() {
                        return Err("model package ID is required".into());
                    }
                    if let Some(p) = self.jobs.get(id) {
                        let (mut done, mut total) = (0, 0);
                        if self.manager_api.rm_ac_job_status(*p, &mut done, &mut total) == 0 {
                            return Ok(json!({"job_id":id}));
                        }
                    }
                    if let Some(old) = self.jobs.remove(id) {
                        self.manager_api.rm_ac_job_free(old);
                    }
                    let job = self
                        .manager_api
                        .rm_ac_download(self.manager, c(id)?.as_ptr());
                    if job.is_null() {
                        return Err(text(self.manager_api.rm_ac_error()));
                    }
                    self.jobs.insert(id.into(), job);
                    Ok(json!({"job_id":id}))
                }
                "status" | "cancel" => {
                    let job_id = params["job_id"].as_str().ok_or("job_id is required")?;
                    let p = *self.jobs.get(job_id).ok_or("unknown job_id")?;
                    if method == "cancel" {
                        self.manager_api.rm_ac_job_cancel(p);
                    }
                    let (mut done, mut total) = (0, 0);
                    let state = self.manager_api.rm_ac_job_status(p, &mut done, &mut total);
                    Ok(
                        json!({"job_id":job_id,"state":(["running","completed","failed","cancelled"][state as usize]),"downloaded_bytes":done,"total_bytes":total,"message":self.manager_string(self.manager_api.rm_ac_job_message(p))?}),
                    )
                }
                "load" | "model_info" | "unload" => {
                    let mut cfg = config.clone();
                    cfg.model = id.into();
                    if let Some(opts) = params.get("load_options") {
                        cfg.load_options =
                            serde_json::from_value(opts.clone()).map_err(|e| e.to_string())?;
                    }
                    for (field, dest) in [
                        ("family", &mut cfg.family),
                        ("config_id", &mut cfg.config_id),
                        ("weight_id", &mut cfg.weight_id),
                        ("model_spec_override", &mut cfg.model_spec_override),
                    ] {
                        if let Some(v) = params[field].as_str() {
                            *dest = v.into();
                        }
                    }
                    if method == "unload" {
                        let key = self.key(&cfg)?;
                        if self.sessions.values().any(|s| s.model == key) {
                            return Err("model is in use by a streaming session".into());
                        }
                        if let Some(p) = self.models.remove(&key) {
                            self.api.audiocpp_model_free(p);
                        }
                        return Ok(json!({"unloaded":true}));
                    }
                    let key = self.load(&cfg)?;
                    self.inspect_model(*self.models.get(&key).unwrap(), &key)
                }
                "loaded" => Ok(
                    json!({"models":self.models.keys().collect::<Vec<_>>(),"sessions":self.sessions.len()}),
                ),
                "remove" => {
                    if let Some(job) = self.jobs.get(id) {
                        let (mut d, mut t) = (0, 0);
                        if self.manager_api.rm_ac_job_status(*job, &mut d, &mut t) == 0 {
                            return Err("download is active".into());
                        }
                    }
                    let resolved = self.path(id)?;
                    let path = if self.imports.contains_key(id) {
                        resolved.canonicalize().unwrap_or(resolved)
                    } else {
                        resolved.canonicalize().map_err(|e| e.to_string())?
                    };
                    for key in self.models.keys() {
                        let k: Value = serde_json::from_str(key).map_err(|e| e.to_string())?;
                        let loaded_path =
                            Path::new(k[0].as_str().ok_or("invalid model cache key")?);
                        if path.starts_with(loaded_path) || loaded_path.starts_with(&path) {
                            return Err("unload model before removing it".into());
                        }
                    }
                    if let Some(old) = self.imports.remove(id) {
                        if let Err(e) = self.save_imports() {
                            self.imports.insert(id.into(), old);
                            return Err(e);
                        }
                        return Ok(json!({"unregistered":id}));
                    }
                    if !path.starts_with(&self.cache) {
                        return Err("refusing removal outside the managed cache".into());
                    }
                    Ok(
                        json!({"message":self.manager_string(self.manager_api.rm_ac_remove(self.manager,c(id)?.as_ptr()))?}),
                    )
                }
                _ => Err(format!("unknown model method: {method}")),
            }
        }
    }
    fn save_imports(&self) -> Result<()> {
        use std::io::Write;
        let mut temp = tempfile::NamedTempFile::new_in(&self.cache).map_err(|e| e.to_string())?;
        temp.write_all(&serde_json::to_vec_pretty(&self.imports).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        temp.as_file().sync_all().map_err(|e| e.to_string())?;
        temp.persist(self.cache.join("imports.json"))
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    unsafe fn inspect_model(&self, p: *mut audiocpp_model, key: &str) -> Result<Value> {
        let mut tasks = Vec::new();
        for i in 0..self.api.audiocpp_task_count() {
            let task = self.api.audiocpp_task_name(i);
            tasks.push(json!({"task":text(task),"offline":self.api.audiocpp_model_supports(p,task,c("offline")?.as_ptr())!=0,"streaming":self.api.audiocpp_model_supports(p,task,c("streaming")?.as_ptr())!=0}));
        }
        let mut options = Vec::new();
        for scope in 0..3 {
            for i in 0..self.api.audiocpp_model_option_count(p, scope) {
                let (mut name, mut value, mut desc, mut default, mut min, mut max) = (
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                    ptr::null(),
                );
                let mut required = 0;
                self.check(self.api.audiocpp_model_option(
                    p,
                    scope,
                    i,
                    &mut name,
                    &mut value,
                    &mut desc,
                    &mut default,
                    &mut min,
                    &mut max,
                    &mut required,
                ))?;
                options.push(json!({"scope":(["request","session","load"][scope as usize]),"name":text(name),"value":text(value),"description":text(desc),"default":text(default),"min":text(min),"max":text(max),"required":required!=0}));
            }
        }
        let mut languages = Vec::new();
        for i in 0..self.api.audiocpp_model_language_count(p) {
            let mut s = ptr::null();
            self.check(self.api.audiocpp_model_language(p, i, &mut s))?;
            languages.push(text(s));
        }
        Ok(
            json!({"key":key,"family":text(self.api.audiocpp_model_family(p)),"description":text(self.api.audiocpp_model_description(p)),"tasks":tasks,"options":options,"languages":languages,
            "timestamps":self.api.audiocpp_model_supports_timestamps(p)!=0,"speaker_reference":self.api.audiocpp_model_supports_speaker_reference(p)!=0,"style_condition":self.api.audiocpp_model_supports_style_condition(p)!=0}),
        )
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        unsafe {
            for (_, s) in self.sessions.drain() {
                self.api.audiocpp_session_free(s.handle);
            }
            for (_, p) in self.models.drain() {
                self.api.audiocpp_model_free(p);
            }
            for (_, p) in self.jobs.drain() {
                self.manager_api.rm_ac_job_free(p);
            }
            self.api.audiocpp_registry_free(self.registry);
            self.manager_api.rm_ac_destroy(self.manager);
        }
    }
}

impl Engine {
    unsafe fn request(&self, v: &Value, config: &Config) -> Result<*mut audiocpp_request> {
        let p = self.api.audiocpp_request_create();
        if p.is_null() {
            return Err("request allocation failed".into());
        }
        let result = (|| {
            if let Some(s) = v["text"].as_str() {
                self.check(
                    self.api
                        .audiocpp_request_set_text(p, c(s)?.as_ptr(), ptr::null()),
                )?;
            }
            if let Some(s) = v["language"].as_str() {
                self.check(
                    self.api
                        .audiocpp_request_set_text_language(p, c(s)?.as_ptr()),
                )?;
            }
            for key in ["audio", "voice_audio"] {
                if let Some(value) = v.get(key) {
                    let a: Audio =
                        serde_json::from_value(value.clone()).map_err(|e| e.to_string())?;
                    let frames = a.frames()?;
                    let status = if key == "audio" {
                        self.api.audiocpp_request_set_audio(
                            p,
                            a.samples.as_ptr(),
                            frames,
                            a.sample_rate,
                            a.channels,
                        )
                    } else {
                        self.api.audiocpp_request_set_voice_audio(
                            p,
                            a.samples.as_ptr(),
                            frames,
                            a.sample_rate,
                            a.channels,
                        )
                    };
                    self.check(status)?;
                }
            }
            for (key, set) in [
                ("voice_id", self.api.audiocpp_request_set_voice_id),
                (
                    "style_language",
                    self.api.audiocpp_request_set_style_language,
                ),
                ("emotion", self.api.audiocpp_request_set_emotion),
            ] {
                if let Some(s) = v[key].as_str() {
                    self.check(set(p, c(s)?.as_ptr()))?;
                }
            }
            for (key, set) in [
                ("speaking_rate", self.api.audiocpp_request_set_speaking_rate),
                ("pitch_shift", self.api.audiocpp_request_set_pitch_shift),
                ("energy_scale", self.api.audiocpp_request_set_energy_scale),
            ] {
                if let Some(n) = v[key].as_f64() {
                    self.check(set(p, n as f32))?;
                }
            }
            if let Some(tags) = v["style_tags"].as_object() {
                for (k, s) in tags {
                    self.check(self.api.audiocpp_request_set_style_tag(
                        p,
                        c(k)?.as_ptr(),
                        c(s.as_str().ok_or("style tags must be strings")?)?.as_ptr(),
                    ))?;
                }
            }
            let mut options = config.request_options.clone();
            if let Some(o) = v.get("options") {
                options.extend(
                    serde_json::from_value::<BTreeMap<String, Value>>(o.clone())
                        .map_err(|e| e.to_string())?,
                );
            }
            for (k, value) in options {
                if let Some(array) = value.as_array() {
                    let strings: Vec<_> = array
                        .iter()
                        .map(|v| c(v.as_str().ok_or("list options must contain strings")?))
                        .collect::<Result<_>>()?;
                    let pointers: Vec<_> = strings.iter().map(|s| s.as_ptr()).collect();
                    self.check(self.api.audiocpp_request_set_option_array(
                        p,
                        c(&k)?.as_ptr(),
                        pointers.as_ptr(),
                        pointers.len(),
                    ))?;
                } else {
                    let s = match value {
                        Value::String(s) => s,
                        Value::Number(n) => n.to_string(),
                        Value::Bool(b) => b.to_string(),
                        _ => return Err("options must be scalar values or string lists".into()),
                    };
                    self.check(self.api.audiocpp_request_set_option(
                        p,
                        c(&k)?.as_ptr(),
                        c(&s)?.as_ptr(),
                    ))?;
                }
            }
            if let Some(artifacts) = v["artifacts"].as_array() {
                for a in artifacts {
                    let kind = a["kind"]
                        .as_u64()
                        .filter(|k| *k <= 8)
                        .ok_or("artifact kind must be 0..8")?
                        as audiocpp_artifact_kind;
                    let bytes: Vec<u8> =
                        serde_json::from_value(a["payload"].clone()).map_err(|e| e.to_string())?;
                    let mut index = 0;
                    self.check(self.api.audiocpp_request_add_artifact(
                        p,
                        kind,
                        c(a["id"].as_str().unwrap_or(""))?.as_ptr(),
                        bytes.as_ptr().cast(),
                        bytes.len(),
                        &mut index,
                    ))?;
                    if let Some(meta) = a["metadata"].as_object() {
                        for (k, v) in meta {
                            self.check(self.api.audiocpp_request_set_artifact_meta(
                                p,
                                index,
                                c(k)?.as_ptr(),
                                c(v.as_str().ok_or("artifact metadata must be strings")?)?.as_ptr(),
                            ))?;
                        }
                    }
                }
            }
            Ok(p)
        })();
        if result.is_err() {
            self.api.audiocpp_request_free(p);
        }
        result
    }
    unsafe fn result(&self, p: *const audiocpp_result) -> Result<Value> {
        let mut out = json!({"audio":[],"named_audio":[],"segments":[],"speaker_turns":[],"words":[],"artifacts":[]});
        let (mut samples, mut frames, mut rate, mut channels) = (ptr::null(), 0, 0, 0);
        let status =
            self.api
                .audiocpp_result_audio(p, &mut samples, &mut frames, &mut rate, &mut channels);
        if status == 0 {
            out["audio"]
                .as_array_mut()
                .unwrap()
                .push(self.audio_value(samples, frames, rate, channels)?);
        } else if status != 7 {
            self.check(status)?;
        }
        let (mut txt, mut lang) = (ptr::null(), ptr::null());
        let status = self.api.audiocpp_result_text(p, &mut txt, &mut lang);
        if status == 0 {
            out["text"] = json!(text(txt));
            out["language"] = json!(text(lang));
        } else if status != 7 {
            self.check(status)?;
        }
        for i in 0..self.api.audiocpp_result_named_audio_count(p) {
            let mut name = ptr::null();
            self.check(self.api.audiocpp_result_named_audio(
                p,
                i,
                &mut name,
                &mut samples,
                &mut frames,
                &mut rate,
                &mut channels,
            ))?;
            let mut a = self.audio_value(samples, frames, rate, channels)?;
            a["name"] = json!(text(name));
            out["named_audio"].as_array_mut().unwrap().push(a);
        }
        for i in 0..self.api.audiocpp_result_segment_count(p) {
            let (mut start, mut end, mut conf, mut txt) = (0, 0, 0., ptr::null());
            self.check(
                self.api
                    .audiocpp_result_segment(p, i, &mut start, &mut end, &mut conf, &mut txt),
            )?;
            out["segments"].as_array_mut().unwrap().push(
                json!({"start_sample":start,"end_sample":end,"confidence":conf,"text":text(txt)}),
            );
        }
        for i in 0..self.api.audiocpp_result_speaker_turn_count(p) {
            let (mut start, mut end, mut conf, mut txt, mut speaker) =
                (0, 0, 0., ptr::null(), ptr::null());
            self.check(self.api.audiocpp_result_speaker_turn(
                p,
                i,
                &mut start,
                &mut end,
                &mut speaker,
                &mut conf,
                &mut txt,
            ))?;
            out["speaker_turns"].as_array_mut().unwrap().push(json!({"start_sample":start,"end_sample":end,"confidence":conf,"text":text(txt),"speaker_id":text(speaker)}));
        }
        for i in 0..self.api.audiocpp_result_word_count(p) {
            let (mut start, mut end, mut conf, mut txt) = (0, 0, 0., ptr::null());
            self.check(
                self.api
                    .audiocpp_result_word(p, i, &mut txt, &mut start, &mut end, &mut conf),
            )?;
            out["words"].as_array_mut().unwrap().push(
                json!({"start_sample":start,"end_sample":end,"confidence":conf,"text":text(txt)}),
            );
        }
        for i in 0..self.api.audiocpp_result_artifact_count(p) {
            let (mut kind, mut id, mut payload, mut len) = (0, ptr::null(), ptr::null(), 0);
            self.check(self.api.audiocpp_result_artifact(
                p,
                i,
                &mut kind,
                &mut id,
                &mut payload,
                &mut len,
            ))?;
            let bytes = if len == 0 {
                vec![]
            } else {
                if payload.is_null() {
                    return Err("null artifact buffer".into());
                }
                std::slice::from_raw_parts(payload.cast::<u8>(), len).to_vec()
            };
            let mut meta = serde_json::Map::new();
            for j in 0..self.api.audiocpp_result_artifact_meta_count(p, i) {
                let (mut k, mut v) = (ptr::null(), ptr::null());
                self.check(
                    self.api
                        .audiocpp_result_artifact_meta(p, i, j, &mut k, &mut v),
                )?;
                meta.insert(text(k), json!(text(v)));
            }
            out["artifacts"]
                .as_array_mut()
                .unwrap()
                .push(json!({"kind":kind,"id":text(id),"payload":bytes,"metadata":meta}));
        }
        Ok(out)
    }
    unsafe fn audio_value(
        &self,
        p: *const f32,
        frames: usize,
        rate: i32,
        channels: i32,
    ) -> Result<Value> {
        if channels <= 0 || rate <= 0 {
            return Err("invalid native audio shape".into());
        }
        let len = frames
            .checked_mul(channels as usize)
            .filter(|n| *n <= isize::MAX as usize / 4)
            .ok_or("audio length overflow")?;
        let samples = if len == 0 {
            vec![]
        } else {
            if p.is_null() {
                return Err("null audio buffer".into());
            }
            std::slice::from_raw_parts(p, len).to_vec()
        };
        Ok(json!({"samples":samples,"sample_rate":rate,"channels":channels}))
    }
    unsafe fn event(
        &self,
        p: *mut audiocpp_event,
        id: &Value,
        emit: &mut dyn FnMut(Value) -> Result<()>,
    ) -> Result<bool> {
        if p.is_null() {
            return Ok(false);
        }
        let result = (|| {
            let mut v = self.result(self.api.audiocpp_event_as_result(p))?;
            v["request_id"] = id.clone();
            v["final"] = json!(self.api.audiocpp_event_is_final(p) != 0);
            let mut activity = Vec::new();
            for i in 0..self.api.audiocpp_event_voice_activity_count(p) {
                let (mut kind, mut sample, mut probability) = (0, 0, 0.);
                self.check(self.api.audiocpp_event_voice_activity(
                    p,
                    i,
                    &mut kind,
                    &mut sample,
                    &mut probability,
                ))?;
                activity.push(json!({"kind":kind,"sample":sample,"probability":probability}));
            }
            v["voice_activity"] = json!(activity);
            let audio = v["audio"].as_array().is_some_and(|a| !a.is_empty())
                || v["named_audio"].as_array().is_some_and(|a| !a.is_empty());
            emit(v)?;
            Ok(audio)
        })();
        self.api.audiocpp_event_free(p);
        result
    }
    pub fn infer(
        &mut self,
        cfg: &Config,
        sid: &str,
        v: &Value,
        emit: &mut dyn FnMut(Value) -> Result<()>,
    ) -> Result<()> {
        unsafe {
            let operation = v["operation"].as_str().unwrap_or(if cfg.mode == "offline" {
                "run"
            } else if v.get("audio").is_some() {
                "push"
            } else {
                "generate"
            });
            if operation == "reset" {
                if let Some(s) = self.sessions.remove(sid) {
                    self.api.audiocpp_stream_reset(s.handle);
                    self.api.audiocpp_session_free(s.handle);
                }
                return Ok(());
            }
            if cfg.mode == "offline" && operation != "run" {
                return Err("offline mode accepts only run".into());
            }
            if cfg.mode == "streaming"
                && !["start", "push", "poll", "finish", "generate"].contains(&operation)
            {
                return Err(
                    "streaming operation must be start, push, poll, finish, generate or reset"
                        .into(),
                );
            }
            if !self.sessions.contains_key(sid) {
                if ["poll", "finish"].contains(&operation) {
                    return Err("no active stream".into());
                }
                let key = self.load(cfg)?;
                let model = self.models[&key];
                let task = c(&cfg.task)?;
                let mode = c(&cfg.mode)?;
                if self
                    .api
                    .audiocpp_model_supports(model, task.as_ptr(), mode.as_ptr())
                    == 0
                {
                    return Err(format!("model does not support {} {}", cfg.task, cfg.mode));
                }
                let backend = c(&cfg.backend)?;
                let backend_cfg = audiocpp_backend_config {
                    backend: backend.as_ptr(),
                    device: cfg.device,
                    threads: cfg.threads,
                };
                let opts = self.options(&cfg.session_options)?;
                let implementation = c(&cfg.implementation)?;
                let mut handle = ptr::null_mut();
                let status = self.api.audiocpp_session_create_with_impl(
                    model,
                    task.as_ptr(),
                    mode.as_ptr(),
                    &backend_cfg,
                    implementation.as_ptr(),
                    opts,
                    &mut handle,
                );
                self.api.audiocpp_options_free(opts);
                self.check(status)?;
                self.sessions.insert(
                    sid.into(),
                    Session {
                        handle,
                        model: key,
                        request_id: v["request_id"].clone(),
                        offset: 0,
                        emitted_audio: false,
                    },
                );
                if cfg.mode == "streaming" {
                    // The audio chunks are pushed separately; do not also place them in start's request.
                    let mut start = v.clone();
                    start
                        .as_object_mut()
                        .ok_or("request must be an object")?
                        .remove("audio");
                    let r = self.request(&start, cfg);
                    match r {
                        Ok(r) => {
                            let status = self.api.audiocpp_stream_start(handle, r);
                            self.api.audiocpp_request_free(r);
                            if let Err(e) = self.check(status) {
                                self.close_session(sid);
                                return Err(e);
                            }
                            let (mut input, mut output, mut frames, mut seconds) = (0, 0, 0, 0.);
                            let policy = self.check(self.api.audiocpp_stream_policy(
                                handle,
                                &mut input,
                                &mut output,
                                &mut frames,
                                &mut seconds,
                            ));
                            if let Err(e) = policy {
                                self.close_session(sid);
                                return Err(e);
                            }
                            if let Err(e) = emit(
                                json!({"request_id":v["request_id"],"final":false,"stream_policy":{"input_kind":input,"output_kind":output,"preferred_chunk_samples":frames,"preferred_chunk_seconds":seconds}}),
                            ) {
                                self.close_session(sid);
                                return Err(e);
                            }
                        }
                        Err(e) => {
                            self.close_session(sid);
                            return Err(e);
                        }
                    }
                }
            } else if operation == "start" || operation == "generate" {
                return Err("stream already active; finish or reset it first".into());
            }
            let s = &self.sessions[sid];
            let handle = s.handle;
            let id = s.request_id.clone();
            let offset = s.offset;
            let result = (|| {
                if cfg.mode == "offline" {
                    let request = self.request(v, cfg)?;
                    let mut p = ptr::null_mut();
                    let status = self.api.audiocpp_session_run(handle, request, &mut p);
                    self.api.audiocpp_request_free(request);
                    self.check(status)?;
                    let result = self.result(p);
                    self.api.audiocpp_result_free(p);
                    let mut result = result?;
                    result["request_id"] = id;
                    result["final"] = json!(true);
                    emit(result)?;
                } else {
                    if operation == "push" {
                        let a: Audio = serde_json::from_value(v["audio"].clone())
                            .map_err(|e| e.to_string())?;
                        let frames = a.frames()?;
                        let mut event = ptr::null_mut();
                        self.check(self.api.audiocpp_stream_push(
                            handle,
                            a.samples.as_ptr(),
                            frames,
                            a.sample_rate,
                            a.channels,
                            offset,
                            &mut event,
                        ))?;
                        self.sessions.get_mut(sid).unwrap().offset = offset
                            .checked_add(frames as i64)
                            .ok_or("stream timestamp overflow")?;
                        let audio = self.event(event, &id, emit)?;
                        self.sessions.get_mut(sid).unwrap().emitted_audio |= audio;
                    }
                    loop {
                        let mut event = ptr::null_mut();
                        self.check(self.api.audiocpp_stream_next_event(handle, &mut event))?;
                        if event.is_null() {
                            break;
                        }
                        let audio = self.event(event, &id, emit)?;
                        self.sessions.get_mut(sid).unwrap().emitted_audio |= audio;
                    }
                    if operation == "finish" || operation == "generate" {
                        let mut p = ptr::null_mut();
                        self.check(self.api.audiocpp_stream_finish(handle, &mut p))?;
                        let result = self.result(p);
                        self.api.audiocpp_result_free(p);
                        let mut result = result?;
                        result["request_id"] = id;
                        result["final"] = json!(true);
                        result["audio_already_streamed"] = json!(self.sessions[sid].emitted_audio);
                        emit(result)?;
                    }
                }
                Ok(())
            })();
            if cfg.mode == "offline"
                || operation == "finish"
                || operation == "generate"
                || result.is_err()
            {
                if let Some(s) = self.sessions.remove(sid) {
                    self.api.audiocpp_session_free(s.handle);
                }
            }
            result
        }
    }
}
