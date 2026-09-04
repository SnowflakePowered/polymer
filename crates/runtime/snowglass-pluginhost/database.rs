use deno_core::{OpState, op2};
use deno_error::JsErrorBox;
use serde_json::Map;
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Mutex};
use turso::Value as SqlValue;

pub(crate) struct DatabaseOptions {
    path: PathBuf,
}

impl DatabaseOptions {
    pub(crate) fn for_plugin(directories: &snowglass_library::Directories, plugin: &str) -> Self {
        Self {
            path: directories.plugin_database(plugin),
        }
    }
}

struct DatabaseState {
    path: PathBuf,
    connection: Mutex<Option<turso::Connection>>,
}

fn database_error(error: impl std::fmt::Display) -> JsErrorBox {
    JsErrorBox::generic(error.to_string())
}

impl DatabaseState {
    fn from_state(state: &Rc<RefCell<OpState>>) -> Rc<Self> {
        state.borrow().borrow::<Rc<Self>>().clone()
    }

    fn connection(&self) -> Result<turso::Connection, JsErrorBox> {
        self.connection
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| JsErrorBox::generic("plugin database is closed"))
    }

    fn values(value: serde_json::Value) -> Result<Vec<SqlValue>, JsErrorBox> {
        let serde_json::Value::Array(values) = value else {
            return Err(JsErrorBox::type_error("SQL parameters must be an array"));
        };
        values
            .into_iter()
            .map(|value| match value {
                serde_json::Value::Null => Ok(SqlValue::Null),
                serde_json::Value::Bool(value) => Ok(SqlValue::Integer(i64::from(value))),
                serde_json::Value::Number(value) if value.is_i64() => {
                    Ok(SqlValue::Integer(value.as_i64().unwrap()))
                }
                serde_json::Value::Number(value) if value.is_f64() => {
                    Ok(SqlValue::Real(value.as_f64().unwrap()))
                }
                serde_json::Value::String(value) => Ok(SqlValue::Text(value)),
                serde_json::Value::Array(values)
                    if values
                        .iter()
                        .all(|value| value.as_u64().is_some_and(|value| value <= 255)) =>
                {
                    Ok(SqlValue::Blob(
                        values
                            .into_iter()
                            .map(|value| value.as_u64().unwrap() as u8)
                            .collect(),
                    ))
                }
                _ => Err(JsErrorBox::type_error("unsupported SQL parameter value")),
            })
            .collect()
    }

    fn json(value: SqlValue) -> serde_json::Value {
        match value {
            SqlValue::Null => serde_json::Value::Null,
            SqlValue::Integer(value) => serde_json::Value::from(value),
            SqlValue::Real(value) => serde_json::Value::from(value),
            SqlValue::Text(value) => serde_json::Value::String(value),
            SqlValue::Blob(value) => {
                serde_json::Value::Array(value.into_iter().map(serde_json::Value::from).collect())
            }
        }
    }
}

#[op2]
async fn op_database_connect(state: Rc<RefCell<OpState>>) -> Result<(), JsErrorBox> {
    let state = DatabaseState::from_state(&state);
    if state.connection.lock().unwrap().is_some() {
        return Ok(());
    }
    if let Some(parent) = state.path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(database_error)?;
    }
    let database = turso::Builder::new_local(
        state
            .path
            .to_str()
            .ok_or_else(|| JsErrorBox::type_error("plugin database path is not UTF-8"))?,
    )
    .build()
    .await
    .map_err(database_error)?;
    *state.connection.lock().unwrap() = Some(database.connect().map_err(database_error)?);
    Ok(())
}

#[op2(fast)]
fn op_database_close(state: &mut OpState) {
    state
        .borrow::<Rc<DatabaseState>>()
        .connection
        .lock()
        .unwrap()
        .take();
}

#[op2]
async fn op_database_exec(
    state: Rc<RefCell<OpState>>,
    #[string] sql: String,
) -> Result<(), JsErrorBox> {
    DatabaseState::from_state(&state)
        .connection()?
        .execute_batch(sql)
        .await
        .map_err(database_error)
}

#[op2]
#[serde]
async fn op_database_run(
    state: Rc<RefCell<OpState>>,
    #[string] sql: String,
    #[serde] params: serde_json::Value,
) -> Result<serde_json::Value, JsErrorBox> {
    let values = DatabaseState::values(params)?;
    let connection = DatabaseState::from_state(&state).connection()?;
    let changed = connection
        .execute(sql, values)
        .await
        .map_err(database_error)?;
    Ok(serde_json::json!({
        "changes": changed,
        "lastInsertRowid": connection.last_insert_rowid()
    }))
}

#[op2]
#[serde]
async fn op_database_all(
    state: Rc<RefCell<OpState>>,
    #[string] sql: String,
    #[serde] params: serde_json::Value,
) -> Result<Vec<serde_json::Value>, JsErrorBox> {
    let values = DatabaseState::values(params)?;
    let connection = DatabaseState::from_state(&state).connection()?;
    let mut rows = connection
        .query(sql, values)
        .await
        .map_err(database_error)?;
    let columns = rows.column_names();
    let mut result = Vec::new();
    while let Some(row) = rows.next().await.map_err(database_error)? {
        let mut value = Map::new();
        for (index, column) in columns.iter().enumerate() {
            value.insert(
                column.clone(),
                DatabaseState::json(row.get_value(index).map_err(database_error)?),
            );
        }
        result.push(serde_json::Value::Object(value));
    }
    Ok(result)
}

deno_core::extension!(snowglass_database,
    ops = [op_database_connect, op_database_close, op_database_exec, op_database_run, op_database_all],
    esm_entry_point = "ext:snowglass_database/database.js",
    esm = [dir "js", "database.js"],
    options = { options: DatabaseOptions },
    state = |state, options| {
        state.put(Rc::new(DatabaseState {
            path: options.options.path,
            connection: Mutex::new(None),
        }));
    },
);

pub(crate) fn extension(options: DatabaseOptions) -> deno_core::Extension {
    snowglass_database::init(options)
}
