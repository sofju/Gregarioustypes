//! Minimal bencode parser that keeps the raw byte span of dictionaries (needed to
//! hash the `info` dictionary exactly as stored).

use std::ops::Range;

#[derive(Debug, Clone)]
pub enum Value<'a> {
    Int(i64),
    Bytes(&'a [u8]),
    List(Vec<Value<'a>>),
    Dict(Vec<(&'a [u8], Value<'a>)>, Range<usize>),
}

impl<'a> Value<'a> {
    pub fn get(&self, key: &str) -> Option<&Value<'a>> {
        match self {
            Value::Dict(entries, _) => entries.iter().find(|(k, _)| *k == key.as_bytes()).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i),
            _ => None,
        }
    }
    pub fn bytes(&self) -> Option<&'a [u8]> {
        match self {
            Value::Bytes(b) => Some(b),
            _ => None,
        }
    }
    pub fn str(&self) -> Option<String> {
        self.bytes().map(|b| String::from_utf8_lossy(b).into_owned())
    }
    pub fn list(&self) -> Option<&[Value<'a>]> {
        match self {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
    pub fn dict(&self) -> Option<&[(&'a [u8], Value<'a>)]> {
        match self {
            Value::Dict(d, _) => Some(d),
            _ => None,
        }
    }
    pub fn span(&self) -> Option<Range<usize>> {
        match self {
            Value::Dict(_, r) => Some(r.clone()),
            _ => None,
        }
    }
}

pub struct Parser<'a> {
    data: &'a [u8],
    pos: usize,
}

const MAX_DEPTH: usize = 256;

impl<'a> Parser<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Value<'a>, String> {
        let mut p = Parser { data, pos: 0 };
        p.value(0)
    }

    fn peek(&self) -> Result<u8, String> {
        self.data.get(self.pos).copied().ok_or_else(|| "unexpected end of data".to_string())
    }

    fn value(&mut self, depth: usize) -> Result<Value<'a>, String> {
        if depth > MAX_DEPTH {
            return Err("nesting too deep".into());
        }
        match self.peek()? {
            b'i' => {
                self.pos += 1;
                let end = self.find(b'e')?;
                let s = std::str::from_utf8(&self.data[self.pos..end]).map_err(|_| "bad integer")?;
                let v = s.parse::<i64>().map_err(|_| format!("bad integer {s:?}"))?;
                self.pos = end + 1;
                Ok(Value::Int(v))
            }
            b'l' => {
                self.pos += 1;
                let mut items = Vec::new();
                while self.peek()? != b'e' {
                    items.push(self.value(depth + 1)?);
                }
                self.pos += 1;
                Ok(Value::List(items))
            }
            b'd' => {
                let start = self.pos;
                self.pos += 1;
                let mut entries = Vec::new();
                while self.peek()? != b'e' {
                    let key = match self.value(depth + 1)? {
                        Value::Bytes(b) => b,
                        _ => return Err("dictionary key is not a string".into()),
                    };
                    let v = self.value(depth + 1)?;
                    entries.push((key, v));
                }
                self.pos += 1;
                Ok(Value::Dict(entries, start..self.pos))
            }
            b'0'..=b'9' => {
                let colon = self.find(b':')?;
                let len: usize = std::str::from_utf8(&self.data[self.pos..colon]).ok().and_then(|s| s.parse().ok()).ok_or("bad string length")?;
                let start = colon + 1;
                let end = start.checked_add(len).filter(|&e| e <= self.data.len()).ok_or("string past end of data")?;
                self.pos = end;
                Ok(Value::Bytes(&self.data[start..end]))
            }
            c => Err(format!("unexpected byte 0x{c:02x} at {}", self.pos)),
        }
    }

    fn find(&self, b: u8) -> Result<usize, String> {
        self.data[self.pos..].iter().position(|&c| c == b).map(|i| self.pos + i).ok_or_else(|| "unterminated value".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nested() {
        let v = Parser::parse(b"d3:cow3:moo4:spaml1:a1:bi42eee").unwrap();
        assert_eq!(v.get("cow").unwrap().str().unwrap(), "moo");
        assert_eq!(v.get("spam").unwrap().list().unwrap().len(), 3);
        assert_eq!(v.span().unwrap(), 0..30);
    }

    #[test]
    fn rejects_garbage() {
        assert!(Parser::parse(b"d3:cow").is_err());
        assert!(Parser::parse(b"99999999999:x").is_err());
    }
}
