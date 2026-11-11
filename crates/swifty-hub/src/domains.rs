//! Domain separation strings for the hub's own cryptography.
//!
//! They are frozen, byte for byte. Each one is mixed into a signature or a hash
//! that already exists: the audit head's signature, the binding a confirmation
//! commits to, and the challenge of a step-up. Changing a byte would stop the
//! audit chain written so far from verifying. They are separators, not branding,
//! so they keep the name the product had when they were first used.

/// Put in front of the audit head before it is signed with the audit key.
pub const AUDIT_HEAD: &[u8] = b"glimdock-audit-head-v1\0";

/// The first element of the array a confirmation or step-up binding hashes.
pub const BINDING: &str = "glimdock-binding-v1";

/// Put in front of the nonce and the binding when a step-up challenge is derived.
/// A NUL byte follows it.
pub const STEP_UP: &str = "glimdock-step-up-v1";

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::*;

    /// Written as the SHA-256 of each string, so that a search and replace over the
    /// source cannot change a string and its test together.
    #[test]
    fn the_domain_strings_are_frozen() {
        let sha256 = |bytes: &[u8]| hex::encode(Sha256::digest(bytes));
        assert_eq!(
            (AUDIT_HEAD.len(), sha256(AUDIT_HEAD)),
            (23, "90edcb729b5fded0b9d8506b806bed0fa3c23b3d631b8d4f91370790a5b6346e".to_string())
        );
        assert_eq!(
            (BINDING.len(), sha256(BINDING.as_bytes())),
            (19, "0e333e2c015739cbff5ab8adde9d088fbd59e882e273e3a68cbc81ed318d2300".to_string())
        );
        assert_eq!(
            (STEP_UP.len(), sha256(STEP_UP.as_bytes())),
            (19, "37299997798ff695edc68b02a56477b1af7f28fbc96306ddfb9ee0cca9388f0d".to_string())
        );
    }
}
