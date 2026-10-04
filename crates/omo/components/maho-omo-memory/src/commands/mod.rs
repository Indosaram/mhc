pub mod args;
pub mod backup;
pub mod doctor;
pub mod doctor_checks;
pub mod dream;
pub mod dream_staging;
pub mod facts;
pub mod facts_status;
pub mod init;
pub mod memfs;
pub mod memfs_extra;
pub mod memfs_shared;
pub mod memory;
pub mod memory_repository;
pub mod people;
pub mod people_ask;
pub mod people_query;
pub mod people_render;
pub mod people_search;
pub mod recompile;
pub mod reflect;
pub mod register;
pub mod remember;
pub mod repo;
pub mod search;
pub mod skill_frontmatter;
pub mod sleeptime;
pub mod tokens;
pub mod types;

#[cfg(test)]
pub mod people_test_support;

#[cfg(test)]
pub mod test_support;
