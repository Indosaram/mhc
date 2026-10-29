use memory_core::facts::{FactsFailuresFile,failures_store::{FactsFailureStore,FactsFailureStoreError}};
pub trait FactsFailureReadPort {fn read_failures(&self)->Result<FactsFailuresFile,FactsFailureStoreError>;}
impl FactsFailureReadPort for FactsFailureStore {fn read_failures(&self)->Result<FactsFailuresFile,FactsFailureStoreError>{FactsFailureStore::read_failures(self)}}
pub enum FactsFailuresRead { Read{failures:FactsFailuresFile},Refused }
pub fn read_launchable_failures(port:&dyn FactsFailureReadPort,warn:impl FnOnce(&str,&FactsFailureStoreError,bool))->FactsFailuresRead {
    match port.read_failures(){Ok(failures)=>FactsFailuresRead::Read{failures},Err(error)=>{warn("facts failure ledger is unreadable; refusing to launch",&error,matches!(error,FactsFailureStoreError::Corrupt(_)));FactsFailuresRead::Refused}}
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    struct Unreadable(Cell<usize>);
    impl FactsFailureReadPort for Unreadable {fn read_failures(&self)->Result<FactsFailuresFile,FactsFailureStoreError>{self.0.set(self.0.get()+1);Err(FactsFailureStoreError::Io(std::io::Error::from(std::io::ErrorKind::PermissionDenied)))}}
    #[test]fn unreadable_is_refused_and_read_once(){let port=Unreadable(Cell::new(0));let warned=Cell::new(false);assert!(matches!(read_launchable_failures(&port,|_,_,corrupt|{assert!(!corrupt);warned.set(true);}),FactsFailuresRead::Refused));assert_eq!(port.0.get(),1);assert!(warned.get());}
}
