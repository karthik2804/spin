use anyhow::Result;
use spin_app::{AppComponent, DynamicHostComponent};
use spin_core::{async_trait, HostComponent};
use spin_world::v2::app_config;

use crate::Error;

pub struct AppConfigHostComponent {}

impl AppConfigHostComponent {
    pub fn new() -> Self {
        Self {}
    }
}

impl HostComponent for AppConfigHostComponent {
    type Data = ComponentAppConfig;

    fn add_to_linker<T: Send>(
        linker: &mut spin_core::Linker<T>,
        get: impl Fn(&mut spin_core::Data<T>) -> &mut Self::Data + Send + Sync + Copy + 'static,
    ) -> anyhow::Result<()> {
        spin_world::v2::app_config::add_to_linker(linker, get)?;
        app_config::add_to_linker(linker, get)
    }

    fn build_data(&self) -> Self::Data {
        ComponentAppConfig {
            spin_toml: String::new(),
        }
    }
}

impl DynamicHostComponent for AppConfigHostComponent {
    fn update_data(&self, data: &mut Self::Data, component: &AppComponent) -> anyhow::Result<()> {
        data.spin_toml = component.id().into();
        Ok(())
    }
}

/// TODO
pub struct ComponentAppConfig {
    spin_toml: String,
}

#[async_trait]
impl app_config::Host for ComponentAppConfig {
    async fn get(&mut self) -> Result<Result<String, app_config::Error>> {
        Ok(Ok(String::from("we did it")))
    }
}

impl From<Error> for app_config::Error {
    fn from(err: Error) -> Self {
        match err {
            Error::InvalidName(msg) => Self::InvalidName(msg),
            Error::Undefined(msg) => Self::Undefined(msg),
            Error::Provider(err) => Self::Provider(err.to_string()),
            other => Self::Other(format!("{other}")),
        }
    }
}
