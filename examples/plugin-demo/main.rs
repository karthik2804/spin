//! Plugin Demo
//!
//! Demonstrates loading a Spin plugin shared library at runtime
//! and using it as a Factor.

use std::path::PathBuf;

use spin_plugin::{PluginFactor, PluginLoader};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let plugin_path = std::env::args().nth(1).unwrap_or_else(|| {
        // Default: look for the built plugin in target/release
        let mut path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        path.push("../target/release");
        #[cfg(target_os = "macos")]
        {
            path.push("libspin_plugin_cache.dylib");
        }
        #[cfg(target_os = "linux")]
        {
            path.push("libspin_plugin_cache.so");
        }
        #[cfg(target_os = "windows")]
        {
            path.push("spin_plugin_cache.dll");
        }
        path.display().to_string()
    });

    println!("Loading plugin from: {}", plugin_path);

    let loader = PluginLoader::new();
    let plugin = loader.load(&plugin_path)?;

    println!("Loaded plugin: {} v{}", plugin.name(), plugin.version());

    let plugin = std::sync::Arc::new(plugin);
    let factor = PluginFactor::new(plugin);

    println!(
        "Created PluginFactor: name={}, version={}",
        factor.name(),
        factor.version()
    );

    println!(
        "\nPlugin system is ready. The factor can now be used with Spin's RuntimeFactors system."
    );
    println!("\nTo use this factor in a Spin app:");
    println!("1. Add the PluginFactor to your RuntimeFactors struct");
    println!("2. Call factors.init(), factors.configure_app(), factors.prepare() as usual");
    println!("3. The plugin's C ABI will be invoked transparently");

    Ok(())
}
