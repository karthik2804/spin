pub mod service;
#[cfg(test)]
mod tests;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use spin_factors::{Factor, PrepareContext, RuntimeFactors};
use wasmtime::component::Val;

use service::{
    configure_service, create_host_engine, get_manifest_key, instantiate_host_component,
    is_configure_interface, load_host_component, ExportedInterface, LoadedHostComponent,
    SharedService,
};

/// Metadata key for per-component host_component access config.
/// Stored as a JSON map of { key: table } in the locked app metadata.
const HOST_COMPONENT_METADATA_KEY: spin_app::MetadataKey<
    HashMap<String, serde_json::Value>,
> = spin_app::MetadataKey::new("host_component");

/// Metadata key for app-level host_component runtime config.
/// Stored as a JSON map of { key: table } in the locked app metadata.
const APP_HOST_COMPONENT_METADATA_KEY: spin_app::MetadataKey<
    HashMap<String, serde_json::Value>,
> = spin_app::MetadataKey::new("host_component");

/// Convert a wasmtime::Error into anyhow::Error
fn wt(e: wasmtime::Error) -> anyhow::Error {
    anyhow::Error::msg(e.to_string())
}

/// A lazy reference to a service that doesn't exist yet at linker setup time.
/// Populated later when the host component is actually instantiated.
type LazyService = Arc<OnceLock<SharedService>>;

/// A Factor that loads user-provided Wasm components at runtime and wires their
/// exports as imports available to guest components.
///
/// This enables arbitrary host interfaces at `spin up` time - different users
/// can provide different host components without baking interfaces into Spin.
///
/// Host component instances are long-lived and shared across all guest
/// invocations, allowing them to maintain state (e.g., an in-memory cache).
pub struct HostComponentsFactor {
    /// Map of component name -> path to Wasm file
    component_paths: Vec<(String, PathBuf)>,
    /// Map of component name -> JSON config string (from CLI --host-component-config)
    component_configs: HashMap<String, String>,
    /// Optional data directory for host component filesystem access
    data_dir: Option<PathBuf>,
    /// Loaded components (populated in init)
    loaded: Vec<LoadedHostComponent>,
    /// The dedicated engine for host components
    host_engine: wasmtime::Engine,
    /// Lazy service references: interface_name -> LazyService
    /// Created during init(), populated during configure_app()
    service_slots: HashMap<String, LazyService>,
}

impl HostComponentsFactor {
    /// Create a new HostComponentsFactor.
    ///
    /// `components` is a list of (name, path) pairs for host component Wasm files.
    /// `configs` is a map of component name -> JSON configuration string (from CLI).
    /// `data_dir` is an optional directory for host component filesystem persistence.
    pub fn new(
        components: Vec<(String, PathBuf)>,
        configs: HashMap<String, String>,
        data_dir: Option<PathBuf>,
    ) -> Result<Self> {
        let host_engine = create_host_engine()?;
        Ok(Self {
            component_paths: components,
            component_configs: configs,
            data_dir,
            loaded: Vec::new(),
            host_engine,
            service_slots: HashMap::new(),
        })
    }

    /// Create an empty HostComponentsFactor (no host components).
    pub fn empty() -> Result<Self> {
        Self::new(Vec::new(), HashMap::new(), None)
    }
}

impl Factor for HostComponentsFactor {
    type RuntimeConfig = ();
    type AppState = HostComponentsAppState;
    type InstanceBuilder = HostComponentsInstanceBuilder;

    fn init(&mut self, ctx: &mut impl spin_factors::InitContext<Self>) -> Result<()> {
        if self.component_paths.is_empty() {
            return Ok(());
        }

        // Load each host component and inspect its exports
        for (name, path) in &self.component_paths {
            let loaded = load_host_component(&self.host_engine, name, path)?;

            // Register forwarding functions in the guest's linker for each exported interface
            // (skip configure interfaces — those are for operator config, not guest use)
            for iface in &loaded.exported_interfaces {
                if is_configure_interface(&iface.name) {
                    continue;
                }

                let lazy_service: LazyService = Arc::new(OnceLock::new());
                self.service_slots
                    .insert(iface.name.clone(), lazy_service.clone());

                register_forwarding_functions(
                    ctx.linker(),
                    name,
                    iface,
                    lazy_service,
                )?;
            }

            self.loaded.push(loaded);
        }

        Ok(())
    }

    fn configure_app<T: RuntimeFactors>(
        &self,
        ctx: spin_factors::ConfigureAppContext<T, Self>,
    ) -> Result<Self::AppState> {
        let mut services = HashMap::new();
        // Maps manifest_key -> set of interface names for this host component
        let mut key_to_interfaces: HashMap<String, Vec<String>> = HashMap::new();
        // Maps manifest_key -> component_name for error messages
        let mut key_to_component: HashMap<String, String> = HashMap::new();

        // Read app-level host_component config from manifest metadata
        let app_host_configs: HashMap<String, serde_json::Value> = ctx
            .app()
            .get_metadata(APP_HOST_COMPONENT_METADATA_KEY)
            .unwrap_or(None)
            .unwrap_or_default();

        // Instantiate each loaded host component into its own Store
        for loaded in &self.loaded {
            let service = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(instantiate_host_component(
                    &self.host_engine,
                    loaded,
                    self.data_dir.as_deref(),
                ))
            })
            .with_context(|| {
                format!("failed to instantiate host component '{}'", loaded.name)
            })?;

            // Query the host component for its manifest key via get-key
            let manifest_key = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(get_manifest_key(
                    &service,
                    &loaded.name,
                    &loaded.exported_interfaces,
                ))
            })
            .with_context(|| {
                format!(
                    "failed to get manifest key from host component '{}'",
                    loaded.name
                )
            })?;

            // Build the config JSON for this host component:
            // 1. Start with manifest [host_component.<key>] table (if any)
            // 2. Override/merge with CLI --host-component-config (if any)
            let config_json = if let Some(ref key) = manifest_key {
                let manifest_config = app_host_configs.get(key);
                let cli_config = self.component_configs.get(&loaded.name);

                match (manifest_config, cli_config) {
                    (None, None) => String::new(),
                    (Some(manifest_val), None) => serde_json::to_string(manifest_val)
                        .context("failed to serialize manifest host_component config")?,
                    (None, Some(cli_val)) => cli_val.clone(),
                    (Some(manifest_val), Some(cli_val)) => {
                        // CLI overrides manifest: merge the two JSON objects
                        let mut base: serde_json::Value = manifest_val.clone();
                        let cli_parsed: serde_json::Value = serde_json::from_str(cli_val)
                            .with_context(|| {
                                format!(
                                    "failed to parse CLI config for host component '{}'",
                                    loaded.name
                                )
                            })?;
                        if let (Some(base_obj), Some(cli_obj)) =
                            (base.as_object_mut(), cli_parsed.as_object())
                        {
                            for (k, v) in cli_obj {
                                base_obj.insert(k.clone(), v.clone());
                            }
                        }
                        serde_json::to_string(&base)
                            .context("failed to serialize merged host_component config")?
                    }
                }
            } else {
                // No manifest key — fall back to CLI-only config
                self.component_configs
                    .get(&loaded.name)
                    .cloned()
                    .unwrap_or_default()
            };

            // Call the configure interface's init function
            tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(configure_service(
                    &service,
                    &config_json,
                    &loaded.name,
                    &loaded.exported_interfaces,
                ))
            })
            .with_context(|| {
                format!("failed to configure host component '{}'", loaded.name)
            })?;

            // Populate the lazy service slots and build the services map
            // (skip configure interfaces — not forwarded to guests)
            let mut iface_names = Vec::new();
            for iface in &loaded.exported_interfaces {
                if is_configure_interface(&iface.name) {
                    continue;
                }

                if services.contains_key(&iface.name) {
                    anyhow::bail!(
                        "Multiple host components export interface '{}'. \
                         Each interface must be provided by exactly one host component.",
                        iface.name
                    );
                }

                // Populate the OnceLock so forwarding functions can find the service
                let slot = self.service_slots.get(&iface.name).unwrap();
                slot.set(service.clone()).map_err(|_| {
                    anyhow::anyhow!(
                        "service slot for '{}' was already populated (bug)",
                        iface.name
                    )
                })?;

                tracing::info!(
                    "Host component '{}' providing interface '{}' with functions: [{}]",
                    loaded.name,
                    iface.name,
                    iface.functions.join(", ")
                );

                iface_names.push(iface.name.clone());
                services.insert(iface.name.clone(), service.clone());
            }

            // Track which manifest key maps to which interfaces
            if let Some(ref key) = manifest_key {
                key_to_interfaces.insert(key.clone(), iface_names);
                key_to_component.insert(key.clone(), loaded.name.clone());
            }
        }

        if !services.is_empty() {
            tracing::info!(
                "Loaded {} host component interface(s)",
                services.len()
            );
        }

        // Build per-component allowed interfaces set from manifest metadata.
        // A component can access a host component's interfaces if it has
        // [component.X.host_component.<key>] in the manifest.
        let mut component_allowed_interfaces: HashMap<String, HashSet<String>> = HashMap::new();
        for component in ctx.app().components() {
            let component_id = component.id().to_string();
            let host_component_config: HashMap<String, serde_json::Value> = component
                .get_metadata(HOST_COMPONENT_METADATA_KEY)
                .unwrap_or(None)
                .unwrap_or_default();

            let mut allowed = HashSet::new();
            for key in host_component_config.keys() {
                if let Some(iface_names) = key_to_interfaces.get(key) {
                    for iface_name in iface_names {
                        allowed.insert(iface_name.clone());
                    }
                    tracing::info!(
                        "Component '{}' granted access to host component '{}' (key '{}')",
                        component_id,
                        key_to_component.get(key).unwrap_or(key),
                        key
                    );
                } else if !key_to_interfaces.is_empty() {
                    tracing::warn!(
                        "Component '{}' references host_component key '{}' but no host \
                         component declared that key",
                        component_id,
                        key
                    );
                }
            }

            // If no host components are loaded, or the component has at least one
            // host_component entry, store the allowed set.
            // If NO host components are loaded at all, all components get unrestricted access
            // (backward compatibility for apps that don't use per-component config).
            if key_to_interfaces.is_empty() {
                // No host components have manifest keys — allow everything
                allowed = services.keys().cloned().collect();
            }

            component_allowed_interfaces.insert(component_id, allowed);
        }

        Ok(HostComponentsAppState {
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
        Ok(HostComponentsInstanceBuilder {
            services: app_state.services.clone(),
            allowed_interfaces,
        })
    }
}

/// Shared application state holding all running host component services.
pub struct HostComponentsAppState {
    services: Arc<HashMap<String, SharedService>>,
    /// Per-component allowed interface names.
    component_allowed_interfaces: Arc<HashMap<String, HashSet<String>>>,
}

impl HostComponentsAppState {
    /// Get a shared reference to a host component service by interface name.
    pub fn get_service(&self, interface_name: &str) -> Option<&SharedService> {
        self.services.get(interface_name)
    }
}

/// Per-instance builder for the host components factor.
pub struct HostComponentsInstanceBuilder {
    services: Arc<HashMap<String, SharedService>>,
    allowed_interfaces: HashSet<String>,
}

impl spin_factors::FactorInstanceBuilder for HostComponentsInstanceBuilder {
    type InstanceState = HostComponentsInstanceState;

    fn build(self) -> Result<Self::InstanceState> {
        Ok(HostComponentsInstanceState {
            _services: self.services,
            _allowed_interfaces: self.allowed_interfaces,
        })
    }
}

/// Per-instance state - holds a reference to the shared services and access control.
pub struct HostComponentsInstanceState {
    _services: Arc<HashMap<String, SharedService>>,
    _allowed_interfaces: HashSet<String>,
}

/// Register forwarding functions in the guest's Linker for a host component's interface.
///
/// For each function exported by the interface, we register a `func_new_async`
/// that (when called by a guest) acquires the host component's mutex and
/// forwards the call synchronously.
///
/// The `lazy_service` is an `Arc<OnceLock<SharedService>>` that will be populated
/// after the host component is actually instantiated (in `configure_app`).
fn register_forwarding_functions<T: Send + 'static>(
    linker: &mut wasmtime::component::Linker<T>,
    component_name: &str,
    iface: &ExportedInterface,
    lazy_service: LazyService,
) -> Result<()> {
    let mut linker_instance = linker
        .instance(&iface.name)
        .map_err(wt)
        .with_context(|| {
            format!(
                "failed to create linker instance for interface '{}' from host component '{}'",
                iface.name, component_name
            )
        })?;

    for func_name in &iface.functions {
        let iface_name = iface.name.clone();
        let func_name_owned = func_name.clone();
        let component_name_for_closure = component_name.to_string();
        let service_ref = lazy_service.clone();

        linker_instance.func_new_async(
            func_name,
            move |_store_ctx, _func_type, params, results| {
                let iface_name = iface_name.clone();
                let func_name = func_name_owned.clone();
                let component_name = component_name_for_closure.clone();
                let service_ref = service_ref.clone();
                let owned_params: Vec<Val> = params.to_vec();
                let num_results = results.len();
                Box::new(async move {
                    let service = service_ref.get().ok_or_else(|| {
                        wasmtime::Error::msg(format!(
                            "Host component '{}' not yet initialized (interface '{}')",
                            component_name, iface_name
                        ))
                    })?.clone();

                    let mut svc = service.lock().await;
                    let mut owned_results = vec![Val::Bool(false); num_results];
                    svc.call_func(
                        &iface_name,
                        &func_name,
                        &owned_params,
                        &mut owned_results,
                    )
                    .await
                    .map_err(|e| wasmtime::Error::msg(e.to_string()))?;

                    for (dst, src) in results.iter_mut().zip(owned_results) {
                        *dst = src;
                    }
                    Ok(())
                })
            },
        ).map_err(wt).with_context(|| {
            format!(
                "failed to register forwarding function '{}/{}' for host component '{}'",
                iface.name, func_name, component_name
            )
        })?;
    }

    Ok(())
}
