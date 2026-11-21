# Security Policy / 安全策略

## Reporting a vulnerability

Please open a private security advisory on GitHub:
<https://github.com/WUNELEZI1/mtkclient-rs/security/advisories/new>

Do not disclose exploitable details in public issues before a fix is released.

## Release verification

Every tagged release publishes:

- `mtkclient-rs-windows-x86_64.zip` — signed when a code-signing certificate is configured.
- `SHA256SUMS.txt` — SHA-256 for every artifact.
- `sbom.cdx.json` — CycloneDX 1.5 software bill of materials.

Verify the checksum before flashing:

```bash
sha256sum -c SHA256SUMS.txt
```

On Windows:

```bat
certutil -hashfile mtkclient-rs-windows-x86_64.zip SHA256
```

Compare the output with the matching line in `SHA256SUMS.txt`. If the archive is Authenticode-signed, `signtool verify /pa` (or the file properties dialog) must report a valid signature.

## Scope

This tool performs low-level storage writes. Destructive operations require explicit confirmation (`DangerConfirm`); release builds never bypass it. Reports about bricking risks, signature bypasses, or unsafe default behaviour are treated as security issues.
