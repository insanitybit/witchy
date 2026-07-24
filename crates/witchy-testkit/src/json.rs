use std::collections::BTreeMap;
use std::fmt;

use serde::de::{MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer};

use crate::{FixturePlan, PlanValidationError, PlanValidationLimits};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanDecodeError {
    message: String,
}

impl PlanDecodeError {
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for PlanDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PlanDecodeError {}

impl From<serde_json::Error> for PlanDecodeError {
    fn from(error: serde_json::Error) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

impl From<PlanValidationError> for PlanDecodeError {
    fn from(error: PlanValidationError) -> Self {
        Self {
            message: error.to_string(),
        }
    }
}

pub fn parse_fixture_plan(bytes: &[u8]) -> Result<FixturePlan, PlanDecodeError> {
    let limits = PlanValidationLimits::default();
    if bytes.len() > limits.max_json_bytes {
        return Err(PlanDecodeError {
            message: format!(
                "fixture plan is {} bytes; limit is {}",
                bytes.len(),
                limits.max_json_bytes
            ),
        });
    }
    let value: UniqueValue = serde_json::from_slice(bytes)?;
    let plan: FixturePlan = serde_json::from_value(value.into_json())?;
    plan.validate_with(&limits)?;
    Ok(plan)
}

pub fn canonical_plan_json(plan: &FixturePlan) -> Result<String, PlanDecodeError> {
    plan.validate_with(&PlanValidationLimits::default())?;
    serde_json::to_string(plan).map_err(Into::into)
}

enum UniqueValue {
    Null,
    Bool(bool),
    I64(i64),
    U64(u64),
    F64(f64),
    String(String),
    Array(Vec<Self>),
    Object(BTreeMap<String, Self>),
}

impl UniqueValue {
    fn into_json(self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(value) => serde_json::Value::Bool(value),
            Self::I64(value) => value.into(),
            Self::U64(value) => value.into(),
            Self::F64(value) => serde_json::Number::from_f64(value)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Self::String(value) => serde_json::Value::String(value),
            Self::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(Self::into_json).collect())
            }
            Self::Object(values) => serde_json::Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, value.into_json()))
                    .collect(),
            ),
        }
    }
}

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueValueVisitor)
    }
}

struct UniqueValueVisitor;

impl<'de> Visitor<'de> for UniqueValueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue::Null)
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue::I64(value))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue::U64(value))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E> {
        Ok(UniqueValue::F64(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(UniqueValue::String(value.to_owned()))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue::String(value))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element()? {
            values.push(value);
        }
        Ok(UniqueValue::Array(values))
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = BTreeMap::new();
        while let Some((key, value)) = map.next_entry::<String, UniqueValue>()? {
            if values.insert(key.clone(), value).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "duplicate object key `{key}`"
                )));
            }
        }
        Ok(UniqueValue::Object(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_keys_are_rejected_at_every_depth() {
        let error =
            parse_fixture_plan(br#"{"version":1,"env":{"values":{"A":"1","A":"2"}}}"#)
                .expect_err("duplicate map key must fail");
        assert!(error.message().contains("duplicate object key `A`"), "{error}");
    }

    #[test]
    fn unknown_fields_and_raw_net_are_rejected() {
        let unknown = parse_fixture_plan(br#"{"version":1,"typo":true}"#)
            .expect_err("unknown field must fail");
        assert!(unknown.message().contains("unknown field `typo`"), "{unknown}");

        let net = parse_fixture_plan(br#"{"version":1,"net":{}}"#)
            .expect_err("raw Net fixture must fail");
        assert!(net.message().contains("unknown field `net`"), "{net}");
    }

    #[test]
    fn canonical_json_sorts_maps_and_uses_decimal_strings() {
        let plan = parse_fixture_plan(
            br#"{"version":1,"env":{"values":{"Z":"last","A":"first"},"allow":["A"]},"rand":{"seed":"9"}}"#,
        )
        .expect("valid plan");
        assert_eq!(
            canonical_plan_json(&plan).expect("canonical plan"),
            r#"{"version":1,"rand":{"seed":"9","script":[]},"env":{"values":{"A":"first","Z":"last"},"allow":["A"],"script":[]},"expectations":{"require_complete_scripts":false,"calls":[],"absent_families":[]}}"#
        );
    }
}
