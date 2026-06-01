use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use tokio::sync::Mutex;

use super::PluginLibrary;

/// Metadata about a plugin's exported interface.
#[derive(Clone)]
pub struct ExportedInterface {
    /// Fully qualified interface name (e.g., "example:cache/store@0.1.0")
    pub name: String,
    /// Function names within this interface
    pub functions: Vec<String>,
}

/// Running plugin service with shared library reference.
pub struct PluginService {
    plugin_name: String,
    library: Arc<PluginLibrary>,
    /// Cached dispatch names: interface_name -> {func_name -> fully_qualified_name}
    dispatch_names: HashMap<String, HashMap<String, String>>,
}

impl PluginService {
    pub(crate) fn new(
        plugin_name: String,
        library: Arc<PluginLibrary>,
        exported_interfaces: &[ExportedInterface],
    ) -> Self {
        let mut dispatch_names = HashMap::new();
        for iface in exported_interfaces {
            let mut func_names = HashMap::new();
            for func in &iface.functions {
                let qualified = format!("{}#{}", iface.name, func);
                func_names.insert(func.clone(), qualified);
            }
            dispatch_names.insert(iface.name.clone(), func_names);
        }

        Self {
            plugin_name,
            library,
            dispatch_names,
        }
    }

    /// Call a function on the plugin via dispatch with postcard-serialized params.
    pub async fn call_dispatch(
        &self,
        interface_name: &str,
        func_name: &str,
        params_bytes: &[u8],
    ) -> Result<Vec<u8>> {
        let func_names = self.dispatch_names.get(interface_name).ok_or_else(|| {
            anyhow::anyhow!(
                "plugin '{}' does not export interface '{}'",
                self.plugin_name,
                interface_name
            )
        })?;

        let qualified_name = func_names.get(func_name).ok_or_else(|| {
            anyhow::anyhow!(
                "interface '{}' does not export function '{}'",
                interface_name,
                func_name
            )
        })?;

        self.library
            .call_dispatch(qualified_name, params_bytes)
            .with_context(|| {
                format!(
                    "failed to call {}/{} on plugin '{}'",
                    interface_name, func_name, self.plugin_name
                )
            })
    }
}

/// Thread-safe handle to a running plugin service.
pub type SharedPluginService = Arc<Mutex<PluginService>>;

/// Information about a loaded plugin.
pub struct LoadedPlugin {
    pub name: String,
    pub library: Arc<PluginLibrary>,
    pub exported_interfaces: Vec<ExportedInterface>,
}
