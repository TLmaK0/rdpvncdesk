//! rdpvncdesk patch (issue #4): text clipboard sharing for macOS and Linux.
//!
//! Upstream only has a native backend on Windows and uses a stub elsewhere. This backend
//! shares plain text (CF_UNICODETEXT) through the OS clipboard via `arboard`:
//! - local -> remote: the local clipboard is polled; when its text changes we advertise it,
//!   and send it when the server asks for it.
//! - remote -> local: when the server advertises text we fetch it right away and put it on
//!   the local clipboard.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ironrdp_cliprdr::backend::{ClipboardMessage, ClipboardMessageProxy, CliprdrBackend, CliprdrBackendFactory};
use ironrdp_cliprdr::pdu::{
    ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest, FileContentsResponse,
    FormatDataRequest, FormatDataResponse, LockDataId,
};
use tracing::{debug, warn};

use crate::clipboard::ClientClipboardMessageProxy;

const POLL_INTERVAL: Duration = Duration::from_millis(400);

pub(crate) fn factory(proxy: ClientClipboardMessageProxy) -> Box<dyn CliprdrBackendFactory + Send> {
    Box::new(Factory { proxy })
}

struct Factory {
    proxy: ClientClipboardMessageProxy,
}

impl CliprdrBackendFactory for Factory {
    fn build_cliprdr_backend(&self) -> Box<dyn CliprdrBackend> {
        Box::new(TextClipboard::new(self.proxy.clone()))
    }
}

struct Shared {
    /// Kept alive for the whole session: on X11 the data disappears with the owner.
    clipboard: Mutex<Option<arboard::Clipboard>>,
    /// Text both sides already have, so neither side's copy is echoed back.
    last_text: Mutex<Option<String>>,
    stop: AtomicBool,
}

impl Shared {
    fn get_text(&self) -> Option<String> {
        let mut guard = self.clipboard.lock().ok()?;
        let text = guard.as_mut()?.get_text().ok()?;
        (!text.is_empty()).then_some(text)
    }

    fn set_text(&self, text: &str) {
        if let Ok(mut guard) = self.clipboard.lock()
            && let Some(clipboard) = guard.as_mut()
            && let Err(e) = clipboard.set_text(text)
        {
            warn!(error = %e, "Failed to write the local clipboard");
        }
    }
}

struct TextClipboard {
    proxy: ClientClipboardMessageProxy,
    shared: Arc<Shared>,
    poller_started: bool,
}

impl core::fmt::Debug for TextClipboard {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TextClipboard").finish_non_exhaustive()
    }
}

ironrdp_core::impl_as_any!(TextClipboard);

impl TextClipboard {
    fn new(proxy: ClientClipboardMessageProxy) -> Self {
        let clipboard = match arboard::Clipboard::new() {
            Ok(c) => Some(c),
            Err(e) => {
                warn!(error = %e, "Local clipboard unavailable; clipboard sharing disabled");
                None
            }
        };
        Self {
            proxy,
            shared: Arc::new(Shared {
                clipboard: Mutex::new(clipboard),
                last_text: Mutex::new(None),
                stop: AtomicBool::new(false),
            }),
            poller_started: false,
        }
    }

    fn advertise_text(proxy: &ClientClipboardMessageProxy) {
        proxy.send_clipboard_message(ClipboardMessage::SendInitiateCopy(vec![ClipboardFormat::new(
            ClipboardFormatId::CF_UNICODETEXT,
        )]));
    }

    fn start_poller(&mut self) {
        if self.poller_started {
            return;
        }
        self.poller_started = true;
        let shared = Arc::clone(&self.shared);
        let proxy = self.proxy.clone();
        std::thread::Builder::new()
            .name("clipboard-poll".into())
            .spawn(move || {
                while !shared.stop.load(Ordering::Relaxed) {
                    std::thread::sleep(POLL_INTERVAL);
                    let Some(text) = shared.get_text() else { continue };
                    let Ok(mut last) = shared.last_text.lock() else { break };
                    if last.as_deref() != Some(text.as_str()) {
                        debug!(len = text.len(), "Local clipboard changed");
                        *last = Some(text);
                        drop(last);
                        Self::advertise_text(&proxy);
                    }
                }
            })
            .ok();
    }
}

impl Drop for TextClipboard {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

impl CliprdrBackend for TextClipboard {
    fn temporary_directory(&self) -> &str {
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::empty()
    }

    fn on_ready(&mut self) {
        self.start_poller();
    }

    fn on_request_format_list(&mut self) {
        // Initial format list: offer what is on the local clipboard right now.
        match self.shared.get_text() {
            Some(text) => {
                if let Ok(mut last) = self.shared.last_text.lock() {
                    *last = Some(text);
                }
                Self::advertise_text(&self.proxy);
            }
            None => self
                .proxy
                .send_clipboard_message(ClipboardMessage::SendInitiateCopy(Vec::new())),
        }
        self.start_poller();
    }

    fn on_process_negotiated_capabilities(&mut self, capabilities: ClipboardGeneralCapabilityFlags) {
        debug!(?capabilities);
    }

    fn on_remote_copy(&mut self, available_formats: &[ClipboardFormat]) {
        if available_formats
            .iter()
            .any(|f| f.id() == ClipboardFormatId::CF_UNICODETEXT)
        {
            self.proxy
                .send_clipboard_message(ClipboardMessage::SendInitiatePaste(ClipboardFormatId::CF_UNICODETEXT));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let response = match (request.format == ClipboardFormatId::CF_UNICODETEXT)
            .then(|| self.shared.get_text())
            .flatten()
        {
            Some(text) => FormatDataResponse::new_unicode_string(&text),
            None => FormatDataResponse::new_error(),
        };
        self.proxy
            .send_clipboard_message(ClipboardMessage::SendFormatData(response));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if response.is_error() {
            return;
        }
        match response.to_unicode_string() {
            Ok(text) => {
                // Remember it first, so the poller doesn't advertise it back to the server.
                if let Ok(mut last) = self.shared.last_text.lock() {
                    *last = Some(text.clone());
                }
                self.shared.set_text(&text);
            }
            Err(e) => warn!(error = %e, "Invalid text from the remote clipboard"),
        }
    }

    fn on_file_contents_request(&mut self, request: FileContentsRequest) {
        debug!(?request, "File clipboard not supported");
    }

    fn on_file_contents_response(&mut self, response: FileContentsResponse<'_>) {
        debug!(?response, "File clipboard not supported");
    }

    fn on_lock(&mut self, data_id: LockDataId) {
        debug!(?data_id);
    }

    fn on_unlock(&mut self, data_id: LockDataId) {
        debug!(?data_id);
    }
}
