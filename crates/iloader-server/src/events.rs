//! Backend → browser events over a WebSocket, broadcast to every connected
//! (authenticated) client.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};

use axum::{
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
};
use futures::{SinkExt, StreamExt};
use iloader_core::{EventSink, PromptHub};
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::AppState;

/// Prompt events and the response event that answers each of them.
pub const PROMPTS: &[(&str, &str)] = &[
    ("2fa-required", "2fa-recieved"),
    ("max-certs-reached", "max-certs-response"),
];

pub struct BroadcastSink {
    tx: broadcast::Sender<Arc<str>>,
    /// Last payload of each open prompt, replayed to clients that connect
    /// (or reload the page) while the core is still waiting for an answer.
    open_prompts: Mutex<HashMap<&'static str, Value>>,
}

impl BroadcastSink {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Self {
            tx,
            open_prompts: Mutex::new(HashMap::new()),
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }

    fn encode(event: &str, payload: &Value) -> Arc<str> {
        json!({ "event": event, "payload": payload }).to_string().into()
    }

    /// Prompts still awaiting an answer, encoded as messages.
    pub fn pending_prompts(&self, prompts: &PromptHub) -> Vec<Arc<str>> {
        let waiting = prompts.waiting_for();
        let mut open = self.open_prompts.lock().unwrap();
        open.retain(|ev, _| {
            PROMPTS
                .iter()
                .any(|(q, a)| q == ev && waiting.iter().any(|w| w == a))
        });
        open.iter().map(|(ev, p)| Self::encode(ev, p)).collect()
    }
}

impl Default for BroadcastSink {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for BroadcastSink {
    fn emit(&self, event: &str, payload: Value) {
        if let Some((q, _)) = PROMPTS.iter().find(|(q, _)| *q == event) {
            self.open_prompts.lock().unwrap().insert(q, payload.clone());
        }
        // an error only means nobody is connected right now
        let _ = self.tx.send(Self::encode(event, &payload));
    }
}

pub async fn ws_handler(State(state): State<Arc<AppState>>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| client_loop(state, socket))
}

async fn client_loop(state: Arc<AppState>, socket: WebSocket) {
    let mut rx = state.sink.subscribe();
    let (mut sender, mut receiver) = socket.split();

    for msg in state.sink.pending_prompts(&state.core.prompts) {
        if sender.send(Message::Text(msg.as_ref().into())).await.is_err() {
            return;
        }
    }

    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(msg) => {
                    if sender.send(Message::Text(msg.as_ref().into())).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    tracing::debug!(target: "events", "WebSocket client lagged, dropped {n} events");
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = receiver.next() => match incoming {
                // the browser only sends pings/close; answers go through POST /api/emit
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
}
