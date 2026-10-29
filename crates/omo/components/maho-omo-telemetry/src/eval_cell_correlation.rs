use std::collections::{HashMap,HashSet};
#[derive(Default)]
pub struct EvalCellCorrelation { owners_by_cell:HashMap<String,HashSet<String>>,cells_by_session:HashMap<String,HashSet<String>>,ambiguous_cells:HashSet<String> }
impl EvalCellCorrelation {
    pub fn track(&mut self,session:&str,cell:&str) { let owners=self.owners_by_cell.entry(cell.into()).or_default(); owners.insert(session.into()); if owners.len()>1 { self.ambiguous_cells.insert(cell.into()); } self.cells_by_session.entry(session.into()).or_default().insert(cell.into()); }
    pub fn clear_session(&mut self,session:&str) { if let Some(cells)=self.cells_by_session.remove(session) { for cell in cells { if let Some(owners)=self.owners_by_cell.get_mut(&cell) { owners.remove(session); if owners.is_empty() { self.owners_by_cell.remove(&cell); self.ambiguous_cells.remove(&cell); } } } } }
    pub fn consume(&mut self,cell:&str) -> Option<String> { let owners=self.owners_by_cell.remove(cell)?; let session=if owners.len()==1 && !self.ambiguous_cells.contains(cell) { owners.iter().next().cloned() } else {None}; for owner in owners { if let Some(cells)=self.cells_by_session.get_mut(&owner) { cells.remove(cell); if cells.is_empty() { self.cells_by_session.remove(&owner); } } } self.ambiguous_cells.remove(cell); session }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn consumes_once() { let mut c=EvalCellCorrelation::default(); c.track("a","cell"); assert_eq!(c.consume("cell").as_deref(),Some("a")); assert!(c.consume("cell").is_none()); }
    #[test] fn ambiguity_survives_owner_clear() { let mut c=EvalCellCorrelation::default(); c.track("a","cell"); c.track("b","cell"); c.clear_session("a"); assert!(c.consume("cell").is_none()); }
    #[test] fn clear_allows_reuse() { let mut c=EvalCellCorrelation::default(); c.track("a","cell"); c.clear_session("a"); c.track("b","cell"); assert_eq!(c.consume("cell").as_deref(),Some("b")); }
}
