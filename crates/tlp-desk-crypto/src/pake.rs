//! SPAKE2 password-authenticated key exchange with channel binding and key confirmation.
//!
//! Used for the one-time code and the unattended password. The secret never crosses the wire.
//!
//! * **Group**: `spake2::Ed25519Group`. Viewer = side A, host = side B.
//! * **Identities** (bound into the SPAKE2 transcript):
//!   `"tlp-desk/v2/viewer/" || hex(viewer_fp)` and `"tlp-desk/v2/host/" || hex(host_fp)`,
//!   where the fingerprints are the SHA-256 SPKI hashes of the TLS certificates each side
//!   actually saw in the mutually-authenticated QUIC handshake.
//! * **Channel binding**: the transcript hash also covers caller-supplied `channel_binding`
//!   bytes; the net layer passes 32 bytes of TLS exporter keying material for the QUIC
//!   connection, so the PAKE result cannot be relayed into a different TLS session.
//! * **Key confirmation**: with `K` the SPAKE2 key and
//!   `T = SHA256(len-prefixed: DOMAIN, method, viewer_fp, host_fp, channel_binding, msg_a, msg_b)`,
//!   viewer sends `HMAC-SHA256(K, "tlp-desk/kc/viewer" || T)` and host sends
//!   `HMAC-SHA256(K, "tlp-desk/kc/host" || T)`. MACs are compared in constant time.
//!   A wrong code yields a different `K` on each side, so the MACs fail on both sides cleanly.

use hmac::{Hmac, Mac};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeIdentity, Password, Spake2};

use crate::device_id::sha256;
use crate::Fingerprint;

const DOMAIN: &[u8] = b"tlp-desk/pake/v2";

/// Which secret the exchange is keyed by (bound into the transcript so a code exchange can
/// never be confused with a password exchange).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PakeMethod {
    OneTimeCode,
    Password,
}

impl PakeMethod {
    fn tag(self) -> &'static [u8] {
        match self {
            PakeMethod::OneTimeCode => b"code",
            PakeMethod::Password => b"password",
        }
    }
}

/// Which side of the exchange this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PakeRole {
    /// The connecting side (SPAKE2 side A).
    Viewer,
    /// The side being controlled (SPAKE2 side B).
    Host,
}

/// Inputs shared by both sides; both must pass identical values or confirmation fails.
#[derive(Debug, Clone)]
pub struct PakeContext {
    pub method: PakeMethod,
    pub viewer_fp: Fingerprint,
    pub host_fp: Fingerprint,
    /// E.g. TLS exporter keying material of the connection.
    pub channel_binding: Vec<u8>,
}

/// Errors from the PAKE exchange.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PakeError {
    #[error("malformed SPAKE2 message")]
    BadMessage,
    #[error("key confirmation failed (wrong code/password or tampering)")]
    ConfirmationFailed,
}

/// One side of an in-progress exchange.
pub struct PakeSession {
    role: PakeRole,
    ctx: PakeContext,
    state: Spake2<Ed25519Group>,
    my_msg: Vec<u8>,
}

impl std::fmt::Debug for PakeSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PakeSession")
            .field("role", &self.role)
            .field("method", &self.ctx.method)
            .finish_non_exhaustive()
    }
}

fn spake_id(label: &str, fp: &Fingerprint) -> SpakeIdentity {
    SpakeIdentity::new(format!("tlp-desk/v2/{label}/{}", fp.to_hex()).as_bytes())
}

impl PakeSession {
    /// Starts the exchange; returns the session and the message to send to the peer.
    pub fn start(role: PakeRole, secret: &[u8], ctx: PakeContext) -> (Self, Vec<u8>) {
        let pw = Password::new(secret);
        let a = spake_id("viewer", &ctx.viewer_fp);
        let b = spake_id("host", &ctx.host_fp);
        let (state, msg) = match role {
            PakeRole::Viewer => Spake2::<Ed25519Group>::start_a(&pw, &a, &b),
            PakeRole::Host => Spake2::<Ed25519Group>::start_b(&pw, &a, &b),
        };
        (
            Self {
                role,
                ctx,
                state,
                my_msg: msg.clone(),
            },
            msg,
        )
    }

    /// Finishes with the peer's SPAKE2 message and derives the confirmation keys.
    pub fn finish(self, peer_msg: &[u8]) -> Result<PakeKeys, PakeError> {
        let key = self
            .state
            .finish(peer_msg)
            .map_err(|_| PakeError::BadMessage)?;
        let (msg_a, msg_b) = match self.role {
            PakeRole::Viewer => (self.my_msg.as_slice(), peer_msg),
            PakeRole::Host => (peer_msg, self.my_msg.as_slice()),
        };
        let transcript = sha256(&[
            DOMAIN,
            self.ctx.method.tag(),
            self.ctx.viewer_fp.as_bytes(),
            self.ctx.host_fp.as_bytes(),
            &self.ctx.channel_binding,
            msg_a,
            msg_b,
        ]);
        let mac = |label: &[u8]| -> [u8; 32] {
            #[allow(
                clippy::expect_used,
                reason = "HMAC accepts keys of any length: new_from_slice cannot fail"
            )]
            let mut m = <Hmac<Sha256> as Mac>::new_from_slice(&key).expect("HMAC takes any key");
            m.update(label);
            m.update(&transcript);
            m.finalize().into_bytes().into()
        };
        let session_key = mac(b"tlp-desk/session-key");
        Ok(PakeKeys {
            role: self.role,
            viewer_mac: mac(b"tlp-desk/kc/viewer"),
            host_mac: mac(b"tlp-desk/kc/host"),
            session_key,
        })
    }
}

/// Result of a finished SPAKE2 exchange, before confirmation.
pub struct PakeKeys {
    role: PakeRole,
    viewer_mac: [u8; 32],
    host_mac: [u8; 32],
    session_key: [u8; 32],
}

impl std::fmt::Debug for PakeKeys {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PakeKeys")
            .field("role", &self.role)
            .finish_non_exhaustive()
    }
}

impl PakeKeys {
    /// Confirmation MAC to send to the peer.
    pub fn my_confirmation(&self) -> Vec<u8> {
        match self.role {
            PakeRole::Viewer => self.viewer_mac.to_vec(),
            PakeRole::Host => self.host_mac.to_vec(),
        }
    }

    /// Verifies the peer's confirmation MAC in constant time.
    pub fn verify_peer(&self, mac: &[u8]) -> Result<(), PakeError> {
        let expected = match self.role {
            PakeRole::Viewer => &self.host_mac,
            PakeRole::Host => &self.viewer_mac,
        };
        if constant_time_eq(expected, mac) {
            Ok(())
        } else {
            Err(PakeError::ConfirmationFailed)
        }
    }

    /// A 32-byte key both sides share after successful confirmation (not used by M1 transport,
    /// which is already encrypted by TLS; exposed for future use).
    pub fn session_key(&self) -> &[u8; 32] {
        &self.session_key
    }
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(cb: &[u8]) -> PakeContext {
        PakeContext {
            method: PakeMethod::OneTimeCode,
            viewer_fp: Fingerprint([1; 32]),
            host_fp: Fingerprint([2; 32]),
            channel_binding: cb.to_vec(),
        }
    }

    fn run(
        viewer_secret: &[u8],
        host_secret: &[u8],
        vctx: PakeContext,
        hctx: PakeContext,
    ) -> (Result<(), PakeError>, Result<(), PakeError>) {
        let (v, vmsg) = PakeSession::start(PakeRole::Viewer, viewer_secret, vctx);
        let (h, hmsg) = PakeSession::start(PakeRole::Host, host_secret, hctx);
        let vk = v.finish(&hmsg).unwrap();
        let hk = h.finish(&vmsg).unwrap();
        (
            hk.verify_peer(&vk.my_confirmation()),
            vk.verify_peer(&hk.my_confirmation()),
        )
    }

    #[test]
    fn pake_same_code_confirms_both_ways() {
        let (h, v) = run(b"123456", b"123456", ctx(b"cb"), ctx(b"cb"));
        assert_eq!(h, Ok(()));
        assert_eq!(v, Ok(()));
    }

    #[test]
    fn pake_wrong_code_fails_both_sides() {
        let (h, v) = run(b"123456", b"654321", ctx(b"cb"), ctx(b"cb"));
        assert_eq!(h, Err(PakeError::ConfirmationFailed));
        assert_eq!(v, Err(PakeError::ConfirmationFailed));
    }

    #[test]
    fn pake_channel_binding_mismatch_fails() {
        let (h, v) = run(b"123456", b"123456", ctx(b"tls-a"), ctx(b"tls-b"));
        assert!(h.is_err() && v.is_err());
        let mut other = ctx(b"cb");
        other.viewer_fp = Fingerprint([9; 32]);
        let (h, v) = run(b"123456", b"123456", ctx(b"cb"), other);
        assert!(h.is_err() && v.is_err());
    }

    #[test]
    fn pake_garbage_message_is_error() {
        let (v, _) = PakeSession::start(PakeRole::Viewer, b"1", ctx(b""));
        assert_eq!(v.finish(&[1, 2, 3]).unwrap_err(), PakeError::BadMessage);
    }
}
