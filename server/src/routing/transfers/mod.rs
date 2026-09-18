//! Movement between distinct transit boarding points.

mod index;
mod resolver;
mod static_walking;

pub use index::{TransferIndex, TransferShortcut, TransferShortcutIndex};
pub use resolver::{ResolvedTransfer, TransferNeed, TransferResolver, TransferTarget};
