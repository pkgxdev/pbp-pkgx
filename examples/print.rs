extern crate ed25519_dalek as dalek;
extern crate pbp_pkgx;
extern crate rand;
extern crate sha2;

use dalek::Keypair;
use pbp::{KeyFlags, PgpKey};
use rand::OsRng;
use sha2::{Sha256, Sha512};

fn main() {
    let mut cspring = OsRng::new().unwrap();
    let keypair = Keypair::generate::<Sha512>(&mut cspring);

    let key = PgpKey::from_dalek::<Sha256, Sha512>(&keypair, KeyFlags::NONE, "withoutboats");
    println!("{}", key);
}
