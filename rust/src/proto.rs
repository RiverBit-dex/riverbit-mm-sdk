//! Hand-written Protobuf codec for the bin-ws frames in `proto/binws.proto`
//! (package `riverbit.api.v1`). Users do not need `protoc`.
//!
//! Only varint and length-delimited fields occur. Decoding accepts both packed
//! and unpacked repeated scalars and skips unknown fields. Prices and sizes
//! are fixed-point integers: `value = integer / 10^decimals`; use
//! [`format_fixed`](crate::decimal::format_fixed) to render them exactly.

use crate::error::Error;

type Result<T> = std::result::Result<T, Error>;

fn bad(msg: impl Into<String>) -> Error {
    Error::Decode(msg.into())
}

// ---------------------------------------------------------------- wire level

enum Wire<'a> {
    Varint(u64),
    Fixed64,
    Len(&'a [u8]),
    Fixed32,
}

struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *self
                .buf
                .get(self.pos)
                .ok_or_else(|| bad("truncated varint"))?;
            self.pos += 1;
            if shift == 63 && byte > 1 {
                return Err(bad("varint overflows 64 bits"));
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(bad("varint too long"))
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(n).filter(|&e| e <= self.buf.len());
        let end = end.ok_or_else(|| bad("truncated field"))?;
        let out = &self.buf[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    fn next(&mut self) -> Result<Option<(u32, Wire<'a>)>> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let field = u32::try_from(key >> 3).map_err(|_| bad("field number too large"))?;
        if field == 0 {
            return Err(bad("field number 0"));
        }
        let wire = match key & 7 {
            0 => Wire::Varint(self.varint()?),
            1 => {
                self.take(8)?;
                Wire::Fixed64
            }
            2 => {
                let len = usize::try_from(self.varint()?).map_err(|_| bad("length too large"))?;
                Wire::Len(self.take(len)?)
            }
            5 => {
                self.take(4)?;
                Wire::Fixed32
            }
            other => return Err(bad(format!("unsupported wire type {other}"))),
        };
        Ok(Some((field, wire)))
    }
}

fn want_varint(wire: &Wire<'_>, what: &str) -> Result<u64> {
    match wire {
        Wire::Varint(v) => Ok(*v),
        _ => Err(bad(format!("{what}: expected varint"))),
    }
}

fn want_u32(wire: &Wire<'_>, what: &str) -> Result<u32> {
    u32::try_from(want_varint(wire, what)?).map_err(|_| bad(format!("{what}: value exceeds u32")))
}

fn want_bytes<'a>(wire: &Wire<'a>, what: &str) -> Result<&'a [u8]> {
    match wire {
        Wire::Len(b) => Ok(b),
        _ => Err(bad(format!("{what}: expected length-delimited"))),
    }
}

fn want_string(wire: &Wire<'_>, what: &str) -> Result<String> {
    String::from_utf8(want_bytes(wire, what)?.to_vec())
        .map_err(|_| bad(format!("{what}: invalid UTF-8")))
}

fn push_u64s(wire: &Wire<'_>, out: &mut Vec<u64>, what: &str) -> Result<()> {
    match wire {
        Wire::Varint(v) => out.push(*v),
        Wire::Len(b) => {
            let mut r = Reader::new(b);
            while r.pos < b.len() {
                out.push(r.varint()?);
            }
        }
        _ => return Err(bad(format!("{what}: expected varint or packed"))),
    }
    Ok(())
}

fn push_u32s(wire: &Wire<'_>, out: &mut Vec<u32>, what: &str) -> Result<()> {
    let mut tmp = Vec::new();
    push_u64s(wire, &mut tmp, what)?;
    for v in tmp {
        out.push(u32::try_from(v).map_err(|_| bad(format!("{what}: value exceeds u32")))?);
    }
    Ok(())
}

fn put_uvarint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_key(out: &mut Vec<u8>, field: u32, wire: u8) {
    put_uvarint(out, u64::from(field) << 3 | u64::from(wire));
}

/// Proto3 scalar: omitted when zero.
fn put_varint(out: &mut Vec<u8>, field: u32, v: u64) {
    if v != 0 {
        put_varint_always(out, field, v);
    }
}

/// `optional` scalar or oneof member: written even when zero.
fn put_varint_always(out: &mut Vec<u8>, field: u32, v: u64) {
    put_key(out, field, 0);
    put_uvarint(out, v);
}

fn put_bytes_always(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    put_key(out, field, 2);
    put_uvarint(out, b.len() as u64);
    out.extend_from_slice(b);
}

fn put_bytes(out: &mut Vec<u8>, field: u32, b: &[u8]) {
    if !b.is_empty() {
        put_bytes_always(out, field, b);
    }
}

fn put_packed(out: &mut Vec<u8>, field: u32, values: &[u64]) {
    if values.is_empty() {
        return;
    }
    let mut body = Vec::new();
    for v in values {
        put_uvarint(&mut body, *v);
    }
    put_bytes_always(out, field, &body);
}

fn put_packed32(out: &mut Vec<u8>, field: u32, values: &[u32]) {
    let wide: Vec<u64> = values.iter().map(|v| u64::from(*v)).collect();
    put_packed(out, field, &wide);
}

// ---------------------------------------------------------------- login

/// `WsV2Login`: the first frame on every connection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Login {
    /// Milliseconds since the epoch; within plus or minus 30 seconds of server time.
    pub timestamp_ms: u64,
    /// 16 raw random bytes; never reused.
    pub nonce: Vec<u8>,
    /// 65 bytes `r || s || v`.
    pub signature: Vec<u8>,
}

impl Login {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, 1, self.timestamp_ms);
        put_bytes(&mut out, 2, &self.nonce);
        put_bytes(&mut out, 3, &self.signature);
        out
    }

    /// Decodes the message.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = Login::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.timestamp_ms = want_varint(&wire, "login.timestamp_ms")?,
                2 => m.nonce = want_bytes(&wire, "login.nonce")?.to_vec(),
                3 => m.signature = want_bytes(&wire, "login.signature")?.to_vec(),
                _ => {}
            }
        }
        Ok(m)
    }
}

/// `WsV2LoginResult`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoginResult {
    /// True if the login was accepted.
    pub ok: bool,
    /// Resolved master account, 20 bytes.
    pub user: Vec<u8>,
    /// Recovered signer (the API wallet or the master itself), 20 bytes.
    pub signer: Vec<u8>,
    /// Failure text when `ok` is false.
    pub error: String,
}

impl LoginResult {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, 1, u64::from(self.ok));
        put_bytes(&mut out, 2, &self.user);
        put_bytes(&mut out, 3, &self.signer);
        put_bytes(&mut out, 4, self.error.as_bytes());
        out
    }

    /// Decodes the message.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = LoginResult::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.ok = want_varint(&wire, "login result.ok")? != 0,
                2 => m.user = want_bytes(&wire, "login result.user")?.to_vec(),
                3 => m.signer = want_bytes(&wire, "login result.signer")?.to_vec(),
                4 => m.error = want_string(&wire, "login result.error")?,
                _ => {}
            }
        }
        Ok(m)
    }
}

// ---------------------------------------------------------------- market data

/// `L2BookFrame`: a full book. Arrays of one side have equal length.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct L2BookFrame {
    /// Coin name.
    pub coin: String,
    /// Milliseconds since the epoch.
    pub time: u64,
    /// Decimals of every price in the frame.
    pub px_decimals: u32,
    /// Decimals of every size in the frame.
    pub sz_decimals: u32,
    /// Bid prices, best first.
    pub bid_px: Vec<u64>,
    /// Bid sizes.
    pub bid_sz: Vec<u64>,
    /// Orders per bid level.
    pub bid_n: Vec<u32>,
    /// Ask prices, best first.
    pub ask_px: Vec<u64>,
    /// Ask sizes.
    pub ask_sz: Vec<u64>,
    /// Orders per ask level.
    pub ask_n: Vec<u32>,
}

impl L2BookFrame {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes(&mut out, 1, self.coin.as_bytes());
        put_varint(&mut out, 2, self.time);
        put_varint(&mut out, 3, u64::from(self.px_decimals));
        put_varint(&mut out, 4, u64::from(self.sz_decimals));
        put_packed(&mut out, 5, &self.bid_px);
        put_packed(&mut out, 6, &self.bid_sz);
        put_packed32(&mut out, 7, &self.bid_n);
        put_packed(&mut out, 8, &self.ask_px);
        put_packed(&mut out, 9, &self.ask_sz);
        put_packed32(&mut out, 10, &self.ask_n);
        out
    }

    /// Decodes the message and checks that parallel arrays have equal length.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = L2BookFrame::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.coin = want_string(&wire, "l2 book.coin")?,
                2 => m.time = want_varint(&wire, "l2 book.time")?,
                3 => m.px_decimals = want_u32(&wire, "l2 book.px_decimals")?,
                4 => m.sz_decimals = want_u32(&wire, "l2 book.sz_decimals")?,
                5 => push_u64s(&wire, &mut m.bid_px, "l2 book.bid_px")?,
                6 => push_u64s(&wire, &mut m.bid_sz, "l2 book.bid_sz")?,
                7 => push_u32s(&wire, &mut m.bid_n, "l2 book.bid_n")?,
                8 => push_u64s(&wire, &mut m.ask_px, "l2 book.ask_px")?,
                9 => push_u64s(&wire, &mut m.ask_sz, "l2 book.ask_sz")?,
                10 => push_u32s(&wire, &mut m.ask_n, "l2 book.ask_n")?,
                _ => {}
            }
        }
        if m.bid_px.len() != m.bid_sz.len() || m.ask_px.len() != m.ask_sz.len() {
            return Err(bad(format!(
                "l2 book {}: px/sz arrays differ in length",
                m.coin
            )));
        }
        if (!m.bid_n.is_empty() && m.bid_n.len() != m.bid_px.len())
            || (!m.ask_n.is_empty() && m.ask_n.len() != m.ask_px.len())
        {
            return Err(bad(format!(
                "l2 book {}: n array differs in length",
                m.coin
            )));
        }
        Ok(m)
    }
}

/// `L2DeltaUpdate`: changes against the previous book.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct L2DeltaUpdate {
    /// Coin name.
    pub coin: String,
    /// Milliseconds since the epoch.
    pub time: u64,
    /// Decimals of prices; must match the latest snapshot.
    pub px_decimals: u32,
    /// Decimals of sizes; must match the latest snapshot.
    pub sz_decimals: u32,
    /// New or size-changed bid prices.
    pub bid_px: Vec<u64>,
    /// Sizes of those bids.
    pub bid_sz: Vec<u64>,
    /// New or size-changed ask prices.
    pub ask_px: Vec<u64>,
    /// Sizes of those asks.
    pub ask_sz: Vec<u64>,
    /// Removed bid levels, as indices into the previous book.
    pub bid_removed: Vec<u32>,
    /// Removed ask levels, as indices into the previous book.
    pub ask_removed: Vec<u32>,
    /// Ask minus bid in `px_decimals`, present when `s` was subscribed.
    pub spread: Option<u64>,
}

impl L2DeltaUpdate {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes(&mut out, 1, self.coin.as_bytes());
        put_varint(&mut out, 2, self.time);
        put_varint(&mut out, 3, u64::from(self.px_decimals));
        put_varint(&mut out, 4, u64::from(self.sz_decimals));
        put_packed(&mut out, 5, &self.bid_px);
        put_packed(&mut out, 6, &self.bid_sz);
        put_packed(&mut out, 7, &self.ask_px);
        put_packed(&mut out, 8, &self.ask_sz);
        put_packed32(&mut out, 9, &self.bid_removed);
        put_packed32(&mut out, 10, &self.ask_removed);
        if let Some(s) = self.spread {
            put_varint_always(&mut out, 11, s);
        }
        out
    }

    /// Decodes the message and checks that parallel arrays have equal length.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = L2DeltaUpdate::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.coin = want_string(&wire, "l2 update.coin")?,
                2 => m.time = want_varint(&wire, "l2 update.time")?,
                3 => m.px_decimals = want_u32(&wire, "l2 update.px_decimals")?,
                4 => m.sz_decimals = want_u32(&wire, "l2 update.sz_decimals")?,
                5 => push_u64s(&wire, &mut m.bid_px, "l2 update.bid_px")?,
                6 => push_u64s(&wire, &mut m.bid_sz, "l2 update.bid_sz")?,
                7 => push_u64s(&wire, &mut m.ask_px, "l2 update.ask_px")?,
                8 => push_u64s(&wire, &mut m.ask_sz, "l2 update.ask_sz")?,
                9 => push_u32s(&wire, &mut m.bid_removed, "l2 update.bid_removed")?,
                10 => push_u32s(&wire, &mut m.ask_removed, "l2 update.ask_removed")?,
                11 => m.spread = Some(want_varint(&wire, "l2 update.spread")?),
                _ => {}
            }
        }
        if m.bid_px.len() != m.bid_sz.len() || m.ask_px.len() != m.ask_sz.len() {
            return Err(bad(format!(
                "l2 update {}: px/sz arrays differ in length",
                m.coin
            )));
        }
        Ok(m)
    }
}

/// `L2DeltaFrame` (channel `l2`): a snapshot that replaces the whole book, or
/// an update against the previous book.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum L2DeltaFrame {
    /// Replaces the entire book.
    Snapshot(L2BookFrame),
    /// Changes the previous book.
    Update(L2DeltaUpdate),
}

impl L2DeltaFrame {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        match self {
            L2DeltaFrame::Snapshot(s) => put_bytes_always(&mut out, 1, &s.encode()),
            L2DeltaFrame::Update(u) => put_bytes_always(&mut out, 2, &u.encode()),
        }
        out
    }

    /// Decodes the message.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut found: Option<L2DeltaFrame> = None;
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            let next = match field {
                1 => L2DeltaFrame::Snapshot(L2BookFrame::decode(want_bytes(
                    &wire,
                    "l2 delta.snapshot",
                )?)?),
                2 => L2DeltaFrame::Update(L2DeltaUpdate::decode(want_bytes(
                    &wire,
                    "l2 delta.update",
                )?)?),
                _ => continue,
            };
            if found.replace(next).is_some() {
                return Err(bad("l2 delta has both snapshot and update"));
            }
        }
        found.ok_or_else(|| bad("l2 delta without snapshot or update"))
    }
}

/// `BboFrame`: a side with no level has all of its fields absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BboFrame {
    /// Coin name.
    pub coin: String,
    /// Milliseconds since the epoch.
    pub time: u64,
    /// Decimals of prices.
    pub px_decimals: u32,
    /// Decimals of sizes.
    pub sz_decimals: u32,
    /// Best bid price.
    pub bid_px: Option<u64>,
    /// Best bid size.
    pub bid_sz: Option<u64>,
    /// Orders at the best bid.
    pub bid_n: Option<u32>,
    /// Best ask price.
    pub ask_px: Option<u64>,
    /// Best ask size.
    pub ask_sz: Option<u64>,
    /// Orders at the best ask.
    pub ask_n: Option<u32>,
}

impl BboFrame {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes(&mut out, 1, self.coin.as_bytes());
        put_varint(&mut out, 2, self.time);
        put_varint(&mut out, 3, u64::from(self.px_decimals));
        put_varint(&mut out, 4, u64::from(self.sz_decimals));
        // The golden frames in testdata/frames.json write the counts after the
        // prices and sizes of both sides (5, 6, 8, 9, 7, 10). Any order is valid
        // Protobuf; this one keeps encoded bytes identical to the golden hex.
        let optional = [
            (5, self.bid_px),
            (6, self.bid_sz),
            (8, self.ask_px),
            (9, self.ask_sz),
            (7, self.bid_n.map(u64::from)),
            (10, self.ask_n.map(u64::from)),
        ];
        for (field, value) in optional {
            if let Some(v) = value {
                put_varint_always(&mut out, field, v);
            }
        }
        out
    }

    /// Decodes the message.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = BboFrame::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.coin = want_string(&wire, "bbo.coin")?,
                2 => m.time = want_varint(&wire, "bbo.time")?,
                3 => m.px_decimals = want_u32(&wire, "bbo.px_decimals")?,
                4 => m.sz_decimals = want_u32(&wire, "bbo.sz_decimals")?,
                5 => m.bid_px = Some(want_varint(&wire, "bbo.bid_px")?),
                6 => m.bid_sz = Some(want_varint(&wire, "bbo.bid_sz")?),
                7 => m.bid_n = Some(want_u32(&wire, "bbo.bid_n")?),
                8 => m.ask_px = Some(want_varint(&wire, "bbo.ask_px")?),
                9 => m.ask_sz = Some(want_varint(&wire, "bbo.ask_sz")?),
                10 => m.ask_n = Some(want_u32(&wire, "bbo.ask_n")?),
                _ => {}
            }
        }
        Ok(m)
    }
}

/// `CandleFrame`. Timestamps are milliseconds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CandleFrame {
    /// Coin name.
    pub coin: String,
    /// Interval such as `1m`.
    pub interval: String,
    /// Open time.
    pub open_time: u64,
    /// Close time.
    pub close_time: u64,
    /// Decimals of prices.
    pub px_decimals: u32,
    /// Decimals of sizes.
    pub sz_decimals: u32,
    /// Open price.
    pub open: u64,
    /// Close price.
    pub close: u64,
    /// High price.
    pub high: u64,
    /// Low price.
    pub low: u64,
    /// Volume, in `sz_decimals`.
    pub volume: u64,
    /// Number of trades.
    pub trade_count: u32,
}

impl CandleFrame {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes(&mut out, 1, self.coin.as_bytes());
        put_bytes(&mut out, 2, self.interval.as_bytes());
        put_varint(&mut out, 3, self.open_time);
        put_varint(&mut out, 4, self.close_time);
        put_varint(&mut out, 5, u64::from(self.px_decimals));
        put_varint(&mut out, 6, u64::from(self.sz_decimals));
        put_varint(&mut out, 7, self.open);
        put_varint(&mut out, 8, self.close);
        put_varint(&mut out, 9, self.high);
        put_varint(&mut out, 10, self.low);
        put_varint(&mut out, 11, self.volume);
        put_varint(&mut out, 12, u64::from(self.trade_count));
        out
    }

    /// Decodes the message.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = CandleFrame::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.coin = want_string(&wire, "candle.coin")?,
                2 => m.interval = want_string(&wire, "candle.interval")?,
                3 => m.open_time = want_varint(&wire, "candle.open_time")?,
                4 => m.close_time = want_varint(&wire, "candle.close_time")?,
                5 => m.px_decimals = want_u32(&wire, "candle.px_decimals")?,
                6 => m.sz_decimals = want_u32(&wire, "candle.sz_decimals")?,
                7 => m.open = want_varint(&wire, "candle.open")?,
                8 => m.close = want_varint(&wire, "candle.close")?,
                9 => m.high = want_varint(&wire, "candle.high")?,
                10 => m.low = want_varint(&wire, "candle.low")?,
                11 => m.volume = want_varint(&wire, "candle.volume")?,
                12 => m.trade_count = want_u32(&wire, "candle.trade_count")?,
                _ => {}
            }
        }
        Ok(m)
    }
}

/// One trade of a `TradesFrame`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Trade {
    /// True if the aggressor bought.
    pub buy: bool,
    /// Price, in the frame's `px_decimals`.
    pub px: u64,
    /// Size, in the frame's `sz_decimals`.
    pub sz: u64,
    /// Milliseconds since the epoch.
    pub time: u64,
    /// Trade ID.
    pub tid: u64,
    /// Transaction hash.
    pub hash: Vec<u8>,
    /// Buyer address, 20 bytes.
    pub buyer: Vec<u8>,
    /// Seller address, 20 bytes.
    pub seller: Vec<u8>,
}

impl Trade {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, 1, u64::from(self.buy));
        put_varint(&mut out, 2, self.px);
        put_varint(&mut out, 3, self.sz);
        put_varint(&mut out, 4, self.time);
        put_varint(&mut out, 5, self.tid);
        put_bytes(&mut out, 6, &self.hash);
        put_bytes(&mut out, 7, &self.buyer);
        put_bytes(&mut out, 8, &self.seller);
        out
    }

    fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = Trade::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.buy = want_varint(&wire, "trade.buy")? != 0,
                2 => m.px = want_varint(&wire, "trade.px")?,
                3 => m.sz = want_varint(&wire, "trade.sz")?,
                4 => m.time = want_varint(&wire, "trade.time")?,
                5 => m.tid = want_varint(&wire, "trade.tid")?,
                6 => m.hash = want_bytes(&wire, "trade.hash")?.to_vec(),
                7 => m.buyer = want_bytes(&wire, "trade.buyer")?.to_vec(),
                8 => m.seller = want_bytes(&wire, "trade.seller")?.to_vec(),
                _ => {}
            }
        }
        Ok(m)
    }
}

/// `TradesFrame`: several trades of one coin.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TradesFrame {
    /// Coin name.
    pub coin: String,
    /// Decimals of prices.
    pub px_decimals: u32,
    /// Decimals of sizes.
    pub sz_decimals: u32,
    /// The trades.
    pub trades: Vec<Trade>,
}

impl TradesFrame {
    /// Encodes the message.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_bytes(&mut out, 1, self.coin.as_bytes());
        put_varint(&mut out, 2, u64::from(self.px_decimals));
        put_varint(&mut out, 3, u64::from(self.sz_decimals));
        for t in &self.trades {
            put_bytes_always(&mut out, 4, &t.encode());
        }
        out
    }

    /// Decodes the message.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut m = TradesFrame::default();
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            match field {
                1 => m.coin = want_string(&wire, "trades.coin")?,
                2 => m.px_decimals = want_u32(&wire, "trades.px_decimals")?,
                3 => m.sz_decimals = want_u32(&wire, "trades.sz_decimals")?,
                4 => m
                    .trades
                    .push(Trade::decode(want_bytes(&wire, "trades.trade")?)?),
                _ => {}
            }
        }
        Ok(m)
    }
}

// ---------------------------------------------------------------- envelopes

/// The body of a `WsV2ClientFrame`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientBody {
    /// Exact signed action JSON bytes (field 2).
    ActionJson(Vec<u8>),
    /// Info query JSON (field 3).
    InfoJson(Vec<u8>),
    /// Subscription object JSON (field 4).
    SubscribeJson(Vec<u8>),
    /// Unsubscription object JSON (field 5).
    UnsubscribeJson(Vec<u8>),
    /// Empty ping (field 6).
    Ping,
    /// The login, which must be the first frame (field 7).
    Login(Login),
}

/// `WsV2ClientFrame`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClientFrame {
    /// Request id for action and info bodies; ignored otherwise.
    pub id: u64,
    /// The body.
    pub body: ClientBody,
}

impl ClientFrame {
    /// Encodes the frame. The body is always written, even when empty: for
    /// oneof members presence is the information (ping).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, 1, self.id);
        match &self.body {
            ClientBody::ActionJson(b) => put_bytes_always(&mut out, 2, b),
            ClientBody::InfoJson(b) => put_bytes_always(&mut out, 3, b),
            ClientBody::SubscribeJson(b) => put_bytes_always(&mut out, 4, b),
            ClientBody::UnsubscribeJson(b) => put_bytes_always(&mut out, 5, b),
            ClientBody::Ping => put_bytes_always(&mut out, 6, &[]),
            ClientBody::Login(l) => put_bytes_always(&mut out, 7, &l.encode()),
        }
        out
    }

    /// Decodes a client frame (used by test servers and tools).
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut id = 0;
        let mut body: Option<ClientBody> = None;
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            let next = match field {
                1 => {
                    id = want_varint(&wire, "client frame.id")?;
                    continue;
                }
                2 => ClientBody::ActionJson(want_bytes(&wire, "action_json")?.to_vec()),
                3 => ClientBody::InfoJson(want_bytes(&wire, "info_json")?.to_vec()),
                4 => ClientBody::SubscribeJson(want_bytes(&wire, "subscribe_json")?.to_vec()),
                5 => ClientBody::UnsubscribeJson(want_bytes(&wire, "unsubscribe_json")?.to_vec()),
                6 => {
                    want_bytes(&wire, "ping")?;
                    ClientBody::Ping
                }
                7 => ClientBody::Login(Login::decode(want_bytes(&wire, "login")?)?),
                _ => continue,
            };
            if body.replace(next).is_some() {
                return Err(bad("client frame has more than one body"));
            }
        }
        Ok(ClientFrame {
            id,
            body: body.ok_or_else(|| bad("client frame has no body"))?,
        })
    }
}

/// The body of a `WsV2ServerFrame`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerBody {
    /// A complete JSON frame, byte-identical to the JSON endpoint (field 2).
    Json(Vec<u8>),
    /// A full book (field 3).
    L2Book(L2BookFrame),
    /// An `l2` snapshot or update (field 4).
    L2Delta(L2DeltaFrame),
    /// Best bid and offer (field 5).
    Bbo(BboFrame),
    /// A candle (field 6).
    Candle(CandleFrame),
    /// Trades (field 7).
    Trades(TradesFrame),
    /// The login result (field 8).
    Login(LoginResult),
}

/// `WsV2ServerFrame`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerFrame {
    /// Request id for replies; 0 for pushes, acknowledgments and pong.
    pub id: u64,
    /// The body.
    pub body: ServerBody,
}

impl ServerFrame {
    /// Encodes the frame (used by test servers).
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, 1, self.id);
        match &self.body {
            ServerBody::Json(b) => put_bytes_always(&mut out, 2, b),
            ServerBody::L2Book(b) => put_bytes_always(&mut out, 3, &b.encode()),
            ServerBody::L2Delta(b) => put_bytes_always(&mut out, 4, &b.encode()),
            ServerBody::Bbo(b) => put_bytes_always(&mut out, 5, &b.encode()),
            ServerBody::Candle(b) => put_bytes_always(&mut out, 6, &b.encode()),
            ServerBody::Trades(b) => put_bytes_always(&mut out, 7, &b.encode()),
            ServerBody::Login(b) => put_bytes_always(&mut out, 8, &b.encode()),
        }
        out
    }

    /// Decodes a server frame. Unknown fields are skipped; a frame without a
    /// known body, or with two, is an error.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        let mut id = 0;
        let mut body: Option<ServerBody> = None;
        let mut r = Reader::new(buf);
        while let Some((field, wire)) = r.next()? {
            let next = match field {
                1 => {
                    id = want_varint(&wire, "server frame.id")?;
                    continue;
                }
                2 => ServerBody::Json(want_bytes(&wire, "json")?.to_vec()),
                3 => ServerBody::L2Book(L2BookFrame::decode(want_bytes(&wire, "l2_book")?)?),
                4 => ServerBody::L2Delta(L2DeltaFrame::decode(want_bytes(&wire, "l2_delta")?)?),
                5 => ServerBody::Bbo(BboFrame::decode(want_bytes(&wire, "bbo")?)?),
                6 => ServerBody::Candle(CandleFrame::decode(want_bytes(&wire, "candle")?)?),
                7 => ServerBody::Trades(TradesFrame::decode(want_bytes(&wire, "trades")?)?),
                8 => ServerBody::Login(LoginResult::decode(want_bytes(&wire, "login")?)?),
                _ => continue,
            };
            if body.replace(next).is_some() {
                return Err(bad("server frame has more than one body"));
            }
        }
        Ok(ServerFrame {
            id,
            body: body.ok_or_else(|| bad("server frame has no known body"))?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn client_frames_round_trip() {
        let frames = [
            ClientFrame {
                id: 0,
                body: ClientBody::Ping,
            },
            ClientFrame {
                id: 7,
                body: ClientBody::ActionJson(b"{\"a\":1}".to_vec()),
            },
            ClientFrame {
                id: 8,
                body: ClientBody::InfoJson(b"{}".to_vec()),
            },
            ClientFrame {
                id: 0,
                body: ClientBody::SubscribeJson(b"{\"type\":\"l2\",\"c\":\"BTC\"}".to_vec()),
            },
            ClientFrame {
                id: 0,
                body: ClientBody::UnsubscribeJson(b"{}".to_vec()),
            },
            ClientFrame {
                id: 0,
                body: ClientBody::Login(Login {
                    timestamp_ms: 1792168694973,
                    nonce: vec![1; 16],
                    signature: vec![2; 65],
                }),
            },
        ];
        for f in frames {
            assert_eq!(ClientFrame::decode(&f.encode()).unwrap(), f);
        }
        // A ping is a present, empty body: field 6, length 0.
        assert_eq!(
            ClientFrame {
                id: 0,
                body: ClientBody::Ping
            }
            .encode(),
            vec![0x32, 0x00]
        );
    }

    #[test]
    fn server_frames_round_trip() {
        let book = L2BookFrame {
            coin: "BTC".into(),
            time: 5,
            px_decimals: 1,
            sz_decimals: 5,
            bid_px: vec![1010, 1000],
            bid_sz: vec![5, 6],
            bid_n: vec![1, 2],
            ask_px: vec![1020],
            ask_sz: vec![7],
            ask_n: vec![3],
        };
        let update = L2DeltaUpdate {
            coin: "BTC".into(),
            time: 6,
            px_decimals: 1,
            sz_decimals: 5,
            bid_px: vec![1005],
            bid_sz: vec![9],
            bid_removed: vec![0],
            ask_removed: vec![0, 2],
            spread: Some(0),
            ..Default::default()
        };
        let frames = [
            ServerFrame {
                id: 3,
                body: ServerBody::Json(b"{\"channel\":\"pong\"}".to_vec()),
            },
            ServerFrame {
                id: 0,
                body: ServerBody::L2Book(book.clone()),
            },
            ServerFrame {
                id: 0,
                body: ServerBody::L2Delta(L2DeltaFrame::Snapshot(book)),
            },
            ServerFrame {
                id: 0,
                body: ServerBody::L2Delta(L2DeltaFrame::Update(update)),
            },
            ServerFrame {
                id: 0,
                body: ServerBody::Bbo(BboFrame {
                    coin: "ETH".into(),
                    time: 1,
                    px_decimals: 2,
                    sz_decimals: 4,
                    bid_px: Some(0),
                    ask_n: Some(2),
                    ..Default::default()
                }),
            },
            ServerFrame {
                id: 0,
                body: ServerBody::Candle(CandleFrame {
                    coin: "BTC".into(),
                    interval: "1m".into(),
                    open_time: 1,
                    close_time: 2,
                    open: 3,
                    close: 4,
                    high: 5,
                    low: 2,
                    volume: 9,
                    trade_count: 3,
                    px_decimals: 1,
                    sz_decimals: 5,
                }),
            },
            ServerFrame {
                id: 0,
                body: ServerBody::Trades(TradesFrame {
                    coin: "BTC".into(),
                    px_decimals: 1,
                    sz_decimals: 5,
                    trades: vec![Trade {
                        buy: true,
                        px: 10,
                        sz: 20,
                        time: 30,
                        tid: 40,
                        hash: vec![9; 32],
                        buyer: vec![1; 20],
                        seller: vec![2; 20],
                    }],
                }),
            },
            ServerFrame {
                id: 0,
                body: ServerBody::Login(LoginResult {
                    ok: true,
                    user: vec![1; 20],
                    signer: vec![2; 20],
                    error: String::new(),
                }),
            },
        ];
        for f in frames {
            assert_eq!(ServerFrame::decode(&f.encode()).unwrap(), f);
        }
    }

    #[test]
    fn decodes_unpacked_repeated_fields_and_skips_unknown() {
        // field 5 (bid_px) unpacked twice, field 6 (bid_sz) unpacked twice, unknown field 15 varint.
        let mut buf = Vec::new();
        buf.extend_from_slice(&[0x28, 10, 0x28, 20, 0x30, 1, 0x30, 2, 0x78, 5]);
        let book = L2BookFrame::decode(&buf).unwrap();
        assert_eq!(book.bid_px, vec![10, 20]);
        assert_eq!(book.bid_sz, vec![1, 2]);
    }

    #[test]
    fn rejects_malformed_input() {
        assert!(
            ServerFrame::decode(&[0x12, 0x05, 1]).is_err(),
            "truncated length"
        );
        assert!(ServerFrame::decode(&[]).is_err(), "no body");
        assert!(ServerFrame::decode(&[0x08]).is_err(), "truncated varint");
        let two = [
            ServerFrame {
                id: 0,
                body: ServerBody::Json(vec![]),
            }
            .encode(),
            ServerFrame {
                id: 0,
                body: ServerBody::Json(vec![]),
            }
            .encode(),
        ]
        .concat();
        assert!(ServerFrame::decode(&two).is_err(), "two bodies");
        // px/sz arrays of different length
        let bad_book = L2BookFrame {
            bid_px: vec![1, 2],
            bid_sz: vec![1],
            ..Default::default()
        }
        .encode();
        assert!(L2BookFrame::decode(&bad_book).is_err());
    }
}
