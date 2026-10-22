//! The INFO round-trip: reading the database's DDL back into SDL,
//! writing canonical SDL, and normalizing one against the other. The
//! writer is [`write`]; the readers are [`read`]; the merge core is
//! [`normalize`]; canonical placement is [`place`], the merge's other
//! half; the differ ([`diff`]) has two renderers, the drift report
//! ([`report`]) and the REMOVE convergence script ([`script`]).

pub mod diff;
pub mod normalize;
mod place;
pub mod read;
pub mod report;
pub mod script;
pub mod staging;
pub mod write;
