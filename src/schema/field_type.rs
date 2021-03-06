use schema::TextOptions;
use schema::U32Options;

use rustc_serialize::json::Json;
use schema::Value;


/// Possible error that may occur while parsing a field value
/// At this point the JSON is known to be valid.
#[derive(Debug)]
pub enum ValueParsingError {
    /// Encounterred a numerical value that overflows or underflow its integer type.
    OverflowError(String),
    /// The json node is not of the correct type. (e.g. 3 for a `Str` type or `"abc"` for a u32 type)
    /// Tantivy will try to autocast values.  
    TypeError(String),
}


/// A `FieldType` describes the type (text, u32) of a field as well as 
/// how it should be handled by tantivy.
#[derive(Clone, Debug, RustcDecodable, RustcEncodable)]
pub enum FieldType {
    /// String field type configuration
    Str(TextOptions),
    /// U32 field type configuration
    U32(U32Options),
}

impl FieldType {
    
    /// Parses a field value from json, given the target FieldType.
    ///
    /// Tantivy will not try to cast values.
    /// For instance, If the json value is the integer `3` and the 
    /// target field is a `Str`, this method will return an Error. 
    pub fn value_from_json(&self, json: &Json) -> Result<Value, ValueParsingError> {
        match *json {
            Json::String(ref field_text) => {
                match *self {
                    FieldType::Str(_) => {
                        Ok(Value::Str(field_text.clone()))
                    }
                    FieldType::U32(_) => {
                        Err(ValueParsingError::TypeError(format!("Expected a u32 int, got {:?}", json)))
                    }
                }
            }
            Json::U64(ref field_val_u64) => {
                match *self {
                    FieldType::U32(_) => {
                        if *field_val_u64 > (u32::max_value() as u64) {
                            Err(ValueParsingError::OverflowError(format!("Expected u32, but value {:?} overflows.", field_val_u64)))
                        }
                        else {
                            Ok(Value::U32(*field_val_u64 as u32))
                        }
                    }
                    _ => {
                        Err(ValueParsingError::TypeError(format!("Expected a string, got {:?}", json)))
                    }
                }
            },
            _ => {
                Err(ValueParsingError::TypeError(format!("Expected a string or a u32, got {:?}", json)))
            }
        }
    }
}