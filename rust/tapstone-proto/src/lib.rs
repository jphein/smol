#![no_std]
#![forbid(unsafe_code)]
//! Tapstone's SMOLv1 `MATCH ` frames (protocol draft §2–§6 and arena spec §6). One codec, two
//! hosts: vendored into smol beside `tapstone-rules`, and linked natively by the arena.

pub mod follower;
pub mod frame;
pub mod ids;
pub mod shrine;
pub mod transcript;
mod wire;
