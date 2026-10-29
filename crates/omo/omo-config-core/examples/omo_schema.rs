//! Build-time generator for the published `assets/omo.schema.json`.
//!
//! Prints the native published-schema projection
//! ([`omo_config_core::omo_config_json_schema`], the native counterpart of the TypeScript
//! `script/build-omo-schema.ts`) as pretty JSON on stdout. `tools/omo-schema.mjs` runs this example
//! to regenerate the committed asset and to check it for drift; `tools/package-native.mjs` stages
//! that committed asset into the distribution, so an installed user never needs cargo.

fn main() {
    let document = omo_config_core::omo_config_json_schema();
    match serde_json::to_string_pretty(&document) {
        Ok(text) => println!("{text}"),
        Err(error) => {
            eprintln!("omo_schema: could not serialize the published config schema: {error}");
            std::process::exit(1);
        }
    }
}
