//! Which Zcash network a home directory belongs to.

use std::fmt;
use std::str::FromStr;

use zcash_protocol::consensus::{BlockHeight, Network, NetworkType, NetworkUpgrade, Parameters};
use zcash_protocol::local_consensus::LocalNetwork;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NetName {
    Regtest,
    Test,
    Main,
}

impl FromStr for NetName {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "regtest" => Ok(NetName::Regtest),
            "test" | "testnet" => Ok(NetName::Test),
            "main" | "mainnet" => Ok(NetName::Main),
            _ => Err(format!("unknown network {s}; use regtest, test or main")),
        }
    }
}

impl fmt::Display for NetName {
    /// The spelling auction terms use.
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(match self {
            NetName::Regtest => "regtest",
            NetName::Test => "test",
            NetName::Main => "main",
        })
    }
}

impl NetName {
    pub fn params(self) -> Net {
        match self {
            NetName::Regtest => Net::Regtest(tacit_wallet::regtest()),
            NetName::Test => Net::Public(Network::TestNetwork),
            NetName::Main => Net::Public(Network::MainNetwork),
        }
    }

    /// A light wallet server to use when none is configured. Only regtest has
    /// one we can assume (Zebra serving gRPC itself, see regtest/zebrad.toml).
    pub fn default_server(self) -> Option<&'static str> {
        match self {
            NetName::Regtest => Some("http://127.0.0.1:9067"),
            _ => None,
        }
    }
}

/// Consensus parameters for any of the three networks, as one type, so the
/// wallet code doesn't have to be generic all the way up.
#[derive(Clone, Debug)]
pub enum Net {
    Regtest(LocalNetwork),
    Public(Network),
}

impl Parameters for Net {
    fn network_type(&self) -> NetworkType {
        match self {
            Net::Regtest(n) => n.network_type(),
            Net::Public(n) => n.network_type(),
        }
    }

    fn activation_height(&self, nu: NetworkUpgrade) -> Option<BlockHeight> {
        match self {
            Net::Regtest(n) => n.activation_height(nu),
            Net::Public(n) => n.activation_height(nu),
        }
    }
}
