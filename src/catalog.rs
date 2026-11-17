//! TEMPORARY STUB with the contract signatures. Replaced by agent A's catalog module.
use serde_json::Value;

pub struct Operation {
    pub ids: Vec<String>,
    pub kind: String,
    pub verified: bool,
    pub response: Option<String>,
    pub endpoint: String,
    pub min_protocol: Option<i64>,
}

pub fn catalog() -> &'static Value {
    unimplemented!()
}

pub fn operation(_class: &str) -> Option<&'static Operation> {
    unimplemented!()
}

pub fn operations() -> impl Iterator<Item = (&'static str, &'static Operation)> {
    std::iter::empty()
}

pub fn enum_name(_enum_name: &str, _value: i64) -> Option<&'static str> {
    unimplemented!()
}

pub fn enum_value(_enum_name: &str, _name: &str) -> Option<i64> {
    unimplemented!()
}

pub fn message_type(_class: &str) -> Option<&'static str> {
    unimplemented!()
}

pub fn trigger_parameters_class(_type_id: i64, _version: i64) -> Option<&'static str> {
    unimplemented!()
}
