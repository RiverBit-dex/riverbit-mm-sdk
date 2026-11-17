//! Decoded server frames.

use serde_json::Value;

use crate::error::{Error, Result};
use crate::proto::{
    BboFrame, CandleFrame, L2BookFrame, L2DeltaFrame, ServerBody, ServerFrame, TradesFrame,
};

/// A JSON frame: `{"channel": ..., "data": ...}`, byte-identical to the JSON
/// endpoint. Streams without a typed form arrive this way, and so do the typed
/// streams (`l2`, `l2Book`, `bbo`, `trades`, `candle`) when precision or counts
/// cannot be represented in typed form.
#[derive(Clone, Debug, PartialEq)]
pub struct JsonFrame {
    /// The outer frame ID; 0 for pushes, acknowledgments and pong.
    pub id: u64,
    /// The parsed JSON.
    pub value: Value,
    /// The frame's bytes as received.
    pub raw: Vec<u8>,
}

impl JsonFrame {
    /// The `channel` member, for example `subscriptionResponse`, `pong`,
    /// `orderUpdates`, `post`.
    pub fn channel(&self) -> Option<&str> {
        self.value.get("channel").and_then(Value::as_str)
    }

    /// The `data` member.
    pub fn data(&self) -> &Value {
        &self.value["data"]
    }
}

/// A server push, typed where the protocol has a typed form.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// Channel `l2`: a snapshot, then updates against the previous book.
    L2Delta(L2DeltaFrame),
    /// Channel `l2Book`: a full book.
    L2Book(L2BookFrame),
    /// Channel `bbo`.
    Bbo(BboFrame),
    /// Channel `trades`.
    Trades(TradesFrame),
    /// Channel `candle`.
    Candle(CandleFrame),
    /// Everything else, and typed streams that fell back to JSON.
    Json(JsonFrame),
}

impl Frame {
    /// Converts a decoded server frame. A login result is not a push and is an error here.
    pub fn from_server(frame: ServerFrame) -> Result<Frame> {
        Ok(match frame.body {
            ServerBody::Json(raw) => {
                let value = serde_json::from_slice(&raw)
                    .map_err(|e| Error::Decode(format!("JSON frame: {e}")))?;
                Frame::Json(JsonFrame {
                    id: frame.id,
                    value,
                    raw,
                })
            }
            ServerBody::L2Book(b) => Frame::L2Book(b),
            ServerBody::L2Delta(b) => Frame::L2Delta(b),
            ServerBody::Bbo(b) => Frame::Bbo(b),
            ServerBody::Candle(b) => Frame::Candle(b),
            ServerBody::Trades(b) => Frame::Trades(b),
            ServerBody::Login(_) => {
                return Err(Error::Decode("unexpected login result after login".into()))
            }
        })
    }
}
