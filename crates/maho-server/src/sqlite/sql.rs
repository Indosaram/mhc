use rusqlite::{Connection, OptionalExtension, Row, params_from_iter, types::Value};

#[derive(Clone, Debug, PartialEq)]
pub struct SqlQuery {
    pub query_text: String,
    pub params: Vec<Value>,
}
pub enum SqlValue {
    Parameter(Value),
    Query(SqlQuery),
}
impl SqlQuery {
    pub fn new(query_text: impl Into<String>, params: Vec<Value>) -> Self {
        Self {
            query_text: query_text.into(),
            params,
        }
    }
    pub fn compose(strings: &[&str], values: Vec<SqlValue>) -> Self {
        let mut query_text = strings.first().copied().unwrap_or_default().to_owned();
        let mut params = Vec::new();
        for (i, value) in values.into_iter().enumerate() {
            match value {
                SqlValue::Parameter(value) => {
                    query_text.push('?');
                    params.push(value);
                }
                SqlValue::Query(query) => {
                    query_text.push_str(&query.query_text);
                    params.extend(query.params);
                }
            }
            query_text.push_str(strings.get(i + 1).copied().unwrap_or_default());
        }
        Self { query_text, params }
    }
    pub fn exec(&self, db: &Connection) -> rusqlite::Result<()> {
        if !self.params.is_empty() {
            return Err(rusqlite::Error::InvalidParameterCount(self.params.len(), 0));
        }
        db.execute_batch(&self.query_text)
    }
    pub fn run(&self, db: &Connection) -> rusqlite::Result<(usize, i64)> {
        db.execute(&self.query_text, params_from_iter(self.params.iter()))
            .map(|changes| (changes, db.last_insert_rowid()))
    }
    pub fn get<T>(
        &self,
        db: &Connection,
        map: impl FnOnce(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Option<T>> {
        db.query_row(&self.query_text, params_from_iter(self.params.iter()), map)
            .optional()
    }
    pub fn all<T>(
        &self,
        db: &Connection,
        map: impl FnMut(&Row<'_>) -> rusqlite::Result<T>,
    ) -> rusqlite::Result<Vec<T>> {
        db.prepare(&self.query_text)?
            .query_map(params_from_iter(self.params.iter()), map)?
            .collect()
    }
}
pub fn join_sql_fragments(fragments: Vec<SqlQuery>, separator: &str) -> SqlQuery {
    let mut query_text = String::new();
    let mut params = Vec::new();
    for (i, fragment) in fragments.into_iter().enumerate() {
        if i > 0 {
            query_text.push_str(separator);
        }
        query_text.push_str(&fragment.query_text);
        params.extend(fragment.params);
    }
    SqlQuery { query_text, params }
}
