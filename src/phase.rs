//! Tracks which step of the connection sequence IronRDP is in, so a failure can be explained.
//!
//! IronRDP doesn't say *when* a connection failed: a server that rejects the credentials simply
//! closes the socket, which surfaces as "not enough bytes". But IronRDP logs the connector state
//! before every read, so a tracing layer can remember the last one.

use std::sync::Mutex;

use tracing::field::{Field, Visit};
use tracing_subscriber::layer::{Context, Layer};

static LAST_STATE: Mutex<String> = Mutex::new(String::new());

/// Connector state name of the last step IronRDP started (e.g. "Credssp").
pub fn last_state() -> String {
    LAST_STATE.lock().map(|s| s.clone()).unwrap_or_default()
}

pub const CONNECTED: &str = "Connected";

/// True once the connection sequence finished.
pub fn is_connected() -> bool {
    last_state() == CONNECTED
}

/// True when the connection broke while the server was checking the credentials (NLA).
pub fn failed_during_authentication() -> bool {
    last_state() == "Credssp"
}

pub struct PhaseTracker;

impl<S: tracing::Subscriber> Layer<S> for PhaseTracker {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        struct StateField(Option<String>);
        impl Visit for StateField {
            fn record_str(&mut self, field: &Field, value: &str) {
                if field.name() == "connector.state" {
                    self.0 = Some(value.to_owned());
                }
            }
            fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
                if field.name() == "connector.state" {
                    self.0 = Some(format!("{value:?}").trim_matches('"').to_owned());
                } else if field.name() == "message"
                    && format!("{value:?}") == "Connected with success"
                {
                    self.0 = Some(CONNECTED.to_owned());
                }
            }
        }
        let mut visitor = StateField(None);
        event.record(&mut visitor);
        if let Some(state) = visitor.0
            && let Ok(mut last) = LAST_STATE.lock()
        {
            *last = state;
        }
    }
}
