use std::fmt::{self, Debug, Display};
use std::str::FromStr;

use byteorder::{BigEndian, ByteOrder};
use digest::Digest;
use typenum::U32;

#[cfg(feature = "dalek")]
use dalek::Signer;
#[cfg(feature = "dalek")]
use ed25519_dalek as dalek;
#[cfg(feature = "dalek")]
use typenum::U64;

use crate::ascii_armor::{ascii_armor, remove_ascii_armor};
use crate::packet::*;
use crate::Base64;
use crate::PgpError;
use crate::{Fingerprint, Signature};

/// The valid types of OpenPGP signatures.
#[allow(missing_docs)]
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub enum SigType {
    BinaryDocument = 0x00,
    TextDocument = 0x01,
    Standalone = 0x02,
    GenericCertification = 0x10,
    PersonaCertification = 0x11,
    CasualCertification = 0x12,
    PositiveCertification = 0x13,
    SubkeyBinding = 0x18,
    PrimaryKeyBinding = 0x19,
    DirectlyOnKey = 0x1F,
    KeyRevocation = 0x20,
    SubkeyRevocation = 0x28,
    CertificationRevocation = 0x30,
    Timestamp = 0x40,
    ThirdPartyConfirmation = 0x50,
}

/// A subpacket to be hashed into the signed data.
///
/// See RFC 4880 for more information.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Ord, PartialOrd, Debug)]
pub struct SubPacket<'a> {
    /// The tag for this subpacket.
    pub tag: u8,
    /// The data in this subpacket.
    pub data: &'a [u8],
}

/// An OpenPGP formatted ed25519 signature.
#[derive(Eq, PartialEq, Hash)]
pub struct PgpSig {
    data: Vec<u8>,
}

impl PgpSig {
    /// Construct a new PGP signature.
    ///
    /// This will construct a valid OpenPGP signature using the ed25519
    /// signing algorithm & SHA-256 hashing algorithm. It will contain
    /// these hashed subpackets:
    ///  - A version 4 key fingerprint
    ///  - A timestamp
    ///  - Whatever subpackets you pass as arguments
    ///
    /// It will contain the key id as an unhashed subpacket.
    pub fn new<Sha256, F>(
        data: &[u8],
        fingerprint: Fingerprint,
        sig_type: SigType,
        unix_time: u32,
        subpackets: &[SubPacket],
        sign: F,
    ) -> PgpSig
    where
        Sha256: Digest<OutputSize = U32>,
        F: Fn(&[u8]) -> Signature,
    {
        let data = prepare_packet(2, |packet| {
            packet.push(4); // version number
            packet.push(sig_type as u8); // signature class
            packet.push(22); // signing algorithm (EdDSA)
            packet.push(8); // hash algorithm (SHA-256)

            write_subpackets(packet, |hashed_subpackets| {
                // fingerprint
                write_single_subpacket(hashed_subpackets, 33, |packet| {
                    packet.push(4);
                    packet.extend(&fingerprint);
                });

                // timestamp
                write_single_subpacket(hashed_subpackets, 2, |packet| {
                    packet.extend(&bigendian_u32(unix_time))
                });

                for &SubPacket { tag, data } in subpackets {
                    write_single_subpacket(hashed_subpackets, tag, |packet| packet.extend(data));
                }
            });

            let hash = {
                let mut hasher = Sha256::default();

                hasher.process(data);

                hasher.process(&packet[3..]);

                hasher.process(&[0x04, 0xff]);
                hasher.process(&bigendian_u32((packet.len() - 3) as u32));

                hasher.fixed_result()
            };

            write_subpackets(packet, |unhashed_subpackets| {
                write_single_subpacket(unhashed_subpackets, 16, |packet| {
                    packet.extend(&fingerprint[12..]);
                });
            });

            packet.extend(&hash[0..2]);

            let signature = sign(&hash[..]);
            write_mpi(packet, &signature[00..32]);
            write_mpi(packet, &signature[32..64]);
        });

        PgpSig { data }
    }

    /// Parse an OpenPGP signature from binary data.
    ///
    /// This must be an ed25519 signature using SHA-256 for hashing,
    /// and it must be in the subset of OpenPGP supported by this library.
    pub fn from_bytes(bytes: &[u8]) -> Result<PgpSig, PgpError> {
        // TODO: convert to three byte header
        let (data, packet) = find_signature_packet(bytes)?;
        has_correct_structure(packet)?;
        has_correct_hashed_subpackets(packet)?;
        Ok(PgpSig { data })
    }

    /// Parse an OpenPGP signature from ASCII armored data.
    pub fn from_ascii_armor(string: &str) -> Result<PgpSig, PgpError> {
        let data = remove_ascii_armor(string, "BEGIN PGP SIGNATURE", "END PGP SIGNATURE")?;
        PgpSig::from_bytes(&data)
    }

    /// Get the binary representation of this signature.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Get the portion of this signature hashed into the signed data.
    pub fn hashed_section(&self) -> &[u8] {
        let subpackets_len = BigEndian::read_u16(&self.data[7..9]) as usize;
        &self.data[3..(subpackets_len + 9)]
    }

    /// Get the actual ed25519 signature contained.
    pub fn signature(&self) -> Signature {
        let init = self.data.len() - 68;
        let sig_data = &self.data[init..];
        let mut sig = [0; 64];
        sig[00..32].clone_from_slice(&sig_data[2..34]);
        sig[32..64].clone_from_slice(&sig_data[36..68]);
        sig
    }

    /// Get the fingerprint of the public key which made this signature.
    pub fn fingerprint(&self) -> Fingerprint {
        let mut fingerprint = [0; 20];
        fingerprint.clone_from_slice(&self.data[10..30]);
        fingerprint
    }

    /// Get the type of this signature.
    pub fn sig_type(&self) -> SigType {
        match self.data[4] {
            0x00 => SigType::BinaryDocument,
            0x01 => SigType::TextDocument,
            0x02 => SigType::Standalone,
            0x10 => SigType::GenericCertification,
            0x11 => SigType::PersonaCertification,
            0x12 => SigType::CasualCertification,
            0x13 => SigType::PositiveCertification,
            0x18 => SigType::SubkeyBinding,
            0x19 => SigType::PrimaryKeyBinding,
            0x1F => SigType::DirectlyOnKey,
            0x20 => SigType::KeyRevocation,
            0x28 => SigType::SubkeyRevocation,
            0x30 => SigType::CertificationRevocation,
            0x40 => SigType::Timestamp,
            0x50 => SigType::ThirdPartyConfirmation,
            _ => panic!("Unrecognized signature type."),
        }
    }

    /// Verify data against this signature.
    ///
    /// The data to be verified should be inputed by hashing it into the
    /// SHA-256 hasher using the input function.
    pub fn verify<Sha256, F1, F2>(&self, input: F1, verify: F2) -> bool
    where
        Sha256: Digest<OutputSize = U32>,
        F1: FnOnce(&mut Sha256),
        F2: FnOnce(&[u8], Signature) -> bool,
    {
        let hash = {
            let mut hasher = Sha256::default();

            input(&mut hasher);

            let hashed_section = self.hashed_section();
            hasher.process(hashed_section);

            hasher.process(&[0x04, 0xff]);
            hasher.process(&bigendian_u32(hashed_section.len() as u32));

            hasher.fixed_result()
        };

        verify(&hash[..], self.signature())
    }

    #[cfg(feature = "dalek")]
    /// Convert this signature from an ed25519-dalek signature.
    pub fn from_dalek<Sha256, Sha512>(
        keypair: &dalek::SigningKey,
        data: &[u8],
        fingerprint: Fingerprint,
        sig_type: SigType,
        timestamp: u32,
    ) -> PgpSig
    where
        Sha256: Digest<OutputSize = U32>,
        Sha512: Digest<OutputSize = U64>,
    {
        PgpSig::new::<Sha256, _>(data, fingerprint, sig_type, timestamp, &[], |data| {
            keypair.sign(data).to_bytes()
        })
    }

    #[cfg(feature = "dalek")]
    /// Convert this signature to an ed25519-dalek signature.
    pub fn to_dalek(&self) -> dalek::Signature {
        dalek::Signature::from_bytes(&self.signature())
    }

    #[cfg(feature = "dalek")]
    /// Verify this signature against an ed25519-dalek public key.
    pub fn verify_dalek<Sha256, Sha512, F>(&self, key: &dalek::VerifyingKey, input: F) -> bool
    where
        Sha256: Digest<OutputSize = U32>,
        Sha512: Digest<OutputSize = U64>,
        F: FnOnce(&mut Sha256),
    {
        self.verify::<Sha256, _, _>(input, |data, signature| {
            let sig = dalek::Signature::from_bytes(&signature);
            key.verify_strict(data, &sig).is_ok()
        })
    }
}

impl Debug for PgpSig {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("PgpSig")
            .field("key", &Base64(&self.data[..]))
            .finish()
    }
}

impl Display for PgpSig {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        ascii_armor(
            "BEGIN PGP SIGNATURE",
            "END PGP SIGNATURE",
            &self.data[..],
            f,
        )
    }
}

impl FromStr for PgpSig {
    type Err = PgpError;
    fn from_str(s: &str) -> Result<PgpSig, PgpError> {
        PgpSig::from_ascii_armor(s)
    }
}

fn find_signature_packet(data: &[u8]) -> Result<(Vec<u8>, &[u8]), PgpError> {
    let (init, len) = match data.first() {
        Some(&0x88) => {
            if data.len() < 2 {
                return Err(PgpError::InvalidPacketHeader);
            }
            (2, data[1] as usize)
        }
        Some(&0x89) => {
            if data.len() < 3 {
                return Err(PgpError::InvalidPacketHeader);
            }
            let len = BigEndian::read_u16(&data[1..3]);
            (3, len as usize)
        }
        Some(&0x8a) => {
            if data.len() < 5 {
                return Err(PgpError::InvalidPacketHeader);
            }
            let len = BigEndian::read_u32(&data[1..5]);
            if len > u16::MAX as u32 {
                return Err(PgpError::UnsupportedPacketLength);
            }
            (5, len as usize)
        }
        _ => return Err(PgpError::UnsupportedPacketLength),
    };

    if data.len() < init + len {
        return Err(PgpError::InvalidPacketHeader);
    }

    let packet = &data[init..][..len];

    if init == 3 {
        Ok((data.to_owned(), packet))
    } else {
        let mut vec = Vec::with_capacity(3 + len);
        let len = bigendian_u16(len as u16);
        vec.push(0x89);
        vec.push(len[0]);
        vec.push(len[1]);
        vec.extend(packet.iter().cloned());
        Ok((vec, packet))
    }
}

fn has_correct_structure(packet: &[u8]) -> Result<(), PgpError> {
    if packet.len() < 6 {
        return Err(PgpError::UnsupportedSignaturePacket);
    }

    if !(packet[0] == 4 && packet[2] == 22 && packet[3] == 8) {
        return Err(PgpError::UnsupportedSignaturePacket);
    }

    let hashed_len = BigEndian::read_u16(&packet[4..6]) as usize;
    if packet.len() < hashed_len + 8 {
        return Err(PgpError::UnsupportedSignaturePacket);
    }

    let unhashed_len = BigEndian::read_u16(&packet[(hashed_len + 6)..][..2]) as usize;
    if packet.len() != unhashed_len + hashed_len + 78 {
        return Err(PgpError::UnsupportedSignaturePacket);
    }

    Ok(())
}

fn has_correct_hashed_subpackets(packet: &[u8]) -> Result<(), PgpError> {
    let hashed_len = BigEndian::read_u16(&packet[4..6]) as usize;
    if hashed_len < 23 {
        return Err(PgpError::MissingFingerprintSubpacket);
    }

    // check that the first subpacket is a fingerprint subpacket
    if !(packet[6] == 22 && packet[7] == 33 && packet[8] == 4) {
        return Err(PgpError::MissingFingerprintSubpacket);
    }

    Ok(())
}
