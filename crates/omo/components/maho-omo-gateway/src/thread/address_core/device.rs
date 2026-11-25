//! Device resolution over the roster (`packages/thread-address-core/src/device.ts`).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::listing::DeviceListing;
use super::name::normalize_name;
use super::parse::{DEVICE_PREFIX_MIN, HERE};
use super::refusals::{
    AddressRefusal, DeviceRow, ShareBy, device_offline, devices_share, no_device,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceRef {
    pub id: String,
    pub name: String,
    pub online: bool,
    pub here: bool,
    pub listing: DeviceListing,
}

#[derive(Debug, Clone, Default)]
pub struct DeviceRoster {
    pub here: Option<String>,
    pub devices: Vec<DeviceRow>,
    pub listings: HashMap<String, DeviceListing>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceResolution {
    Ok(DeviceRef),
    Refusal(AddressRefusal),
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ResolveDeviceOptions {
    pub online: bool,
    pub exact_id: bool,
}

/// The one device resolver (design section 3): exact id > `here` > name (online twins beat offline)
/// > unique id prefix of 6+ chars.
pub fn resolve_device(
    roster: &DeviceRoster,
    text: &str,
    opts: ResolveDeviceOptions,
) -> DeviceResolution {
    let found = |row: &DeviceRow| -> DeviceResolution {
        if opts.online && !row.online {
            return DeviceResolution::Refusal(device_offline(text, row, std::slice::from_ref(row)));
        }
        let listing = roster
            .listings
            .get(&row.id)
            .copied()
            .unwrap_or(if row.online {
                DeviceListing::Listed
            } else {
                DeviceListing::Offline
            });
        DeviceResolution::Ok(DeviceRef {
            id: row.id.clone(),
            name: row.name.clone(),
            online: row.online,
            here: roster.here.as_deref() == Some(row.id.as_str()),
            listing,
        })
    };

    if let Some(by_id) = roster.devices.iter().find(|row| row.id == text) {
        return found(by_id);
    }
    if opts.exact_id {
        return DeviceResolution::Refusal(no_device(text, &roster.devices));
    }
    if text == HERE {
        if let Some(self_row) = roster
            .devices
            .iter()
            .find(|row| roster.here.as_deref() == Some(row.id.as_str()))
        {
            return found(self_row);
        }
        return match roster.here.as_deref() {
            None => DeviceResolution::Refusal(no_device(text, &roster.devices)),
            Some(here_id) => DeviceResolution::Ok(DeviceRef {
                id: here_id.to_owned(),
                name: HERE.to_owned(),
                online: true,
                here: true,
                listing: DeviceListing::Listed,
            }),
        };
    }

    let wanted = normalize_name(text);
    let named: Vec<DeviceRow> = roster
        .devices
        .iter()
        .filter(|row| normalize_name(&row.name) == wanted)
        .cloned()
        .collect();
    let online: Vec<DeviceRow> = named.iter().filter(|row| row.online).cloned().collect();
    let winners: &[DeviceRow] = if online.is_empty() { &named } else { &online };
    if opts.online
        && online.is_empty()
        && let Some(offline) = named.first()
    {
        return DeviceResolution::Refusal(device_offline(text, offline, &named));
    }
    if winners.len() == 1
        && let Some(only) = winners.first()
    {
        return found(only);
    }
    if winners.len() > 1 {
        return DeviceResolution::Refusal(devices_share(ShareBy::DevicesShareName, text, winners));
    }
    let prefix: Vec<DeviceRow> = if wanted.chars().count() >= DEVICE_PREFIX_MIN {
        roster
            .devices
            .iter()
            .filter(|row| row.id.to_lowercase().starts_with(&wanted))
            .cloned()
            .collect()
    } else {
        Vec::new()
    };
    if prefix.len() == 1
        && let Some(only) = prefix.first()
    {
        return found(only);
    }
    if prefix.len() > 1 {
        return DeviceResolution::Refusal(devices_share(
            ShareBy::DevicesSharePrefix,
            text,
            &prefix,
        ));
    }
    DeviceResolution::Refusal(no_device(text, &roster.devices))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_here_and_prefers_online_twin_by_name() {
        let roster = DeviceRoster {
            here: Some("dev-111111".into()),
            devices: vec![
                DeviceRow {
                    id: "dev-111111".into(),
                    name: "workstation".into(),
                    online: true,
                    last_seen_ms: None,
                },
                DeviceRow {
                    id: "dev-222222".into(),
                    name: "laptop".into(),
                    online: false,
                    last_seen_ms: Some(100),
                },
                DeviceRow {
                    id: "dev-333333".into(),
                    name: "laptop".into(),
                    online: true,
                    last_seen_ms: None,
                },
            ],
            listings: HashMap::new(),
        };
        match resolve_device(&roster, "here", ResolveDeviceOptions::default()) {
            DeviceResolution::Ok(d) => assert_eq!(d.id, "dev-111111"),
            other => panic!("expected Ok, got {other:?}"),
        }
        match resolve_device(
            &roster,
            "laptop",
            ResolveDeviceOptions {
                online: true,
                exact_id: false,
            },
        ) {
            DeviceResolution::Ok(d) => assert_eq!(d.id, "dev-333333"),
            other => panic!("expected online twin, got {other:?}"),
        }
    }

    #[test]
    fn refuses_offline_device_when_online_required() {
        let roster = DeviceRoster {
            here: Some("dev-111111".into()),
            devices: vec![DeviceRow {
                id: "dev-222222".into(),
                name: "studio".into(),
                online: false,
                last_seen_ms: Some(100),
            }],
            listings: HashMap::new(),
        };
        match resolve_device(
            &roster,
            "studio",
            ResolveDeviceOptions {
                online: true,
                exact_id: false,
            },
        ) {
            DeviceResolution::Refusal(AddressRefusal::DeviceOffline { device, .. }) => {
                assert_eq!(device, "dev-222222")
            }
            other => panic!("expected DeviceOffline, got {other:?}"),
        }
    }

    #[test]
    fn refuses_ambiguous_devices_sharing_name_or_prefix() {
        let roster = DeviceRoster {
            here: None,
            devices: vec![
                DeviceRow {
                    id: "dev-aaa111".into(),
                    name: "studio".into(),
                    online: true,
                    last_seen_ms: None,
                },
                DeviceRow {
                    id: "dev-aaa222".into(),
                    name: "studio".into(),
                    online: true,
                    last_seen_ms: None,
                },
            ],
            listings: HashMap::new(),
        };
        match resolve_device(&roster, "studio", ResolveDeviceOptions::default()) {
            DeviceResolution::Refusal(AddressRefusal::DevicesShare {
                by: ShareBy::DevicesShareName,
                candidates,
                ..
            }) => assert_eq!(candidates.len(), 2),
            other => panic!("expected DevicesShareName, got {other:?}"),
        }
        match resolve_device(&roster, "dev-aaa", ResolveDeviceOptions::default()) {
            DeviceResolution::Refusal(AddressRefusal::DevicesShare {
                by: ShareBy::DevicesSharePrefix,
                candidates,
                ..
            }) => assert_eq!(candidates.len(), 2),
            other => panic!("expected DevicesSharePrefix, got {other:?}"),
        }
    }

    #[test]
    fn resolves_by_exact_id_and_unique_prefix_and_refuses_missing_here() {
        let roster = DeviceRoster {
            here: None,
            devices: vec![DeviceRow {
                id: "dev-999111".into(),
                name: "mini".into(),
                online: true,
                last_seen_ms: None,
            }],
            listings: HashMap::new(),
        };
        match resolve_device(&roster, "dev-999111", ResolveDeviceOptions::default()) {
            DeviceResolution::Ok(d) => assert_eq!(d.name, "mini"),
            other => panic!("expected Ok, got {other:?}"),
        }
        match resolve_device(&roster, "dev-999", ResolveDeviceOptions::default()) {
            DeviceResolution::Ok(d) => assert_eq!(d.id, "dev-999111"),
            other => panic!("expected prefix Ok, got {other:?}"),
        }
        match resolve_device(&roster, "here", ResolveDeviceOptions::default()) {
            DeviceResolution::Refusal(AddressRefusal::NoDevice { .. }) => {}
            other => panic!("expected NoDevice when here absent, got {other:?}"),
        }
    }

    #[test]
    fn exact_id_option_skips_name_lookup() {
        let roster = DeviceRoster {
            here: Some("dev-111111".into()),
            devices: vec![DeviceRow {
                id: "dev-222222".into(),
                name: "mini".into(),
                online: true,
                last_seen_ms: None,
            }],
            listings: HashMap::new(),
        };
        match resolve_device(
            &roster,
            "mini",
            ResolveDeviceOptions {
                online: false,
                exact_id: true,
            },
        ) {
            DeviceResolution::Refusal(AddressRefusal::NoDevice { .. }) => {}
            other => panic!("expected exact_id NoDevice refusal, got {other:?}"),
        }
    }
}
