use nauti_fabric::sponsor::hex_decode;
#[test] fn decodes() { assert_eq!(hex_decode::<2>("0aFF").unwrap(), [0x0a, 0xff]); }
#[test] fn trims_spaces() { assert_eq!(hex_decode::<1>(" 7f ").unwrap(), [0x7f]); }
#[test] fn short_is_error() { assert!(hex_decode::<2>("0a").is_err()); }
#[test] fn long_is_error() { assert!(hex_decode::<1>("0a0b").is_err()); }
#[test] fn not_hex_is_error_not_panic() { assert!(hex_decode::<1>("zz").is_err()); }
#[test] fn non_ascii_is_error_not_panic() { assert!(hex_decode::<1>("é").is_err()); }
