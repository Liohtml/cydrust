# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | ✅ Yes    |

## Reporting a Vulnerability

Please **do not** open a public GitHub issue for security vulnerabilities.

Send a report to: **lionel.machire@googlemail.com**

Include:
- Description of the vulnerability
- Steps to reproduce
- Potential impact
- Suggested fix (optional)

**Response timeline:**
- Acknowledgment within 48 hours
- Status update within 7 days
- Fix target within 14 days for critical issues

## Known Security Considerations

CYDRUST is designed for **local network use**. The following are known design trade-offs, not bugs:

### API Token in config.toml

The bridge reads its authentication token from `config.toml`. This file should have restricted filesystem permissions and must never be committed to version control (it is listed in `.gitignore`).

**Mitigation:** Use `chmod 600 bridge/config.toml` on Linux/macOS. On Windows, restrict access via file properties. Consider moving to an environment variable in a future release.

### No Transport Encryption (HTTP)

The bridge serves plain HTTP on the local network. All API traffic, including the auth token in request headers, is unencrypted.

**Mitigation:** This is acceptable for local-only deployments. Do not expose port 5151 to the internet. If remote access is needed, place a reverse proxy with TLS in front of the bridge.

### Token Comparison

The `hub.rs` token check uses a standard string equality comparison, which may be vulnerable to timing attacks in adversarial environments.

**Mitigation:** On a local loopback network, timing-based token enumeration is not a practical threat. A future release may switch to a constant-time comparison.

### WiFi Credentials as Build-Time Environment Variables

When building firmware with the `wifi` feature, WiFi credentials (`VIBE_SSID`, `VIBE_PASS`) are embedded in the compiled binary as string literals. Anyone with physical access to the device can extract these with a firmware dump.

**Mitigation:** Use a dedicated IoT WiFi VLAN with network isolation. Do not use your primary network password. Consider using WPA Enterprise or certificate-based auth if your router supports it.

### No Input Validation on Hook Endpoint

The `POST /hook` endpoint accepts arbitrary JSON and attempts to extract session identifiers. Malformed or oversized payloads may cause unexpected behavior.

**Mitigation:** The endpoint requires a valid auth token, limiting exposure to authenticated callers only.
