use std::collections::BTreeMap;
#[derive(Default)]
pub struct SessionAttachOrder {
    next: u64,
    order: BTreeMap<String, BTreeMap<String, u64>>,
}
impl SessionAttachOrder {
    pub fn attached(&mut self, session: &str, connection: &str) {
        let connections = self.order.entry(session.into()).or_default();
        if !connections.contains_key(connection) {
            connections.insert(connection.into(), self.next);
            self.next += 1;
        }
    }
    pub fn detached(&mut self, session: &str, connection: &str) {
        if let Some(connections) = self.order.get_mut(session) {
            connections.remove(connection);
            if connections.is_empty() {
                self.order.remove(session);
            }
        }
    }
    pub fn forget_session(&mut self, session: &str) {
        self.order.remove(session);
    }
    pub fn connections(&self, session: &str) -> Vec<String> {
        let mut connections = self
            .order
            .get(session)
            .into_iter()
            .flat_map(|connections| connections.iter())
            .collect::<Vec<_>>();
        connections.sort_by_key(|(_, order)| **order);
        connections.into_iter().map(|(id, _)| id.clone()).collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn order_is_per_session_and_duplicate_attach_keeps_place() {
        let mut order = SessionAttachOrder::default();
        order.attached("other", "a");
        order.attached("s", "b");
        order.attached("s", "a");
        order.attached("s", "b");
        assert_eq!(order.connections("s"), vec!["b", "a"]);
        order.detached("s", "b");
        order.attached("s", "b");
        assert_eq!(order.connections("s"), vec!["a", "b"]);
    }
}
