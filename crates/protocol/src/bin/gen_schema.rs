use schemars::generate::SchemaSettings;
use std::env;
use std::fs;
use std::path::Path;
use ytm_protocol::ProtocolSchemaRoot;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_path = env::args()
        .nth(1)
        .unwrap_or_else(|| "packages/protocol/schema/protocol.v1.schema.json".to_string());
    let path = Path::new(&out_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let generator = SchemaSettings::draft07().into_generator();
    let schema = generator.into_root_schema_for::<ProtocolSchemaRoot>();
    let json = serde_json::to_string_pretty(&schema)?;
    fs::write(path, format!("{json}\n"))?;
    eprintln!("wrote schema to {}", path.display());
    Ok(())
}
