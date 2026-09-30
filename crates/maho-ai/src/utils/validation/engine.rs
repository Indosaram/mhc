//! Port of TypeBox 1.3.34 `schema/engine` `Error*` evaluation with the `en_US` locale.
//!
//! Keyword order, error accumulation (non-short-circuit `&`), fresh contexts for logical
//! operands, the 8-error cap per context, and the evaluated key/index stack (including the
//! "no pop on failure" behavior of `ErrorSchemaPushStack`) follow the TypeBox build.
//! `Check` is the error walk's boolean: an error is only ever recorded on a failing path.

use std::collections::HashSet;

use serde_json::{Map, Value};

use super::formats::{js_regex, test_format};
use crate::utils::js::number_to_string;

/// TypeBox `Settings.maxErrors` default.
const MAX_ERRORS: usize = 8;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Issue {
    pub keyword: &'static str,
    pub instance_path: String,
    pub message: String,
    pub required_properties: Vec<String>,
}

#[derive(Default)]
struct Frame {
    indices: HashSet<usize>,
    keys: HashSet<String>,
}

struct Ctx {
    errors: Vec<Issue>,
    stack: Vec<Frame>,
}

impl Ctx {
    fn new() -> Self {
        Self { errors: Vec::new(), stack: vec![Frame::default()] }
    }

    fn at_capacity(&self) -> bool {
        self.errors.len() >= MAX_ERRORS
    }

    fn add(&mut self, keyword: &'static str, instance_path: &str, message: String) -> bool {
        self.add_issue(Issue { keyword, instance_path: instance_path.to_owned(), message, required_properties: Vec::new() })
    }

    fn add_issue(&mut self, issue: Issue) -> bool {
        if !self.at_capacity() {
            self.errors.push(issue);
        }
        false
    }

    fn add_errors(&mut self, errors: Vec<Issue>) {
        for issue in errors {
            self.add_issue(issue);
        }
    }

    fn push(&mut self) {
        self.stack.push(Frame::default());
    }

    fn pop(&mut self) {
        self.stack.pop();
    }

    fn add_key(&mut self, key: &str) -> bool {
        if let Some(top) = self.stack.last_mut() {
            top.keys.insert(key.to_owned());
        }
        true
    }

    fn add_index(&mut self, index: usize) -> bool {
        if let Some(top) = self.stack.last_mut() {
            top.indices.insert(index);
        }
        true
    }

    fn has_key(&self, key: &str) -> bool {
        self.stack.last().is_some_and(|top| top.keys.contains(key))
    }

    fn has_index(&self, index: usize) -> bool {
        self.stack.last().is_some_and(|top| top.indices.contains(&index))
    }

    fn merge(&mut self, other: &Ctx) {
        if let (Some(top), Some(from)) = (self.stack.last_mut(), other.stack.last()) {
            top.indices.extend(from.indices.iter().copied());
            top.keys.extend(from.keys.iter().cloned());
        }
    }
}

/// `Pathing.EncodeFragment` (RFC 6901 escaping).
fn encode_fragment(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn child(path: &str, key: &str) -> String {
    format!("{path}/{}", encode_fragment(key))
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64()
}

fn limit_text(value: &Value) -> String {
    number(value).map_or_else(|| value.to_string(), number_to_string)
}

/// `G.IsEqual` / `G.IsDeepEqual` over JSON values (numbers compare as doubles).
pub(super) fn js_deep_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(x, y)| js_deep_equal(x, y)),
        (Value::Object(a), Value::Object(b)) => {
            a.len() == b.len() && a.iter().all(|(key, x)| b.get(key).is_some_and(|y| js_deep_equal(x, y)))
        }
        _ => left == right,
    }
}

fn is_multiple_of(dividend: f64, divisor: f64) -> bool {
    if dividend.fract() == 0.0 && (1.0 / divisor) % 1.0 == 0.0 {
        return true;
    }
    let modulo = dividend % divisor;
    modulo.abs().min((modulo - divisor).abs()).min((modulo + divisor).abs()) < 1e-10
}

fn type_matches(name: &str, value: &Value) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "boolean" => value.is_boolean(),
        "integer" => number(value).is_some_and(|n| n.fract() == 0.0),
        "number" => value.is_number(),
        "null" => value.is_null(),
        "string" => value.is_string(),
        _ => false,
    }
}

fn type_names(schema: &Map<String, Value>) -> Option<Vec<&str>> {
    match schema.get("type")? {
        Value::String(name) => Some(vec![name.as_str()]),
        Value::Array(names) => names.iter().map(Value::as_str).collect(),
        _ => None,
    }
}

fn regex_test(pattern: &str, text: &str) -> bool {
    js_regex(pattern, false).is_some_and(|re| re.is_match(text).unwrap_or(false))
}

fn is_schema(value: &Value) -> bool {
    value.is_object() || value.is_boolean()
}

struct Engine<'a> {
    root: &'a Value,
}

impl<'a> Engine<'a> {
    fn resolve(&self, reference: &str) -> Option<&'a Value> {
        let fragment = reference.strip_prefix('#')?;
        if fragment.is_empty() {
            return Some(self.root);
        }
        if !fragment.starts_with('/') {
            return None;
        }
        self.root.pointer(fragment)
    }

    fn check(&self, schema: &Value, value: &Value) -> (bool, Ctx) {
        let mut ctx = Ctx::new();
        let ok = self.schema(&mut ctx, "", schema, value);
        (ok, ctx)
    }

    /// `ErrorSchemaPushStack`: the frame is only popped when the operand passes.
    fn push_stack(&self, ctx: &mut Ctx, path: &str, schema: &Value, value: &Value) -> bool {
        ctx.push();
        let ok = self.schema(ctx, path, schema, value);
        if ok {
            ctx.pop();
        }
        ok
    }

    fn schema(&self, ctx: &mut Ctx, path: &str, schema: &Value, value: &Value) -> bool {
        if ctx.at_capacity() {
            return false;
        }
        let s = match schema {
            Value::Bool(true) => return true,
            Value::Bool(false) => return ctx.add("boolean", path, "schema is false".into()),
            Value::Object(s) => s,
            _ => return true,
        };
        let mut ok = true;
        if let Some(names) = type_names(s)
            && !names.iter().any(|name| type_matches(name, value))
        {
            let message = match s.get("type") {
                Some(Value::String(name)) => format!("must be {name}"),
                _ => format!("must be either {}", names.join(" or ")),
            };
            ok &= ctx.add("type", path, message);
        }
        if let Value::Object(object) = value {
            ok &= self.object_keywords(ctx, path, s, object, value);
        }
        if let Value::Array(items) = value {
            ok &= self.array_keywords(ctx, path, s, items);
        }
        if let Value::String(text) = value {
            ok &= Self::string_keywords(ctx, path, s, text);
        }
        if let Some(n) = number(value) {
            ok &= Self::number_keywords(ctx, path, s, n);
        }
        ok &= self.logical_keywords(ctx, path, s, value);
        ok
    }

    fn object_keywords(&self, ctx: &mut Ctx, path: &str, s: &Map<String, Value>, object: &Map<String, Value>, value: &Value) -> bool {
        let mut ok = true;
        if let Some(Value::Array(required)) = s.get("required") {
            let missing: Vec<String> =
                required.iter().filter_map(Value::as_str).filter(|key| !object.contains_key(*key)).map(str::to_owned).collect();
            if !missing.is_empty() {
                ok &= ctx.add_issue(Issue {
                    keyword: "required",
                    instance_path: path.to_owned(),
                    message: format!("must have required properties {}", missing.join(", ")),
                    required_properties: missing,
                });
            }
        }
        if let Some(additional) = s.get("additionalProperties").filter(|v| is_schema(v)) {
            let properties = s.get("properties").and_then(Value::as_object);
            let patterns: Vec<&String> = s.get("patternProperties").and_then(Value::as_object).map(|p| p.keys().collect()).unwrap_or_default();
            let mut failed = false;
            for (key, item) in object {
                let known = properties.is_some_and(|p| p.contains_key(key)) || patterns.iter().any(|pattern| regex_test(pattern, key));
                if !(known || self.push_stack(ctx, &child(path, key), additional, item) && ctx.add_key(key)) {
                    failed = true;
                }
            }
            if failed {
                ok &= ctx.add("additionalProperties", path, "must not have additional properties".into());
            }
        }
        if let Some(Value::Object(dependencies)) = s.get("dependencies") {
            for (key, dependency) in dependencies {
                if !object.contains_key(key) {
                    continue;
                }
                if let Value::Array(names) = dependency {
                    for name in names.iter().filter_map(Value::as_str) {
                        if !object.contains_key(name) {
                            ok &= ctx.add("dependencies", path, dependency_message(key, names));
                            break;
                        }
                    }
                } else {
                    ok &= self.schema(ctx, path, dependency, value);
                }
            }
        }
        if let Some(Value::Object(dependent)) = s.get("dependentRequired") {
            for (key, names) in dependent {
                let (true, Value::Array(list)) = (object.contains_key(key), names) else { continue };
                for name in list.iter().filter_map(Value::as_str) {
                    if !object.contains_key(name) {
                        ok &= ctx.add("dependentRequired", path, dependency_message(key, list));
                    }
                }
            }
        }
        if let Some(Value::Object(dependent)) = s.get("dependentSchemas") {
            for (key, dependency) in dependent {
                if object.contains_key(key) {
                    ok &= self.schema(ctx, path, dependency, value);
                }
            }
        }
        if let Some(Value::Object(patterns)) = s.get("patternProperties") {
            for (pattern, sub) in patterns {
                for (key, item) in object {
                    if regex_test(pattern, key) {
                        ok &= self.push_stack(ctx, &child(path, key), sub, item) && ctx.add_key(key);
                    }
                }
            }
        }
        if let Some(Value::Object(properties)) = s.get("properties") {
            for (key, sub) in properties {
                if let Some(item) = object.get(key) {
                    ok &= self.push_stack(ctx, &child(path, key), sub, item) && ctx.add_key(key);
                }
            }
        }
        if let Some(names) = s.get("propertyNames").filter(|v| is_schema(v)) {
            let mut invalid = Vec::new();
            for key in object.keys() {
                if !self.schema(ctx, &child(path, key), names, &Value::String(key.clone())) {
                    invalid.push(key.clone());
                }
            }
            if !invalid.is_empty() {
                ok &= ctx.add("propertyNames", path, format!("property names {} are invalid", invalid.join(", ")));
            }
        }
        let count = object.len() as f64;
        if let Some(limit) = s.get("minProperties").filter(|v| v.is_number())
            && number(limit).is_some_and(|l| count < l)
        {
            ok &= ctx.add("minProperties", path, format!("must not have fewer than {} properties", limit_text(limit)));
        }
        if let Some(limit) = s.get("maxProperties").filter(|v| v.is_number())
            && number(limit).is_some_and(|l| count > l)
        {
            ok &= ctx.add("maxProperties", path, format!("must not have more than {} properties", limit_text(limit)));
        }
        ok
    }

    fn contains_count(&self, contains: &Value, items: &[Value], ctx: &mut Ctx) -> usize {
        let mut count = 0;
        for (index, item) in items.iter().enumerate() {
            if self.check(contains, item).0 {
                ctx.add_index(index);
                count += 1;
            }
        }
        count
    }

    fn array_keywords(&self, ctx: &mut Ctx, path: &str, s: &Map<String, Value>, items: &[Value]) -> bool {
        let mut ok = true;
        let tuple = s.get("items").and_then(Value::as_array);
        if let (Some(additional), Some(tuple)) = (s.get("additionalItems").filter(|v| is_schema(v)), tuple) {
            for (index, item) in items.iter().enumerate() {
                if index >= tuple.len() && !(self.push_stack(ctx, &format!("{path}/{index}"), additional, item) && ctx.add_index(index)) {
                    ok = false;
                    break;
                }
            }
        }
        let contains = s.get("contains").filter(|v| is_schema(v));
        let min_contains = s.get("minContains").and_then(number);
        if let Some(contains) = contains
            && min_contains != Some(0.0)
            && (items.is_empty() || self.contains_count(contains, items, ctx) == 0)
        {
            ok &= ctx.add("contains", path, "must contain at least 1 valid item".into());
        }
        match s.get("items") {
            Some(Value::Array(tuple)) => {
                for (index, sub) in tuple.iter().enumerate() {
                    if let Some(item) = items.get(index) {
                        ok &= self.push_stack(ctx, &format!("{path}/{index}"), sub, item) && ctx.add_index(index);
                    }
                }
            }
            Some(sub) if is_schema(sub) => {
                let offset = s.get("prefixItems").and_then(Value::as_array).map_or(0, Vec::len);
                for (index, item) in items.iter().enumerate().skip(offset) {
                    ok &= self.push_stack(ctx, &format!("{path}/{index}"), sub, item) && ctx.add_index(index);
                }
            }
            _ => {}
        }
        if let (Some(contains), Some(min)) = (contains, min_contains)
            && (self.contains_count(contains, items, ctx) as f64) < min
        {
            ok &= ctx.add("contains", path, "must contain at least 1 valid item".into());
        }
        if let (Some(contains), Some(max)) = (contains, s.get("maxContains").and_then(number))
            && (self.contains_count(contains, items, ctx) as f64) > max
        {
            ok &= ctx.add("contains", path, "must contain at least 1 valid item".into());
        }
        let count = items.len() as f64;
        if let Some(limit) = s.get("minItems").filter(|v| v.is_number())
            && number(limit).is_some_and(|l| count < l)
        {
            ok &= ctx.add("minItems", path, format!("must not have fewer than {} items", limit_text(limit)));
        }
        if let Some(limit) = s.get("maxItems").filter(|v| v.is_number())
            && number(limit).is_some_and(|l| count > l)
        {
            ok &= ctx.add("maxItems", path, format!("must not have more than {} items", limit_text(limit)));
        }
        if let Some(Value::Array(prefix)) = s.get("prefixItems")
            && !items.is_empty()
        {
            for (index, sub) in prefix.iter().enumerate() {
                if let Some(item) = items.get(index) {
                    ok &= self.push_stack(ctx, &format!("{path}/{index}"), sub, item) && ctx.add_index(index);
                }
            }
        }
        if s.get("uniqueItems") == Some(&Value::Bool(true)) {
            let duplicate = items.iter().enumerate().any(|(i, item)| items[..i].iter().any(|prev| js_deep_equal(prev, item)));
            if duplicate {
                ok &= ctx.add("uniqueItems", path, "must not have duplicate items".into());
            }
        }
        ok
    }

    fn string_keywords(ctx: &mut Ctx, path: &str, s: &Map<String, Value>, text: &str) -> bool {
        let mut ok = true;
        let length = text.chars().count() as f64;
        if let Some(limit) = s.get("minLength").filter(|v| v.is_number())
            && number(limit).is_some_and(|l| length < l)
        {
            ok &= ctx.add("minLength", path, format!("must not have fewer than {} characters", limit_text(limit)));
        }
        if let Some(limit) = s.get("maxLength").filter(|v| v.is_number())
            && number(limit).is_some_and(|l| length > l)
        {
            ok &= ctx.add("maxLength", path, format!("must not have more than {} characters", limit_text(limit)));
        }
        if let Some(Value::String(format)) = s.get("format")
            && !test_format(format, text)
        {
            ok &= ctx.add("format", path, format!("must match format \"{format}\""));
        }
        if let Some(Value::String(pattern)) = s.get("pattern")
            && !regex_test(pattern, text)
        {
            ok &= ctx.add("pattern", path, format!("must match pattern \"{pattern}\""));
        }
        ok
    }

    fn number_keywords(ctx: &mut Ctx, path: &str, s: &Map<String, Value>, n: f64) -> bool {
        let mut ok = true;
        type NumberBound = (&'static str, &'static str, fn(f64, f64) -> bool);
        let bounds: [NumberBound; 4] = [
            ("exclusiveMinimum", ">", |v, l| v > l),
            ("exclusiveMaximum", "<", |v, l| v < l),
            ("minimum", ">=", |v, l| v >= l),
            ("maximum", "<=", |v, l| v <= l),
        ];
        for (keyword, comparison, holds) in bounds {
            if let Some(limit) = s.get(keyword).and_then(number)
                && !holds(n, limit)
            {
                ok &= ctx.add(keyword, path, format!("must be {comparison} {}", number_to_string(limit)));
            }
        }
        if let Some(divisor) = s.get("multipleOf").and_then(number)
            && !is_multiple_of(n, divisor)
        {
            ok &= ctx.add("multipleOf", path, format!("must be multiple of {}", number_to_string(divisor)));
        }
        ok
    }

    fn logical_keywords(&self, ctx: &mut Ctx, path: &str, s: &Map<String, Value>, value: &Value) -> bool {
        let mut ok = true;
        if let Some(Value::String(reference)) = s.get("$ref") {
            let mut next = Ctx::new();
            let valid = self.resolve(reference).filter(|t| is_schema(t)).is_some_and(|target| self.schema(&mut next, path, target, value));
            if valid {
                ctx.merge(&next);
            } else {
                if self.resolve(reference).is_none() {
                    next.add("boolean", path, "schema is false".into());
                }
                ctx.add_errors(next.errors);
            }
            ok &= valid;
        }
        if let Some(constant) = s.get("const")
            && !js_deep_equal(value, constant)
        {
            ok &= ctx.add("const", path, "must be equal to constant".into());
        }
        if let Some(Value::Array(options)) = s.get("enum")
            && !options.iter().any(|option| js_deep_equal(value, option))
        {
            ok &= ctx.add("enum", path, "must be equal to one of the allowed values".into());
        }
        if let Some(condition) = s.get("if").filter(|v| is_schema(v)) {
            let truthy = Value::Bool(true);
            let then_schema = s.get("then").filter(|v| is_schema(v)).unwrap_or(&truthy);
            let else_schema = s.get("else").filter(|v| is_schema(v)).unwrap_or(&truthy);
            let mut true_ctx = Ctx::new();
            let is_if = if self.schema(&mut true_ctx, path, condition, value) {
                self.schema(&mut true_ctx, path, then_schema, value) || ctx.add("if", path, "must match \"then\" schema".into())
            } else {
                self.schema(ctx, path, else_schema, value) || ctx.add("if", path, "must match \"else\" schema".into())
            };
            if is_if {
                ctx.merge(&true_ctx);
            }
            ok &= is_if;
        }
        if let Some(negated) = s.get("not").filter(|v| is_schema(v)) {
            let (passed, next) = self.check(negated, value);
            if passed {
                ok &= ctx.add("not", path, "must not be valid".into());
            } else {
                ctx.merge(&next);
            }
        }
        if let Some(Value::Array(all)) = s.get("allOf") {
            let (passed, failed) = self.operands(path, all, value);
            if failed.is_empty() {
                for next in &passed {
                    ctx.merge(next);
                }
            } else {
                failed.into_iter().for_each(|next| ctx.add_errors(next.errors));
                ok = false;
            }
        }
        if let Some(Value::Array(any)) = s.get("anyOf") {
            let (passed, failed) = self.operands(path, any, value);
            if passed.is_empty() {
                failed.into_iter().for_each(|next| ctx.add_errors(next.errors));
                ok &= ctx.add("anyOf", path, "must match a schema in anyOf".into());
            } else {
                for next in &passed {
                    ctx.merge(next);
                }
            }
        }
        if let Some(Value::Array(one)) = s.get("oneOf") {
            let (passed, failed) = self.operands(path, one, value);
            if passed.len() == 1 {
                ctx.merge(&passed[0]);
            } else {
                if passed.is_empty() {
                    failed.into_iter().for_each(|next| ctx.add_errors(next.errors));
                }
                ok &= ctx.add("oneOf", path, "must match exactly one schema in oneOf".into());
            }
        }
        if let (Value::Array(items), Some(sub)) = (value, s.get("unevaluatedItems").filter(|v| is_schema(v))) {
            let mut failed = false;
            for (index, item) in items.iter().enumerate() {
                if (ctx.has_index(index) || self.check_in(path, sub, item)) && ctx.add_index(index) {
                    continue;
                }
                failed = true;
            }
            if failed {
                ok &= ctx.add("unevaluatedItems", path, "must not have unevaluated items".into());
            }
        }
        if let (Value::Object(object), Some(sub)) = (value, s.get("unevaluatedProperties").filter(|v| is_schema(v))) {
            let mut failed = false;
            for (key, item) in object {
                if ctx.has_key(key) || (self.check_in(path, sub, item) && ctx.add_key(key)) {
                    continue;
                }
                failed = true;
            }
            if failed {
                ok &= ctx.add("unevaluatedProperties", path, "must not have unevaluated properties".into());
            }
        }
        ok
    }

    fn check_in(&self, path: &str, schema: &Value, value: &Value) -> bool {
        self.schema(&mut Ctx::new(), path, schema, value)
    }

    /// Evaluates each operand in a fresh context: (passing contexts, failing contexts).
    fn operands(&self, path: &str, schemas: &[Value], value: &Value) -> (Vec<Ctx>, Vec<Ctx>) {
        let mut passed = Vec::new();
        let mut failed = Vec::new();
        for sub in schemas {
            let mut next = Ctx::new();
            if self.schema(&mut next, path, sub, value) {
                passed.push(next);
            } else {
                failed.push(next);
            }
        }
        (passed, failed)
    }
}

fn dependency_message(key: &str, names: &[Value]) -> String {
    let names: Vec<&str> = names.iter().filter_map(Value::as_str).collect();
    format!("must have properties {} when property {key} is present", names.join(", "))
}

/// `Compile(schema).Errors(value)` with the `en_US` locale; empty when the value is valid.
pub(super) fn errors(schema: &Value, value: &Value) -> Vec<Issue> {
    let engine = Engine { root: schema };
    let mut ctx = Ctx::new();
    engine.schema(&mut ctx, "", schema, value);
    ctx.errors
}

/// `Compile(schema).Check(value)`.
pub(super) fn check(schema: &Value, value: &Value) -> bool {
    Engine { root: schema }.check(schema, value).0
}
