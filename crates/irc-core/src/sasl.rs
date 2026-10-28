// SPDX-License-Identifier: MPL-2.0
//! The SASL exchange after the `sasl` capability is acknowledged: PLAIN and
//! SCRAM (see [`crate::scram`]). Everything it records for the connection
//! diagnostics is a stage, a byte count or a fixed error; no payload, proof,
//! signature, nonce or password is ever put into a note.

use base64::Engine;
use irc::proto::{Command as IrcCommand, Message as IrcMessage, Response};
use zeroize::Zeroize;

use crate::scram::{self, Scram};
use crate::{SaslCredentials, SaslMechanism};

/// One `AUTHENTICATE` line carries at most this many base64 characters.
const CHUNK: usize = 400;
/// The most base64 characters accepted for one server message.
const MAX_INCOMING: usize = 8 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    /// `AUTHENTICATE <mechanism>` sent; waiting for the server's `+`.
    Starting,
    /// SCRAM client-first sent; waiting for server-first.
    ScramFirst,
    /// Server-first accepted; the key derivation runs off the worker.
    Deriving,
    /// Client-final sent; waiting for server-final.
    ScramFinal,
    /// Credentials sent (PLAIN) or the server signature verified (SCRAM);
    /// waiting for 903.
    WaitingForResult,
    Complete,
}

/// What one incoming message makes the handshake do.
#[derive(Debug, Default)]
pub(crate) struct Out {
    pub send: Vec<IrcCommand>,
    pub notes: Vec<String>,
    /// SCRAM's derivation, to run with `spawn_blocking`; its result goes to
    /// [`SaslHandshake::derived`].
    pub derive: Option<scram::Job>,
}

pub(crate) struct SaslHandshake {
    mechanism: SaslMechanism,
    username: String,
    /// Cleared once handed to the exchange that needs it.
    password: String,
    phase: Phase,
    scram: Option<Scram>,
    /// Base64 of the server message being received.
    incoming: String,
}

impl SaslHandshake {
    pub(crate) fn new(credentials: crate::SaslCredentials) -> Self {
        let SaslCredentials {
            mechanism,
            username,
            password,
        } = credentials;
        Self {
            mechanism,
            username,
            password,
            phase: Phase::Idle,
            scram: None,
            incoming: String::new(),
        }
    }

    pub(crate) fn mechanism(&self) -> SaslMechanism {
        self.mechanism
    }

    pub(crate) fn complete(&self) -> bool {
        self.phase == Phase::Complete
    }

    /// The stage the handshake is in, for the registration timeout.
    pub(crate) fn stage(&self) -> &'static str {
        match self.phase {
            Phase::Idle => "waiting for the sasl capability",
            Phase::Starting => "waiting for the server's AUTHENTICATE +",
            Phase::ScramFirst => "waiting for the SCRAM server-first message",
            Phase::Deriving => "deriving the SCRAM keys",
            Phase::ScramFinal => "waiting for the SCRAM server-final message",
            Phase::WaitingForResult => "waiting for the SASL result",
            Phase::Complete => "complete",
        }
    }

    pub(crate) fn begin(&mut self) -> Result<Out, String> {
        let mut out = Out::default();
        self.scram = match self.mechanism.scram_hash() {
            Some(hash) => Some(Scram::new(hash, &self.username, &self.password)?),
            None => None,
        };
        self.phase = Phase::Starting;
        out.notes.push(format!(
            "SASL: starting {} (TLS-protected exchange).",
            self.mechanism.name()
        ));
        out.send
            .push(IrcCommand::AUTHENTICATE(self.mechanism.name().into()));
        Ok(out)
    }

    pub(crate) fn observe(&mut self, message: &IrcMessage) -> Result<Out, String> {
        let mut out = Out::default();
        match &message.command {
            IrcCommand::AUTHENTICATE(chunk) => {
                if let Some(payload) = self.receive(chunk)? {
                    self.handle(&payload, &mut out)?;
                }
            }
            IrcCommand::Response(Response::RPL_SASLSUCCESS, _) => match self.phase {
                Phase::WaitingForResult => {
                    self.phase = Phase::Complete;
                    out.notes
                        .push("SASL: authentication succeeded (903).".into());
                }
                Phase::Idle | Phase::Complete => {}
                _ => {
                    return Err(format!(
                        "The server reported SASL success while {}.",
                        self.stage()
                    ));
                }
            },
            IrcCommand::Response(
                Response::ERR_NICKLOCKED
                | Response::ERR_SASLFAIL
                | Response::ERR_SASLTOOLONG
                | Response::ERR_SASLABORT
                | Response::ERR_SASLALREADY,
                _,
            ) => {
                // No fallback to another mechanism: the user chose this one.
                out.notes.push(format!(
                    "SASL: {} failed while {}.",
                    self.mechanism.name(),
                    self.stage()
                ));
                return Err(crate::SASL_FAILED.into());
            }
            _ => {}
        }
        Ok(out)
    }

    /// Reassembles a server message from its `AUTHENTICATE` lines: a line of
    /// 400 characters continues, a shorter one or `+` ends the message.
    fn receive(&mut self, chunk: &str) -> Result<Option<Vec<u8>>, String> {
        if matches!(self.phase, Phase::Idle | Phase::Complete) {
            return Ok(None);
        }
        if chunk == "+" {
            return self.finish_incoming().map(Some);
        }
        if chunk.len() > CHUNK || self.incoming.len() + chunk.len() > MAX_INCOMING {
            self.incoming.zeroize();
            return Err("The SASL challenge from the server is too long.".into());
        }
        self.incoming.push_str(chunk);
        if chunk.len() == CHUNK {
            return Ok(None);
        }
        self.finish_incoming().map(Some)
    }

    fn finish_incoming(&mut self) -> Result<Vec<u8>, String> {
        let decoded = base64::engine::general_purpose::STANDARD.decode(self.incoming.as_bytes());
        self.incoming.zeroize();
        decoded.map_err(|_| "The SASL challenge from the server is not base64.".into())
    }

    fn handle(&mut self, payload: &[u8], out: &mut Out) -> Result<(), String> {
        match self.phase {
            Phase::Starting => {
                if !payload.is_empty() {
                    return Err("The server's first SASL challenge was not empty.".into());
                }
                let response = match self.scram.as_ref() {
                    Some(scram) => {
                        self.phase = Phase::ScramFirst;
                        scram.first().into_bytes()
                    }
                    None => {
                        self.phase = Phase::WaitingForResult;
                        let mut plain = format!("\0{}\0{}", self.username, self.password);
                        let bytes = plain.as_bytes().to_vec();
                        plain.zeroize();
                        bytes
                    }
                };
                self.password.zeroize();
                out.notes.push(format!(
                    "SASL: sending the {} initial response ({} bytes).",
                    self.mechanism.name(),
                    response.len()
                ));
                out.send.extend(authenticate_lines(&response));
            }
            Phase::ScramFirst => {
                let scram = self.scram.as_mut().ok_or("Unexpected SASL state.")?;
                let job = scram.server_first(payload)?;
                out.notes.push(format!(
                    "SASL SCRAM: server-first accepted ({} bytes, {} iterations).",
                    payload.len(),
                    scram.iterations()
                ));
                self.phase = Phase::Deriving;
                out.derive = Some(job);
            }
            Phase::ScramFinal => {
                let scram = self.scram.as_mut().ok_or("Unexpected SASL state.")?;
                scram.server_final(payload)?;
                out.notes
                    .push("SASL SCRAM: server signature verified.".into());
                self.scram = None;
                self.phase = Phase::WaitingForResult;
                out.send.push(IrcCommand::AUTHENTICATE("+".into()));
            }
            _ => {
                return Err(format!("Unexpected SASL challenge while {}.", self.stage()));
            }
        }
        Ok(())
    }

    /// The finished SCRAM derivation: sends client-final.
    pub(crate) fn derived(&mut self, derived: scram::Derived) -> Result<Out, String> {
        let mut out = Out::default();
        let scram = self.scram.as_mut().ok_or("Unexpected SASL state.")?;
        if self.phase != Phase::Deriving {
            return Err("Unexpected SCRAM derivation result.".into());
        }
        let response = scram.derived(derived);
        self.phase = Phase::ScramFinal;
        out.notes.push(format!(
            "SASL SCRAM: client proof computed; sending client-final ({} bytes).",
            response.len()
        ));
        out.send.extend(authenticate_lines(response.as_bytes()));
        Ok(out)
    }
}

impl Drop for SaslHandshake {
    fn drop(&mut self) {
        self.password.zeroize();
        self.incoming.zeroize();
    }
}

/// `AUTHENTICATE` lines of at most 400 characters; a message that fills its
/// last line (or is empty) ends with `+`.
fn authenticate_lines(payload: &[u8]) -> Vec<IrcCommand> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(payload);
    let mut lines: Vec<IrcCommand> = encoded
        .as_bytes()
        .chunks(CHUNK)
        .map(|chunk| {
            IrcCommand::AUTHENTICATE(std::str::from_utf8(chunk).expect("base64 is ASCII").into())
        })
        .collect();
    if encoded.len().is_multiple_of(CHUNK) {
        lines.push(IrcCommand::AUTHENTICATE("+".into()));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scram::{Hash, mock};

    fn handshake(mechanism: SaslMechanism, password: &str) -> SaslHandshake {
        SaslHandshake::new(SaslCredentials {
            mechanism,
            username: "ac=co,unt".into(),
            password: password.into(),
        })
    }

    fn server(text: &str) -> IrcMessage {
        text.parse().unwrap()
    }

    fn b64(text: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(text)
    }

    /// The server's `AUTHENTICATE` lines for `payload`.
    fn challenge(payload: &[u8]) -> Vec<IrcMessage> {
        authenticate_lines(payload)
            .into_iter()
            .map(IrcMessage::from)
            .collect()
    }

    /// What the client sent, decoded back (lines of one message joined).
    fn decode(send: &[IrcCommand]) -> String {
        let text: String = send
            .iter()
            .map(|command| match command {
                IrcCommand::AUTHENTICATE(data) if data != "+" => data.clone(),
                IrcCommand::AUTHENTICATE(_) => String::new(),
                other => panic!("{other:?}"),
            })
            .collect();
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(text)
            .unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// Feeds a server message (several lines) and gathers the output.
    fn feed(sasl: &mut SaslHandshake, lines: &[IrcMessage]) -> Result<Out, String> {
        let mut all = Out::default();
        for line in lines {
            let out = sasl.observe(line)?;
            all.send.extend(out.send);
            all.notes.extend(out.notes);
            if out.derive.is_some() {
                all.derive = out.derive;
            }
        }
        Ok(all)
    }

    /// Runs the exchange up to the server-first message; returns the
    /// handshake, the expected client-final and the server-final text.
    fn up_to_client_final(
        mechanism: SaslMechanism,
        hash: Hash,
        salt_len: usize,
    ) -> (SaslHandshake, String, String, Vec<String>) {
        let mut sasl = handshake(mechanism, "pencil");
        let begin = sasl.begin().unwrap();
        assert_eq!(
            String::from(&begin.send[0]),
            format!("AUTHENTICATE {}", mechanism.name())
        );
        let mut notes = begin.notes;
        let out = feed(&mut sasl, &[server("AUTHENTICATE +")]).unwrap();
        notes.extend(out.notes);
        let first = decode(&out.send);
        assert!(first.starts_with("n,,n=ac=3Dco=2Cunt,r="), "{first}");
        let (server_first, client_final, server_final) =
            mock::server(hash, "pencil", &first, salt_len);
        let out = feed(&mut sasl, &challenge(server_first.as_bytes())).unwrap();
        notes.extend(out.notes);
        let derived = out.derive.expect("derivation requested").run();
        let out = sasl.derived(derived).unwrap();
        notes.extend(out.notes);
        assert_eq!(decode(&out.send), client_final);
        (sasl, client_final, server_final, notes)
    }

    #[test]
    fn scram_exchanges_complete_for_every_hash() {
        for (mechanism, hash) in [
            (SaslMechanism::ScramSha1, Hash::Sha1),
            (SaslMechanism::ScramSha256, Hash::Sha256),
            (SaslMechanism::ScramSha512, Hash::Sha512),
        ] {
            let (mut sasl, client_final, server_final, mut notes) =
                up_to_client_final(mechanism, hash, 16);
            let out = feed(&mut sasl, &challenge(server_final.as_bytes())).unwrap();
            assert_eq!(
                out.send.iter().map(String::from).collect::<Vec<_>>(),
                ["AUTHENTICATE +"]
            );
            notes.extend(out.notes);
            assert!(!sasl.complete());
            let out = sasl
                .observe(&server(":s 903 me :SASL authentication successful"))
                .unwrap();
            notes.extend(out.notes);
            assert!(sasl.complete());
            // Diagnostics carry stages and sizes only.
            let notes = notes.join("\n");
            let proof = client_final.rsplit_once(",p=").unwrap().1;
            for secret in ["pencil", proof, server_final.trim_start_matches("v=")] {
                assert!(!notes.contains(secret), "{notes}");
            }
            assert!(notes.contains("server signature verified"), "{notes}");
            assert!(notes.contains("4096 iterations"), "{notes}");
            assert!(sasl.password.is_empty(), "the password is not kept");
        }
    }

    #[test]
    fn a_wrong_server_signature_fails_and_success_is_not_accepted_early() {
        let (mut sasl, _, server_final, _) =
            up_to_client_final(SaslMechanism::ScramSha256, Hash::Sha256, 16);
        let mut forged = server_final.clone().into_bytes();
        forged[3] = if forged[3] == b'A' { b'B' } else { b'A' };
        assert!(feed(&mut sasl, &challenge(&forged)).is_err());
        // A 903 before the signature was checked is no success.
        let (mut sasl, ..) = up_to_client_final(SaslMechanism::ScramSha1, Hash::Sha1, 16);
        assert!(sasl.observe(&server(":s 903 me :ok")).is_err());
        assert!(!sasl.complete());
    }

    #[test]
    fn failure_numerics_end_the_handshake_without_fallback() {
        for numeric in ["902", "904", "905", "906", "907"] {
            let mut sasl = handshake(SaslMechanism::ScramSha512, "pencil");
            sasl.begin().unwrap();
            let error = sasl
                .observe(&server(&format!(":s {numeric} me :failed")))
                .unwrap_err();
            assert_eq!(error, crate::SASL_FAILED);
        }
    }

    #[test]
    fn long_server_messages_are_reassembled_from_chunks() {
        // 700 salt bytes make server-first span several 400 character lines.
        let (mut sasl, ..) = up_to_client_final(SaslMechanism::ScramSha256, Hash::Sha256, 700);
        assert!(!sasl.complete());
        // A line of exactly 400 characters continues; `+` ends it.
        let mut exact = handshake(SaslMechanism::ScramSha256, "pencil");
        exact.begin().unwrap();
        let first = feed(&mut exact, &[server("AUTHENTICATE +")]).unwrap();
        let (server_first, ..) = mock::server(Hash::Sha256, "pencil", &decode(&first.send), 700);
        let encoded = b64(server_first.as_bytes());
        let mut lines: Vec<IrcMessage> = encoded
            .as_bytes()
            .chunks(400)
            .map(|chunk| {
                server(&format!(
                    "AUTHENTICATE {}",
                    std::str::from_utf8(chunk).unwrap()
                ))
            })
            .collect();
        if encoded.len().is_multiple_of(400) {
            lines.push(server("AUTHENTICATE +"));
        }
        assert!(feed(&mut exact, &lines).unwrap().derive.is_some());
        assert_eq!(
            authenticate_lines(&[b'a'; 300]).len(),
            2,
            "400 chars end with +"
        );
        assert_eq!(authenticate_lines(&[]).len(), 1);
        let _ = &mut sasl;
    }

    #[test]
    fn oversized_malformed_and_unexpected_challenges_are_refused() {
        let mut sasl = handshake(SaslMechanism::ScramSha256, "pencil");
        sasl.begin().unwrap();
        feed(&mut sasl, &[server("AUTHENTICATE +")]).unwrap();
        let line = "A".repeat(CHUNK);
        let chunks = vec![server(&format!("AUTHENTICATE {line}")); MAX_INCOMING / CHUNK + 1];
        assert!(feed(&mut sasl, &chunks).is_err());

        let mut sasl = handshake(SaslMechanism::ScramSha256, "pencil");
        sasl.begin().unwrap();
        assert!(sasl.observe(&server("AUTHENTICATE !!!!")).is_err());

        // The first challenge must be empty.
        let mut sasl = handshake(SaslMechanism::Plain, "pencil");
        sasl.begin().unwrap();
        assert!(
            sasl.observe(&server(&format!("AUTHENTICATE {}", b64(b"x"))))
                .is_err()
        );

        // A challenge once the derivation was requested has no place.
        let mut sasl = handshake(SaslMechanism::ScramSha256, "pencil");
        sasl.begin().unwrap();
        let first = feed(&mut sasl, &[server("AUTHENTICATE +")]).unwrap();
        let (server_first, ..) = mock::server(Hash::Sha256, "pencil", &decode(&first.send), 16);
        feed(&mut sasl, &challenge(server_first.as_bytes())).unwrap();
        assert!(sasl.observe(&server("AUTHENTICATE +")).is_err());
    }

    #[test]
    fn unnormalizable_password_is_not_sent() {
        let mut sasl = handshake(SaslMechanism::ScramSha1, "bad\u{0007}");
        assert!(sasl.begin().is_err());
    }

    #[test]
    fn plain_sends_the_unchanged_payload_and_forgets_the_password() {
        let mut sasl = handshake(SaslMechanism::Plain, "pencil");
        sasl.begin().unwrap();
        let out = feed(&mut sasl, &[server("AUTHENTICATE +")]).unwrap();
        assert_eq!(decode(&out.send), "\0ac=co,unt\0pencil");
        assert!(sasl.password.is_empty());
        sasl.observe(&server(":s 903 me :ok")).unwrap();
        assert!(sasl.complete());
    }
}
