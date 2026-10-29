use std::ffi::OsString;
use std::{cell::RefCell, collections::BTreeMap};

thread_local! {
    static ENV: RefCell<BTreeMap<String, OsString>> = const { RefCell::new(BTreeMap::new()) };
}

pub fn env_var(name: &str) -> Result<String, std::env::VarError> {
    ENV.with(|env| env.borrow().get(name).cloned())
        .map(|s| s.to_string_lossy().into_owned())
        .map(Ok)
        .unwrap_or_else(|| std::env::var(name))
}

pub struct EnvOverride {
    name: String,
    previous: Option<OsString>,
}

impl EnvOverride {
    pub fn set(name: &str, value: &str) -> Self {
        let previous = ENV.with(|env| {
            env.borrow_mut()
                .insert(name.to_owned(), OsString::from(value))
        });
        Self {
            name: name.to_owned(),
            previous,
        }
    }
}

impl Drop for EnvOverride {
    fn drop(&mut self) {
        ENV.with(|env| {
            let mut env = env.borrow_mut();
            match &self.previous {
                Some(value) => {
                    env.insert(self.name.clone(), value.clone());
                }
                None => {
                    env.remove(&self.name);
                }
            }
        });
    }
}
