//! Bolt 5.x requests and responses (research/bolt.md, section 3).

use crate::packstream::PackValue;

#[derive(Clone, PartialEq)]
pub enum Request {
    Hello {
        extra: PackValue,
    },
    Logon {
        auth: PackValue,
    },
    Logoff,
    Telemetry,
    Run {
        query: String,
        params: PackValue,
        extra: PackValue,
    },
    Pull {
        extra: PackValue,
    },
    Discard {
        extra: PackValue,
    },
    Begin {
        extra: PackValue,
    },
    Commit,
    Rollback,
    Reset,
    Goodbye,
    Route,
}

// Message payloads may contain credentials, including legacy HELLO metadata.
impl std::fmt::Debug for Request {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Hello { .. } => "Hello [redacted]",
            Self::Logon { .. } => "Logon [redacted]",
            Self::Logoff => "Logoff",
            Self::Telemetry => "Telemetry",
            Self::Run { .. } => "Run",
            Self::Pull { .. } => "Pull",
            Self::Discard { .. } => "Discard",
            Self::Begin { .. } => "Begin",
            Self::Commit => "Commit",
            Self::Rollback => "Rollback",
            Self::Reset => "Reset",
            Self::Goodbye => "Goodbye",
            Self::Route => "Route",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MessageError {
    #[error("not a message structure")]
    NotAStruct,
    #[error("unknown message tag {0:#04x}")]
    UnknownTag(u8),
    #[error("bad fields for message {0:#04x}")]
    BadFields(u8),
}

fn map_field(fields: &mut Vec<PackValue>, tag: u8) -> Result<PackValue, MessageError> {
    match fields.pop() {
        Some(v @ PackValue::Map(_)) => Ok(v),
        _ => Err(MessageError::BadFields(tag)),
    }
}

pub fn parse_request(value: PackValue) -> Result<Request, MessageError> {
    let PackValue::Struct { tag, mut fields } = value else {
        return Err(MessageError::NotAStruct);
    };
    let expect = |n: usize, fields: &Vec<PackValue>| {
        if fields.len() == n {
            Ok(())
        } else {
            Err(MessageError::BadFields(tag))
        }
    };
    Ok(match tag {
        0x01 => {
            expect(1, &fields)?;
            Request::Hello {
                extra: map_field(&mut fields, tag)?,
            }
        }
        0x02 => Request::Goodbye,
        0x0F => Request::Reset,
        0x10 => {
            expect(3, &fields)?;
            let extra = map_field(&mut fields, tag)?;
            let params = map_field(&mut fields, tag)?;
            let query = match fields.pop() {
                Some(PackValue::String(q)) => q,
                _ => return Err(MessageError::BadFields(tag)),
            };
            Request::Run {
                query,
                params,
                extra,
            }
        }
        0x11 => {
            expect(1, &fields)?;
            Request::Begin {
                extra: map_field(&mut fields, tag)?,
            }
        }
        0x12 => Request::Commit,
        0x13 => Request::Rollback,
        0x2F => {
            expect(1, &fields)?;
            Request::Discard {
                extra: map_field(&mut fields, tag)?,
            }
        }
        0x3F => {
            expect(1, &fields)?;
            Request::Pull {
                extra: map_field(&mut fields, tag)?,
            }
        }
        0x54 => Request::Telemetry,
        0x66 => Request::Route,
        0x6A => {
            expect(1, &fields)?;
            Request::Logon {
                auth: map_field(&mut fields, tag)?,
            }
        }
        0x6B => Request::Logoff,
        other => return Err(MessageError::UnknownTag(other)),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Success(Vec<(String, PackValue)>),
    Record(Vec<PackValue>),
    Ignored,
    Failure { code: String, message: String },
}

impl Response {
    pub fn to_value(&self) -> PackValue {
        match self {
            Response::Success(meta) => PackValue::Struct {
                tag: 0x70,
                fields: vec![PackValue::Map(meta.clone())],
            },
            Response::Record(values) => PackValue::Struct {
                tag: 0x71,
                fields: vec![PackValue::List(values.clone())],
            },
            Response::Ignored => PackValue::Struct {
                tag: 0x7E,
                fields: vec![],
            },
            Response::Failure { code, message } => PackValue::Struct {
                tag: 0x7F,
                fields: vec![PackValue::Map(vec![
                    ("code".into(), PackValue::String(code.clone())),
                    ("message".into(), PackValue::String(message.clone())),
                ])],
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(tag: u8, fields: Vec<PackValue>) -> PackValue {
        PackValue::Struct { tag, fields }
    }

    #[test]
    fn parses_run_with_its_three_fields() {
        let req = parse_request(s(
            0x10,
            vec![
                PackValue::String("RETURN 1".into()),
                PackValue::Map(vec![]),
                PackValue::Map(vec![]),
            ],
        ))
        .unwrap();
        assert!(matches!(req, Request::Run { query, .. } if query == "RETURN 1"));
    }

    #[test]
    fn parses_the_simple_messages() {
        assert!(matches!(
            parse_request(s(0x0F, vec![])).unwrap(),
            Request::Reset
        ));
        assert!(matches!(
            parse_request(s(0x12, vec![])).unwrap(),
            Request::Commit
        ));
        assert!(matches!(
            parse_request(s(0x02, vec![])).unwrap(),
            Request::Goodbye
        ));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(
            parse_request(PackValue::Int(1)).unwrap_err(),
            MessageError::NotAStruct
        );
        assert_eq!(
            parse_request(s(0x99, vec![])).unwrap_err(),
            MessageError::UnknownTag(0x99)
        );
        assert_eq!(
            parse_request(s(0x10, vec![])).unwrap_err(),
            MessageError::BadFields(0x10)
        );
    }

    #[test]
    fn responses_become_structs() {
        let failure = Response::Failure {
            code: "Neo.ClientError.Statement.SyntaxError".into(),
            message: "m".into(),
        }
        .to_value();
        assert_eq!(
            failure,
            s(
                0x7F,
                vec![PackValue::Map(vec![
                    (
                        "code".into(),
                        PackValue::String("Neo.ClientError.Statement.SyntaxError".into())
                    ),
                    ("message".into(), PackValue::String("m".into())),
                ])]
            )
        );
        assert_eq!(Response::Ignored.to_value(), s(0x7E, vec![]));
    }
}
