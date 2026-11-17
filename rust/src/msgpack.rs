//! A minimal MessagePack writer for the signed image of an action.
//!
//! The node converts the received JSON to MessagePack in received key order:
//! nonnegative integers use the most compact uint encoding, strings use `str`,
//! booleans use `bool`, and numbers with a decimal point use `float64`.

use serde_json::Value;

use crate::error::Error;

/// Encodes a JSON value (with insertion-ordered maps) as MessagePack.
pub(crate) fn encode(value: &Value) -> Result<Vec<u8>, Error> {
    let mut out = Vec::new();
    write(&mut out, value)?;
    Ok(out)
}

fn write(out: &mut Vec<u8>, value: &Value) -> Result<(), Error> {
    match value {
        Value::Null => out.push(0xc0),
        Value::Bool(false) => out.push(0xc2),
        Value::Bool(true) => out.push(0xc3),
        Value::Number(n) => {
            if let Some(u) = n.as_u64() {
                write_uint(out, u);
            } else if let Some(i) = n.as_i64() {
                write_int(out, i);
            } else if let Some(f) = n.as_f64() {
                out.push(0xcb);
                out.extend_from_slice(&f.to_be_bytes());
            } else {
                return Err(Error::InvalidInput("unsupported number in action".into()));
            }
        }
        Value::String(s) => write_str(out, s)?,
        Value::Array(items) => {
            write_len(out, items.len(), 0x90, None, 0xdc, 0xdd)?;
            for item in items {
                write(out, item)?;
            }
        }
        Value::Object(map) => {
            write_len(out, map.len(), 0x80, None, 0xde, 0xdf)?;
            for (key, item) in map {
                write_str(out, key)?;
                write(out, item)?;
            }
        }
    }
    Ok(())
}

fn write_uint(out: &mut Vec<u8>, v: u64) {
    match v {
        0..=0x7f => out.push(v as u8),
        0x80..=0xff => out.extend_from_slice(&[0xcc, v as u8]),
        0x100..=0xffff => {
            out.push(0xcd);
            out.extend_from_slice(&(v as u16).to_be_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xce);
            out.extend_from_slice(&(v as u32).to_be_bytes());
        }
        _ => {
            out.push(0xcf);
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
}

fn write_int(out: &mut Vec<u8>, v: i64) {
    // Called only for negative values (nonnegative ones are u64 in serde_json).
    match v {
        -32..=-1 => out.push(v as i8 as u8),
        -128..=-33 => out.extend_from_slice(&[0xd0, v as i8 as u8]),
        -32768..=-129 => {
            out.push(0xd1);
            out.extend_from_slice(&(v as i16).to_be_bytes());
        }
        -2_147_483_648..=-32769 => {
            out.push(0xd2);
            out.extend_from_slice(&(v as i32).to_be_bytes());
        }
        _ => {
            out.push(0xd3);
            out.extend_from_slice(&v.to_be_bytes());
        }
    }
}

fn write_str(out: &mut Vec<u8>, s: &str) -> Result<(), Error> {
    write_len(out, s.len(), 0xa0, Some(0xd9), 0xda, 0xdb)?;
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

/// Writes a length header. `fix` is the fixed-length prefix (max 31 for str,
/// 15 for array and map); `b8`, `b16`, `b32` are the wider markers.
fn write_len(
    out: &mut Vec<u8>,
    len: usize,
    fix: u8,
    b8: Option<u8>,
    b16: u8,
    b32: u8,
) -> Result<(), Error> {
    let fix_max = if fix == 0xa0 { 31 } else { 15 };
    let too_long = || Error::InvalidInput("action too large to encode".into());
    if len <= fix_max {
        out.push(fix | len as u8);
    } else if let (Some(marker), true) = (b8, len <= 0xff) {
        out.extend_from_slice(&[marker, len as u8]);
    } else if len <= 0xffff {
        out.push(b16);
        out.extend_from_slice(&(len as u16).to_be_bytes());
    } else {
        let len = u32::try_from(len).map_err(|_| too_long())?;
        out.push(b32);
        out.extend_from_slice(&len.to_be_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(json: &str) -> String {
        hex::encode(encode(&serde_json::from_str::<Value>(json).unwrap()).unwrap())
    }

    #[test]
    fn compact_uints_and_scalars() {
        assert_eq!(
            enc("[0,127,128,255,256,65535,65536,4294967295,4294967296]"),
            "99007fcc80ccffcd0100cdffffce00010000ceffffffffcf0000000100000000"
        );
        assert_eq!(enc("[true,false,null]"), "93c3c2c0");
        assert_eq!(enc("[-1,-32,-33,-129]"), "94ffe0d0dfd1ff7f");
        assert_eq!(enc("1.5"), "cb3ff8000000000000");
    }

    #[test]
    fn strings_keep_exact_text_and_key_order() {
        assert_eq!(
            enc(r#"{"b":"52.000","a":"52"}"#),
            "82a162a63532 2e303030a161a23532".replace(' ', "")
        );
        let long = "x".repeat(40);
        assert_eq!(&enc(&format!("\"{long}\""))[..4], "d928");
    }
}
