//! Block production for a local Zebra regtest chain.
//!
//! Zebra's release builds don't ship the internal miner, and regtest skips
//! proof of work, so this asks Zebra for a block template, serializes it into
//! a block with an empty Equihash solution, and submits it. Coinbase goes to
//! whatever `miner_address` Zebra is configured with; point that at a unified
//! address and the reward lands in Ironwood.

use serde_json::{json, Value};

pub const DEFAULT_RPC: &str = "http://127.0.0.1:18232";
/// Size of a full Equihash (200, 9) solution, which is what Zebra itself uses
/// for proposal blocks.
const SOLUTION_SIZE: usize = 1344;

/// Mines `count` blocks, returning the height of the last one.
pub fn mine(rpc: &str, count: u32) -> Result<u64, String> {
    let mut height = 0;
    for _ in 0..count {
        height = mine_one(rpc)?;
    }
    Ok(height)
}

pub fn height(rpc: &str) -> Result<u64, String> {
    call(rpc, "getblockcount", json!([]))?
        .as_u64()
        .ok_or_else(|| "getblockcount returned a non-number".into())
}

pub fn mine_one(rpc: &str) -> Result<u64, String> {
    let t = template_with_mempool(rpc)?;
    let block = block_from_template(&t)?;
    let res = call(rpc, "submitblock", json!([hex::encode(block)]))?;
    if !res.is_null() {
        return Err(format!("submitblock rejected the block: {res}"));
    }
    t["height"].as_u64().ok_or_else(|| "template has no height".into())
}

/// Zebra precomputes block templates in the background, so right after a
/// transaction is broadcast the template can still be one built without it.
/// Wait until the template covers everything in the mempool.
fn template_with_mempool(rpc: &str) -> Result<Value, String> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    loop {
        let mempool = call(rpc, "getrawmempool", json!([]))?;
        let t = call(rpc, "getblocktemplate", json!([]))?;
        let in_template: Vec<&str> = t["transactions"]
            .as_array()
            .map(|txs| txs.iter().filter_map(|tx| tx["hash"].as_str()).collect())
            .unwrap_or_default();
        let missing = mempool
            .as_array()
            .map(|m| m.iter().filter_map(Value::as_str).filter(|id| !in_template.contains(id)).count())
            .unwrap_or(0);
        if missing == 0 {
            return Ok(t);
        }
        if std::time::Instant::now() > deadline {
            return Err(format!("{missing} mempool transaction(s) never made it into a block template"));
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
}

fn block_from_template(t: &Value) -> Result<Vec<u8>, String> {
    let field = |path: &[&str]| -> Result<&Value, String> {
        path.iter()
            .try_fold(t, |v, k| v.get(k))
            .ok_or_else(|| format!("template is missing {}", path.join(".")))
    };
    let text = |path: &[&str]| -> Result<&str, String> {
        field(path)?.as_str().ok_or_else(|| format!("{} is not a string", path.join(".")))
    };

    let mut out = Vec::with_capacity(2048);
    let version = field(&["version"])?.as_i64().ok_or("version is not a number")? as i32;
    out.extend_from_slice(&version.to_le_bytes());
    // RPC hashes are in display order; the header stores them reversed.
    for path in [
        &["previousblockhash"][..],
        &["defaultroots", "merkleroot"][..],
        &["defaultroots", "blockcommitmentshash"][..],
    ] {
        let mut h = hex::decode(text(path)?).map_err(|e| e.to_string())?;
        if h.len() != 32 {
            return Err(format!("{} is not 32 bytes", path.join(".")));
        }
        h.reverse();
        out.extend_from_slice(&h);
    }
    let time = field(&["curtime"])?.as_u64().ok_or("curtime is not a number")? as u32;
    out.extend_from_slice(&time.to_le_bytes());
    let bits = u32::from_str_radix(text(&["bits"])?, 16).map_err(|e| e.to_string())?;
    out.extend_from_slice(&bits.to_le_bytes());
    out.extend_from_slice(&[0u8; 32]); // nonce
    compact_size(&mut out, SOLUTION_SIZE as u64);
    out.extend_from_slice(&[0u8; SOLUTION_SIZE]);

    let txs = field(&["transactions"])?.as_array().ok_or("transactions is not a list")?;
    compact_size(&mut out, 1 + txs.len() as u64);
    out.extend(hex::decode(text(&["coinbasetxn", "data"])?).map_err(|e| e.to_string())?);
    for tx in txs {
        let data = tx["data"].as_str().ok_or("transaction without data")?;
        out.extend(hex::decode(data).map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn compact_size(out: &mut Vec<u8>, n: u64) {
    match n {
        0..=0xfc => out.push(n as u8),
        0xfd..=0xffff => {
            out.push(0xfd);
            out.extend_from_slice(&(n as u16).to_le_bytes());
        }
        0x1_0000..=0xffff_ffff => {
            out.push(0xfe);
            out.extend_from_slice(&(n as u32).to_le_bytes());
        }
        _ => {
            out.push(0xff);
            out.extend_from_slice(&n.to_le_bytes());
        }
    }
}

pub fn call(rpc: &str, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let mut res = ureq::post(rpc)
        .header("content-type", "application/json")
        .send(body.to_string())
        .map_err(|e| format!("{method}: {e}"))?;
    let text = res.body_mut().read_to_string().map_err(|e| e.to_string())?;
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("{method}: bad JSON: {e}"))?;
    if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
        return Err(format!("{method}: {err}"));
    }
    Ok(v["result"].clone())
}

#[cfg(test)]
mod tests {
    use super::compact_size;

    #[test]
    fn compact_sizes() {
        let enc = |n| {
            let mut v = vec![];
            compact_size(&mut v, n);
            v
        };
        assert_eq!(enc(1), [1]);
        assert_eq!(enc(1344), [0xfd, 0x40, 0x05]);
        assert_eq!(enc(70_000), [0xfe, 0x70, 0x11, 0x01, 0x00]);
    }
}
