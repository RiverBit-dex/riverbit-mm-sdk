//! A local order book that replays an `l2` subscription.
//!
//! Rules (guide section 5): a snapshot replaces the entire book; an update
//! removes `bid_removed` and `ask_removed` as sets of indices into the
//! **previous** book, then overwrites or inserts levels by price and sorts
//! bids descending and asks ascending. Update decimals must match the latest
//! snapshot; otherwise unsubscribe and resubscribe.

use serde_json::Value;

use crate::decimal::{format_fixed, Decimal};
use crate::error::{Error, Result};
use crate::frames::Frame;
use crate::proto::{L2BookFrame, L2DeltaFrame, L2DeltaUpdate};

/// One price level. `px` and `sz` keep the wire text; compare prices with
/// [`px_decimal`](Level::px_decimal), never as floats.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Level {
    /// Price, a decimal string.
    pub px: String,
    /// Size, a decimal string.
    pub sz: String,
    /// Number of orders, when the source carries it (snapshots).
    pub n: Option<u32>,
    key: Decimal,
}

impl Level {
    fn new(px: String, sz: String, n: Option<u32>) -> Result<Level> {
        let key =
            Decimal::parse(&px).ok_or_else(|| Error::Book(format!("invalid price {px:?}")))?;
        if Decimal::parse(&sz).is_none() {
            return Err(Error::Book(format!("invalid size {sz:?}")));
        }
        Ok(Level { px, sz, n, key })
    }

    /// The price as an exact number.
    pub fn px_decimal(&self) -> Decimal {
        self.key
    }
}

/// The book of one coin.
#[derive(Clone, Debug)]
pub struct Book {
    coin: String,
    bids: Vec<Level>,
    asks: Vec<Level>,
    px_decimals: u32,
    sz_decimals: u32,
    /// True after a typed snapshot: typed updates must carry the same decimals.
    locked: bool,
    has_book: bool,
    time: u64,
    spread: Option<String>,
}

impl Book {
    /// An empty book for `coin`. Frames for other coins are rejected.
    pub fn new(coin: impl Into<String>) -> Self {
        Book {
            coin: coin.into(),
            bids: vec![],
            asks: vec![],
            px_decimals: 0,
            sz_decimals: 0,
            locked: false,
            has_book: false,
            time: 0,
            spread: None,
        }
    }

    /// The coin.
    pub fn coin(&self) -> &str {
        &self.coin
    }

    /// Bids, best (highest) first.
    pub fn bids(&self) -> &[Level] {
        &self.bids
    }

    /// Asks, best (lowest) first.
    pub fn asks(&self) -> &[Level] {
        &self.asks
    }

    /// The best bid, if any.
    pub fn best_bid(&self) -> Option<&Level> {
        self.bids.first()
    }

    /// The best ask, if any.
    pub fn best_ask(&self) -> Option<&Level> {
        self.asks.first()
    }

    /// True once a snapshot has been applied.
    pub fn has_book(&self) -> bool {
        self.has_book
    }

    /// The time of the last applied frame, milliseconds.
    pub fn time(&self) -> u64 {
        self.time
    }

    /// The spread (ask minus bid) carried by the last update, if it had one.
    pub fn spread(&self) -> Option<&str> {
        self.spread.as_deref()
    }

    /// Price and size decimals of the last typed snapshot.
    pub fn decimals(&self) -> (u32, u32) {
        (self.px_decimals, self.sz_decimals)
    }

    /// Applies a frame if it belongs to this book: a typed `l2` delta or a
    /// JSON frame of channel `l2`. Returns `Ok(false)` for any other frame.
    pub fn apply_frame(&mut self, frame: &Frame) -> Result<bool> {
        match frame {
            Frame::L2Delta(delta) => self.apply_delta(delta).map(|()| true),
            Frame::Json(json) if json.channel() == Some("l2") => {
                self.apply_json(json.data()).map(|()| true)
            }
            _ => Ok(false),
        }
    }

    /// Applies one typed `l2` frame.
    pub fn apply_delta(&mut self, delta: &L2DeltaFrame) -> Result<()> {
        match delta {
            L2DeltaFrame::Snapshot(s) => self.apply_snapshot(s),
            L2DeltaFrame::Update(u) => self.apply_update(u),
        }
    }

    fn check_coin(&self, coin: &str, what: &str) -> Result<()> {
        if !self.coin.is_empty() && !coin.is_empty() && coin != self.coin {
            return Err(Error::Book(format!(
                "{what} for {coin:?}, subscribed {:?}",
                self.coin
            )));
        }
        Ok(())
    }

    fn apply_snapshot(&mut self, s: &L2BookFrame) -> Result<()> {
        self.check_coin(&s.coin, "l2 snapshot")?;
        let side = |px: &[u64], sz: &[u64], n: &[u32]| -> Result<Vec<Level>> {
            px.iter()
                .zip(sz)
                .enumerate()
                .map(|(i, (p, z))| {
                    Level::new(
                        format_fixed(*p, s.px_decimals),
                        format_fixed(*z, s.sz_decimals),
                        n.get(i).copied(),
                    )
                })
                .collect()
        };
        let bids = side(&s.bid_px, &s.bid_sz, &s.bid_n)?;
        let asks = side(&s.ask_px, &s.ask_sz, &s.ask_n)?;
        check_ordered(&bids, &asks)?;
        self.bids = bids;
        self.asks = asks;
        self.px_decimals = s.px_decimals;
        self.sz_decimals = s.sz_decimals;
        self.locked = true;
        self.has_book = true;
        self.time = s.time;
        self.spread = None;
        Ok(())
    }

    fn apply_update(&mut self, u: &L2DeltaUpdate) -> Result<()> {
        if !self.has_book {
            return Err(Error::Book("l2 update before any snapshot".into()));
        }
        self.check_coin(&u.coin, "l2 update")?;
        if !self.locked {
            return Err(Error::Book(
                "typed l2 update on a book built from a JSON snapshot; resubscribe".into(),
            ));
        }
        if u.px_decimals != self.px_decimals || u.sz_decimals != self.sz_decimals {
            return Err(Error::Book(format!(
                "l2 update decimals ({},{}) differ from the snapshot's ({},{}); unsubscribe and resubscribe",
                u.px_decimals, u.sz_decimals, self.px_decimals, self.sz_decimals
            )));
        }
        let changed_side = |px: &[u64], sz: &[u64]| -> Result<Vec<Level>> {
            px.iter()
                .zip(sz)
                .map(|(p, z)| {
                    Level::new(
                        format_fixed(*p, u.px_decimals),
                        format_fixed(*z, u.sz_decimals),
                        None,
                    )
                })
                .collect()
        };
        let changed = [
            changed_side(&u.bid_px, &u.bid_sz)?,
            changed_side(&u.ask_px, &u.ask_sz)?,
        ];
        self.apply(changed, [&u.bid_removed, &u.ask_removed])?;
        self.time = u.time;
        self.spread = u.spread.map(|s| format_fixed(s, u.px_decimals));
        Ok(())
    }

    /// Applies the `data` member of a JSON frame of channel `l2`: either
    /// `{"s": {"coin","time","levels":[[{px,sz,n}],[..]]}}` (snapshot) or
    /// `{"u": {"c","t","l":[[{p,s}],[..]],"r":[[idx..],[idx..]],"s": spread}}`.
    pub fn apply_json(&mut self, data: &Value) -> Result<()> {
        if let Some(s) = data.get("s").filter(|v| v.is_object()) {
            self.check_coin(s["coin"].as_str().unwrap_or_default(), "l2 JSON snapshot")?;
            let mut sides: [Vec<Level>; 2] = [vec![], vec![]];
            for (i, side) in sides.iter_mut().enumerate() {
                for l in s["levels"][i]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                {
                    side.push(Level::new(
                        text_of(&l["px"])?,
                        text_of(&l["sz"])?,
                        l["n"].as_u64().and_then(|n| u32::try_from(n).ok()),
                    )?);
                }
            }
            check_ordered(&sides[0], &sides[1])?;
            let [bids, asks] = sides;
            self.bids = bids;
            self.asks = asks;
            self.locked = false;
            self.has_book = true;
            self.time = s["time"].as_u64().unwrap_or(0);
            self.spread = None;
            return Ok(());
        }
        if let Some(u) = data.get("u").filter(|v| v.is_object()) {
            if !self.has_book {
                return Err(Error::Book("l2 JSON update before any snapshot".into()));
            }
            self.check_coin(u["c"].as_str().unwrap_or_default(), "l2 JSON update")?;
            let mut changed: [Vec<Level>; 2] = [vec![], vec![]];
            let mut removed: [Vec<u32>; 2] = [vec![], vec![]];
            for i in 0..2 {
                for l in u["l"][i].as_array().map(Vec::as_slice).unwrap_or_default() {
                    changed[i].push(Level::new(text_of(&l["p"])?, text_of(&l["s"])?, None)?);
                }
                for idx in u["r"][i].as_array().map(Vec::as_slice).unwrap_or_default() {
                    let idx = idx
                        .as_u64()
                        .and_then(|v| u32::try_from(v).ok())
                        .ok_or_else(|| Error::Book("invalid removed index".into()))?;
                    removed[i].push(idx);
                }
            }
            self.apply(changed, [&removed[0], &removed[1]])?;
            self.time = u["t"].as_u64().unwrap_or(self.time);
            self.spread = match &u["s"] {
                Value::Null => None,
                other => text_of(other).ok().filter(|t| Decimal::parse(t).is_some()),
            };
            return Ok(());
        }
        Err(Error::Book("l2 JSON frame without s or u".into()))
    }

    /// Rules 2 to 4. Both sides are computed first and committed together, so
    /// an invalid update leaves the book untouched.
    fn apply(&mut self, changed: [Vec<Level>; 2], removed: [&[u32]; 2]) -> Result<()> {
        let previous = [&self.bids, &self.asks];
        let mut next: [Vec<Level>; 2] = [vec![], vec![]];
        for side in 0..2 {
            let prev = previous[side];
            // Rule 2: indices refer to the previous book and go as one set.
            let mut drop = vec![false; prev.len()];
            for &i in removed[side] {
                *drop.get_mut(i as usize).ok_or_else(|| {
                    Error::Book(format!(
                        "removed index {i} outside the previous book ({} levels)",
                        prev.len()
                    ))
                })? = true;
            }
            let mut levels: Vec<Level> = prev
                .iter()
                .zip(&drop)
                .filter(|(_, d)| !**d)
                .map(|(l, _)| l.clone())
                .collect();
            // Rule 3: overwrite by price, or insert.
            for level in &changed[side] {
                match levels.iter_mut().find(|l| l.key == level.key) {
                    Some(existing) => *existing = level.clone(),
                    None => levels.push(level.clone()),
                }
            }
            // Rule 4: bids descending, asks ascending.
            if side == 0 {
                levels.sort_by_key(|l| std::cmp::Reverse(l.key));
            } else {
                levels.sort_by_key(|l| l.key);
            }
            next[side] = levels;
        }
        check_ordered(&next[0], &next[1])?;
        let [bids, asks] = next;
        self.bids = bids;
        self.asks = asks;
        Ok(())
    }
}

fn text_of(v: &Value) -> Result<String> {
    v.as_str()
        .map(str::to_string)
        .ok_or_else(|| Error::Book("level price or size is not a string".into()))
}

fn check_ordered(bids: &[Level], asks: &[Level]) -> Result<()> {
    for (name, levels, descending) in [("bid", bids, true), ("ask", asks, false)] {
        for (i, pair) in levels.windows(2).enumerate() {
            let ordered = if descending {
                pair[0].key > pair[1].key
            } else {
                pair[0].key < pair[1].key
            };
            if !ordered {
                return Err(Error::Book(format!(
                    "{name} side is not strictly ordered at level {} ({}, {})",
                    i + 1,
                    pair[0].px,
                    pair[1].px
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snapshot() -> L2DeltaFrame {
        L2DeltaFrame::Snapshot(L2BookFrame {
            coin: "BTC".into(),
            time: 1,
            px_decimals: 1,
            sz_decimals: 3,
            bid_px: vec![1000, 990, 980],
            bid_sz: vec![1000, 2000, 3000],
            bid_n: vec![1, 2, 3],
            ask_px: vec![1010, 1020],
            ask_sz: vec![500, 600],
            ask_n: vec![1, 1],
        })
    }

    fn update() -> L2DeltaUpdate {
        L2DeltaUpdate {
            coin: "BTC".into(),
            time: 2,
            px_decimals: 1,
            sz_decimals: 3,
            ..Default::default()
        }
    }

    fn prices(levels: &[Level]) -> Vec<&str> {
        levels.iter().map(|l| l.px.as_str()).collect()
    }

    #[test]
    fn snapshot_then_updates() {
        let mut book = Book::new("BTC");
        book.apply_delta(&snapshot()).unwrap();
        assert_eq!(prices(book.bids()), ["100.0", "99.0", "98.0"]);
        assert_eq!(book.best_ask().unwrap().px, "101.0");
        assert_eq!(book.best_bid().unwrap().n, Some(1));

        // Remove bid index 0 and 2 (of the previous book) as one set, resize ask 102.0,
        // add a new better bid 99.5 and a new ask 101.5.
        let mut u = update();
        u.bid_removed = vec![0, 2];
        u.bid_px = vec![995];
        u.bid_sz = vec![7];
        u.ask_px = vec![1020, 1015];
        u.ask_sz = vec![1, 2];
        u.spread = Some(15);
        book.apply_delta(&L2DeltaFrame::Update(u)).unwrap();
        assert_eq!(prices(book.bids()), ["99.5", "99.0"]);
        assert_eq!(prices(book.asks()), ["101.0", "101.5", "102.0"]);
        assert_eq!(book.asks()[2].sz, "0.001");
        assert_eq!(book.spread(), Some("1.5"));
        assert_eq!(book.time(), 2);
    }

    #[test]
    fn a_new_snapshot_replaces_everything() {
        let mut book = Book::new("BTC");
        book.apply_delta(&snapshot()).unwrap();
        book.apply_delta(&L2DeltaFrame::Snapshot(L2BookFrame {
            coin: "BTC".into(),
            time: 9,
            px_decimals: 2,
            sz_decimals: 2,
            bid_px: vec![9900],
            bid_sz: vec![1],
            ..Default::default()
        }))
        .unwrap();
        assert_eq!(prices(book.bids()), ["99.00"]);
        assert!(book.asks().is_empty());
        assert_eq!(book.decimals(), (2, 2));
    }

    #[test]
    fn invalid_updates_leave_the_book_untouched() {
        let mut book = Book::new("BTC");
        assert!(
            book.apply_delta(&L2DeltaFrame::Update(update())).is_err(),
            "before snapshot"
        );
        book.apply_delta(&snapshot()).unwrap();
        let before = (prices(book.bids()).join(","), prices(book.asks()).join(","));

        let mut bad_index = update();
        bad_index.bid_removed = vec![9];
        assert!(book.apply_delta(&L2DeltaFrame::Update(bad_index)).is_err());

        let mut bad_decimals = update();
        bad_decimals.px_decimals = 2;
        assert!(book
            .apply_delta(&L2DeltaFrame::Update(bad_decimals))
            .is_err());

        // A valid update that removes every bid and adds one is applied.
        let mut replace_bids = update();
        replace_bids.bid_px = vec![2000];
        replace_bids.bid_sz = vec![1];
        replace_bids.bid_removed = vec![0, 1, 2];
        book.apply_delta(&L2DeltaFrame::Update(replace_bids))
            .unwrap();
        assert_eq!(prices(book.bids()), ["200.0"]);

        let mut other_coin = update();
        other_coin.coin = "ETH".into();
        assert!(book.apply_delta(&L2DeltaFrame::Update(other_coin)).is_err());
        assert_ne!(
            before.0,
            prices(book.bids()).join(","),
            "the valid update did change the book"
        );
        assert_eq!(prices(book.bids()), ["200.0"], "the failed update did not");
    }

    #[test]
    fn json_fallback_replays_the_same_rules() {
        let mut book = Book::new("BTC");
        book.apply_json(&json!({"s": {"coin": "BTC", "time": 5, "levels": [
            [{"px": "100.0", "sz": "1.0", "n": 1}, {"px": "99", "sz": "2", "n": 1}],
            [{"px": "101.0", "sz": "1.5", "n": 2}]
        ]}}))
        .unwrap();
        // Prices match by value: "100" overwrites "100.0".
        book.apply_json(&json!({"u": {"c": "BTC", "t": 6,
            "l": [[{"p": "100", "s": "9"}], [{"p": "100.5", "s": "1"}]],
            "r": [[1], []], "s": "0.5"}}))
            .unwrap();
        assert_eq!(prices(book.bids()), ["100"]);
        assert_eq!(book.bids()[0].sz, "9");
        assert_eq!(prices(book.asks()), ["100.5", "101.0"]);
        assert_eq!(book.spread(), Some("0.5"));
        // A typed update cannot follow a JSON snapshot.
        assert!(book.apply_delta(&L2DeltaFrame::Update(update())).is_err());
        assert!(book.apply_json(&json!({"x": 1})).is_err());
    }

    #[test]
    fn apply_frame_ignores_other_channels() {
        use crate::frames::JsonFrame;
        let mut book = Book::new("BTC");
        let json = |v: Value| {
            Frame::Json(JsonFrame {
                id: 0,
                raw: vec![],
                value: v,
            })
        };
        assert!(!book.apply_frame(&json(json!({"channel": "pong"}))).unwrap());
        assert!(book.apply_frame(&Frame::L2Delta(snapshot())).unwrap());
        assert!(!book.apply_frame(&Frame::Bbo(Default::default())).unwrap());
        assert!(book.has_book());
    }
}
