//! Synchronous CamillaDSP websocket client.
//!
//! Protocol shape (extracted from pycamilladsp source):
//!   - URL is `ws://host:port`, no path.
//!   - Command without args: send a JSON-encoded string, e.g. `"GetState"`.
//!   - Command with args: send a JSON object `{"SetConfigFilePath": "/path"}`.
//!   - Response: `{"<CmdName>": {"result": "Ok", "value": <data>}}` on success;
//!     anything else (or non-Ok result) is treated as an error here.
//!
//! Used only to atomically swap CamillaDSP between `bridge.yml` (PC source connected)
//! and `music.yml` (idle / Tidal). We tolerate WS failures: if the swap fails,
//! audio still flows — just at whatever format CamillaDSP was already running.

use std::net::TcpStream;
use std::time::Duration;

use serde_json::{Value, json};
use thiserror::Error;
use tracing::{debug, warn};
use tungstenite::{Message, WebSocket, stream::MaybeTlsStream};

const READ_TIMEOUT: Duration = Duration::from_secs(3);
const WRITE_TIMEOUT: Duration = Duration::from_secs(3);

pub struct CamillaWs {
    socket: WebSocket<MaybeTlsStream<TcpStream>>,
}

#[derive(Debug, Error)]
pub enum WsError {
    #[error("websocket transport: {0}")]
    Tungstenite(#[from] tungstenite::Error),
    #[error("response was not text")]
    NotText,
    #[error("invalid JSON response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("CamillaDSP rejected `{cmd}`: {detail}")]
    Rejected { cmd: String, detail: String },
}

impl CamillaWs {
    pub fn connect(host: &str, port: u16) -> Result<Self, WsError> {
        let url = format!("ws://{host}:{port}");
        let (socket, _resp) = tungstenite::connect(&url)?;

        // Apply read/write timeouts on the underlying stream so a frozen
        // CamillaDSP can't hang our shutdown path forever.
        if let MaybeTlsStream::Plain(s) = socket.get_ref() {
            let _ = s.set_read_timeout(Some(READ_TIMEOUT));
            let _ = s.set_write_timeout(Some(WRITE_TIMEOUT));
        }

        Ok(Self { socket })
    }

    fn query_raw(&mut self, payload: Value) -> Result<Value, WsError> {
        let cmd_name = match &payload {
            Value::String(s) => s.clone(),
            Value::Object(m) => m.keys().next().cloned().unwrap_or_default(),
            _ => String::new(),
        };

        let wire = serde_json::to_string(&payload)?;
        debug!(cmd = %cmd_name, payload = %wire, "ws send");
        self.socket.send(Message::Text(wire.into()))?;

        let msg = self.socket.read()?;
        let text = match msg {
            Message::Text(t) => t.to_string(),
            _ => return Err(WsError::NotText),
        };
        debug!(cmd = %cmd_name, response = %text, "ws recv");

        let parsed: Value = serde_json::from_str(&text)?;
        let body = parsed
            .get(&cmd_name)
            .ok_or_else(|| WsError::Rejected {
                cmd: cmd_name.clone(),
                detail: format!("response missing key `{cmd_name}`: {text}"),
            })?
            .clone();

        let result = body.get("result").and_then(Value::as_str).unwrap_or("");
        if result != "Ok" {
            return Err(WsError::Rejected {
                cmd: cmd_name,
                detail: text,
            });
        }

        Ok(body.get("value").cloned().unwrap_or(Value::Null))
    }

    pub fn set_config_file_path(&mut self, path: &str) -> Result<(), WsError> {
        self.query_raw(json!({ "SetConfigFilePath": path }))?;
        Ok(())
    }

    pub fn reload(&mut self) -> Result<(), WsError> {
        self.query_raw(json!("Reload"))?;
        Ok(())
    }

    pub fn switch_to(&mut self, path: &str) -> Result<(), WsError> {
        self.set_config_file_path(path)?;
        self.reload()?;
        Ok(())
    }

    pub fn close(mut self) {
        let _ = self.socket.close(None);
    }
}

/// Best-effort switch — never propagates errors to the caller. The bridge keeps
/// streaming PCM regardless of whether CamillaDSP accepted the new config.
pub fn try_switch(host: &str, port: u16, path: &str, label: &str) {
    match CamillaWs::connect(host, port) {
        Ok(mut ws) => match ws.switch_to(path) {
            Ok(()) => {
                tracing::info!(target: "camilla", path, label, "config swapped");
                ws.close();
            }
            Err(e) => warn!(target: "camilla", error = %e, path, label, "config swap failed"),
        },
        Err(e) => warn!(target: "camilla", error = %e, "ws connect failed"),
    }
}
