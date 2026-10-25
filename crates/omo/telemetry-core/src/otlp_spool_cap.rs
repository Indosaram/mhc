use crate::otlp_spool_dir::*;
use crate::otlp_spool_file::Signal;
use std::{
    collections::{HashMap, HashSet},
    io,
};
pub type SpoolSizeCache = HashMap<String, u64>;
fn size_key(signal: Signal, id: &str) -> String {
    format!("{signal}-{id}")
}
pub fn enforce_spool_cap(ctx: &SpoolContext, sizes: &mut SpoolSizeCache) -> io::Result<u64> {
    let mut total = 0;
    let mut sealed = Vec::new();
    let mut present = HashSet::new();
    let quarantined = list_quarantine(ctx)?;
    for file in &quarantined {
        total += file.size;
    }
    for entry in list_spool(&ctx.dir)? {
        if entry.name() == QUARANTINE_DIR {
            continue;
        }
        let key = match &entry {
            SpoolEntry::Batch {
                signal, batch_id, ..
            }
            | SpoolEntry::Sending {
                signal, batch_id, ..
            } => Some(size_key(*signal, batch_id)),
            _ => None,
        };
        let size = match key.as_ref().and_then(|k| sizes.get(k)).copied() {
            Some(s) => s,
            None => {
                let Some(stat) = file_stat(&ctx.dir, entry.name())? else {
                    continue;
                };
                if let Some(k) = &key {
                    sizes.insert(k.clone(), stat.size);
                }
                stat.size
            }
        };
        if let Some(key) = key {
            present.insert(key);
        }
        total += size;
        if let SpoolEntry::Batch { name, batch_id, .. } = entry {
            sealed.push((name, batch_id, size));
        }
    }
    sizes.retain(|k, _| present.contains(k));
    sealed.sort_by(|a, b| a.1.cmp(&b.1));
    for file in quarantined {
        if total <= ctx.cap_bytes {
            break;
        }
        if delete_file(&ctx.dir, &file.path)? {
            total = total.saturating_sub(file.size);
        }
    }
    for (name, _, size) in sealed {
        if total <= ctx.cap_bytes {
            break;
        }
        drop_file(ctx, &name, DropReason::Evicted)?;
        total = total.saturating_sub(size);
    }
    Ok(total)
}
pub struct SpoolCap {
    ctx: SpoolContext,
    sizes: SpoolSizeCache,
    bound: Option<u64>,
}
impl SpoolCap {
    pub fn new(ctx: SpoolContext) -> Self {
        Self {
            ctx,
            sizes: HashMap::new(),
            bound: None,
        }
    }
    pub fn sealed(&mut self, signal: Signal, id: &str, bytes: u64) -> io::Result<()> {
        self.sizes.insert(size_key(signal, id), bytes);
        if let Some(bound) = self.bound
            && bound.saturating_add(bytes) <= self.ctx.cap_bytes {
                self.bound = Some(bound + bytes);
                return Ok(());
            }
        self.enforce()
    }
    pub fn enforce(&mut self) -> io::Result<()> {
        self.bound = None;
        self.bound = Some(enforce_spool_cap(&self.ctx, &mut self.sizes)?);
        Ok(())
    }
}
