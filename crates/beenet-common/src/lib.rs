//! Shared foundation for Beenet runtime components.
//!
//! This crate owns content identifiers, configuration, display names, and the
//! versioned Gateway ↔ Worker wire types exposed through [`proto`].

pub mod config;
pub mod display_name;
pub mod proto;

use std::fmt;
use std::str::FromStr;

use cid::Cid;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// IPLD raw codec (0x55). Wasm bytes are addressed as raw blocks.
pub const CODEC_RAW: u64 = 0x55;

/// libp2p protocol name for Gateway ↔ Worker invoke.
pub const INVOKE_PROTOCOL: &str = "/beenet/invoke/1.0";

/// Content ID wrapper — a thin, serializable newtype over [`cid::Cid`].
///
/// M1 computes CIDs as CIDv1 / raw / sha256 over the *whole* packaged wasm
/// (including the embedded manifest custom section), matching `readme.md §3.5.6`:
/// "CID 天然绑定 manifest".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct BeenetCid(Cid);

impl BeenetCid {
    /// Compute the CID of the given bytes (CIDv1 / raw / sha2-256).
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        let hash = multihash::Multihash::<64>::wrap(0x12, &digest).expect("valid sha2-256 digest");
        Self(Cid::new_v1(CODEC_RAW, hash))
    }

    pub fn as_cid(&self) -> &Cid {
        &self.0
    }
}

impl fmt::Display for BeenetCid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for BeenetCid {
    type Err = BeenetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Cid::try_from(s)
            .map(BeenetCid)
            .map_err(|e| BeenetError::InvalidCid(e.to_string()))
    }
}

impl Serialize for BeenetCid {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for BeenetCid {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BeenetError {
    #[error("invalid CID: {0}")]
    InvalidCid(String),

    #[error("wasm module missing required custom section: {0}")]
    MissingSection(&'static str),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    #[error("{0}")]
    Other(#[from] anyhow::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cid_roundtrip() {
        let c = BeenetCid::from_bytes(b"hello beenet");
        let s = c.to_string();
        let c2: BeenetCid = s.parse().unwrap();
        assert_eq!(c, c2);
        assert!(
            s.starts_with("bafkrei"),
            "cidv1 raw sha256 -> bafkrei*: got {s}"
        );
    }
}
