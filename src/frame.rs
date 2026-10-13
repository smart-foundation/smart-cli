//! The serve frame codec — the schema-blind half of the wire spec.
//!
//! ## The rule this module obeys (the same rule `smart-lib/serve/frame.rs`
//! states): the envelope is transport machinery and never names a domain; the
//! payload is the contract's own capnp bytes. This is a SEPARATE, independent
//! implementation of the spec on the client side — like HTTP, both sides
//! implement their own. There is no shared code and no dependency on
//! smart-lib; the fixed-offset little-endian framing below is the contract the
//! service and this client both honour.
//!
//! ## Request frame (little-endian)
//!
//! ```text
//! [0..8]   interfaceId  u64   the domain schema id (@0x… as an integer)
//! [8..10]  methodOrdinal u16  the contract's own ordinal
//! [10..12] paramsLen    u16   length of the params bytes that follow
//! [12..16] flags        u32   reserved, always 0
//! [16..16+paramsLen]             params bytes (capnp-serialized param struct)
//! [16+paramsLen..]               attached capability bytes (empty for the net surface)
//! ```
//!
//! Subscription pseudo-ordinals (a capability cannot travel as frame bytes, so
//! the service-minted subscription id rides in-band through these):
//!   * `SUB_PULL   = 0xFFFE` — params `[0..8] subId u64 + [8..12] limit u32`.
//!   * `SUB_CANCEL = 0xFFFF` — params `[0..8] subId u64`.
//!
//! ## Response frame (little-endian)
//!
//! ```text
//! [0..2]   status     u16   0 = ok, 1 = error
//! [2..4]   msgLen     u16   UTF-8 message length
//! [4..8]   payloadLen u32   payload length (the contract's own bytes)
//! [8..8+msgLen]             message bytes (UTF-8, may be empty)
//! [8+msgLen..]              payload bytes (a result struct, or the T.Error on error)
//! ```
//!
//! The serve frame travels as the SINGLE SEGMENT of a one-segment capnp
//! message inside the stream transport's KIND_DATA frame, so it is word-padded
//! at the tail (the header lengths stay exact; the padding is zero filler the
//! reader never looks at). The outer capnp flat-message wrap/unwrap is
//! [`wrap_one_segment`] / [`segment0`].

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// Bytes of a capnp word — the unit the stream framing pads to.
pub const WORD_BYTES: usize = 8;

/// Request header length: interfaceId, ordinal, paramsLen, flags.
pub const REQUEST_HEADER_LEN: usize = 16;

/// Response header length: status, msgLen, payloadLen.
pub const RESPONSE_HEADER_LEN: usize = 8;

/// `status` value for a successful response.
pub const STATUS_OK: u16 = 0;

/// `status` value for an error response (its payload is the serialized `T.Error`).
pub const STATUS_ERROR: u16 = 1;

/// Pseudo-ordinal: pull one subscription's next event(s).
pub const SUB_PULL: u16 = 0xFFFE;

/// Pseudo-ordinal: retire a subscription.
pub const SUB_CANCEL: u16 = 0xFFFF;

/// Length of the service-minted subscription id prefix on subscription payloads.
pub const SUB_ID_LEN: usize = 8;

/// Encode a request frame, word-padded like the service expects. `params` is
/// the capnp-serialized param struct; `caps` is any attached capability bytes
/// (empty for every method the network surface serves).
pub fn encode_request(interface_id: u64, ordinal: u16, params: &[u8], caps: &[u8]) -> Vec<u8> {
    let len = REQUEST_HEADER_LEN + params.len() + caps.len();
    let mut out = Vec::with_capacity(padded_len(len));
    out.extend_from_slice(&interface_id.to_le_bytes());
    out.extend_from_slice(&ordinal.to_le_bytes());
    out.extend_from_slice(&(params.len() as u16).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(params);
    out.extend_from_slice(caps);
    out.resize(padded_len(len), 0);
    out
}

/// A parsed response frame, borrowing the bytes it was parsed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResponseView<'a> {
    /// 0 ok / 1 error.
    pub status: u16,
    /// The message bytes (UTF-8 when the peer wrote UTF-8).
    pub message: &'a [u8],
    /// The payload bytes (a result struct, or the `T.Error` on error).
    pub payload: &'a [u8],
}

impl<'a> ResponseView<'a> {
    /// The message as an owned string (lossy: a peer's invalid UTF-8 reads as
    /// replacement characters rather than fabricating a message).
    pub fn message_text(&self) -> String {
        String::from_utf8_lossy(self.message).into_owned()
    }

    /// True for a successful response.
    pub fn is_ok(&self) -> bool {
        self.status == STATUS_OK
    }
}

/// Parse one response frame (the client side of this format).
pub fn parse_response(bytes: &[u8]) -> Result<ResponseView<'_>, String> {
    let malformed = |detail: String| Err(format!("bad-frame: {detail}"));
    if bytes.len() < RESPONSE_HEADER_LEN {
        return malformed(format!(
            "response frame is {} bytes; the header alone is {RESPONSE_HEADER_LEN}",
            bytes.len()
        ));
    }
    let status = u16::from_le_bytes([bytes[0], bytes[1]]);
    let msg_len = u16::from_le_bytes([bytes[2], bytes[3]]) as usize;
    let payload_len = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
    let msg_end = match RESPONSE_HEADER_LEN.checked_add(msg_len) {
        Some(end) => end,
        None => return malformed("message length overflows the frame".to_string()),
    };
    let payload_end = match msg_end.checked_add(payload_len) {
        Some(end) => end,
        None => return malformed("payload length overflows the frame".to_string()),
    };
    if bytes.len() < payload_end {
        return malformed(format!(
            "declared message {msg_len} + payload {payload_len} bytes exceed the {} bytes the frame carries",
            bytes.len() - RESPONSE_HEADER_LEN
        ));
    }
    Ok(ResponseView {
        status,
        message: &bytes[RESPONSE_HEADER_LEN..msg_end],
        payload: &bytes[msg_end..payload_end],
    })
}

/// Encode `SUB_PULL` params: `[0..8] subId u64 + [8..12] limit u32` (limit 0 =
/// one). The full request frame carries these as the `params`.
pub fn encode_pull_params(sub_id: u64, limit: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(12);
    out.extend_from_slice(&sub_id.to_le_bytes());
    out.extend_from_slice(&limit.to_le_bytes());
    out
}

/// Encode `SUB_CANCEL` params: `[0..8] subId u64`.
pub fn encode_cancel_params(sub_id: u64) -> Vec<u8> {
    sub_id.to_le_bytes().to_vec()
}

/// Prepend the service-minted subscription id to an event's payload — the shape
/// `SUB_PULL` responses share.
pub fn payload_with_sub_id(sub_id: u64, event: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(SUB_ID_LEN + event.len());
    out.extend_from_slice(&sub_id.to_le_bytes());
    out.extend_from_slice(event);
    out
}

/// Read the subscription id back off a subscription payload, leaving the
/// event bytes after it.
pub fn split_sub_id(payload: &[u8]) -> Option<(u64, &[u8])> {
    let head = payload.get(..SUB_ID_LEN)?;
    let id = u64::from_le_bytes(head.try_into().unwrap());
    Some((id, &payload[SUB_ID_LEN..]))
}

/// Wrap `segment` as a one-segment capnp flat message (segment table + segment
/// verbatim). `segment` MUST already be a whole number of capnp words — the
/// serve frame is word-padded by [`encode_request`], and responses are padded
/// by the service. This is the capnp stream format for a single segment:
/// word 0 holds `(count-1)=0` and segment 0's length-in-words.
pub fn wrap_one_segment(segment: &[u8]) -> Vec<u8> {
    debug_assert!(
        segment.len() % WORD_BYTES == 0,
        "the serve frame segment must be word-aligned"
    );
    let mut out = Vec::with_capacity(WORD_BYTES + segment.len());
    out.extend_from_slice(&0u32.to_le_bytes()); // (count - 1) = 0
    out.extend_from_slice(&((segment.len() / WORD_BYTES) as u32).to_le_bytes());
    out.extend_from_slice(segment);
    out
}

/// Extract segment 0 from a received one-segment capnp flat message: the serve
/// frame's bytes. Returns `None` when the segment table is implausible.
pub fn segment0(message: &[u8]) -> Option<&[u8]> {
    if message.len() < WORD_BYTES {
        return None;
    }
    let count_minus_one = u32::from_le_bytes([message[0], message[1], message[2], message[3]]);
    if count_minus_one != 0 {
        return None; // the serve protocol only ever carries one segment
    }
    let seg_words =
        u32::from_le_bytes([message[4], message[5], message[6], message[7]]) as usize;
    let end = WORD_BYTES.checked_add(seg_words.checked_mul(WORD_BYTES)?)?;
    if message.len() < end {
        return None;
    }
    Some(&message[WORD_BYTES..end])
}

/// `len` rounded up to a whole capnp word.
pub fn padded_len(len: usize) -> usize {
    (len + WORD_BYTES - 1) & !(WORD_BYTES - 1)
}

// ── The server side of the same spec (used by the mock test double) ───────
// A test double speaks the frame protocol too; it parses requests and encodes
// responses with the SAME fixed-offset layout, proving both directions of the
// independently-implemented wire agree (like HTTP request and response).

/// A parsed request frame, borrowing the bytes it was parsed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RequestView<'a> {
    /// The domain schema id this call names.
    pub interface_id: u64,
    /// The contract method ordinal (or a subscription pseudo-ordinal).
    pub ordinal: u16,
    /// The reserved flags word (always 0).
    pub flags: u32,
    /// The capnp-serialized params struct.
    pub params: &'a [u8],
    /// Attached capability bytes (empty for the network surface).
    pub caps: &'a [u8],
}

/// Parse one request frame. Returns [`String`] describing the fault when the
/// bytes are not a well-formed frame.
pub fn parse_request(bytes: &[u8]) -> Result<RequestView<'_>, String> {
    let malformed = |detail: String| Err(format!("bad-frame: {detail}"));
    if bytes.len() < REQUEST_HEADER_LEN {
        return malformed(format!(
            "request frame is {} bytes; the header alone is {REQUEST_HEADER_LEN}",
            bytes.len()
        ));
    }
    let interface_id = u64::from_le_bytes(bytes[0..8].try_into().unwrap());
    let ordinal = u16::from_le_bytes(bytes[8..10].try_into().unwrap());
    let params_len = u16::from_le_bytes(bytes[10..12].try_into().unwrap()) as usize;
    let flags = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
    if flags != 0 {
        return malformed(format!("reserved flags word is {flags:#x}; expected 0"));
    }
    let params_start = REQUEST_HEADER_LEN;
    let params_end = params_start
        .checked_add(params_len)
        .ok_or_else(|| "params length overflows the frame".to_string())?;
    if bytes.len() < params_end {
        return malformed(format!(
            "params length {params_len} exceeds the {} bytes the frame carries",
            bytes.len() - REQUEST_HEADER_LEN
        ));
    }
    Ok(RequestView {
        interface_id,
        ordinal,
        flags,
        params: &bytes[params_start..params_end],
        caps: &bytes[params_end..],
    })
}

/// Encode a response frame, word-padded like the service emits. An ok frame
/// carries the contract result bytes; an error frame carries the message plus
/// the (empty or `T.Error`) payload the caller supplies.
pub fn encode_response(status: u16, message: &str, payload: &[u8]) -> Vec<u8> {
    let message = message.as_bytes();
    let len = RESPONSE_HEADER_LEN + message.len() + payload.len();
    let mut out = Vec::with_capacity(padded_len(len));
    out.extend_from_slice(&status.to_le_bytes());
    out.extend_from_slice(&(message.len() as u16).to_le_bytes());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(message);
    out.extend_from_slice(payload);
    out.resize(padded_len(len), 0);
    out
}

/// A convenience: an ok response carrying `payload`.
pub fn encode_ok(payload: &[u8]) -> Vec<u8> {
    encode_response(STATUS_OK, "", payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_through_its_own_header() {
        let params = [1u8, 2, 3, 4, 5, 6, 7, 8]; // one word
        let frame = encode_request(0xa1b2c3d4e5f6000f, 1, &params, &[]);
        assert_eq!(frame.len(), REQUEST_HEADER_LEN + 8);
        let interface_id = u64::from_le_bytes(frame[0..8].try_into().unwrap());
        let ordinal = u16::from_le_bytes(frame[8..10].try_into().unwrap());
        let params_len = u16::from_le_bytes(frame[10..12].try_into().unwrap());
        assert_eq!(interface_id, 0xa1b2c3d4e5f6000f);
        assert_eq!(ordinal, 1);
        assert_eq!(params_len, 8);
        assert_eq!(&frame[16..24], &params);
    }

    #[test]
    fn one_segment_wrap_and_unwrap_is_exact() {
        let frame = encode_request(7, 0x1234, &[9u8; 8], &[]);
        let wrapped = wrap_one_segment(&frame);
        let seg0 = segment0(&wrapped).expect("one segment");
        assert_eq!(seg0, &frame[..]);
    }

    #[test]
    fn response_parse_ok_and_error() {
        // A hand-built response: status ok, empty message, 8-byte payload.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&STATUS_OK.to_le_bytes());
        bytes.extend_from_slice(&0u16.to_le_bytes());
        bytes.extend_from_slice(&8u32.to_le_bytes());
        bytes.extend_from_slice(&[0u8; 8]);
        let view = parse_response(&bytes).unwrap();
        assert!(view.is_ok());
        assert_eq!(view.payload.len(), 8);

        let mut err = Vec::new();
        err.extend_from_slice(&STATUS_ERROR.to_le_bytes());
        err.extend_from_slice(&5u16.to_le_bytes()); // message of 5 bytes
        err.extend_from_slice(&0u32.to_le_bytes());
        err.extend_from_slice(b"hello");
        let view = parse_response(&err).unwrap();
        assert!(!view.is_ok());
        assert_eq!(view.message_text(), "hello");
    }

    #[test]
    fn truncated_response_is_malformed_not_a_panic() {
        let err = parse_response(&[0u8, 0, 0]);
        assert!(err.is_err());
    }

    #[test]
    fn split_sub_id_reads_the_prefix() {
        let payload = payload_with_sub_id(42, &[1, 2, 3, 4, 5, 6, 7, 8]);
        let (id, event) = split_sub_id(&payload).unwrap();
        assert_eq!(id, 42);
        assert_eq!(event.len(), 8);
    }
}
