//! Bounded external-path projection with recency ordering and byte-budget fitting.
use std::collections::BTreeMap;
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ExternalProjectionLimits {
    pub max_entries_per_directory: usize,
    pub max_bytes: usize,
}
impl Default for ExternalProjectionLimits {
    fn default() -> Self {
        Self {
            max_entries_per_directory: 40,
            max_bytes: 24576,
        }
    }
}
#[derive(Debug, Default)]
pub struct ExternalProjectionInput {
    pub times: BTreeMap<String, i64>,
    pub limits: ExternalProjectionLimits,
    pub recency_unavailable: bool,
    pub recency_error: Option<String>,
}
#[derive(Debug, Clone)]
pub struct ExternalProjectionStats {
    pub shown: usize,
    pub omitted: usize,
    pub bytes: usize,
    pub max_bytes: usize,
    pub overflow: bool,
    pub recency_unavailable: bool,
}
struct Directory {
    label: String,
    pointer: String,
    names: Vec<String>,
}
fn line(d: &Directory, n: usize) -> String {
    let shown = d.names[..n].join(", ");
    let omitted = d.names.len() - n;
    if omitted == 0 {
        format!("{}: {shown}", d.label)
    } else {
        format!(
            "{}: {shown}{}(+{omitted} more; read {} to list)",
            d.label,
            if n > 0 { " " } else { "" },
            d.pointer
        )
    }
}
fn render(dirs: &[Directory], counts: &[usize], bare: bool) -> String {
    let mut lines = vec!["<external_projection>".to_owned()];
    if bare {
        lines.push("$MEMORY_DIR/".into());
    }
    for (d, n) in dirs.iter().zip(counts) {
        lines.push(line(d, *n));
    }
    lines.push("</external_projection>".into());
    lines.join("\n")
}
fn layout(paths: &[String], input: &ExternalProjectionInput) -> (Vec<Directory>, Vec<usize>, bool) {
    let mut grouped: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for path in paths {
        let (dir, name) = path.rsplit_once('/').unwrap_or(("", path));
        if !name.is_empty() {
            grouped
                .entry(dir.to_owned())
                .or_default()
                .push((name.to_owned(), path.clone()));
        }
    }
    let bare = !grouped.contains_key("");
    let bounded = input.limits.max_entries_per_directory > 0 || input.limits.max_bytes > 0;
    let dirs: Vec<Directory> = grouped
        .into_iter()
        .map(|(dir, mut names)| {
            names.sort_by(|a, b| {
                if bounded {
                    let order = input.times.get(&b.1).cmp(&input.times.get(&a.1));
                    if !order.is_eq() {
                        return order;
                    }
                }
                a.0.cmp(&b.0)
            });
            let parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
            let safe = parts
                .iter()
                .map(|p| crate::sync::redact::redact_secret_like_material(p))
                .collect::<Vec<_>>()
                .join("/");
            let readable = parts
                .iter()
                .take_while(|p| crate::sync::redact::redact_secret_like_material(p) == **p)
                .copied()
                .collect::<Vec<_>>()
                .join("/");
            Directory {
                label: if dir.is_empty() {
                    "$MEMORY_DIR/".into()
                } else {
                    format!("{safe}/")
                },
                pointer: if readable.is_empty() {
                    "$MEMORY_DIR/".into()
                } else {
                    format!("$MEMORY_DIR/{readable}/")
                },
                names: names
                    .into_iter()
                    .map(|(n, _)| crate::sync::redact::redact_secret_like_material(&n))
                    .collect(),
            }
        })
        .collect();
    let mut counts: Vec<usize> = dirs
        .iter()
        .map(|d| {
            if input.limits.max_entries_per_directory == 0 {
                d.names.len()
            } else {
                d.names.len().min(input.limits.max_entries_per_directory)
            }
        })
        .collect();
    let start = counts.clone();
    if input.limits.max_bytes > 0 {
        while render(&dirs, &counts, bare).len() > input.limits.max_bytes {
            let mut widest = None;
            for (i, n) in counts.iter().enumerate() {
                if *n > widest.map_or(0, |j: usize| counts[j]) {
                    widest = Some(i);
                }
            }
            match widest {
                Some(i) => counts[i] -= 1,
                None => break,
            }
        }
        if render(&dirs, &counts, bare).len() > input.limits.max_bytes {
            for (i, d) in dirs.iter().enumerate() {
                let mut best = start[i];
                for n in (0..start[i]).rev() {
                    if line(d, n).len() < line(d, best).len() {
                        best = n;
                    }
                }
                counts[i] = best;
            }
        }
    }
    (dirs, counts, bare)
}
pub fn render_external_projection(paths: &[String], input: &ExternalProjectionInput) -> String {
    let (d, n, b) = layout(paths, input);
    render(&d, &n, b)
}
pub fn render_external_projection_stats(
    paths: &[String],
    input: &ExternalProjectionInput,
) -> ExternalProjectionStats {
    let (d, n, b) = layout(paths, input);
    let bytes = render(&d, &n, b).len();
    let shown = n.iter().sum();
    ExternalProjectionStats {
        shown,
        omitted: d.iter().map(|d| d.names.len()).sum::<usize>() - shown,
        bytes,
        max_bytes: input.limits.max_bytes,
        overflow: input.limits.max_bytes > 0 && bytes > input.limits.max_bytes,
        recency_unavailable: input.recency_unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recency_cap_and_byte_budget_are_machine_measurable() {
        let paths = vec![
            "notes/a.md".into(),
            "notes/b.md".into(),
            "notes/c.md".into(),
        ];
        let input = ExternalProjectionInput {
            times: BTreeMap::from([("notes/c.md".into(), 10)]),
            limits: ExternalProjectionLimits {
                max_entries_per_directory: 1,
                max_bytes: 0,
            },
            ..Default::default()
        };
        let stats = render_external_projection_stats(&paths, &input);
        assert_eq!(stats.shown, 1);
        assert_eq!(stats.omitted, 2);
        let block = render_external_projection(&paths, &input);
        assert!(block.contains("c.md"));
        assert!(!block.contains("a.md"));
    }
}
