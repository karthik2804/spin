/// Serializable representation of wasmtime::component::Val for postcard.
/// This enum MUST be identical in both the host factor and the plugin
/// for postcard serialization to work correctly.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum PluginVal {
    Bool(bool),
    S8(i8),
    U8(u8),
    S16(i16),
    U16(u16),
    S32(i32),
    U32(u32),
    S64(i64),
    U64(u64),
    Float32(f32),
    Float64(f64),
    Char(u32),
    String(Vec<u8>),
    List(Vec<PluginVal>),
    Record(Vec<(String, PluginVal)>),
    Tuple(Vec<PluginVal>),
    Variant(String, Option<Box<PluginVal>>),
    Enum(String),
    Option(Option<Box<PluginVal>>),
    /// Result(Ok_value, Err_value) - one is Some, the other is None
    Result(Option<Box<PluginVal>>, Option<Box<PluginVal>>),
    Flags(Vec<String>),
    /// Unit for empty results (e.g., result<(), E>)
    Unit,
}

impl PluginVal {
    /// Create a String variant
    pub fn string(s: &str) -> Self {
        Self::String(s.as_bytes().to_vec())
    }

    /// Create a Result::Ok with a String value
    pub fn result_ok_string(s: &str) -> Self {
        Self::Result(Some(Box::new(Self::String(s.as_bytes().to_vec()))), None)
    }

    /// Create a Result::Ok with an Option<String> value
    pub fn result_ok_option_string(s: Option<&str>) -> Self {
        Self::Result(
            Some(Box::new(Self::Option(
                s.map(|v| Box::new(Self::String(v.as_bytes().to_vec()))),
            ))),
            None,
        )
    }

    /// Create a Result::Ok with a bool value
    pub fn result_ok_bool(b: bool) -> Self {
        Self::Result(Some(Box::new(Self::Bool(b))), None)
    }

    /// Create a Result::Err with a String error
    pub fn result_err_string(s: &str) -> Self {
        Self::Result(None, Some(Box::new(Self::String(s.as_bytes().to_vec()))))
    }

    /// Create a Result::Ok with Unit value
    pub fn result_ok_unit() -> Self {
        Self::Result(Some(Box::new(Self::Unit)), None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_postcard_roundtrip() {
        // Verify Vec<PluginVal> serialization uses length prefix
        let vals = vec![PluginVal::result_ok_unit()];
        let bytes = postcard::to_allocvec(&vals).unwrap();
        assert_eq!(bytes[0], 1, "Vec length prefix should be 1");

        // Verify round-trip for Vec<PluginVal>
        let roundtrip: Vec<PluginVal> = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(roundtrip.len(), 1);

        // Verify all helper constructors produce serializable values
        for val in [
            PluginVal::Bool(true),
            PluginVal::S8(42),
            PluginVal::U8(42),
            PluginVal::S16(42),
            PluginVal::U16(42),
            PluginVal::S32(42),
            PluginVal::U32(42),
            PluginVal::S64(42),
            PluginVal::U64(42),
            PluginVal::Float32(1.5),
            PluginVal::Float64(1.5),
            PluginVal::Char(65),
            PluginVal::string("hello"),
            PluginVal::List(vec![PluginVal::U8(1), PluginVal::U8(2)]),
            PluginVal::Record(vec![("key".to_string(), PluginVal::U8(1))]),
            PluginVal::Tuple(vec![PluginVal::U8(1), PluginVal::Bool(true)]),
            PluginVal::Variant("some".to_string(), Some(Box::new(PluginVal::U8(1)))),
            PluginVal::Enum("active".to_string()),
            PluginVal::Option(None),
            PluginVal::Option(Some(Box::new(PluginVal::U8(42)))),
            PluginVal::result_ok_unit(),
            PluginVal::result_ok_bool(true),
            PluginVal::result_ok_string("ok value"),
            PluginVal::result_ok_option_string(Some("option value")),
            PluginVal::result_ok_option_string(None),
            PluginVal::result_err_string("error message"),
            PluginVal::Flags(vec!["read".to_string(), "write".to_string()]),
            PluginVal::Unit,
        ] {
            let bytes = postcard::to_allocvec(&val).unwrap();
            let _roundtrip: PluginVal = postcard::from_bytes(&bytes).unwrap();
        }

        // Verify Vec<Vec<PluginVal>> round-trip (plugin pattern)
        let result_val = PluginVal::result_err_string("test error");
        let results = postcard::to_allocvec(&vec![result_val]).unwrap();
        let roundtrip: Vec<PluginVal> = postcard::from_bytes(&results).unwrap();
        assert_eq!(roundtrip.len(), 1);
    }
}
