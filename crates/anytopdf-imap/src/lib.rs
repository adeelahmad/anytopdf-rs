//! IMAP mailbox watcher: fetches new messages, spools them as raw `.eml` files and
//! hands each one to a [`MessageSink`] (normally `anytopdf convert`).
//!
//! The watcher does no MIME parsing; turning a message into pages is the job of
//! whichever importer claims RFC 822 input.
mod config;
mod state;
mod transport;
mod watcher;

pub use config::{ImapConfig, TlsMode, is_loopback_host, read_password};
pub use state::{STATE_SCHEMA, WatchState};
pub use transport::{ImapMailbox, connect};
pub use watcher::{
    Delivery, Mailbox, MailboxStatus, MessageSink, PassReport, SpooledMessage, WatchOptions,
    Watcher, message_stem,
};
