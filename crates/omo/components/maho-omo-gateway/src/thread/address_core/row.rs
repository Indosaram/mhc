//! Row identity, naming, and merging (`packages/thread-address-core/src/row.ts`).

use std::collections::HashSet;

use super::parse::format_address;

fn folder_name(folder: &str) -> &str {
    folder
        .split(['\\', '/'])
        .rfind(|part| !part.is_empty())
        .unwrap_or(folder)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyedRow<'a> {
    pub device: &'a str,
    pub durable_id: Option<&'a str>,
    pub session_id: &'a str,
}

/// One thread, one row: the thread id, or `<device id>/<listing key>` for a session that publishes none.
pub fn row_key(row: &KeyedRow<'_>) -> String {
    match row.durable_id {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => format_address(row.device, row.session_id),
    }
}

/// The address a row is passed back as: `<device id>/<thread id>`, or its key when it has no thread id.
pub fn row_address(row: &KeyedRow<'_>) -> String {
    match row.durable_id {
        Some(id) if !id.is_empty() => format_address(row.device, id),
        _ => row_key(row),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedRow<'a> {
    pub name: Option<&'a str>,
    pub first_message: Option<&'a str>,
    pub folder: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowName {
    pub name: String,
    pub unnamed: bool,
}

/// A session's own name, else the first user message when known, else the folder name (marked `unnamed`).
pub fn row_name(row: &NamedRow<'_>) -> RowName {
    let explicit = row.name.map(str::trim).unwrap_or("");
    if !explicit.is_empty() {
        return RowName {
            name: explicit.to_owned(),
            unnamed: false,
        };
    }
    let message = row.first_message.map(str::trim).unwrap_or("");
    let fallback = if message.is_empty() {
        folder_name(row.folder)
    } else {
        message
    };
    RowName {
        name: fallback.to_owned(),
        unnamed: true,
    }
}

/// This device's rows from its own endpoints, other devices' rows from the account listing.
pub fn merge_rows<T>(
    local: Vec<T>,
    remote: Vec<T>,
    here: Option<&str>,
    key_of: impl Fn(&T) -> &str,
    device_of: impl Fn(&T) -> &str,
) -> Vec<T> {
    let listed: HashSet<String> = local.iter().map(|r| key_of(r).to_owned()).collect();
    let mut out = local;
    for row in remote {
        if here != Some(device_of(&row)) && !listed.contains(key_of(&row)) {
            out.push(row);
        }
    }
    out
}

pub fn listing_key(device_id: &str, key: &str) -> String {
    format_address(device_id, key)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRef<'a> {
    pub device: &'a str,
    pub thread: Option<&'a str>,
    pub key: Option<&'a str>,
}

fn present(value: Option<&str>) -> Option<&str> {
    value.filter(|s| !s.is_empty())
}

pub fn same_session(a: &SessionRef<'_>, b: &SessionRef<'_>) -> bool {
    if a.device != b.device {
        return false;
    }
    if let (Some(ta), Some(tb)) = (present(a.thread), present(b.thread)) {
        return ta == tb;
    }
    matches!((present(a.key), present(b.key)), (Some(ka), Some(kb)) if ka == kb)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_key_prefers_durable_id_and_falls_back_to_device_session() {
        let with_durable = KeyedRow {
            device: "dev-1",
            durable_id: Some("dur-9"),
            session_id: "sess-9",
        };
        assert_eq!(row_key(&with_durable), "dur-9");
        assert_eq!(row_address(&with_durable), "dev-1/dur-9");

        let without_durable = KeyedRow {
            device: "dev-1",
            durable_id: None,
            session_id: "sess-9",
        };
        assert_eq!(row_key(&without_durable), "dev-1/sess-9");
        assert_eq!(row_address(&without_durable), "dev-1/sess-9");
    }

    #[test]
    fn row_name_uses_explicit_else_first_message_else_folder() {
        assert_eq!(
            row_name(&NamedRow {
                name: Some("  Build  "),
                first_message: Some("msg"),
                folder: "/work/repo"
            }),
            RowName {
                name: "Build".into(),
                unnamed: false
            }
        );
        assert_eq!(
            row_name(&NamedRow {
                name: None,
                first_message: Some("Hello world"),
                folder: "/work/repo"
            }),
            RowName {
                name: "Hello world".into(),
                unnamed: true
            }
        );
        assert_eq!(
            row_name(&NamedRow {
                name: Some("  "),
                first_message: None,
                folder: "/work/repo"
            }),
            RowName {
                name: "repo".into(),
                unnamed: true
            }
        );
    }

    #[test]
    fn merge_rows_keeps_local_over_remote_and_drops_here_duplicates() {
        #[derive(Debug, PartialEq)]
        struct R(&'static str, &'static str);
        let merged = merge_rows(
            vec![R("dur-1", "dev-local")],
            vec![
                R("dur-1", "dev-remote"),
                R("dur-local-dup", "dev-local"),
                R("dur-2", "dev-remote"),
            ],
            Some("dev-local"),
            |r| r.0,
            |r| r.1,
        );
        assert_eq!(
            merged,
            vec![R("dur-1", "dev-local"), R("dur-2", "dev-remote")]
        );
    }

    #[test]
    fn same_session_compares_device_and_thread_or_key() {
        assert!(same_session(
            &SessionRef {
                device: "d1",
                thread: Some("t1"),
                key: None
            },
            &SessionRef {
                device: "d1",
                thread: Some("t1"),
                key: Some("k2")
            }
        ));
        assert!(!same_session(
            &SessionRef {
                device: "d1",
                thread: Some("t1"),
                key: None
            },
            &SessionRef {
                device: "d2",
                thread: Some("t1"),
                key: None
            }
        ));
        assert!(same_session(
            &SessionRef {
                device: "d1",
                thread: None,
                key: Some("k1")
            },
            &SessionRef {
                device: "d1",
                thread: None,
                key: Some("k1")
            }
        ));
    }

    #[test]
    fn listing_key_formats_device_and_key() {
        assert_eq!(listing_key("dev-a", "sess-1"), "dev-a/sess-1");
    }

    #[test]
    fn empty_durable_id_falls_back_to_session_key() {
        let empty = KeyedRow {
            device: "dev-x",
            durable_id: Some(""),
            session_id: "sess-x",
        };
        assert_eq!(row_key(&empty), "dev-x/sess-x");
    }
}
