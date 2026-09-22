#![no_std]
#![forbid(unsafe_code)]
//! Tapstone rules. One crate, two hosts: the shrine firmware and the arena service.
pub mod cards;
pub mod event;
pub mod hash;
pub mod rules;
pub mod state;

// The modules are empty stubs until their tasks land; each task uncomments its re-export.
pub use cards::{CardDesign, CardKind, Effect, Faction, Keyword, SET1};
pub use event::{Kind, Record};
pub use hash::Chain;
pub use rules::{Applied, Refusal};
pub use state::{Game, HouseRules, Phase, Winner};
