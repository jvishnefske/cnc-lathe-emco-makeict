// Test vectors made with OpenSSL 3 for keys.rs (see the tests there): a
// throwaway key pair each, its public key and a signature over VECTOR_MSG.
// The private keys were deleted; nothing here signs anything.
const P256_VECTOR_KEY: &str = "vec:p256:MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEjC/+ZLBCPn3NUmqAggB/3b+iLMdDwc9Fqreb75T3aLmVjDNaaxrdULdUZDYvHrg1gAfJWtmiQbpe3eL+T2Anjw==";
const P256_VECTOR_SIG: &str = "/8YFXxUumFKrTNP1VvkWYb3U8uSM5bh5i/TEgP39sN/CXZct/OCz7K97N+8K9AFWY/Zg+Ltnsl13zU9gKoL4KA==";
const P256_VECTOR_FPR: &str = "741a9c5195f7862df28c4d310b731f44efa8994d26c3b401845950218df1504b";
const ED25519_VECTOR_KEY: &str = "vec:l33N6pwHBOehVNHEVubiNxltEQbhLcgzozU50piYqtw=";
const ED25519_VECTOR_SIG: &str = "BZSmW7u14ynmgzkb81fYaqe6ULNNQgv4skMHkvcJWWwbown8NWU37/kdN3hXvpL2kLGed0bR1VggXrd+fI8VBw==";
const ED25519_VECTOR_FPR: &str = "a0f616b206df7b1059a53fc949cc826ab61cdf9830eecaa6e1a49041d47669f7";
