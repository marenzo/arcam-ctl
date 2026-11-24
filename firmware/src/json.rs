//! Just enough JSON: string escaping for the fixed shapes this firmware
//! writes, and a reader for one flat object of strings, numbers and
//! booleans (a command or the settings). Also the HTML form encoding.

use core::fmt::{Display, Write};

/// `s` as a JSON string, quotes included.
pub fn string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{:04x}", u32::from(c));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A value, or `null`.
pub fn opt<T: Display>(v: Option<T>) -> String {
    v.map_or_else(|| "null".into(), |v| v.to_string())
}

/// A string, or `null`.
pub fn opt_str(v: Option<&str>) -> String {
    v.map_or_else(|| "null".into(), string)
}

/// The fields of one flat JSON object, each value in text form: a string
/// unescaped, a number or `true`/`false` as written. `None` when `text` is
/// not such an object; a `null` is no value, so it is refused too.
pub fn fields(text: &str) -> Option<Vec<(String, String)>> {
    use serde::de::{Deserializer, Error, MapAccess, Visitor};
    use serde_json::value::RawValue;

    struct Fields;
    impl<'de> Visitor<'de> for Fields {
        type Value = Vec<(String, String)>;

        fn expecting(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.write_str("a flat object with unique keys and scalar values")
        }

        fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut out = Vec::new();
            while let Some((key, raw)) = map.next_entry::<String, &RawValue>()? {
                let token = raw.get();
                let value = match token.as_bytes()[0] {
                    b'"' => serde_json::from_str(token).map_err(M::Error::custom)?,
                    b'-' | b'0'..=b'9' | b't' | b'f' => token.to_owned(),
                    _ => return Err(M::Error::custom("expected a string, number or boolean")),
                };
                if out.iter().any(|(k, _)| k == &key) {
                    return Err(M::Error::custom("duplicate key"));
                }
                out.push((key, value));
            }
            Ok(out)
        }
    }

    let mut reader = serde_json::Deserializer::from_str(text);
    let fields = reader.deserialize_map(Fields).ok()?;
    reader.end().ok()?;
    Some(fields)
}

/// Decode the entire form before applying any settings.
pub fn form(body: &str) -> Option<Vec<(String, String)>> {
    let mut fields = Vec::new();
    for pair in body.split('&') {
        let (key, value) = pair.split_once('=')?;
        let key = unescape(key)?;
        if key.is_empty() || fields.iter().any(|(k, _)| k == &key) {
            return None;
        }
        fields.push((key, unescape(value)?));
    }
    Some(fields)
}

/// `+` and `%XX` decoded; `None` when the result is not UTF-8.
fn unescape(s: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(s.len());
    let mut it = s.bytes();
    while let Some(b) = it.next() {
        bytes.push(match b {
            b'+' => b' ',
            b'%' => {
                let hex = [it.next()?, it.next()?];
                u8::from_str_radix(core::str::from_utf8(&hex).ok()?, 16).ok()?
            }
            b => b,
        });
    }
    String::from_utf8(bytes).ok()
}
