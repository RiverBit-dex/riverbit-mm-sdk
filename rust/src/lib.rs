#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![doc = include_str!("../README.md")]
pub mod actions;
pub mod binws;
pub mod book;
pub mod cloid;
pub mod decimal;
pub mod error;
pub mod exchange;
pub mod frames;
pub mod info;
mod msgpack;
pub mod nonce;
pub mod proto;
pub mod receipt;
pub mod reconcile;
pub mod signing;
pub mod subscription;
pub mod transport;

pub use actions::{
    Action, ActionKind, BatchModify, Cancel, CancelByCloid, CancelByCloidItem, CancelItem, Modify,
    ModifyItem, Order, OrderRef, Place, ReserveRequestWeight, Tif,
};
pub use binws::{BinWsConfig, BinWsSession, Event, Events, ReconnectPolicy};
pub use book::Book;
pub use cloid::Cloid;
pub use error::{
    is_quota_rejection, Error, Rejected, Result, UnknownOutcome, UnknownReason,
    CODE_PURCHASE_OVERFLOW, CODE_QUOTA_EXHAUSTED, QUOTA_REJECTION_TEXT,
};
pub use exchange::ExchangeClient;
pub use frames::{Frame, JsonFrame};
pub use info::{InfoClient, UserRateLimit};
pub use nonce::NonceSource;
pub use receipt::{LegStatus, Receipt};
pub use reconcile::{reconcile, reconcile_orders, OrderReport};
pub use signing::{Address, SignedRequest, Signer, SigningDomain};
pub use subscription::Subscription;
