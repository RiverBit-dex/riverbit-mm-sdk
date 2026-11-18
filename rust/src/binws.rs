//! The bin-ws session: connect, log in, subscribe, post requests, reconnect.
//!
//! Every message is one Protobuf frame. The first frame on a connection is the
//! login. A background task owns the socket: it sends pings, matches
//! `info_json` replies to their requests by outer id, passes everything else
//! to [`Events`], and, when the connection drops, reconnects, logs in again and
//! resubscribes.
//!
//! Queries that were in flight when a connection ended fail; ask again after
//! [`Event::Reconnected`].
//!
//! bin-ws carries market data, order and fill events and `info_json` queries.
//! Orders, cancellations and modifications are sent with REST
//! `POST /exchange` (see [`ExchangeClient`](crate::ExchangeClient)).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_util::{SinkExt, StreamExt};
use serde_json::value::RawValue;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval_at, sleep, timeout, Instant, MissedTickBehavior};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async_with_config, MaybeTlsStream, WebSocketStream};

use crate::error::{Error, Result};
use crate::frames::Frame;
use crate::proto::{ClientBody, ClientFrame, LoginResult, ServerBody, ServerFrame};
use crate::signing::{login_message, recover_personal, Address, Signer};
use crate::subscription::Subscription;
use crate::transport::{BoxFuture, InfoTransport, TransportError};

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Reconnect behavior after a connection is lost.
#[derive(Clone, Debug)]
pub struct ReconnectPolicy {
    /// Wait before the first attempt; doubles after each failure.
    pub initial_backoff: Duration,
    /// Upper bound of the wait. The server allows 30 new connections a minute.
    pub max_backoff: Duration,
    /// Give up after this many failed attempts in a row. `None` retries forever.
    pub max_attempts: Option<u32>,
}

impl Default for ReconnectPolicy {
    fn default() -> Self {
        ReconnectPolicy {
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(30),
            max_attempts: None,
        }
    }
}

/// Configuration of one bin-ws connection. Nothing is tied to one environment.
#[derive(Clone, Debug)]
pub struct BinWsConfig {
    /// The WebSocket URL, for example `wss://pre-api.riverbit.io/bin-ws`.
    pub url: String,
    /// The `host:` line of the login message. It must match the public server
    /// hostname, for example `pre-api.riverbit.io`.
    pub login_host: String,
    /// Signs the login: the API wallet, or the master account itself.
    pub signer: Signer,
    /// How often to ping. The server drops a connection that receives
    /// nothing from it for 60 seconds; a ping elicits a pong. Default 20 s.
    pub ping_interval: Duration,
    /// Bound on connecting and on waiting for the login result. The server
    /// itself allows 10 seconds. Default 10 s.
    pub login_timeout: Duration,
    /// Treat the connection as dead if nothing arrives for this long. Default 60 s.
    pub idle_timeout: Duration,
    /// Bound on waiting for the reply to a posted request. A request that is
    /// still queued when it runs out is dropped unwritten. Default 20 s.
    pub request_timeout: Duration,
    /// Reconnect behavior. `None` ends the session when the connection drops.
    pub reconnect: Option<ReconnectPolicy>,
    /// Capacity of the event queue. When it is full the session waits for you,
    /// so drain [`Events`] promptly. Default 4096.
    pub event_capacity: usize,
}

impl BinWsConfig {
    /// A configuration with defaults and reconnect enabled.
    pub fn new(url: impl Into<String>, login_host: impl Into<String>, signer: Signer) -> Self {
        BinWsConfig {
            url: url.into(),
            login_host: login_host.into(),
            signer,
            ping_interval: Duration::from_secs(20),
            login_timeout: Duration::from_secs(10),
            idle_timeout: Duration::from_secs(60),
            request_timeout: Duration::from_secs(20),
            reconnect: Some(ReconnectPolicy::default()),
            event_capacity: 4096,
        }
    }
}

/// What the session reports besides replies to your requests.
#[derive(Clone, Debug)]
pub enum Event {
    /// A push, a subscription acknowledgment (`subscriptionResponse`) or pong.
    Frame(Frame),
    /// The connection was lost. Requests in flight have failed.
    Disconnected {
        /// Why.
        reason: String,
    },
    /// A new connection is logged in and all subscriptions were sent again.
    /// Local state is stale until you rebuild it: wait for the new snapshots
    /// and acknowledgments, query open orders, reconcile unknown outcomes, and
    /// only then process updates.
    Reconnected {
        /// The master account the server resolved from the login.
        user: Address,
        /// The signer the server recovered.
        signer: Address,
        /// How many connection attempts this outage took.
        attempts: u32,
    },
    /// The session has ended; no more events follow.
    Closed {
        /// Why.
        reason: String,
    },
}

/// The receiving end of a session's events.
pub struct Events {
    rx: mpsc::Receiver<Event>,
}

impl Events {
    /// The next event, or `None` after the session ended and the queue drained.
    pub async fn next(&mut self) -> Option<Event> {
        self.rx.recv().await
    }
}

/// Where a posted query is: not yet written, written, or given up on by its
/// caller. Whoever moves it out of `QUEUED` first decides: the session task
/// writes the frame only if it wins.
struct PostState(AtomicU8);

impl PostState {
    const QUEUED: u8 = 0;
    const WRITTEN: u8 = 1;
    const CANCELLED: u8 = 2;

    fn new() -> Arc<Self> {
        Arc::new(PostState(AtomicU8::new(Self::QUEUED)))
    }

    /// The session task is about to write the frame. False if the caller has
    /// already given up, in which case the frame must not be written.
    fn claim_for_write(&self) -> bool {
        self.0
            .compare_exchange(
                Self::QUEUED,
                Self::WRITTEN,
                Ordering::SeqCst,
                Ordering::SeqCst,
            )
            .is_ok()
    }

    /// The caller gives up. A frame that had not been claimed for writing is
    /// never written.
    fn cancel(&self) {
        let _ = self.0.compare_exchange(
            Self::QUEUED,
            Self::CANCELLED,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

/// Cancels a queued post when the caller's future is dropped (for example by
/// an outer timeout), so a query nobody waits for is never written later.
struct CancelOnDrop(Arc<PostState>);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

enum Cmd {
    Post {
        id: u64,
        frame: Vec<u8>,
        state: Arc<PostState>,
        reply: oneshot::Sender<std::result::Result<ServerFrame, TransportError>>,
    },
    Subscribe {
        json: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    Unsubscribe {
        json: Vec<u8>,
        reply: oneshot::Sender<Result<()>>,
    },
    Ping {
        reply: oneshot::Sender<Result<()>>,
    },
    Close,
}

struct Inner {
    cmd: mpsc::Sender<Cmd>,
    next_id: AtomicU64,
    request_timeout: Duration,
    connected: AtomicBool,
    login: Mutex<(Address, Address)>,
}

/// A logged-in bin-ws connection. Cheap to clone; all clones share one
/// connection. Dropping every clone closes it.
#[derive(Clone)]
pub struct BinWsSession {
    inner: Arc<Inner>,
}

impl BinWsSession {
    /// Connects, logs in as the first frame and starts the session task.
    /// The first connection is not retried: a login failure is returned.
    pub async fn connect(config: BinWsConfig) -> Result<(BinWsSession, Events)> {
        let (ws, result) = dial(&config).await?;
        let login = parse_login(&config, &result)?;
        let (cmd_tx, cmd_rx) = mpsc::channel(256);
        let (event_tx, event_rx) = mpsc::channel(config.event_capacity.max(1));
        let inner = Arc::new(Inner {
            cmd: cmd_tx,
            next_id: AtomicU64::new(0),
            request_timeout: config.request_timeout,
            connected: AtomicBool::new(true),
            login: Mutex::new(login),
        });
        let task = Task {
            cfg: config,
            cmd_rx,
            events: event_tx,
            inner: inner.clone(),
            subs: Vec::new(),
            pending: HashMap::new(),
        };
        tokio::spawn(task.run(ws));
        Ok((BinWsSession { inner }, Events { rx: event_rx }))
    }

    /// The master account the server resolved from the login.
    pub fn user(&self) -> Address {
        self.login().0
    }

    /// The signer the server recovered from the login.
    pub fn signer(&self) -> Address {
        self.login().1
    }

    fn login(&self) -> (Address, Address) {
        *self.inner.login.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// True while a logged-in connection is up.
    pub fn is_connected(&self) -> bool {
        self.inner.connected.load(Ordering::SeqCst)
    }

    /// Subscribes. The subscription is remembered and sent again after every
    /// reconnect. The acknowledgment arrives as a `subscriptionResponse` frame
    /// in [`Events`].
    pub async fn subscribe(&self, subscription: &Subscription) -> Result<()> {
        self.control(|reply| Cmd::Subscribe {
            json: subscription.to_bytes(),
            reply,
        })
        .await
    }

    /// Unsubscribes and forgets the subscription.
    pub async fn unsubscribe(&self, subscription: &Subscription) -> Result<()> {
        self.control(|reply| Cmd::Unsubscribe {
            json: subscription.to_bytes(),
            reply,
        })
        .await
    }

    /// Sends a ping; the pong arrives as a JSON frame in [`Events`]. The
    /// session already pings on its own every `ping_interval`.
    pub async fn ping(&self) -> Result<()> {
        self.control(|reply| Cmd::Ping { reply }).await
    }

    /// Runs one `info_json` query and returns the answer's `data`, the same
    /// JSON REST would return. Only the query types bin-ws accepts work.
    pub async fn info(&self, query: &Value) -> Result<Value> {
        let bytes = serde_json::to_vec(query).map_err(|e| Error::InvalidInput(e.to_string()))?;
        let data = self
            .post_info(&bytes)
            .await
            .map_err(|e| Error::Transport(format!("bin-ws info: {e:?}")))?;
        serde_json::from_slice(&data).map_err(|e| Error::Decode(format!("bin-ws info: {e}")))
    }

    /// Closes the connection and ends the session. Open orders stay open.
    pub async fn close(&self) {
        let _ = self.inner.cmd.send(Cmd::Close).await;
    }

    async fn control(&self, make: impl FnOnce(oneshot::Sender<Result<()>>) -> Cmd) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.inner
            .cmd
            .send(make(tx))
            .await
            .map_err(|_| Error::Closed("session task ended".into()))?;
        rx.await
            .map_err(|_| Error::Closed("session task ended".into()))?
    }

    async fn post(&self, body: &[u8]) -> std::result::Result<Box<RawValue>, TransportError> {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let frame = ClientFrame {
            id,
            body: ClientBody::InfoJson(body.to_vec()),
        }
        .encode();
        let (tx, rx) = oneshot::channel();
        let state = PostState::new();
        // Declared before the send so that every way out of this function,
        // including the future being dropped while it waits for queue space,
        // marks the post as given up.
        let _guard = CancelOnDrop(state.clone());
        self.inner
            .cmd
            .send(Cmd::Post {
                id,
                frame,
                state: state.clone(),
                reply: tx,
            })
            .await
            .map_err(|_| TransportError::SendFailed("session task ended".into()))?;
        let reply = match timeout(self.inner.request_timeout, rx).await {
            // A frame still queued is dropped by the session task once the
            // guard cancels it.
            Err(_) => return Err(TransportError::Timeout),
            Ok(Err(_)) => return Err(TransportError::Disconnected("session task ended".into())),
            Ok(Ok(result)) => result?,
        };
        parse_post_reply(&reply, id)
    }
}

impl InfoTransport for BinWsSession {
    /// Sends `body` as `info_json`; returns the `data` of the answer.
    fn post_info<'a>(
        &'a self,
        body: &'a [u8],
    ) -> BoxFuture<'a, std::result::Result<Vec<u8>, TransportError>> {
        Box::pin(async move {
            let kind = serde_json::from_slice::<Value>(body)
                .ok()
                .and_then(|v| v["type"].as_str().map(str::to_string))
                .ok_or_else(|| TransportError::Other("info query without a type".into()))?;
            let payload = self.post(body).await?;
            #[derive(serde::Deserialize)]
            struct Wrapped {
                #[serde(rename = "type")]
                kind: String,
                data: Box<RawValue>,
            }
            let wrapped: Wrapped = serde_json::from_str(payload.get())
                .map_err(|e| TransportError::Other(format!("info payload: {e}")))?;
            if wrapped.kind != kind {
                return Err(TransportError::Other(format!(
                    "info reply of type {:?} for a {:?} query",
                    wrapped.kind, kind
                )));
            }
            Ok(wrapped.data.get().as_bytes().to_vec())
        })
    }
}

/// `{"channel":"post","data":{"id":N,"response":{"type":"info"|"error","payload":..}}}`
fn parse_post_reply(
    frame: &ServerFrame,
    id: u64,
) -> std::result::Result<Box<RawValue>, TransportError> {
    #[derive(serde::Deserialize)]
    struct Push {
        channel: String,
        data: Data,
    }
    #[derive(serde::Deserialize)]
    struct Data {
        id: Option<u64>,
        response: Response,
    }
    #[derive(serde::Deserialize)]
    struct Response {
        #[serde(rename = "type")]
        kind: String,
        payload: Option<Box<RawValue>>,
    }
    let other = |m: String| TransportError::Other(m);
    if frame.id != id {
        return Err(other(format!(
            "reply frame id {}, request id {id}",
            frame.id
        )));
    }
    let ServerBody::Json(raw) = &frame.body else {
        return Err(other(format!("reply to request {id} is not a JSON frame")));
    };
    let push: Push =
        serde_json::from_slice(raw).map_err(|e| other(format!("reply to request {id}: {e}")))?;
    if push.channel != "post" {
        return Err(other(format!(
            "reply to request {id} has channel {:?}",
            push.channel
        )));
    }
    if push.data.id.is_some_and(|d| d != id) {
        return Err(other(format!(
            "reply to request {id} carries another data.id"
        )));
    }
    match (push.data.response.kind.as_str(), push.data.response.payload) {
        ("info", Some(payload)) => Ok(payload),
        ("error", payload) => {
            let text = payload
                .map(|p| match serde_json::from_str::<String>(p.get()) {
                    Ok(s) => s,
                    Err(_) => p.get().to_string(),
                })
                .unwrap_or_default();
            Err(TransportError::ErrorReply(text))
        }
        (kind, _) => Err(other(format!(
            "reply to request {id} has response type {kind:?}, want \"info\""
        ))),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn parse_login(cfg: &BinWsConfig, result: &LoginResult) -> Result<(Address, Address)> {
    let to_addr = |bytes: &[u8], what: &str| -> Result<Address> {
        let arr: [u8; 20] = bytes.try_into().map_err(|_| {
            Error::Login(format!(
                "login result {what} is {} bytes, want 20",
                bytes.len()
            ))
        })?;
        Ok(Address::from_bytes(arr))
    };
    let user = to_addr(&result.user, "user")?;
    let signer = to_addr(&result.signer, "signer")?;
    if signer != cfg.signer.address() {
        return Err(Error::Login(format!(
            "server recovered signer {signer}, local key is {} (check the login host)",
            cfg.signer.address()
        )));
    }
    Ok((user, signer))
}

/// Connects and performs the login handshake.
async fn dial(cfg: &BinWsConfig) -> Result<(Ws, LoginResult)> {
    let ws_config = WebSocketConfig::default()
        .max_message_size(Some(2 << 20))
        .max_frame_size(Some(2 << 20));
    let (mut ws, _) = timeout(
        cfg.login_timeout,
        connect_async_with_config(cfg.url.as_str(), Some(ws_config), false),
    )
    .await
    .map_err(|_| Error::Transport(format!("connect {}: timeout", cfg.url)))?
    .map_err(|e| Error::Transport(format!("connect {}: {e}", cfg.url)))?;

    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|e| Error::Login(format!("login nonce: {e}")))?;
    let timestamp = now_ms();
    let login = cfg.signer.sign_login(&cfg.login_host, timestamp, nonce)?;
    // Verify our own signature the way the server will.
    let message = login_message(&cfg.login_host, timestamp, &nonce);
    if recover_personal(message.as_bytes(), &login.signature)? != cfg.signer.address() {
        return Err(Error::Login(
            "local login signature does not recover the signer".into(),
        ));
    }
    let frame = ClientFrame {
        id: 0,
        body: ClientBody::Login(login),
    }
    .encode();
    ws.send(Message::Binary(frame.into()))
        .await
        .map_err(|e| Error::Login(format!("send login: {e}")))?;

    let result = timeout(cfg.login_timeout, async {
        while let Some(message) = ws.next().await {
            match message.map_err(|e| Error::Login(format!("read login result: {e}")))? {
                Message::Binary(bytes) => {
                    let frame = ServerFrame::decode(&bytes)?;
                    return match frame.body {
                        ServerBody::Login(result) => Ok(result),
                        _ => Err(Error::Login(
                            "first server frame is not a login result".into(),
                        )),
                    };
                }
                Message::Ping(_) | Message::Pong(_) => continue,
                Message::Close(_) => {
                    return Err(Error::Login(
                        "server closed the connection during login".into(),
                    ))
                }
                _ => {
                    return Err(Error::Login(
                        "server sent a non-binary frame during login".into(),
                    ))
                }
            }
        }
        Err(Error::Login(
            "connection ended before the login result".into(),
        ))
    })
    .await
    .map_err(|_| Error::Login("timed out waiting for the login result".into()))??;

    if !result.ok {
        return Err(Error::Login(format!("rejected: {}", result.error)));
    }
    Ok((ws, result))
}

type Reply = oneshot::Sender<std::result::Result<ServerFrame, TransportError>>;

enum End {
    Closed,
    Lost(String),
}

struct Task {
    cfg: BinWsConfig,
    cmd_rx: mpsc::Receiver<Cmd>,
    events: mpsc::Sender<Event>,
    inner: Arc<Inner>,
    subs: Vec<Vec<u8>>,
    pending: HashMap<u64, Reply>,
}

impl Task {
    async fn run(mut self, mut ws: Ws) {
        loop {
            match self.serve(&mut ws).await {
                End::Closed => {
                    let _ = timeout(Duration::from_secs(1), ws.close(None)).await;
                    self.go_offline("closed by the client");
                    self.emit(Event::Closed {
                        reason: "closed by the client".into(),
                    })
                    .await;
                    return;
                }
                End::Lost(reason) => {
                    self.go_offline(&reason);
                    self.emit(Event::Disconnected {
                        reason: reason.clone(),
                    })
                    .await;
                    let Some(policy) = self.cfg.reconnect.clone() else {
                        self.emit(Event::Closed { reason }).await;
                        return;
                    };
                    match self.reconnect(&policy).await {
                        Ok(new_ws) => ws = new_ws,
                        Err(reason) => {
                            self.emit(Event::Closed { reason }).await;
                            return;
                        }
                    }
                }
            }
        }
    }

    async fn emit(&self, event: Event) {
        // A dropped Events handle is not an error: the session keeps working.
        let _ = self.events.send(event).await;
    }

    fn go_offline(&mut self, reason: &str) {
        self.inner.connected.store(false, Ordering::SeqCst);
        for (_, reply) in self.pending.drain() {
            let _ = reply.send(Err(TransportError::Disconnected(reason.to_string())));
        }
    }

    async fn write(ws: &mut Ws, frame: ClientFrame) -> std::result::Result<(), String> {
        ws.send(Message::Binary(frame.encode().into()))
            .await
            .map_err(|e| format!("write: {e}"))
    }

    async fn serve(&mut self, ws: &mut Ws) -> End {
        let period = self.cfg.ping_interval.max(Duration::from_millis(10));
        let mut pings = interval_at(Instant::now() + period, period);
        pings.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let idle = sleep(self.cfg.idle_timeout);
        tokio::pin!(idle);
        loop {
            tokio::select! {
                message = ws.next() => {
                    idle.as_mut().reset(Instant::now() + self.cfg.idle_timeout);
                    match message {
                        None => return End::Lost("connection closed".into()),
                        Some(Err(e)) => return End::Lost(format!("read: {e}")),
                        Some(Ok(Message::Binary(bytes))) => {
                            if let Err(reason) = self.on_server_bytes(&bytes).await {
                                return End::Lost(reason);
                            }
                        }
                        Some(Ok(Message::Close(_))) => return End::Lost("closed by the server".into()),
                        Some(Ok(_)) => {}
                    }
                }
                cmd = self.cmd_rx.recv() => {
                    let Some(cmd) = cmd else { return End::Closed };
                    match self.on_cmd(ws, cmd).await {
                        Ok(true) => {}
                        Ok(false) => return End::Closed,
                        Err(reason) => return End::Lost(reason),
                    }
                }
                _ = pings.tick() => {
                    if let Err(reason) = Self::write(ws, ClientFrame { id: 0, body: ClientBody::Ping }).await {
                        return End::Lost(reason);
                    }
                }
                _ = &mut idle => {
                    return End::Lost(format!("no message from the server for {:?}", self.cfg.idle_timeout));
                }
            }
        }
    }

    async fn on_server_bytes(&mut self, bytes: &[u8]) -> std::result::Result<(), String> {
        // A frame that cannot be decoded may hide a book update or a trading
        // reply, so the connection is dropped rather than skipping it.
        let frame = ServerFrame::decode(bytes).map_err(|e| format!("decode server frame: {e}"))?;
        if frame.id != 0 {
            if let Some(reply) = self.pending.remove(&frame.id) {
                let _ = reply.send(Ok(frame));
                return Ok(());
            }
        }
        match Frame::from_server(frame) {
            Ok(frame) => self.emit(Event::Frame(frame)).await,
            Err(e) => return Err(format!("{e}")),
        }
        Ok(())
    }

    /// Returns Ok(false) when the session should end.
    async fn on_cmd(&mut self, ws: &mut Ws, cmd: Cmd) -> std::result::Result<bool, String> {
        match cmd {
            Cmd::Close => return Ok(false),
            Cmd::Post {
                id,
                frame,
                state,
                reply,
            } => {
                // The caller may have given up while this was queued behind
                // a full event queue; nobody waits for the reply any more.
                if !state.claim_for_write() {
                    return Ok(true);
                }
                if self.pending.len() > 64 {
                    self.pending.retain(|_, r| !r.is_closed());
                }
                self.pending.insert(id, reply);
                if let Err(e) = ws.send(Message::Binary(frame.into())).await {
                    if let Some(reply) = self.pending.remove(&id) {
                        let _ =
                            reply.send(Err(TransportError::Disconnected(format!("write: {e}"))));
                    }
                    return Err(format!("write: {e}"));
                }
            }
            Cmd::Subscribe { json, reply } => {
                if !self.subs.contains(&json) {
                    self.subs.push(json.clone());
                }
                let result = Self::write(
                    ws,
                    ClientFrame {
                        id: 0,
                        body: ClientBody::SubscribeJson(json),
                    },
                )
                .await;
                let _ = reply.send(result.clone().map_err(Error::Transport));
                result?;
            }
            Cmd::Unsubscribe { json, reply } => {
                self.subs.retain(|s| *s != json);
                let result = Self::write(
                    ws,
                    ClientFrame {
                        id: 0,
                        body: ClientBody::UnsubscribeJson(json),
                    },
                )
                .await;
                let _ = reply.send(result.clone().map_err(Error::Transport));
                result?;
            }
            Cmd::Ping { reply } => {
                let result = Self::write(
                    ws,
                    ClientFrame {
                        id: 0,
                        body: ClientBody::Ping,
                    },
                )
                .await;
                let _ = reply.send(result.clone().map_err(Error::Transport));
                result?;
            }
        }
        Ok(true)
    }

    /// Answers a command while there is no connection. Returns false to end.
    fn on_offline_cmd(&mut self, cmd: Cmd) -> bool {
        let down = || Error::Closed("not connected; reconnecting".into());
        match cmd {
            Cmd::Close => return false,
            Cmd::Post { reply, .. } => {
                let _ = reply.send(Err(TransportError::SendFailed("not connected".into())));
            }
            // Subscriptions are remembered and sent after the next login.
            Cmd::Subscribe { json, reply } => {
                if !self.subs.contains(&json) {
                    self.subs.push(json);
                }
                let _ = reply.send(Ok(()));
            }
            Cmd::Unsubscribe { json, reply } => {
                self.subs.retain(|s| *s != json);
                let _ = reply.send(Ok(()));
            }
            Cmd::Ping { reply } => {
                let _ = reply.send(Err(down()));
            }
        }
        true
    }

    async fn reconnect(&mut self, policy: &ReconnectPolicy) -> std::result::Result<Ws, String> {
        let mut attempts = 0u32;
        let mut backoff = policy.initial_backoff;
        loop {
            // Wait, answering commands meanwhile.
            let wait = sleep(backoff);
            tokio::pin!(wait);
            loop {
                tokio::select! {
                    _ = &mut wait => break,
                    cmd = self.cmd_rx.recv() => match cmd {
                        None => return Err("all session handles dropped".into()),
                        Some(cmd) => if !self.on_offline_cmd(cmd) { return Err("closed by the client".into()) },
                    }
                }
            }
            attempts += 1;
            let cfg = self.cfg.clone();
            let mut attempt = Box::pin(dial(&cfg));
            let outcome = loop {
                tokio::select! {
                    result = &mut attempt => break result,
                    cmd = self.cmd_rx.recv() => match cmd {
                        None => return Err("all session handles dropped".into()),
                        Some(cmd) => if !self.on_offline_cmd(cmd) { return Err("closed by the client".into()) },
                    }
                }
            };
            drop(attempt);
            match self.finish_reconnect(outcome, attempts).await {
                Ok(ws) => return Ok(ws),
                Err(reason) => {
                    if policy.max_attempts.is_some_and(|max| attempts >= max) {
                        return Err(format!("giving up after {attempts} attempts: {reason}"));
                    }
                    backoff = (backoff * 2).min(policy.max_backoff);
                }
            }
        }
    }

    async fn finish_reconnect(
        &mut self,
        outcome: Result<(Ws, LoginResult)>,
        attempts: u32,
    ) -> std::result::Result<Ws, String> {
        let (mut ws, result) = outcome.map_err(|e| e.to_string())?;
        let (user, signer) = parse_login(&self.cfg, &result).map_err(|e| e.to_string())?;
        for json in self.subs.clone() {
            Self::write(
                &mut ws,
                ClientFrame {
                    id: 0,
                    body: ClientBody::SubscribeJson(json),
                },
            )
            .await?;
        }
        *self.inner.login.lock().unwrap_or_else(|p| p.into_inner()) = (user, signer);
        self.inner.connected.store(true, Ordering::SeqCst);
        self.emit(Event::Reconnected {
            user,
            signer,
            attempts,
        })
        .await;
        Ok(ws)
    }
}
