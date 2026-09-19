//! Provider-neutral Via concepts shared by API, routing, and adapters.

use time::{OffsetDateTime, UtcOffset};

pub mod bikeshare;
pub mod ids;
pub mod journey;
pub mod location;
pub mod ranking;
pub mod realtime;
pub mod transit;
pub mod transit_timing;

pub fn taipei_offset() -> UtcOffset {
    UtcOffset::from_hms(8, 0, 0).expect("Taipei UTC offset is valid")
}

pub fn normalize_to_taipei(value: OffsetDateTime) -> OffsetDateTime {
    value.to_offset(taipei_offset())
}
