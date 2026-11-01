//! OpacusR viewer client: establish sessions, decode frames, send input,
//! exchange chat/files/terminal — as a library used by the GUI and tests.

pub mod addressbook;
pub mod session;

#[cfg(feature = "audio")]
pub mod audio;

pub use addressbook::{AddressBook, Contact, HistoryEntry, Outcome};
pub use session::{ClientEvent, Command, SessionClient, TransportChoice};
