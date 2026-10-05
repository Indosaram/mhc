use serde_json::{Value, json};
use std::sync::Arc;

pub type ItemNotification = Arc<dyn Fn(Value) -> Value + Send + Sync>;
pub type MethodNotification = Arc<dyn Fn(&str, Value) -> Value + Send + Sync>;
pub struct MessageItemNotifier {
    pub item_id: Arc<dyn Fn(usize) -> String + Send + Sync>,
    pub started: ItemNotification,
    pub completed: ItemNotification,
    pub notification: MethodNotification,
}
struct ActiveTextItem {
    id: String,
    text: String,
    completed: bool,
}
pub struct MessageItemProjector {
    notifier: MessageItemNotifier,
    text_items: Vec<(usize, ActiveTextItem)>,
    reasoning_items: Vec<(usize, ActiveTextItem)>,
}
impl MessageItemProjector {
    pub fn new(notifier: MessageItemNotifier) -> Self { Self { notifier, text_items: Vec::new(), reasoning_items: Vec::new() } }
    fn item(&mut self, index: usize, reasoning: bool, replace: bool) -> &mut ActiveTextItem {
        let items = if reasoning { &mut self.reasoning_items } else { &mut self.text_items };
        let position = items.iter().position(|(key, _)| *key == index);
        let item = ActiveTextItem { id: (self.notifier.item_id)(index), text: String::new(), completed: false };
        let position = match position {
            Some(position) => { if replace { items[position].1 = item; } position },
            None => { items.push((index, item)); items.len() - 1 },
        };
        &mut items[position].1
    }
    pub fn start_text(&mut self, index: usize) -> Vec<Value> {
        let id = self.item(index, false, true).id.clone();
        vec![(self.notifier.started)(json!({"type":"agentMessage","id":id,"text":"","phase":null,"memoryCitation":null}))]
    }
    pub fn delta_text(&mut self, index: usize, delta: &str) -> Vec<Value> {
        let item = self.item(index, false, false);
        item.text.push_str(delta);
        let id = item.id.clone();
        vec![(self.notifier.notification)("item/agentMessage/delta", json!({"itemId":id,"delta":delta}))]
    }
    pub fn complete_text(&mut self, index: usize, text: &str) -> Vec<Value> {
        let item = self.item(index, false, false);
        item.text = text.into(); item.completed = true;
        let id = item.id.clone();
        vec![(self.notifier.completed)(json!({"type":"agentMessage","id":id,"text":text,"phase":null,"memoryCitation":null}))]
    }
    pub fn start_reasoning(&mut self, index: usize) -> Vec<Value> {
        let id = self.item(index, true, true).id.clone();
        vec![(self.notifier.started)(json!({"type":"reasoning","id":id,"summary":[],"content":[]}))]
    }
    pub fn delta_reasoning(&mut self, index: usize, delta: &str) -> Vec<Value> {
        let item = self.item(index, true, false);
        item.text.push_str(delta);
        let id = item.id.clone();
        vec![(self.notifier.notification)("item/reasoning/textDelta", json!({"itemId":id,"delta":delta,"contentIndex":index}))]
    }
    pub fn complete_reasoning(&mut self, index: usize, text: &str) -> Vec<Value> {
        let item = self.item(index, true, false);
        item.text = text.into(); item.completed = true;
        let id = item.id.clone();
        vec![(self.notifier.completed)(json!({"type":"reasoning","id":id,"summary":[],"content":[text]}))]
    }
    pub fn complete_dangling_text(&mut self, message: &Value) -> Vec<Value> {
        let mut notifications = Vec::new();
        for (index, content) in message["content"].as_array().into_iter().flatten().enumerate() {
            match content["type"].as_str() {
                Some("text") if !self.text_items.iter().any(|(key, item)| *key == index && item.completed) => notifications.extend(self.complete_text(index, content["text"].as_str().unwrap_or_default())),
                Some("thinking") if !self.reasoning_items.iter().any(|(key, item)| *key == index && item.completed) => notifications.extend(self.complete_reasoning(index, content["thinking"].as_str().unwrap_or_default())),
                _ => {},
            }
        }
        notifications
    }
    pub fn close_dangling_items(&mut self) -> Vec<Value> {
        let text = self.text_items.iter().filter(|(_, item)| !item.completed).map(|(index, item)| (*index, item.text.clone())).collect::<Vec<_>>();
        let reasoning = self.reasoning_items.iter().filter(|(_, item)| !item.completed).map(|(index, item)| (*index, item.text.clone())).collect::<Vec<_>>();
        let mut notifications = Vec::new();
        for (index, text) in text { notifications.extend(self.complete_text(index, &text)); }
        for (index, text) in reasoning { notifications.extend(self.complete_reasoning(index, &text)); }
        notifications
    }
}
