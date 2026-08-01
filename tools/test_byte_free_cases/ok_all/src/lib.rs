#[cfg(feature = "alpha")]
pub mod filegated;
#[cfg(feature = "beta")]
pub mod decl_only;

// item-level gate backing a claim: the default build is byte-free of this.
#[cfg(feature = "gamma")]
pub static THING: u32 = 0;
