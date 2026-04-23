use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use tokio::sync::Mutex;
use wasmtime::component::{Component, ComponentExportIndex, Instance, Linker, Val};
use wasmtime::{Engine, Store};
use wasmtime_wasi::{DirPerms, FilePerms, WasiCtx, WasiCtxBuilder, WasiView};

/// Convert a wasmtime::Error into anyhow::Error
fn wt(e: wasmtime::Error) -> anyhow::Error {
    anyhow::Error::msg(e.to_string())
}

/// State held in the host component's dedicated Store.
struct HostComponentState {
    wasi: WasiCtx,
    table: wasmtime::component::ResourceTable,
}

impl WasiView for HostComponentState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi,
            table: &mut self.table,
        }
    }
}

/// A running host component service with its own Store and Instance.
///
/// The Instance is long-lived and shared across all guest requests,
/// so it can maintain state (e.g., an in-memory cache).
pub struct HostComponentService {
    store: Store<HostComponentState>,
    instance: Instance,
    /// Cached export indices: interface_name -> (interface_index, {func_name -> func_index})
    export_indices: HashMap<String, (ComponentExportIndex, HashMap<String, ComponentExportIndex>)>,
}

impl HostComponentService {
    /// Call an exported function on this host component.
    ///
    /// `interface_name` is the fully qualified interface name (e.g., "example:cache/store@0.1.0").
    /// `func_name` is the function within that interface.
    pub async fn call_func(
        &mut self,
        interface_name: &str,
        func_name: &str,
        params: &[Val],
        results: &mut [Val],
    ) -> Result<()> {
        let (_, func_indices) = self
            .export_indices
            .get(interface_name)
            .ok_or_else(|| anyhow!("host component does not export interface '{interface_name}'"))?;

        let func_index = func_indices
            .get(func_name)
            .ok_or_else(|| {
                anyhow!(
                    "interface '{interface_name}' does not export function '{func_name}'"
                )
            })?;

        let func = self
            .instance
            .get_func(&mut self.store, func_index)
            .ok_or_else(|| {
                anyhow!("failed to get function '{func_name}' from interface '{interface_name}'")
            })?;

        func.call_async(&mut self.store, params, results)
            .await
            .map_err(wt)
            .with_context(|| {
                format!("failed to call {interface_name}/{func_name} on host component")
            })?;

        Ok(())
    }
}

/// Thread-safe handle to a running host component service.
pub type SharedService = Arc<Mutex<HostComponentService>>;

/// The well-known configure interface defined by Spin for host components.
/// Host components export `spin:host-component/configure@0.1.0` to receive
/// configuration at startup.
pub const CONFIGURE_INTERFACE_PREFIX: &str = "spin:host-component/configure";

/// Detect whether an exported interface is the Spin-defined configure interface.
///
/// Matches `spin:host-component/configure@X.Y.Z` at any version.
/// Also matches the legacy convention of any interface ending with `/configure`
/// for backward compatibility.
pub fn is_configure_interface(name: &str) -> bool {
    let stripped = name.split('@').next().unwrap_or(name);
    stripped == CONFIGURE_INTERFACE_PREFIX || stripped.ends_with("/configure")
}

/// Metadata about a host component's exports, discovered from the Component type before instantiation.
#[derive(Clone)]
pub struct ExportedInterface {
    /// Fully qualified interface name (e.g., "example:cache/store@0.1.0")
    pub name: String,
    /// Function names within this interface
    pub functions: Vec<String>,
}

/// Information about a loaded (but not yet instantiated) host component.
pub struct LoadedHostComponent {
    pub name: String,
    pub component: Component,
    pub exported_interfaces: Vec<ExportedInterface>,
}

/// Load a host component from a Wasm file, inspecting its exports.
pub fn load_host_component(
    engine: &Engine,
    name: &str,
    path: &Path,
) -> Result<LoadedHostComponent> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("failed to read host component '{name}' from {}", path.display()))?;
    load_host_component_from_bytes(engine, name, &bytes)
}

/// Load a host component from bytes, inspecting its exports.
pub fn load_host_component_from_bytes(
    engine: &Engine,
    name: &str,
    bytes: &[u8],
) -> Result<LoadedHostComponent> {
    let component = Component::new(engine, bytes)
        .map_err(wt)
        .with_context(|| format!("failed to compile host component '{name}'"))?;

    let component_type = component.component_type();
    let mut exported_interfaces = Vec::new();

    for (export_name, item) in component_type.exports(engine) {
        if let wasmtime::component::types::ComponentItem::ComponentInstance(instance) = item {
            let mut functions = Vec::new();
            for (func_name, func_item) in instance.exports(engine) {
                if let wasmtime::component::types::ComponentItem::ComponentFunc(_) = func_item {
                    functions.push(func_name.to_string());
                }
            }
            if !functions.is_empty() {
                exported_interfaces.push(ExportedInterface {
                    name: export_name.to_string(),
                    functions,
                });
            }
        }
    }

    tracing::info!(
        "Loaded host component '{name}' with {} exported interface(s): [{}]",
        exported_interfaces.len(),
        exported_interfaces
            .iter()
            .map(|i| format!("{} ({} funcs)", i.name, i.functions.len()))
            .collect::<Vec<_>>()
            .join(", ")
    );

    Ok(LoadedHostComponent {
        name: name.to_string(),
        component,
        exported_interfaces,
    })
}

/// Instantiate a loaded host component into its own Store, returning a SharedService.
///
/// If `data_dir` is provided, the host component gets read-write filesystem
/// access to `<data_dir>/<component_name>/` so it can persist state (e.g., via sqlite).
pub async fn instantiate_host_component(
    engine: &Engine,
    loaded: &LoadedHostComponent,
    data_dir: Option<&Path>,
) -> Result<SharedService> {
    let mut host_linker: Linker<HostComponentState> = Linker::new(engine);

    // Add WASI P2 async support so the host component can use WASI with async
    wasmtime_wasi::p2::add_to_linker_async(&mut host_linker)
        .map_err(wt)
        .context("failed to add WASI to host component linker")?;

    let mut wasi_builder = WasiCtxBuilder::new();
    wasi_builder.inherit_stderr();

    // If a data directory is provided, give the host component read-write
    // filesystem access to its own subdirectory.
    if let Some(base_dir) = data_dir {
        let component_dir = base_dir.join(&loaded.name);
        std::fs::create_dir_all(&component_dir).with_context(|| {
            format!(
                "failed to create data directory for host component '{}' at {}",
                loaded.name,
                component_dir.display()
            )
        })?;
        wasi_builder
            .preopened_dir(&component_dir, "/data", DirPerms::all(), FilePerms::all())
            .map_err(|e| anyhow::anyhow!(e))
            .with_context(|| {
                format!(
                    "failed to preopen data directory {} for host component '{}'",
                    component_dir.display(),
                    loaded.name
                )
            })?;

        tracing::info!(
            "Host component '{}' has read-write access to {} (mounted at /data)",
            loaded.name,
            component_dir.display()
        );
    }

    let wasi = wasi_builder.build();

    let state = HostComponentState {
        wasi,
        table: wasmtime::component::ResourceTable::new(),
    };

    let mut store = Store::new(engine, state);

    let instance = host_linker
        .instantiate_async(&mut store, &loaded.component)
        .await
        .map_err(wt)
        .with_context(|| {
            format!("failed to instantiate host component '{}'", loaded.name)
        })?;

    // Build export index cache for fast lookup
    let mut export_indices = HashMap::new();
    for iface in &loaded.exported_interfaces {
        let iface_index = instance
            .get_export_index(&mut store, None, &iface.name)
            .ok_or_else(|| {
                anyhow!(
                    "host component '{}' missing expected export '{}'",
                    loaded.name,
                    iface.name
                )
            })?;

        let mut func_indices = HashMap::new();
        for func_name in &iface.functions {
            let func_index = instance
                .get_export_index(&mut store, Some(&iface_index), func_name)
                .ok_or_else(|| {
                    anyhow!(
                        "host component '{}' interface '{}' missing function '{}'",
                        loaded.name,
                        iface.name,
                        func_name
                    )
                })?;
            func_indices.insert(func_name.clone(), func_index);
        }

        export_indices.insert(iface.name.clone(), (iface_index, func_indices));
    }

    let service = HostComponentService {
        store,
        instance,
        export_indices,
    };

    Ok(Arc::new(Mutex::new(service)))
}

/// Create a wasmtime Engine suitable for host components with async/WASI P3 support.
pub fn create_host_engine() -> Result<Engine> {
    let mut config = wasmtime::Config::new();
    config.wasm_component_model(true);
    config.wasm_component_model_async(true);
    Engine::new(&config).map_err(wt).context("failed to create host component engine")
}

/// Call the configure interface's `init` function on a host component, if it exports one.
///
/// Configuration is passed transparently as a JSON string — Spin does not
/// interpret the content. The host component decides what to do with it
/// (e.g., set cache size, configure allowed hostnames, etc.).
///
/// If `init` returns `result<_, string>`, the error string is propagated.
/// If `init` returns `()` (legacy), success is assumed.
pub async fn configure_service(
    service: &SharedService,
    config_json: &str,
    component_name: &str,
    exported_interfaces: &[ExportedInterface],
) -> Result<()> {
    // Find the configure interface among the component's exports
    let configure_iface = exported_interfaces
        .iter()
        .find(|i| is_configure_interface(&i.name));

    let configure_iface = match configure_iface {
        Some(iface) => iface,
        None => {
            if !config_json.is_empty() {
                anyhow::bail!(
                    "Configuration provided for host component '{}' but it does not \
                     export the spin:host-component/configure interface",
                    component_name
                );
            }
            return Ok(());
        }
    };

    if !configure_iface.functions.contains(&"init".to_string()) {
        anyhow::bail!(
            "Host component '{}' exports configure interface '{}' but it is \
             missing the required 'init' function",
            component_name,
            configure_iface.name,
        );
    }

    if config_json.is_empty() {
        tracing::debug!(
            "Host component '{}' exports configure interface but no config provided, skipping",
            component_name
        );
        return Ok(());
    }

    tracing::info!(
        "Configuring host component '{}' via {}/init",
        component_name,
        configure_iface.name
    );

    let mut svc = service.lock().await;
    // init may return result<_, string> or () — allocate one result slot
    let mut results = [Val::Result(Ok(None))];
    svc.call_func(
        &configure_iface.name,
        "init",
        &[Val::String(config_json.to_string())],
        &mut results,
    )
    .await
    .with_context(|| {
        format!(
            "failed to call configure init on host component '{}'",
            component_name
        )
    })?;

    // Check if init returned an error via result<_, string>
    match &results[0] {
        Val::Result(Ok(_)) => Ok(()),
        Val::Result(Err(Some(err_val))) => {
            let err_msg = match err_val.as_ref() {
                Val::String(s) => s.clone(),
                other => format!("{:?}", other),
            };
            anyhow::bail!(
                "Host component '{}' rejected configuration: {}",
                component_name,
                err_msg
            )
        }
        Val::Result(Err(None)) => {
            anyhow::bail!(
                "Host component '{}' rejected configuration (no details)",
                component_name,
            )
        }
        // Legacy: init returns () — treat as success
        _ => Ok(()),
    }
}

/// Call the configure interface's `get-key` function to discover the manifest
/// key this host component uses for its configuration tables.
///
/// Returns `None` if the component does not export a configure interface or
/// does not have a `get-key` function.
pub async fn get_manifest_key(
    service: &SharedService,
    component_name: &str,
    exported_interfaces: &[ExportedInterface],
) -> Result<Option<String>> {
    let configure_iface = exported_interfaces
        .iter()
        .find(|i| is_configure_interface(&i.name));

    let configure_iface = match configure_iface {
        Some(iface) => iface,
        None => return Ok(None),
    };

    if !configure_iface.functions.contains(&"get-key".to_string()) {
        return Ok(None);
    }

    let mut svc = service.lock().await;
    let mut results = [Val::Bool(false)]; // placeholder for string result
    svc.call_func(
        &configure_iface.name,
        "get-key",
        &[],
        &mut results,
    )
    .await
    .with_context(|| {
        format!(
            "failed to call get-key on host component '{}'",
            component_name
        )
    })?;

    match &results[0] {
        Val::String(s) => Ok(Some(s.clone())),
        other => anyhow::bail!(
            "get-key on host component '{}' returned unexpected type: {:?}",
            component_name,
            other
        ),
    }
}
