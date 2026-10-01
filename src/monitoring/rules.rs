//! Deterministic bounded comparisons; a matching rule denotes health.
use super::MonitorError;
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MAX_SAFE_NUMBER: f64 = 9_007_199_254_740_992.0;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonitorRule {
    /// JSON pointer into each sample's value; comparison true means healthy.
    pub pointer: String,
    pub operator: RuleOperator,
    pub value: Value,
    pub failure_samples: u32,
    pub success_samples: u32,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleOperator {
    Eq,
    Ne,
    Gt,
    Ge,
    Lt,
    Le,
}

impl MonitorRule {
    pub(super) fn validate(&self) -> Result<(), MonitorError> {
        let invalid = || {
            MonitorError::Configuration(
                "rule requires a valid JSON pointer and bounded bool/number/string comparison"
                    .into(),
            )
        };
        if self.pointer.len() > 1024 || (!self.pointer.is_empty() && !self.pointer.starts_with('/'))
        {
            return Err(invalid());
        }
        let mut chars = self.pointer.chars();
        while let Some(ch) = chars.next() {
            if ch == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return Err(invalid());
            }
        }
        match &self.value {
            Value::Bool(_) => {}
            Value::String(value) if value.len() <= 4096 => {}
            Value::Number(value) if bounded_number(value).is_some() => {}
            _ => return Err(invalid()),
        }
        if !matches!(self.operator, RuleOperator::Eq | RuleOperator::Ne) && !self.value.is_number()
        {
            return Err(invalid());
        }
        if serde_json::to_vec(self).map_or(true, |value| value.len() > 4096) {
            return Err(invalid());
        }
        Ok(())
    }

    pub(super) fn evaluate(&self, value: &Value) -> Result<bool, MonitorError> {
        let observed = value
            .pointer(&self.pointer)
            .ok_or_else(|| MonitorError::Observation("rule field is missing".into()))?;
        let ordering = match (&self.value, observed) {
            (Value::Bool(expected), Value::Bool(actual)) => actual.partial_cmp(expected),
            (Value::String(expected), Value::String(actual)) if actual.len() <= 4096 => {
                actual.partial_cmp(expected)
            }
            (Value::Number(expected), Value::Number(actual)) => {
                let expected = bounded_number(expected);
                let actual = bounded_number(actual);
                actual.zip(expected).and_then(|(a, b)| a.partial_cmp(&b))
            }
            _ => None,
        }
        .ok_or_else(|| {
            MonitorError::Observation("rule field has an invalid type or number range".into())
        })?;
        Ok(match self.operator {
            RuleOperator::Eq => ordering.is_eq(),
            RuleOperator::Ne => !ordering.is_eq(),
            RuleOperator::Gt => ordering.is_gt(),
            RuleOperator::Ge => !ordering.is_lt(),
            RuleOperator::Lt => ordering.is_lt(),
            RuleOperator::Le => !ordering.is_gt(),
        })
    }
}

fn safe_number(number: f64) -> bool {
    number.is_finite() && number.abs() <= MAX_SAFE_NUMBER
}
fn bounded_number(number: &serde_json::Number) -> Option<f64> {
    if number
        .as_u64()
        .is_some_and(|value| value > 9_007_199_254_740_992)
        || number
            .as_i64()
            .is_some_and(|value| value.unsigned_abs() > 9_007_199_254_740_992)
    {
        return None;
    }
    number.as_f64().filter(|number| safe_number(*number))
}
