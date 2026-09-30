//! Process environment access with a per-thread override seam.
//!
//! senpi reads `process.env` at call time and its tests mutate it. Rust 2024 makes
//! `std::env::set_var` unsafe (and this crate forbids unsafe code), so reads go through
//! [`var`], and tests layer per-thread overrides with [`with_overrides`].

use std::cell::RefCell;
use std::collections::HashMap;

thread_local! {
    static OVERRIDES: RefCell<Vec<HashMap<String, Option<String>>>> = const { RefCell::new(Vec::new()) };
}

pub fn var(name: &str) -> Option<String> {
    let overridden = OVERRIDES.with(|stack| {
        stack
            .borrow()
            .iter()
            .rev()
            .find_map(|layer| layer.get(name).cloned())
    });
    match overridden {
        Some(value) => value,
        None => std::env::var(name).ok(),
    }
}

pub fn with_overrides<R>(vars: &[(&str, Option<&str>)], f: impl FnOnce() -> R) -> R {
    struct Pop;
    impl Drop for Pop {
        fn drop(&mut self) {
            OVERRIDES.with(|stack| {
                stack.borrow_mut().pop();
            });
        }
    }
    let layer = vars
        .iter()
        .map(|(k, v)| ((*k).to_string(), v.map(str::to_string)))
        .collect();
    OVERRIDES.with(|stack| stack.borrow_mut().push(layer));
    let _pop = Pop;
    f()
}

pub type Env = HashMap<String, String>;

pub fn env_from(pairs: &[(&str, &str)]) -> Env {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

pub fn current() -> Env {
    let mut env: Env = std::env::vars().collect();
    OVERRIDES.with(|stack| {
        for layer in stack.borrow().iter() {
            for (k, v) in layer {
                match v {
                    Some(v) => {
                        env.insert(k.clone(), v.clone());
                    }
                    None => {
                        env.remove(k);
                    }
                }
            }
        }
    });
    env
}

pub(crate) fn truthy(env: &Env, name: &str) -> bool {
    env.get(name).is_some_and(|v| !v.is_empty())
}
