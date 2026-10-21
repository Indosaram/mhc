use crate::types::{JsonObject, JsonValue};
use std::collections::HashMap;
use std::sync::{Arc, Weak};

#[derive(Default)]
pub struct JsonRevisionValidator {
    arrays: HashMap<usize, Weak<Vec<JsonValue>>>,
    objects: HashMap<usize, Weak<JsonObject>>,
}
impl JsonRevisionValidator {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn validate<'a>(&mut self, value: &'a JsonValue) -> &'a JsonValue {
        self.arrays.retain(|_, v| v.strong_count() > 0);
        self.objects.retain(|_, v| v.strong_count() > 0);
        self.visit(value);
        value
    }
    fn visit(&mut self, value: &JsonValue) {
        match value {
            JsonValue::Array(values) => {
                let reference = Arc::downgrade(values);
                let id = reference.as_ptr().addr();
                if self.arrays.get(&id).is_some_and(|v| v.ptr_eq(&reference)) {
                    return;
                }
                for value in values.iter() {
                    self.visit(value);
                }
                self.arrays.insert(id, reference);
            }
            JsonValue::Object(values) => {
                let reference = Arc::downgrade(values);
                let id = reference.as_ptr().addr();
                if self.objects.get(&id).is_some_and(|v| v.ptr_eq(&reference)) {
                    return;
                }
                for (_, value) in values.iter() {
                    self.visit(value);
                }
                self.objects.insert(id, reference);
            }
            JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) | JsonValue::String(_) => {}
        }
    }
    pub fn cached_containers(&self) -> usize {
        self.arrays.len() + self.objects.len()
    }
}
