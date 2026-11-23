use schemars::generate::SchemaSettings;
use std::{env, fs};
use ytm_control_api::ControlSchemaRoot;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::args_os()
        .nth(1)
        .unwrap_or_else(|| "packages/protocol/schema/control.v1.schema.json".into());
    let schema = SchemaSettings::draft07()
        .into_generator()
        .into_root_schema_for::<ControlSchemaRoot>();
    fs::write(
        path,
        format!("{}\n", serde_json::to_string_pretty(&schema)?),
    )?;
    Ok(())
}
