# Cast device trust data

Offline public trust data used by `janus_cast::trust`. Nothing here is fetched at
runtime; Janus makes no account, cloud or SDK call to authenticate a receiver.

| File | Purpose | SHA-256 |
| --- | --- | --- |
| `cast_root_ca.der` | Device anchor `CN=Cast Root CA` (2014-04-02 → 2034-03-28, pathlen 2) | `809af14700b3fe2611ad597eb1584d6354313b64ccdb3390f097fa3e5826d6ca` |
| `eureka_root_ca.der` | Device anchor `CN=Eureka Root CA` (2012-12-17 → 2032-12-12) | `caf6d1e37b532203e01a76bf07187bb731ccd38801565ab2211a2c0ab7f3bc46` |
| `cast_crl_root_ca.der` | CRL anchor `CN=Cast CRL Root CA` (2016-08-01 → 2036-07-27, pathlen 1) | `8b77c60291f9d62764613083f49750b1c5caac2d9efbe5a28edf301f9d84831d` |
| `cast_fallback_crl.pb` | Signed fallback `CrlBundle`, used when a receiver supplies no valid CRL | `2576ba74902221bd1a231a67cc4fc6862bd85d438edec26881fa1f5dfe5e25fc` |

Sources (BSD-style license, The Chromium Authors), extracted byte-for-byte from
the C arrays:

- Open Screen `cast/common/certificate/{cast_root_ca_cert_der-inc.h,
  eureka_root_ca_der-inc.h, cast_crl_root_ca_cert_der-inc.h}` at commit
  `308a18e24e717ce27231c0aaa713364a41416e54`.
- Chromium `components/media_router/common/providers/cast/certificate/cast_fallback_crl.h`
  at commit `256d14612ecd3ab6b42283197a5d34a3d9f07008` (signer verified at its
  published timestamp `1692255600`).

## Refresh policy

Chromium treats the fallback CRL as valid from build time for 20 weeks. Janus
pins that start as `FALLBACK_CRL_REFRESHED_UNIX` (2026-11-09T00:00:00Z), so the
fallback expires on 2027-03-29. After that, a receiver that does not send its
own valid CRL fails closed with "revocation data unavailable" until this
directory and the constant are refreshed in a new release. Do not extend the
window without updating the file from the upstream source.

Test vectors used by the unit tests are under `../testdata/` and come from Open
Screen `test/data/cast/common/certificate/` at the same commit. They are
public test material, never production anchors.
