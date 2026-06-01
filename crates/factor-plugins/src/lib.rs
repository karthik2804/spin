use std::collections::HashMap;
use std::ptr;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use libloading::Library;
use postcard::to_allocvec;
use spin_factors::{Factor, PrepareContext, RuntimeFactors};
use spin_plugin_abi::PluginVal;

/// Conversion trait for PluginVal <-> wasmtime Val
trait ValConversion {
    fn from_val(val: &Val) -> Self
    where
        Self: Sized;
    fn to_val(self) -> Result<Val>;
}

impl ValConversion for PluginVal {
    fn from_val(val: &Val) -> Self {
        match val {
            Val::Bool(b) => Self::Bool(*b),
            Val::S8(i) => Self::S8(*i),
            Val::U8(u) => Self::U8(*u),
            Val::S16(i) => Self::S16(*i),
            Val::U16(u) => Self::U16(*u),
            Val::S32(i) => Self::S32(*i),
            Val::U32(u) => Self::U32(*u),
            Val::S64(i) => Self::S64(*i),
            Val::U64(u) => Self::U64(*u),
            Val::Float32(f) => Self::Float32(*f),
            Val::Float64(f) => Self::Float64(*f),
            Val::Char(c) => Self::Char(*c as u32),
            Val::String(s) => Self::String(s.as_bytes().to_vec()),
            Val::List(list) => {
                Self::List(list.iter().map(Self::from_val).collect())
            }
            Val::Record(rec) => Self::Record(
                rec.iter()
                    .map(|(k, v)| (k.clone(), Self::from_val(v)))
                    .collect(),
            ),
            Val::Tuple(tuple) => {
                Self::Tuple(tuple.iter().map(Self::from_val).collect())
            }
            Val::Variant(name, opt) => Self::Variant(
                name.clone(),
                opt.as_ref().map(|v| Box::new(Self::from_val(v))),
            ),
            Val::Enum(name) => Self::Enum(name.clone()),
            Val::Option(None) => Self::Option(None),
            Val::Option(Some(inner)) => {
                Self::Option(Some(Box::new(Self::from_val(inner))))
            }
            Val::Result(Ok(None)) => Self::Result(None, None),
            Val::Result(Ok(Some(inner))) => {
                Self::Result(Some(Box::new(Self::from_val(inner))), None)
            }
            Val::Result(Err(None)) => Self::Result(None, Some(Box::new(Self::Unit))),
            Val::Result(Err(Some(inner))) => Self::Result(
                None,
                Some(Box::new(Self::from_val(inner))),
            ),
            Val::Flags(flags) => Self::Flags(flags.clone()),
            _ => Self::String(vec![]),
        }
    }

    fn to_val(self) -> Result<Val> {
        match self {
            Self::Bool(b) => Ok(Val::Bool(b)),
            Self::S8(i) => Ok(Val::S8(i)),
            Self::U8(u) => Ok(Val::U8(u)),
            Self::S16(i) => Ok(Val::S16(i)),
            Self::U16(u) => Ok(Val::U16(u)),
            Self::S32(i) => Ok(Val::S32(i)),
            Self::U32(u) => Ok(Val::U32(u)),
            Self::S64(i) => Ok(Val::S64(i)),
            Self::U64(u) => Ok(Val::U64(u)),
            Self::Float32(f) => Ok(Val::Float32(f)),
            Self::Float64(f) => Ok(Val::Float64(f)),
            Self::Char(c) => Ok(Val::Char(char::from_u32(c).unwrap_or('\0'))),
            Self::String(bytes) => {
                Ok(Val::String(String::from_utf8(bytes).unwrap_or_default()))
            }
            Self::List(vals) => {
                let results: Result<Vec<Val>> = vals.into_iter().map(|v| v.to_val()).collect();
                Ok(Val::List(results?))
            }
            Self::Record(rec) => {
                let results: Result<Vec<(String, Val)>> = rec
                    .into_iter()
                    .map(|(k, v)| v.to_val().map(|v| (k, v)))
                    .collect();
                Ok(Val::Record(results?))
            }
            Self::Tuple(vals) => {
                let results: Result<Vec<Val>> = vals.into_iter().map(|v| v.to_val()).collect();
                Ok(Val::Tuple(results?))
            }
            Self::Variant(name, opt) => {
                let inner = opt.map(|v| v.to_val()).transpose()?.map(Box::new);
                Ok(Val::Variant(name, inner))
            }
            Self::Enum(name) => Ok(Val::Enum(name)),
            Self::Option(None) => Ok(Val::Option(None)),
            Self::Option(Some(inner)) => Ok(Val::Option(Some(Box::new(inner.to_val()?)))),
            Self::Result(None, None) => Ok(Val::Result(Ok(None))),
            Self::Result(Some(ok_val), None) => {
                Ok(Val::Result(Ok(Some(Box::new(ok_val.to_val()?)))))
            }
            Self::Result(None, Some(err_val)) => {
                Ok(Val::Result(Err(Some(Box::new(err_val.to_val()?)))))
            }
            Self::Result(Some(ok_val), Some(_err_val)) => {
                Ok(Val::Result(Ok(Some(Box::new(ok_val.to_val()?)))))
            }
            Self::Flags(flags) => Ok(Val::Flags(flags)),
            Self::Unit => Ok(Val::Bool(false)),
        }
    }
}
use tokio::sync::Mutex;
use wasmtime::component::Val;

mod service;
use service::{LoadedPlugin, PluginService, SharedPluginService};

/// Convert wasmtime::Error to anyhow::Error
fn wt(e: wasmtime::Error) -> anyhow::Error {
    anyhow::Error::msg(e.to_string())
}

/// Metadata keys for plugin configuration.
const PLUGIN_METADATA_KEY: spin_app::MetadataKey<HashMap<String, serde_json::Value>> =
    spin_app::MetadataKey::new("plugin");
const APP_PLUGIN_METADATA_KEY: spin_app::MetadataKey<HashMap<String, serde_json::Value>> =
    spin_app::MetadataKey::new("plugin");

/// Lazy reference to a plugin service, populated after init.
type LazyPluginService = Arc<std::sync::OnceLock<SharedPluginService>>;

// C ABI type aliases
type CPluginWitText = unsafe extern "C" fn() -> *const std::ffi::c_char;
type CPluginInit = unsafe extern "C" fn(*const u8, i32) -> i32;
type CPluginCleanup = unsafe extern "C" fn();
type CPluginDispatch = unsafe extern "C" fn(
    *const std::ffi::c_char, // func_name
    *const u8,              // params_bytes
    i32,                    // params_len
    *mut u8,                // results_buf
    i32,                    // results_buf_len
    *mut i32,               // results_len
    *mut std::ffi::c_char,  // error_buf
    i32,                    // error_buf_len
) -> i32;

/// Loaded shared library with extracted function pointers.
struct PluginLibrary {
    wit_text: CPluginWitText,
    init: Option<CPluginInit>,
    #[allow(dead_code)]
    cleanup: Option<CPluginCleanup>,
    dispatch: CPluginDispatch,
    _lib: Box<Library>,
}

unsafe impl Send for PluginLibrary {}
unsafe impl Sync for PluginLibrary {}

impl PluginLibrary {
    /// Load a plugin from a shared library file.
    fn load(name: &str, path: &std::path::Path) -> Result<Self> {
        let lib = unsafe { Library::new(path) }.with_context(|| {
            format!("failed to load plugin '{}' from {}", name, path.display())
        })?;

        let wit_text = unsafe { lib.get::<CPluginWitText>(b"plugin_wit_text") }.with_context(|| {
            format!("plugin '{}' missing required symbol 'plugin_wit_text'", name)
        })?;

        let dispatch = unsafe { lib.get::<CPluginDispatch>(b"plugin_dispatch") }.with_context(|| {
            format!("plugin '{}' missing required symbol 'plugin_dispatch'", name)
        })?;

        let init: Option<CPluginInit> =
            unsafe { lib.get(b"plugin_init").ok() }.map(|s| *s);
        let cleanup: Option<CPluginCleanup> =
            unsafe { lib.get(b"plugin_cleanup").ok() }.map(|s| *s);

        Ok(Self {
            wit_text: *wit_text,
            init,
            cleanup,
            dispatch: *dispatch,
            _lib: Box::new(lib),
        })
    }

    fn get_wit_text(&self) -> Result<String> {
        let ptr = unsafe { (self.wit_text)() };
        if ptr.is_null() {
            anyhow::bail!("plugin_wit_text returned NULL");
        }
        let c_str = unsafe { std::ffi::CStr::from_ptr(ptr) };
        Ok(c_str.to_str().context("invalid WIT text")?.to_string())
    }

    fn call_init(&self, config_bytes: &[u8]) -> Result<()> {
        if let Some(init_fn) = self.init {
            let ret = unsafe {
                (init_fn)(
                    if config_bytes.is_empty() { ptr::null() } else { config_bytes.as_ptr() },
                    config_bytes.len() as i32,
                )
            };
            if ret != 0 {
                anyhow::bail!("plugin_init returned error code {}", ret);
            }
        }
        Ok(())
    }

    fn call_dispatch(&self, func_name: &str, params_bytes: &[u8]) -> Result<Vec<u8>> {
        let c_func = std::ffi::CString::new(func_name).context("invalid func_name")?;
        let mut results_buf = vec![0u8; 131072];
        let mut results_len: i32 = 0;
        let mut error_buf = vec![0u8; 4096];

        let ret = unsafe {
            (self.dispatch)(
                c_func.as_ptr(),
                params_bytes.as_ptr(),
                params_bytes.len() as i32,
                results_buf.as_mut_ptr(),
                results_buf.len() as i32,
                &mut results_len,
                error_buf.as_mut_ptr() as *mut std::ffi::c_char,
                error_buf.len() as i32,
            )
        };

        if ret != 0 {
            let error_msg = unsafe {
                std::ffi::CStr::from_ptr(error_buf.as_ptr() as *const std::ffi::c_char)
            }
            .to_string_lossy()
            .into_owned();
            anyhow::bail!("plugin_dispatch failed: {}", error_msg);
        }

        if results_len <= 0 || results_len as usize > results_buf.len() {
            anyhow::bail!("plugin_dispatch returned invalid results length: {}", results_len);
        }

        results_buf.truncate(results_len as usize);
        Ok(results_buf)
    }

    #[allow(dead_code)]
    fn call_cleanup(&self) {
        if let Some(cleanup_fn) = self.cleanup {
            unsafe { (cleanup_fn)() };
        }
    }
}

/// Factor that loads C shared library plugins and wires their exports as imports.
pub struct PluginsFactor {
    plugin_paths: Vec<(String, std::path::PathBuf)>,
    plugin_configs: HashMap<String, Vec<u8>>,
    loaded: Vec<LoadedPlugin>,
    service_slots: HashMap<String, LazyPluginService>,
}

impl PluginsFactor {
    pub fn new(
        plugins: Vec<(String, std::path::PathBuf)>,
        configs: HashMap<String, Vec<u8>>,
    ) -> Result<Self> {
        Ok(Self {
            plugin_paths: plugins,
            plugin_configs: configs,
            loaded: Vec::new(),
            service_slots: HashMap::new(),
        })
    }

    pub fn empty() -> Result<Self> {
        Self::new(Vec::new(), HashMap::new())
    }
}

impl Factor for PluginsFactor {
    type RuntimeConfig = ();
    type AppState = PluginsAppState;
    type InstanceBuilder = PluginsInstanceBuilder;

    fn init(&mut self, ctx: &mut impl spin_factors::InitContext<Self>) -> Result<()> {
        if self.plugin_paths.is_empty() {
            return Ok(());
        }

        for (name, path) in &self.plugin_paths {
            let plugin_lib = PluginLibrary::load(name, path)?;
            let wit_text = plugin_lib.get_wit_text()?;
            let parsed = parse_plugin_wit(name, &wit_text)?;
            let lib_arc = Arc::new(plugin_lib);

            for iface in &parsed.exported_interfaces {
                let lazy_service: LazyPluginService = Arc::new(std::sync::OnceLock::new());
                self.service_slots
                    .insert(iface.name.clone(), lazy_service.clone());

                register_forwarding_functions(ctx.linker(), name, iface, lazy_service)?;
            }

            self.loaded.push(LoadedPlugin {
                name: name.clone(),
                library: lib_arc,
                exported_interfaces: parsed.exported_interfaces,
            });
        }

        Ok(())
    }

    fn configure_app<T: RuntimeFactors>(
        &self,
        ctx: spin_factors::ConfigureAppContext<T, Self>,
    ) -> Result<Self::AppState> {
        let mut services = HashMap::new();
        let mut key_to_interfaces: HashMap<String, Vec<String>> = HashMap::new();

        let app_plugin_configs: HashMap<String, serde_json::Value> = ctx
            .app()
            .get_metadata(APP_PLUGIN_METADATA_KEY)
            .unwrap_or(None)
            .unwrap_or_default();

        for loaded in &self.loaded {
            let config_bytes = self
                .plugin_configs
                .get(&loaded.name)
                .cloned()
                .or_else(|| {
                    app_plugin_configs
                        .get(&loaded.name)
                        .and_then(|v| serde_json::to_string(v).ok())
                        .map(|s| s.into_bytes())
                })
                .unwrap_or_default();

            loaded.library.call_init(&config_bytes).with_context(|| {
                format!("failed to initialize plugin '{}'", loaded.name)
            })?;

            let service = PluginService::new(
                loaded.name.clone(),
                loaded.library.clone(),
                &loaded.exported_interfaces,
            );
            let shared = Arc::new(Mutex::new(service));

            for iface in &loaded.exported_interfaces {
                if services.contains_key(&iface.name) {
                    anyhow::bail!(
                        "Multiple plugins export interface '{}'. \
                         Each interface must be provided by exactly one plugin.",
                        iface.name
                    );
                }

                let slot = self.service_slots.get(&iface.name).unwrap();
                slot.set(shared.clone()).map_err(|_| {
                    anyhow!("service slot for '{}' was already populated (bug)", iface.name)
                })?;

                tracing::info!(
                    "Plugin '{}' providing interface '{}' with functions: [{}]",
                    loaded.name,
                    iface.name,
                    iface.functions.join(", ")
                );

                key_to_interfaces
                    .entry(loaded.name.clone())
                    .or_default()
                    .push(iface.name.clone());
                services.insert(iface.name.clone(), shared.clone());
            }
        }

        if !services.is_empty() {
            tracing::info!("Loaded {} plugin interface(s)", services.len());
        }

        let mut component_allowed_interfaces: HashMap<String, std::collections::HashSet<String>> =
            HashMap::new();
        for component in ctx.app().components() {
            let component_id = component.id().to_string();
            let plugin_config: HashMap<String, serde_json::Value> = component
                .get_metadata(PLUGIN_METADATA_KEY)
                .unwrap_or(None)
                .unwrap_or_default();

            let mut allowed = std::collections::HashSet::new();
            for key in plugin_config.keys() {
                if let Some(iface_names) = key_to_interfaces.get(key) {
                    for iface_name in iface_names {
                        allowed.insert(iface_name.clone());
                    }
                    tracing::info!(
                        "Component '{}' granted access to plugin key '{}'",
                        component_id, key
                    );
                }
            }

            if key_to_interfaces.is_empty() {
                allowed = services.keys().cloned().collect();
            }

            component_allowed_interfaces.insert(component_id, allowed);
        }

        Ok(PluginsAppState {
            services: Arc::new(services),
            component_allowed_interfaces: Arc::new(component_allowed_interfaces),
        })
    }

    fn prepare<T: RuntimeFactors>(
        &self,
        ctx: PrepareContext<T, Self>,
    ) -> Result<Self::InstanceBuilder> {
        let app_state = ctx.app_state();
        let component_id = ctx.app_component().id().to_string();
        let allowed_interfaces = app_state
            .component_allowed_interfaces
            .get(&component_id)
            .cloned()
            .unwrap_or_default();
        Ok(PluginsInstanceBuilder {
            services: app_state.services.clone(),
            allowed_interfaces,
        })
    }
}

pub struct PluginsAppState {
    services: Arc<HashMap<String, SharedPluginService>>,
    component_allowed_interfaces: Arc<HashMap<String, std::collections::HashSet<String>>>,
}

impl PluginsAppState {
    pub fn get_service(&self, interface_name: &str) -> Option<&SharedPluginService> {
        self.services.get(interface_name)
    }
}

pub struct PluginsInstanceBuilder {
    services: Arc<HashMap<String, SharedPluginService>>,
    allowed_interfaces: std::collections::HashSet<String>,
}

impl spin_factors::FactorInstanceBuilder for PluginsInstanceBuilder {
    type InstanceState = PluginsInstanceState;

    fn build(self) -> Result<Self::InstanceState> {
        Ok(PluginsInstanceState {
            _services: self.services,
            _allowed_interfaces: self.allowed_interfaces,
        })
    }
}

pub struct PluginsInstanceState {
    _services: Arc<HashMap<String, SharedPluginService>>,
    _allowed_interfaces: std::collections::HashSet<String>,
}

struct ParsedPluginWit {
    exported_interfaces: Vec<service::ExportedInterface>,
}

fn parse_plugin_wit(name: &str, wit_text: &str) -> Result<ParsedPluginWit> {
    let group = wit_parser::UnresolvedPackageGroup::parse("plugin.wit", wit_text)
        .with_context(|| format!("failed to parse WIT for plugin '{}'", name))?;

    let mut resolve = wit_parser::Resolve::new();
    resolve
        .push_group(group)
        .with_context(|| format!("failed to resolve WIT for plugin '{}'", name))?;

    let mut exported_interfaces = Vec::new();

    // Find the package to get the version
    let default_pkg_id = resolve.packages.iter().next().map(|(id, _)| id);

    for (_world_id, world) in &resolve.worlds {
        // Look at exports - these are the interfaces the plugin provides
        for (key, _) in &world.exports {
            if let wit_parser::WorldKey::Interface(iface_id) = key {
                let iface = &resolve.interfaces[*iface_id];

                // Construct the interface name manually to preserve the exact version
                let pkg_id = iface.package.unwrap_or(default_pkg_id.unwrap());
                let pkg = &resolve.packages[pkg_id];
                let version_str = match &pkg.name.version {
                    Some(v) => format!("@{}.{}.{}", v.major, v.minor, v.patch),
                    None => String::new(),
                };
                let iface_name = if let Some(ref iface_name) = iface.name {
                    format!("{}:{}/{}{}", pkg.name.namespace, pkg.name.name, iface_name, version_str)
                } else {
                    format!("{}:{}@{}", pkg.name.namespace, pkg.name.name, version_str)
                };

                tracing::debug!(
                    "Plugin '{}' interface ID={:?}, name='{}', constructed='{}'",
                    name,
                    iface_id,
                    iface.name.as_deref().unwrap_or("none"),
                    iface_name
                );

                let mut functions = Vec::new();
                for (func_name, _) in &iface.functions {
                    functions.push(func_name.to_string());
                }

                if !functions.is_empty() {
                    exported_interfaces.push(service::ExportedInterface {
                        name: iface_name,
                        functions,
                    });
                }
            }
        }
    }

    tracing::info!(
        "Plugin '{}' exports {} interface(s): [{}]",
        name,
        exported_interfaces.len(),
        exported_interfaces
            .iter()
            .map(|i| format!("{} ({} funcs)", i.name, i.functions.len()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    Ok(ParsedPluginWit {
        exported_interfaces,
    })
}

fn register_forwarding_functions<T: Send + 'static>(
    linker: &mut wasmtime::component::Linker<T>,
    plugin_name: &str,
    iface: &service::ExportedInterface,
    lazy_service: LazyPluginService,
) -> Result<()> {
    let mut linker_instance = linker
        .instance(&iface.name)
        .map_err(wt)
        .with_context(|| {
            format!(
                "failed to create linker instance for interface '{}' from plugin '{}'",
                iface.name, plugin_name
            )
        })?;

    for func_name in &iface.functions {
        let iface_name = iface.name.clone();
        let func_name_owned = func_name.clone();
        let plugin_name_owned = plugin_name.to_string();
        let service_ref = lazy_service.clone();

        linker_instance.func_new_async(
            func_name,
            move |_store_ctx, _func_type, params, results| {
                let iface_name = iface_name.clone();
                let func_name = func_name_owned.clone();
                let plugin_name = plugin_name_owned.clone();
                let service_ref = service_ref.clone();
                let owned_params: Vec<Val> = params.to_vec();
                let num_results = results.len();
                Box::new(async move {
                    let service = service_ref.get().ok_or_else(|| {
                        wasmtime::Error::msg(format!(
                            "Plugin '{}' not yet initialized (interface '{}')",
                            plugin_name, iface_name
                        ))
                    })?.clone();

                    let svc = service.lock().await;

                    // Serialize params to postcard bytes
                    let params_bytes =
                        val_slice_to_postcard(&owned_params).map_err(|e| {
                            wasmtime::Error::msg(format!("failed to serialize params: {}", e))
                        })?;

                    // Call plugin dispatch
                    let results_bytes = svc
                        .call_dispatch(&iface_name, &func_name, &params_bytes)
                        .await
                        .map_err(|e| wasmtime::Error::msg(e.to_string()))?;

                    // Deserialize results from postcard bytes
                    let mut owned_results =
                        postcard_to_val_slice(&results_bytes, num_results).map_err(|e| {
                            wasmtime::Error::msg(format!("failed to deserialize results: {}", e))
                        })?;

                    for (dst, src) in results.iter_mut().zip(owned_results.drain(..)) {
                        *dst = src;
                    }
                    Ok(())
                })
            },
        )
        .map_err(wt)
        .with_context(|| {
            format!(
                "failed to register forwarding function '{}/{}' for plugin '{}'",
                iface.name, func_name, plugin_name
            )
        })?;
    }

    Ok(())
}

/// Serialize Val slice to postcard bytes.
fn val_slice_to_postcard(values: &[Val]) -> Result<Vec<u8>> {
    let plugin_vals: Vec<PluginVal> = values.iter().map(ValConversion::from_val).collect();
    to_allocvec(&plugin_vals).context("failed to serialize Val slice to postcard")
}

/// Deserialize postcard bytes to Val slice.
fn postcard_to_val_slice(bytes: &[u8], expected_len: usize) -> Result<Vec<Val>> {
    tracing::debug!("Deserializing {} bytes from plugin", bytes.len());
    if bytes.len() > 100 {
        tracing::debug!("First 100 bytes: {:?}", &bytes[..100]);
    } else {
        tracing::debug!("All bytes: {:?}", bytes);
    }

    let plugin_vals: Vec<PluginVal> =
        postcard::from_bytes(bytes).context("failed to deserialize postcard to PluginVal slice")?;

    tracing::debug!("Deserialized {} PluginVals", plugin_vals.len());

    if plugin_vals.len() != expected_len {
        anyhow::bail!(
            "expected {} results, got {} from plugin",
            expected_len,
            plugin_vals.len()
        );
    }

    plugin_vals
        .into_iter()
        .map(ValConversion::to_val)
        .collect()
}
