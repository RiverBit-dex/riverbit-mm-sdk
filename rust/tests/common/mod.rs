//! Test servers: an in-process HTTP server and an in-process bin-ws server.
//! Both listen on 127.0.0.1 only.

#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use futures_util::{SinkExt, StreamExt};
use riverbit_mm_sdk::proto::{ClientBody, ClientFrame, LoginResult, ServerBody, ServerFrame};
use riverbit_mm_sdk::signing::{login_message, recover_personal, Address};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;

pub const TEST_KEY: &str = "0x0101010101010101010101010101010101010101010101010101010101010101";

// ------------------------------------------------------------------ HTTP

#[derive(Clone, Debug)]
pub struct HttpRequest {
    pub path: String,
    pub body: Vec<u8>,
}

pub struct HttpReply {
    pub status: u16,
    pub headers: Vec<(&'static str, String)>,
    pub body: String,
}

impl HttpReply {
    pub fn ok(body: &str) -> Self {
        HttpReply {
            status: 200,
            headers: vec![],
            body: body.to_string(),
        }
    }
    pub fn status(status: u16, body: &str) -> Self {
        HttpReply {
            status,
            headers: vec![],
            body: body.to_string(),
        }
    }
    pub fn with_header(mut self, name: &'static str, value: &str) -> Self {
        self.headers.push((name, value.to_string()));
        self
    }
}

pub struct HttpServer {
    pub base_url: String,
    pub requests: Arc<Mutex<Vec<HttpRequest>>>,
}

/// Starts a server that answers each request with `handler(index, request)`.
pub async fn http_server<F>(handler: F) -> HttpServer
where
    F: Fn(usize, &HttpRequest) -> HttpReply + Send + Sync + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let handler = Arc::new(handler);
    let log = requests.clone();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let handler = handler.clone();
            let log = log.clone();
            tokio::spawn(async move { serve_http(stream, handler, log).await });
        }
    });
    HttpServer { base_url, requests }
}

async fn serve_http<F>(mut stream: TcpStream, handler: Arc<F>, log: Arc<Mutex<Vec<HttpRequest>>>)
where
    F: Fn(usize, &HttpRequest) -> HttpReply,
{
    let mut buf: Vec<u8> = Vec::new();
    loop {
        // Read one request: headers, then Content-Length bytes.
        let header_end = loop {
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        };
        let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
        let path = head.split_whitespace().nth(1).unwrap_or("/").to_string();
        let length = head
            .lines()
            .find_map(|l| {
                l.to_ascii_lowercase()
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while buf.len() < header_end + length {
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => return,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
        let body = buf[header_end..header_end + length].to_vec();
        buf.drain(..header_end + length);
        let request = HttpRequest { path, body };
        let index = {
            let mut log = log.lock().unwrap();
            log.push(request.clone());
            log.len() - 1
        };
        let reply = handler(index, &request);
        let mut out = format!(
            "HTTP/1.1 {} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n",
            reply.status,
            reply.body.len()
        );
        for (name, value) in &reply.headers {
            out.push_str(&format!("{name}: {value}\r\n"));
        }
        out.push_str("\r\n");
        out.push_str(&reply.body);
        if stream.write_all(out.as_bytes()).await.is_err() {
            return;
        }
    }
}

// ------------------------------------------------------------------ bin-ws

/// What the mock server saw from a client.
#[derive(Debug)]
pub enum Seen {
    /// A successful login with the recovered signer.
    Login {
        signer: Address,
        timestamp_ms: u64,
        nonce: Vec<u8>,
    },
    /// Any frame after the login.
    Frame(ClientFrame),
    /// The connection ended.
    Gone,
}

/// A handle to one accepted connection.
pub struct ServerConn {
    pub send: mpsc::UnboundedSender<ServerAction>,
}

pub enum ServerAction {
    Frame(Box<ServerFrame>),
    Drop,
}

impl ServerConn {
    pub fn push(&self, frame: ServerFrame) {
        self.send
            .send(ServerAction::Frame(Box::new(frame)))
            .unwrap();
    }
    pub fn json(&self, id: u64, body: &str) {
        self.push(ServerFrame {
            id,
            body: ServerBody::Json(body.as_bytes().to_vec()),
        });
    }
    pub fn drop_connection(&self) {
        self.send.send(ServerAction::Drop).unwrap();
    }
}

pub struct WsServer {
    pub url: String,
    /// Events from all connections, in order.
    pub seen: mpsc::UnboundedReceiver<(usize, Seen)>,
    /// Handles of accepted connections, in accept order.
    pub conns: mpsc::UnboundedReceiver<ServerConn>,
}

pub struct WsServerOptions {
    pub login_host: String,
    /// Master account reported in a successful login result.
    pub user: Address,
    /// Reject every login with this text.
    pub reject: Option<String>,
}

pub async fn ws_server(options: WsServerOptions) -> WsServer {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ws://{}/bin-ws", listener.local_addr().unwrap());
    let (seen_tx, seen) = mpsc::unbounded_channel();
    let (conn_tx, conns) = mpsc::unbounded_channel();
    let options = Arc::new(options);
    tokio::spawn(async move {
        let mut index = 0usize;
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                return;
            };
            let seen_tx = seen_tx.clone();
            let conn_tx = conn_tx.clone();
            let options = options.clone();
            let n = index;
            index += 1;
            tokio::spawn(async move {
                let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
                    return;
                };
                // The first frame must be the login.
                let Some(Ok(Message::Binary(first))) = ws.next().await else {
                    return;
                };
                let frame = ClientFrame::decode(&first).unwrap();
                let ClientBody::Login(login) = frame.body else {
                    panic!("first frame is not a login: {frame:?}");
                };
                let message = login_message(&options.login_host, login.timestamp_ms, &login.nonce);
                let signer = recover_personal(message.as_bytes(), &login.signature).unwrap();
                let result = match &options.reject {
                    Some(error) => LoginResult {
                        ok: false,
                        error: error.clone(),
                        ..Default::default()
                    },
                    None => LoginResult {
                        ok: true,
                        user: options.user.as_bytes().to_vec(),
                        signer: signer.as_bytes().to_vec(),
                        error: String::new(),
                    },
                };
                ws.send(Message::Binary(
                    ServerFrame {
                        id: 0,
                        body: ServerBody::Login(result),
                    }
                    .encode()
                    .into(),
                ))
                .await
                .unwrap();
                if options.reject.is_some() {
                    return;
                }
                let _ = seen_tx.send((
                    n,
                    Seen::Login {
                        signer,
                        timestamp_ms: login.timestamp_ms,
                        nonce: login.nonce,
                    },
                ));
                let (act_tx, mut act_rx) = mpsc::unbounded_channel();
                let _ = conn_tx.send(ServerConn { send: act_tx });
                loop {
                    tokio::select! {
                        msg = ws.next() => match msg {
                            Some(Ok(Message::Binary(bytes))) => {
                                let _ = seen_tx.send((n, Seen::Frame(ClientFrame::decode(&bytes).unwrap())));
                            }
                            Some(Ok(_)) => {}
                            _ => break,
                        },
                        act = act_rx.recv() => match act {
                            Some(ServerAction::Frame(f)) => {
                                if ws.send(Message::Binary(f.encode().into())).await.is_err() { break; }
                            }
                            Some(ServerAction::Drop) | None => break,
                        }
                    }
                }
                let _ = seen_tx.send((n, Seen::Gone));
            });
        }
    });
    WsServer { url, seen, conns }
}

/// Waits for the next frame (skipping nothing) and returns it.
pub async fn next_frame(server: &mut WsServer) -> (usize, ClientFrame) {
    loop {
        let (n, seen) = tokio::time::timeout(std::time::Duration::from_secs(5), server.seen.recv())
            .await
            .expect("timed out waiting for the client")
            .expect("server closed");
        if let Seen::Frame(f) = seen {
            // Pings are background noise for most tests.
            if matches!(f.body, ClientBody::Ping) {
                continue;
            }
            return (n, f);
        }
    }
}

pub async fn next_seen(server: &mut WsServer) -> (usize, Seen) {
    tokio::time::timeout(std::time::Duration::from_secs(5), server.seen.recv())
        .await
        .expect("timed out waiting for the client")
        .expect("server closed")
}

pub fn queue<T>(items: Vec<T>) -> Mutex<VecDeque<T>> {
    Mutex::new(items.into())
}
