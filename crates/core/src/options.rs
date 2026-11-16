//! The `tensorust_api_key` setting: `SET tensorust_api_key = 'tdk_...'`.
//!
//! Empty means "no key": the functions return the free preview, unless the
//! `TENSORUST_API_KEY` environment variable is set. The setting wins over the
//! environment variable.

use duckdb_neo::{
    DuckDBType, Error, FromValue, Result, ToValue,
    connection::{Context, SettingScope},
    custom_option::{CustomOptionBuilder, CustomOptionCallbacks},
    extension::Extension,
    value::Value,
};

/// The setting's name, as used in `SET` and `current_setting`.
pub const API_KEY: &str = "tensorust_api_key";

struct ApiKeyOption;

impl CustomOptionCallbacks for ApiKeyOption {
    fn update(
        &self,
        context: &Context,
        value: Value,
        _scope: SettingScope,
        is_reset: bool,
    ) -> Result<Value> {
        if is_reset {
            return String::new().value(context);
        }
        let key = String::from_value(&value)?.unwrap_or_default();
        let key = key.trim();
        if !key.is_empty() && !key.starts_with("tdk_") {
            return Err(Error::invalid_input(
                "tensorust_api_key must be a Tensorust key starting with tdk_ (or '' for the free preview)",
            ));
        }
        key.to_string().value(context)
    }
}

/// The key for this query: the setting if set, otherwise `TENSORUST_API_KEY`.
pub fn api_key(context: &Context) -> Result<Option<String>> {
    let (value, _) = context.config()?.get_value(API_KEY)?;
    let key = String::from_value(&value)?
        .map(|k| k.trim().to_string())
        .filter(|k| !k.is_empty());
    Ok(key.or_else(crate::api::env_key))
}

pub(crate) fn register(extension: &Extension) -> Result<()> {
    CustomOptionBuilder::new(
        API_KEY,
        "Tensorust API key (tdk_...). Empty returns the free preview; an enterprise key returns full rows.",
        String::logical_type(extension)?,
        ApiKeyOption,
    )
    .default_value(String::new().value(extension)?)
    .default_scope(SettingScope::Global)
    .register(extension)
}
