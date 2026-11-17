//! TEMPORARY STUB with the contract signatures. Replaced by agent A's codec module.
use serde_json::Value;
use std::fmt;

#[derive(Debug)]
pub struct CodecError(pub String);

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodecError {}

pub fn encode(_type_name: &str, _value: &Value) -> Result<Vec<u8>, CodecError> {
    unimplemented!()
}

pub fn decode(_type_name: &str, _data: &[u8]) -> Result<Value, CodecError> {
    unimplemented!()
}

pub fn encode_message(_class: &str, _fields: &Value, _requests_id: i64) -> Result<Vec<u8>, CodecError> {
    unimplemented!()
}

pub fn decode_message(_data: &[u8]) -> Result<Value, CodecError> {
    unimplemented!()
}

pub fn ack_stamp(_message: &Value) -> i64 {
    unimplemented!()
}
