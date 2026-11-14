`relay-cert.der` and `relay-key.der` are public test-only TLS fixtures for
localhost. The private key is deliberately committed and must never be used by
a real service. The certificate is self-signed; production trust is unchanged.
Tests explicitly trust this certificate only for their local upstream fixture.
