pub mod sql;
pub mod values;
pub mod session_row;
pub mod entries;
pub use sql::{SqlQuery, SqlValue, join_sql_fragments};
pub fn apply_initial_schema(db: &rusqlite::Connection) -> rusqlite::Result<()> {
    db.execute_batch(include_str!("001_initial.sql"))
}
pub fn transaction<T>(db:&mut rusqlite::Connection,callback:impl FnOnce(&rusqlite::Transaction<'_>)->rusqlite::Result<T>)->rusqlite::Result<T> {
    let transaction=db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let result=callback(&transaction)?;
    transaction.commit()?;
    Ok(result)
}

pub fn open(path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open(path)
}
pub fn open_existing(path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
}
pub fn open_read_only(path: &std::path::Path) -> rusqlite::Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
}
