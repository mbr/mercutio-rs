//! Optional I/O transports.
//!
//! These modules provide ready-made transport implementations for common use cases. Each transport
//! is behind its own feature flag.

#[cfg(feature = "io-stdlib")]
#[cfg_attr(docsrs, doc(cfg(feature = "io-stdlib")))]
pub mod stdlib;
