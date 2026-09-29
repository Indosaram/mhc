use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::internal::jsonc::parse_jsonc_safe;
use crate::internal::posix_path::to_posix_path;
use crate::migration::types::{MigrationEnvironment, MigrationFileSystem};
use crate::writer::types::{FsError, OmoConfigWriteFileSystem};

#[derive(Debug, Default)]
pub struct MemoryMigrationFileSystem {
    pub directories: RefCell<BTreeSet<String>>,
    pub files: RefCell<BTreeMap<String, String>>,
    pub operations: RefCell<Vec<String>>,
}

impl MemoryMigrationFileSystem {
    pub fn new() -> Self {
        Self::default()
    }

    fn key(&self, path: &str) -> String {
        to_posix_path(path)
    }
}

impl OmoConfigWriteFileSystem for MemoryMigrationFileSystem {
    fn copy(&self, source: &str, destination: &str) -> Result<(), FsError> {
        let content = self.read(source)?;
        let dest_key = self.key(destination);
        self.files.borrow_mut().insert(dest_key, content);
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        let key = self.key(path);
        self.files.borrow().contains_key(&key) || self.directories.borrow().contains(&key)
    }

    fn is_symbolic_link(&self, _path: &str) -> Result<bool, FsError> {
        Ok(false)
    }

    fn mkdirs(&self, path: &str) -> Result<(), FsError> {
        let key = self.key(path);
        self.directories.borrow_mut().insert(key.clone());
        self.operations.borrow_mut().push(format!("mkdir:{key}"));
        Ok(())
    }

    fn read(&self, path: &str) -> Result<String, FsError> {
        let key = self.key(path);
        self.files
            .borrow()
            .get(&key)
            .cloned()
            .ok_or_else(|| FsError::not_found(format!("Missing {key}")))
    }

    fn list_dir(&self, path: &str) -> Result<Vec<String>, FsError> {
        let prefix = if path.ends_with('/') {
            path.to_string()
        } else {
            format!("{path}/")
        };
        let mut names = Vec::new();
        for file in self.files.borrow().keys() {
            if let Some(rest) = file.strip_prefix(&prefix)
                && !rest.contains('/')
            {
                names.push(rest.to_string());
            }
        }
        Ok(names)
    }

    fn rename(&self, from: &str, to: &str) -> Result<(), FsError> {
        let content = self.read(from)?;
        let from_key = self.key(from);
        let to_key = self.key(to);
        let mut files = self.files.borrow_mut();
        files.remove(&from_key);
        files.insert(to_key.clone(), content);
        self.operations
            .borrow_mut()
            .push(format!("rename:{from_key}:{to_key}"));
        Ok(())
    }

    fn unlink(&self, path: &str) -> Result<(), FsError> {
        let key = self.key(path);
        if self.files.borrow_mut().remove(&key).is_none() {
            return Err(FsError::not_found(format!("Missing {key}")));
        }
        self.operations.borrow_mut().push(format!("unlink:{key}"));
        Ok(())
    }

    fn write_exclusive(&self, path: &str, content: &str) -> Result<(), FsError> {
        let key = self.key(path);
        let mut files = self.files.borrow_mut();
        if files.contains_key(&key) {
            return Err(FsError::exists(format!("Already exists {key}")));
        }
        files.insert(key.clone(), content.to_string());
        self.operations
            .borrow_mut()
            .push(format!("exclusive:{key}"));
        Ok(())
    }

    fn write(&self, path: &str, content: &str) -> Result<(), FsError> {
        let key = self.key(path);
        self.files
            .borrow_mut()
            .insert(key.clone(), content.to_string());
        self.operations.borrow_mut().push(format!("write:{key}"));
        Ok(())
    }
}

impl MigrationFileSystem for MemoryMigrationFileSystem {
    fn replace_if_contents_match(
        &self,
        path: &str,
        expected: &str,
        content: &str,
    ) -> Result<bool, FsError> {
        let key = self.key(path);
        let mut files = self.files.borrow_mut();
        if files.get(&key).map(String::as_str) != Some(expected) {
            return Ok(false);
        }
        files.insert(key.clone(), content.to_string());
        self.operations.borrow_mut().push(format!("replace:{key}"));
        Ok(true)
    }

    fn remove_if_contents_match(&self, path: &str, expected: &str) -> Result<bool, FsError> {
        let key = self.key(path);
        let mut files = self.files.borrow_mut();
        if files.get(&key).map(String::as_str) != Some(expected) {
            return Ok(false);
        }
        files.remove(&key);
        self.operations.borrow_mut().push(format!("remove:{key}"));
        Ok(true)
    }
}

pub struct MigrationFixture {
    pub env: MigrationEnvironment,
    pub source_path: &'static str,
    pub target_path: &'static str,
}

pub fn migration_fixture() -> MigrationFixture {
    let mut env = MigrationEnvironment::new();
    env.insert("HOME".to_string(), "/home/alice".to_string());
    MigrationFixture {
        env,
        source_path: "/legacy/config.jsonc",
        target_path: "/home/alice/.omo/omo.jsonc",
    }
}

pub fn parse_file(file_system: &MemoryMigrationFileSystem, path: &str) -> Value {
    let content = file_system
        .read(path)
        .unwrap_or_else(|_| panic!("Missing file at {path}"));
    let parsed = parse_jsonc_safe(&content);
    if !parsed.errors.is_empty() || parsed.data.is_none() {
        panic!("Invalid JSONC at {path}");
    }
    let data = parsed.data.unwrap();
    if !data.is_object() {
        panic!("Invalid JSONC at {path}: not an object");
    }
    data
}
