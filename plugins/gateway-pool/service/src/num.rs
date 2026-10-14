//! 与 Python 原实现一致的数值语义。整数与浮点分别保留，使 JSON 输出的 int/float 与原实现相同；
//! `max`/`min` 相等时返回第一个参数；`x or 0` 的假值替换由调用方用 `truthy` 表达。

use serde_json::Value;

#[derive(Clone, Copy, Debug)]
pub enum Num {
    Int(i64),
    Float(f64),
}

impl Num {
    /// 只接受 JSON 数字；布尔值不是数字（与 Python 中 `isinstance(x, (int, float))` 的排除一致）。
    pub fn from_json(value: &Value) -> Option<Self> {
        match value {
            Value::Number(number) => number
                .as_i64()
                .map(Self::Int)
                .or_else(|| number.as_f64().map(Self::Float)),
            _ => None,
        }
    }

    pub fn to_json(self) -> Value {
        match self {
            Self::Int(value) => Value::from(value),
            Self::Float(value) => {
                serde_json::Number::from_f64(value).map_or(Value::Null, Value::Number)
            }
        }
    }

    pub fn as_f64(self) -> f64 {
        match self {
            Self::Int(value) => value as f64,
            Self::Float(value) => value,
        }
    }

    pub fn truthy(self) -> bool {
        match self {
            Self::Int(value) => value != 0,
            Self::Float(value) => value != 0.0,
        }
    }

    pub fn plus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_add(b)
                .map_or_else(|| Self::Float(a as f64 + b as f64), Self::Int),
            _ => Self::Float(self.as_f64() + other.as_f64()),
        }
    }

    pub fn minus(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_sub(b)
                .map_or_else(|| Self::Float(a as f64 - b as f64), Self::Int),
            _ => Self::Float(self.as_f64() - other.as_f64()),
        }
    }

    pub fn times(self, other: Self) -> Self {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a
                .checked_mul(b)
                .map_or_else(|| Self::Float(a as f64 * b as f64), Self::Int),
            _ => Self::Float(self.as_f64() * other.as_f64()),
        }
    }

    /// Python 的真除法总是得到浮点数。
    pub fn over(self, other: Self) -> Self {
        Self::Float(self.as_f64() / other.as_f64())
    }

    pub fn lt(self, other: Self) -> bool {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a < b,
            _ => self.as_f64() < other.as_f64(),
        }
    }

    pub fn le(self, other: Self) -> bool {
        !other.lt(self)
    }

    pub fn gt(self, other: Self) -> bool {
        other.lt(self)
    }

    pub fn ge(self, other: Self) -> bool {
        !self.lt(other)
    }

    /// Python `max(self, other)`：只有 other 严格更大时才取 other。
    pub fn max(self, other: Self) -> Self {
        if other.gt(self) { other } else { self }
    }

    /// Python `min(self, other)`：只有 other 严格更小时才取 other。
    pub fn min(self, other: Self) -> Self {
        if other.lt(self) { other } else { self }
    }

    /// Python `sum()`：从整数 0 开始逐项相加，全部为空时结果是整数 0。
    pub fn sum<I: IntoIterator<Item = Self>>(items: I) -> Self {
        items.into_iter().fold(Self::Int(0), Self::plus)
    }
}
