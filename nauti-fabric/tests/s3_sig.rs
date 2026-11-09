use nauti_fabric::sponsor::check_ed25519;
fn sk() -> iroh::SecretKey { iroh::SecretKey::from_bytes(&[9; 32]) }
#[test] fn good_signature_passes() { let s = sk(); let sig = s.sign(b"hello").to_bytes(); assert!(check_ed25519(s.public().as_bytes(), b"hello", &sig).is_ok()); }
#[test] fn other_message_fails() { let s = sk(); let sig = s.sign(b"hello").to_bytes(); assert!(check_ed25519(s.public().as_bytes(), b"hellp", &sig).is_err()); }
#[test] fn other_key_fails() { let s = sk(); let o = iroh::SecretKey::from_bytes(&[8; 32]); let sig = s.sign(b"hello").to_bytes(); assert!(check_ed25519(o.public().as_bytes(), b"hello", &sig).is_err()); }
#[test] fn flipped_signature_fails() { let s = sk(); let mut sig = s.sign(b"hello").to_bytes(); sig[0] ^= 1; assert!(check_ed25519(s.public().as_bytes(), b"hello", &sig).is_err()); }
