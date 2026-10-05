//! Just enough IPP (RFC 8010) to stamp the signed-in user on a request: the
//! front replaces `requesting-user-name` in the operation attributes so the
//! print helper records the authenticated user as the job's owner, not a name
//! the client chose. Document data after the attributes is never inspected.

use std::fmt;

const OPERATION_GROUP: u8 = 0x01;
const END_OF_ATTRIBUTES: u8 = 0x03;
const NAME_WITHOUT_LANGUAGE: u8 = 0x42;
const REQUESTING_USER_NAME: &[u8] = b"requesting-user-name";

#[derive(Debug, PartialEq, Eq)]
pub struct MalformedIpp(&'static str);

impl fmt::Display for MalformedIpp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "malformed IPP request: {}", self.0)
    }
}

impl std::error::Error for MalformedIpp {}

/// What the receipts log needs from a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IppSummary {
    pub operation: u16,
    pub request_id: u32,
    pub job_name: Option<String>,
}

impl IppSummary {
    /// True for operations that create a job or add a document to one.
    pub fn submits_job(&self) -> bool {
        matches!(self.operation, 0x0002 | 0x0003 | 0x0005 | 0x0006 | 0x0007)
    }

    pub fn operation_name(&self) -> String {
        match self.operation {
            0x0002 => "Print-Job".into(),
            0x0003 => "Print-URI".into(),
            0x0004 => "Validate-Job".into(),
            0x0005 => "Create-Job".into(),
            0x0006 => "Send-Document".into(),
            0x0007 => "Send-URI".into(),
            0x0008 => "Cancel-Job".into(),
            0x0009 => "Get-Job-Attributes".into(),
            0x000A => "Get-Jobs".into(),
            0x000B => "Get-Printer-Attributes".into(),
            other => format!("0x{other:04X}"),
        }
    }
}

struct Attribute<'a> {
    group: u8,
    name: &'a [u8],
    bytes: &'a [u8],
    value: &'a [u8],
}

/// Length of the IPP header plus attributes (through end-of-attributes), or
/// `None` while more bytes are needed.
pub fn attributes_len(body: &[u8]) -> Result<Option<usize>, MalformedIpp> {
    match walk(body)? {
        Some((len, _)) => Ok(Some(len)),
        None => Ok(None),
    }
}

fn walk(body: &[u8]) -> Result<Option<(usize, Vec<Attribute<'_>>)>, MalformedIpp> {
    if body.len() < 8 {
        return Ok(None);
    }
    if body[0] == 0 {
        return Err(MalformedIpp("unsupported version"));
    }
    let mut pos = 8;
    let mut group = 0u8;
    let mut attrs = Vec::new();
    loop {
        let Some(&tag) = body.get(pos) else {
            return Ok(None);
        };
        if tag == END_OF_ATTRIBUTES {
            return Ok(Some((pos + 1, attrs)));
        }
        if tag < 0x10 {
            if tag == 0 {
                return Err(MalformedIpp("reserved delimiter tag"));
            }
            group = tag;
            pos += 1;
            continue;
        }
        if group == 0 {
            return Err(MalformedIpp("attribute outside a group"));
        }
        let start = pos;
        let Some(name_len) = read_u16(body, pos + 1) else {
            return Ok(None);
        };
        let name_end = pos + 3 + name_len;
        let Some(value_len) = read_u16(body, name_end) else {
            return Ok(None);
        };
        let end = name_end + 2 + value_len;
        if body.len() < end {
            return Ok(None);
        }
        attrs.push(Attribute {
            group,
            name: &body[pos + 3..name_end],
            bytes: &body[start..end],
            value: &body[name_end + 2..end],
        });
        pos = end;
    }
}

fn read_u16(body: &[u8], at: usize) -> Option<usize> {
    Some(u16::from_be_bytes([*body.get(at)?, *body.get(at + 1)?]) as usize)
}

/// Rewrites the header and attributes of a request (`attributes`, exactly
/// [`attributes_len`] bytes) so `requesting-user-name` is `user`. With no user
/// the attributes are returned unchanged.
pub fn stamp_user(
    attributes: &[u8],
    user: Option<&str>,
) -> Result<(Vec<u8>, IppSummary), MalformedIpp> {
    let Some((len, attrs)) = walk(attributes)? else {
        return Err(MalformedIpp("truncated attributes"));
    };
    if len != attributes.len() {
        return Err(MalformedIpp("trailing bytes after attributes"));
    }
    let summary = IppSummary {
        operation: u16::from_be_bytes([attributes[2], attributes[3]]),
        request_id: u32::from_be_bytes([
            attributes[4],
            attributes[5],
            attributes[6],
            attributes[7],
        ]),
        job_name: attrs
            .iter()
            .find(|a| a.group == OPERATION_GROUP && a.name == b"job-name")
            .map(|a| String::from_utf8_lossy(a.value).into_owned()),
    };
    if !attrs.iter().any(|a| a.group == OPERATION_GROUP) {
        return Err(MalformedIpp("no operation attributes"));
    }
    let Some(user) = user else {
        return Ok((attributes.to_vec(), summary));
    };
    let user = user.as_bytes();
    if user.len() > 255 {
        return Err(MalformedIpp("user name longer than 255 bytes"));
    }
    let mut out = attributes[..8].to_vec();
    let mut group = 0u8;
    let mut in_user_attr = false;
    let mut stamped = false;
    let mut pos = 8;
    let mut next_attr = attrs.iter().peekable();
    while pos < len {
        let tag = attributes[pos];
        if tag < 0x10 {
            if group == OPERATION_GROUP && !stamped {
                push_user(&mut out, user);
                stamped = true;
            }
            group = tag;
            out.push(tag);
            pos += 1;
            continue;
        }
        let attr = next_attr.next().expect("walk saw this attribute");
        let additional_value = attr.name.is_empty();
        if !additional_value {
            in_user_attr = attr.group == OPERATION_GROUP && attr.name == REQUESTING_USER_NAME;
        }
        if !in_user_attr {
            out.extend_from_slice(attr.bytes);
        }
        pos += attr.bytes.len();
    }
    Ok((out, summary))
}

fn push_user(out: &mut Vec<u8>, user: &[u8]) {
    out.push(NAME_WITHOUT_LANGUAGE);
    out.extend_from_slice(&(REQUESTING_USER_NAME.len() as u16).to_be_bytes());
    out.extend_from_slice(REQUESTING_USER_NAME);
    out.extend_from_slice(&(user.len() as u16).to_be_bytes());
    out.extend_from_slice(user);
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn attr(tag: u8, name: &str, value: &[u8]) -> Vec<u8> {
        let mut out = vec![tag];
        out.extend_from_slice(&(name.len() as u16).to_be_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(value.len() as u16).to_be_bytes());
        out.extend_from_slice(value);
        out
    }

    /// A Print-Job request whose client claims to be `claimed`.
    pub(crate) fn print_job(claimed: &str) -> Vec<u8> {
        let mut body = vec![2, 0, 0, 2, 0, 0, 0, 7, OPERATION_GROUP];
        body.extend(attr(0x47, "attributes-charset", b"utf-8"));
        body.extend(attr(0x48, "attributes-natural-language", b"en"));
        body.extend(attr(
            0x45,
            "printer-uri",
            b"ipps://printer:8631/ipp/print/anytopdf",
        ));
        body.extend(attr(
            NAME_WITHOUT_LANGUAGE,
            "requesting-user-name",
            claimed.as_bytes(),
        ));
        body.extend(attr(NAME_WITHOUT_LANGUAGE, "", b"second value"));
        body.extend(attr(NAME_WITHOUT_LANGUAGE, "job-name", b"Receipt"));
        body.push(0x02);
        body.extend(attr(0x21, "copies", &1u32.to_be_bytes()));
        body.push(END_OF_ATTRIBUTES);
        body
    }

    fn user_of(attributes: &[u8]) -> Vec<String> {
        let (_, attrs) = walk(attributes).unwrap().unwrap();
        let mut users = Vec::new();
        let mut in_user = false;
        for a in attrs {
            if !a.name.is_empty() {
                in_user = a.name == REQUESTING_USER_NAME;
            }
            if in_user {
                users.push(String::from_utf8_lossy(a.value).into_owned());
            }
        }
        users
    }

    #[test]
    fn signed_in_user_replaces_every_claimed_value() {
        let body = print_job("mallory");
        let (out, summary) = stamp_user(&body, Some("adeel")).unwrap();
        assert_eq!(user_of(&out), ["adeel"]);
        assert_eq!(summary.operation_name(), "Print-Job");
        assert_eq!(summary.request_id, 7);
        assert_eq!(summary.job_name.as_deref(), Some("Receipt"));
        assert!(summary.submits_job());
        let (_, attrs) = walk(&out).unwrap().unwrap();
        let names: Vec<_> = attrs
            .iter()
            .map(|a| String::from_utf8_lossy(a.name).into_owned())
            .collect();
        assert_eq!(
            &names[..2],
            ["attributes-charset", "attributes-natural-language"]
        );
        assert!(
            names.contains(&"copies".to_string()),
            "job group kept: {names:?}"
        );
    }

    #[test]
    fn user_is_added_when_the_client_sent_none() {
        let mut body = vec![2, 0, 0, 0x0B, 0, 0, 0, 1, OPERATION_GROUP];
        body.extend(attr(0x47, "attributes-charset", b"utf-8"));
        body.push(END_OF_ATTRIBUTES);
        let (out, summary) = stamp_user(&body, Some("adeel")).unwrap();
        assert_eq!(user_of(&out), ["adeel"]);
        assert!(!summary.submits_job());
    }

    #[test]
    fn without_a_user_attributes_pass_unchanged() {
        let body = print_job("guest");
        assert_eq!(stamp_user(&body, None).unwrap().0, body);
    }

    #[test]
    fn attributes_len_waits_for_more_bytes_then_finds_the_end() {
        let body = print_job("x");
        for cut in [0, 7, 9, 20, body.len() - 1] {
            assert_eq!(attributes_len(&body[..cut]), Ok(None), "cut at {cut}");
        }
        let mut with_data = body.clone();
        with_data.extend_from_slice(b"RaS2...document");
        assert_eq!(attributes_len(&with_data), Ok(Some(body.len())));
    }

    #[test]
    fn malformed_requests_are_rejected() {
        assert!(attributes_len(&[0, 0, 0, 2, 0, 0, 0, 1, 3]).is_err());
        assert!(attributes_len(&[2, 0, 0, 2, 0, 0, 0, 1, 0x47, 0, 1, b'a', 0, 0, 3]).is_err());
        let no_operation_group = [2, 0, 0, 2, 0, 0, 0, 1, 0x02, 3];
        assert!(stamp_user(&no_operation_group, Some("u")).is_err());
    }
}
