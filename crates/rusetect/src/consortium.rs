//! Cross bank mule intelligence without a blockchain network (port of service/consortium.py).
//!
//! An append only ledger where every entry is signed (Ed25519) by the publishing bank and chained to the previous
//! entry's hash. Identifiers are never shared in clear: each is an HMAC with a consortium secret. Three registries:
//! mule accounts, device hashes linked to fraud, SIM swap takeovers. A Merkle root anchors the ledger anywhere.
use crate::pyfmt::{take_chars, utc_now_z};
use crate::security::canon;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier};
use hmac::{Hmac, Mac};
use indexmap::IndexMap;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;

/// Ledger entries kept: verify() and check() walk the whole ledger on every read, so it must stay bounded.
pub const MAX_LEDGER: usize = 10_000;

pub const REGISTRIES: [&str; 3] = ["fraud_device", "mule_account", "sim_swap_takeover"]; // sorted

pub struct Bank {
    pub bank_id: String,
    key: SigningKey,
    pub public_b64: String,
}

impl Bank {
    pub fn new(bank_id: &str) -> Self {
        let key = SigningKey::generate(&mut rand::rngs::OsRng);
        Bank { bank_id: bank_id.into(), public_b64: B64.encode(key.verifying_key().to_bytes()), key }
    }
}

pub struct Consortium {
    secret: Vec<u8>,
    pub banks: IndexMap<String, Bank>,
    pub ledger: Vec<Value>,
}

const BODY_KEYS: [&str; 8] = ["seq", "at", "bank", "registry", "token", "reason", "evidence_sha256", "prev"];

impl Consortium {
    pub fn new(secret: Option<&str>, members: &[&str]) -> Self {
        let s = secret.map(String::from).or_else(|| std::env::var("RUSETECT_CONSORTIUM_SECRET").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "demo-consortium-secret".into());
        Consortium { secret: s.into_bytes(), banks: members.iter().map(|b| (b.to_string(), Bank::new(b))).collect(), ledger: vec![] }
    }

    pub fn demo() -> Self {
        Self::new(None, &["BANK-A", "BANK-B", "BANK-C"])
    }

    pub fn reset(&mut self) {
        self.ledger.clear();
    }

    pub fn token(&self, kind: &str, value: &str) -> String {
        let mut m = Hmac::<Sha256>::new_from_slice(&self.secret).unwrap();
        m.update(format!("{kind}|{value}").as_bytes());
        hex::encode(m.finalize().into_bytes())[..32].to_string()
    }

    pub fn publish(&mut self, bank_id: &str, registry: &str, identifier: &str, reason: &str, evidence_sha256: Option<&str>) -> Result<Value, String> {
        if !REGISTRIES.contains(&registry) {
            return Err(format!("registry must be one of ['{}']", REGISTRIES.join("', '")));
        }
        let bank = self.banks.get(bank_id).ok_or_else(|| format!("unknown member bank {bank_id}"))?;
        if self.ledger.len() >= MAX_LEDGER {
            return Err(format!("consortium ledger is full ({MAX_LEDGER} entries)"));
        }
        let prev = self.ledger.last().and_then(|e| e["hash"].as_str()).map(String::from).unwrap_or_else(|| "0".repeat(64));
        let body = json!({"seq": self.ledger.len() + 1, "at": utc_now_z(), "bank": bank_id, "registry": registry,
                          "token": self.token(registry, identifier), "reason": take_chars(reason, 120), "evidence_sha256": evidence_sha256,
                          "prev": prev});
        let sig = B64.encode(bank.key.sign(&canon(&body)).to_bytes());
        let mut e = body.clone();
        e["sig"] = json!(sig);
        let h = hex::encode(Sha256::digest(canon(&e)));
        e["hash"] = json!(h);
        self.ledger.push(e.clone());
        Ok(e)
    }

    pub fn verify(&self) -> bool {
        let mut prev = "0".repeat(64);
        for e in &self.ledger {
            let mut body = serde_json::Map::new();
            for k in BODY_KEYS {
                body.insert(k.into(), e.get(k).cloned().unwrap_or(Value::Null));
            }
            let mut with_sig = body.clone();
            with_sig.insert("sig".into(), e["sig"].clone());
            if e["prev"].as_str() != Some(prev.as_str()) || hex::encode(Sha256::digest(canon(&Value::Object(with_sig)))) != e["hash"].as_str().unwrap_or("") {
                return false;
            }
            let Some(bank) = e["bank"].as_str().and_then(|b| self.banks.get(b)) else { return false };
            let ok = B64.decode(e["sig"].as_str().unwrap_or("")).ok().and_then(|s| Signature::from_slice(&s).ok())
                .map(|s| bank.key.verifying_key().verify(&canon(&Value::Object(body)), &s).is_ok()).unwrap_or(false);
            if !ok {
                return false;
            }
            prev = e["hash"].as_str().unwrap_or("").to_string();
        }
        true
    }

    pub fn check(&self, registry: &str, identifier: &str) -> Value {
        let tok = self.token(registry, identifier);
        let hits: Vec<&Value> = self.ledger.iter().filter(|e| e["registry"] == registry && e["token"] == tok.as_str()).collect();
        let banks: BTreeSet<String> = hits.iter().filter_map(|e| e["bank"].as_str().map(String::from)).collect();
        json!({"flagged": !hits.is_empty(), "banks": banks, "entries": hits.len()})
    }

    pub fn merkle_root(&self) -> String {
        let mut level: Vec<Vec<u8>> = self.ledger.iter().map(|e| hex::decode(e["hash"].as_str().unwrap_or("")).unwrap_or_default()).collect();
        if level.is_empty() {
            level.push(vec![0u8; 32]);
        }
        while level.len() > 1 {
            if level.len() % 2 == 1 {
                level.push(level.last().unwrap().clone());
            }
            level = level.chunks(2).map(|p| Sha256::digest([p[0].as_slice(), p[1].as_slice()].concat()).to_vec()).collect();
        }
        hex::encode(&level[0])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ledger_is_signed_chained_and_hashed() {
        let mut con = Consortium::new(Some("t"), &["BANK-A", "BANK-B", "BANK-C"]);
        con.publish("BANK-B", "mule_account", "011:MULE9", "confirmed", None).unwrap();
        assert_eq!(con.check("mule_account", "011:MULE9")["flagged"], true);
        assert_eq!(con.check("mule_account", "011:OTHER")["flagged"], false);
        assert!(!serde_json::to_string(&con.ledger).unwrap().contains("MULE9") && con.verify());
        con.ledger[0]["reason"] = json!("edited");
        assert!(!con.verify());
    }

    #[test]
    fn ledger_is_capped() {
        let mut con = Consortium::new(Some("t"), &["BANK-A"]);
        for i in 1..MAX_LEDGER {
            con.publish("BANK-A", "mule_account", &i.to_string(), "r", None).unwrap();
        }
        assert!(con.publish("BANK-A", "mule_account", "last", "r", None).is_ok());
        assert!(con.publish("BANK-A", "mule_account", "one too many", "r", None).is_err());
        assert_eq!(con.ledger.len(), MAX_LEDGER);
    }
}
