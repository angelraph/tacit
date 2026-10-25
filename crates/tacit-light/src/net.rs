//! Which chain we're on.

use serde::{Deserialize, Serialize};
use zcash_protocol::consensus::{BlockHeight, NetworkType, NetworkUpgrade, Parameters, MAIN_NETWORK, TEST_NETWORK};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Net {
    Main,
    Test,
    /// A local Zebra regtest chain with every upgrade, NU6.3 included,
    /// active from height 1.
    Regtest,
}

impl Net {
    /// Parses the `network` field of auction terms.
    pub fn parse(s: &str) -> Option<Net> {
        match s {
            "main" | "mainnet" => Some(Net::Main),
            "test" | "testnet" => Some(Net::Test),
            "regtest" => Some(Net::Regtest),
            _ => None,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Net::Main => "main",
            Net::Test => "test",
            Net::Regtest => "regtest",
        }
    }

    /// The ZIP 32 coin type for this network.
    pub fn coin_type(self) -> u32 {
        match self {
            Net::Main => 133,
            _ => 1,
        }
    }
}

impl Parameters for Net {
    fn network_type(&self) -> NetworkType {
        match self {
            Net::Main => NetworkType::Main,
            Net::Test => NetworkType::Test,
            Net::Regtest => NetworkType::Regtest,
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match self {
            Net::Main => MAIN_NETWORK.activation_height(nu),
            Net::Test => TEST_NETWORK.activation_height(nu),
            // The local Zebra regtest chain stops at NU6.3.
            #[cfg(zcash_unstable = "nu7")]
            Net::Regtest if nu == NetworkUpgrade::Nu7 => None,
            Net::Regtest => Some(BlockHeight::from_u32(1)),
        }
    }
}
