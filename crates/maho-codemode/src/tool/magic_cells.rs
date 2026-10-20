use crate::environments::{js_environments::EnvironmentMode, js_installer::JsInstallerChoice};
pub enum MagicCell {
    Environment(EnvironmentMode),
    JsAdd {
        installer: JsInstallerChoice,
        args: String,
    },
    Load(String),
}
pub fn parse_magic_cell(code: &str) -> Result<Option<MagicCell>, String> {
    let lines = code
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("//"))
        .collect::<Vec<_>>();
    let first = lines.first().copied().unwrap_or("");
    let Some(magic) = first
        .strip_prefix('%')
        .and_then(|line| line.split_whitespace().next())
        .filter(|name| matches!(*name, "bun" | "npm" | "environment" | "load"))
    else {
        return Ok(None);
    };
    if lines.len() > 1 {
        return Err(format!(
            "put %{magic} on its own cell, then run the code that uses it in the next cell"
        ));
    }
    let args = first[magic.len() + 1..].trim();
    match magic {
        "bun" | "npm" => {
            let mut parts = args.split_whitespace();
            if !matches!(parts.next(), Some("add" | "install")) {
                return Err(format!(
                    "%{magic} supports only add: %{magic} add <package ...>"
                ));
            }
            Ok(Some(MagicCell::JsAdd {
                installer: if magic == "bun" {
                    JsInstallerChoice::Bun
                } else {
                    JsInstallerChoice::Npm
                },
                args: parts.collect::<Vec<_>>().join(" "),
            }))
        }
        "environment" => match args {
            "managed" => Ok(Some(MagicCell::Environment(EnvironmentMode::Managed))),
            "project" => Ok(Some(MagicCell::Environment(EnvironmentMode::Project))),
            _ => Err("%environment takes one argument: managed or project".into()),
        },
        "load" if args.is_empty() => {
            Err("%load takes one argument: the path of the file to run".into())
        }
        "load" => Ok(Some(MagicCell::Load(args.into()))),
        _ => unreachable!("recognized magic"),
    }
}
