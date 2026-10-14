//! 数据库与 JSON 的小工具。行转 JSON 时保留 SQLite 的整数、实数、文本与空值，与 Python sqlite3 的行为一致。

use rusqlite::{Connection, Row, params_from_iter, types::Value as SqlValue, types::ValueRef};
use serde_json::{Map, Number, Value};

use crate::{error::Failure, num::Num};

pub type Object = Map<String, Value>;

/// Python 真值判断：None、False、0、空字符串、空容器均为假。
pub fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().is_some_and(|number| number != 0.0),
        Some(Value::String(text)) => !text.is_empty(),
        Some(Value::Array(items)) => !items.is_empty(),
        Some(Value::Object(items)) => !items.is_empty(),
    }
}

pub fn num_of(value: Option<&Value>) -> Option<Num> {
    value.and_then(Num::from_json)
}

pub fn num_field(object: &Object, key: &str) -> Option<Num> {
    num_of(object.get(key))
}

pub fn str_field<'a>(object: &'a Object, key: &str) -> Option<&'a str> {
    object.get(key).and_then(Value::as_str)
}

/// `dict[key] is None`：缺失或为 null。
pub fn is_null_or_missing(object: &Object, key: &str) -> bool {
    object.get(key).is_none_or(Value::is_null)
}

pub fn set(object: &mut Object, key: &str, value: Value) {
    object.insert(key.to_owned(), value);
}

pub fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.to_owned())
}

pub fn sql_of_num(value: Num) -> SqlValue {
    match value {
        Num::Int(value) => SqlValue::Integer(value),
        Num::Float(value) => SqlValue::Real(value),
    }
}

/// 把 JSON 值绑定为 SQL 参数：布尔按整数绑定，与 Python sqlite3 相同。
pub fn sql_of(value: &Value) -> SqlValue {
    match value {
        Value::Null => SqlValue::Null,
        Value::Bool(flag) => SqlValue::Integer(i64::from(*flag)),
        Value::Number(number) => number.as_i64().map_or_else(
            || SqlValue::Real(number.as_f64().unwrap_or(f64::NAN)),
            SqlValue::Integer,
        ),
        Value::String(value) => SqlValue::Text(value.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

/// Python `int(value)` 的常用输入：整数、浮点（截断）、布尔与数字文本。
pub fn python_int(value: &Value) -> Result<i64, Failure> {
    match value {
        Value::Number(number) => match number.as_i64() {
            Some(integer) => Ok(integer),
            None => number
                .as_f64()
                .map(|float| float.trunc() as i64)
                .ok_or_else(|| Failure::Invalid("整数参数无效".to_owned())),
        },
        Value::Bool(flag) => Ok(i64::from(*flag)),
        Value::String(text) => text.trim().parse::<i64>().map_err(|_| {
            Failure::Invalid(format!("invalid literal for int() with base 10: '{text}'"))
        }),
        _ => Err(Failure::Invalid("整数参数无效".to_owned())),
    }
}

/// 解析存储的 JSON 对象文本。
pub fn decode_object(text: &str) -> Result<Object, Failure> {
    match serde_json::from_str::<Value>(text)? {
        Value::Object(object) => Ok(object),
        _ => Err(Failure::Internal("存储的 JSON 不是对象".to_owned())),
    }
}

pub fn decode_value(text: &str) -> Result<Value, Failure> {
    Ok(serde_json::from_str::<Value>(text)?)
}

pub fn encode(value: &Value) -> String {
    value.to_string()
}

fn column(row: &Row<'_>, index: usize) -> rusqlite::Result<Value> {
    Ok(match row.get_ref(index)? {
        ValueRef::Null | ValueRef::Blob(_) => Value::Null,
        ValueRef::Integer(value) => Value::from(value),
        ValueRef::Real(value) => Number::from_f64(value).map_or(Value::Null, Value::Number),
        ValueRef::Text(bytes) => Value::String(String::from_utf8_lossy(bytes).into_owned()),
    })
}

/// 执行查询并把每行转为以列名为键的对象（等价于 Python 的 `dict(row)`）。
pub fn rows(db: &Connection, sql: &str, args: Vec<SqlValue>) -> Result<Vec<Object>, Failure> {
    bounded(db, sql, args, usize::MAX, "")
}

/// 带上限的查询：超过 `maximum` 行时返回与 Python 相同的参数错误。
pub fn bounded(
    db: &Connection,
    sql: &str,
    args: Vec<SqlValue>,
    maximum: usize,
    overflow_message: &str,
) -> Result<Vec<Object>, Failure> {
    let mut statement = db.prepare_cached(sql)?;
    let names: Vec<String> = statement
        .column_names()
        .into_iter()
        .map(str::to_owned)
        .collect();
    let mut cursor = statement.query(params_from_iter(args))?;
    let mut out = Vec::new();
    while let Some(row) = cursor.next()? {
        if out.len() == maximum {
            return Err(Failure::Invalid(overflow_message.to_owned()));
        }
        let mut object = Object::new();
        for (index, name) in names.iter().enumerate() {
            object.insert(name.clone(), column(row, index)?);
        }
        out.push(object);
    }
    Ok(out)
}

pub fn one(db: &Connection, sql: &str, args: Vec<SqlValue>) -> Result<Option<Object>, Failure> {
    Ok(rows(db, sql, args)?.into_iter().next())
}

pub fn exec(db: &Connection, sql: &str, args: Vec<SqlValue>) -> Result<usize, Failure> {
    Ok(db.prepare_cached(sql)?.execute(params_from_iter(args))?)
}

/// 生成 ` WHERE a AND b`；没有条件时为空串。
pub fn where_clause(parts: &[&str]) -> String {
    if parts.is_empty() {
        String::new()
    } else {
        format!(" WHERE {}", parts.join(" AND "))
    }
}

/// 与 Python `dict.update` 的合并顺序一致：后者覆盖前者。
pub fn merged(mut base: Object, extra: Object) -> Object {
    for (key, value) in extra {
        base.insert(key, value);
    }
    base
}
