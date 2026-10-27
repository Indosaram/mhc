#![allow(dead_code)]

use maho_config::{Env, LoadOptions, LoadResult, load};
use std::path::{Path, PathBuf};

pub struct Tree {
    pub _dir: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
}

impl Tree {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().canonicalize().expect("canonical");
        let home = root.join("home");
        std::fs::create_dir_all(&home).expect("home");
        Self {
            _dir: dir,
            root,
            home,
        }
    }

    pub fn env(&self) -> Env {
        let mut env = Env::new();
        env.insert("HOME".into(), self.home.to_string_lossy().into_owned());
        env
    }

    pub fn write(&self, rel: &str, text: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(&path, text).expect("write");
        path
    }

    pub fn mkdir(&self, rel: &str) -> PathBuf {
        let path = self.root.join(rel);
        std::fs::create_dir_all(&path).expect("mkdir");
        path
    }

    pub fn load_at(&self, cwd: &Path, env: Env) -> LoadResult {
        load(
            &LoadOptions::new(cwd)
                .with_env(env)
                .with_account_home(None)
                .with_catalog(true),
        )
    }

    pub fn load(&self, cwd_rel: &str) -> LoadResult {
        let cwd = self.mkdir(cwd_rel);
        self.load_at(&cwd, self.env())
    }
}
