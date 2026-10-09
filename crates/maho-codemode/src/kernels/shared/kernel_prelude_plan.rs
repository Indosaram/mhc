use std::collections::HashSet;
use maho_tools::definition::KernelPrelude;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KernelPreludePlan {
    pub install: Vec<KernelPrelude>,
    pub remove: Vec<String>,
}

#[derive(Clone, Debug, Default)]
pub struct KernelPreludeTracker {
    installed: HashSet<String>,
}

impl KernelPreludeTracker {
    pub fn plan(&mut self, contributions: &[KernelPrelude]) -> KernelPreludePlan {
        let active: HashSet<String> = contributions
            .iter()
            .flat_map(|c| c.exports.iter().cloned())
            .collect();
        let remove: Vec<String> = self
            .installed
            .iter()
            .filter(|name| !active.contains(name.as_str()))
            .cloned()
            .collect();
        self.installed = active;
        KernelPreludePlan {
            install: contributions.to_vec(),
            remove,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tracker_plans_install_and_dynamic_removal() {
        let mut tracker = KernelPreludeTracker::default();
        let p1 = KernelPrelude {
            javascript: "globalThis.foo = 1;".into(),
            python: "foo = 1".into(),
            documentation: "foo facade".into(),
            exports: vec!["foo".into()],
            declarations: None,
            safety: None,
            method_allowlist: None,
        };
        let p2 = KernelPrelude {
            javascript: "globalThis.bar = 2;".into(),
            python: "bar = 2".into(),
            documentation: "bar facade".into(),
            exports: vec!["bar".into()],
            declarations: None,
            safety: None,
            method_allowlist: None,
        };

        let plan1 = tracker.plan(&[p1.clone(), p2.clone()]);
        assert_eq!(plan1.install.len(), 2);
        assert!(plan1.remove.is_empty());

        let plan2 = tracker.plan(std::slice::from_ref(&p1));
        assert_eq!(plan2.install.len(), 1);
        assert_eq!(plan2.remove, vec!["bar".to_string()]);

        let plan3 = tracker.plan(&[]);
        assert!(plan3.install.is_empty());
        assert_eq!(plan3.remove, vec!["foo".to_string()]);
    }
}
