//! Port of senpi `cli/models-command.ts`.
use super::model_discovery::DiscoveryReport;
use std::path::Path;

pub fn is_models_discover_command(args: &[String]) -> bool {
    args.first().is_some_and(|s| s == "models") && args.get(1).is_some_and(|s| s == "discover")
}

fn format_report(report: &DiscoveryReport, path: &Path) -> String {
    let count = report.added.len() + report.updated.len() + report.unchanged.len();
    let mut lines = vec![format!(
        "Discovered {count} models for provider \"{}\" from {}",
        report.provider, report.url
    )];
    for (label, ids) in [
        ("added", &report.added),
        ("updated", &report.updated),
        ("unchanged", &report.unchanged),
    ] {
        for id in ids {
            let description = match report.efforts.iter().find(|(model, ..)| model == id) {
                None => id.clone(),
                Some((_, levels, _, _)) if levels.is_empty() => {
                    format!("{id}  reasoning: off (no usable effort advertised)")
                }
                Some((_, levels, default, _)) => format!(
                    "{id}  reasoning: {}{}",
                    levels.join(", "),
                    default
                        .as_ref()
                        .map(|level| format!(" (default {level})"))
                        .unwrap_or_default()
                ),
            };
            lines.push(format!("  {label:9} {description}"));
        }
    }
    if !report.unlisted.is_empty() {
        lines.push(format!(
            "Kept models the endpoint did not list: {}",
            report.unlisted.join(", ")
        ));
    }
    for (id, _, _, unmapped) in &report.efforts {
        if !unmapped.is_empty() {
            lines.push(format!(
                "Unrecognized reasoning efforts for {id} (not used): {}",
                unmapped.join(", ")
            ));
        }
    }
    if report.efforts_ignored {
        lines.push(format!("Advertised reasoning efforts were ignored: set \"compat\": {{ \"supportsReasoningEffort\": true }} on provider \"{}\" to use them.", report.provider));
    }
    if let Some(backup) = &report.backup {
        lines.push(format!(
            "Updated {} (backup: {}).",
            path.display(),
            backup.display()
        ));
        if report.comments_dropped {
            lines.push("Comments in models.json were not preserved; the backup keeps them.".into());
        }
    } else {
        lines.push(format!(
            "{} already matches the endpoint; nothing written.",
            path.display()
        ));
    }
    format!("{}\n", lines.join("\n"))
}

pub async fn run_models_discover_command(args: &[String]) -> i32 {
    let usage = format!(
        "{} models discover <provider>",
        maho_core::config::app_command()
    );
    if args.iter().any(|s| matches!(s.as_str(), "--help" | "-h")) {
        println!(
            "Usage:\n  {usage}\n\nFetches <baseUrl>/models once for a compatible provider in models.json, merges listed models, and preserves the original as a backup. Set compat.supportsReasoningEffort to map advertised reasoning efforts."
        );
        return 0;
    }
    let [provider] = args else {
        eprintln!("Usage: {usage}");
        return 1;
    };
    if provider.is_empty() || provider.starts_with('-') {
        eprintln!("Usage: {usage}");
        return 1;
    }
    let path = std::path::PathBuf::from(maho_core::config::get_agent_dir()).join("models.json");
    match tokio::time::timeout(
        std::time::Duration::from_secs(30),
        super::model_discovery::discover_provider_models(&path, provider),
    )
    .await
    {
        Ok(Ok(report)) => {
            print!("{}", format_report(&report, &path));
            0
        }
        Ok(Err(error)) => {
            eprintln!("Error: {error}");
            1
        }
        Err(_) => {
            eprintln!("Error: Model discovery timed out.");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn only_discover_pair_is_a_command() {
        // Given ordinary prompts and the discovery command.
        let cases = [
            (vec!["models", "discover", "local"], true),
            (vec!["models", "explain"], false),
            (vec!["models"], false),
        ];
        // When classifying each argument vector.
        for (args, expected) in cases {
            let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
            let actual = super::is_models_discover_command(&args);
            // Then only the command pair is intercepted.
            assert_eq!(actual, expected);
        }
    }
}
