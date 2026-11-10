# MoQ qualification evidence

Status: **candidate tested; production selection remains open**. No independent-peer or native media certification is claimed.

## Sources inspected

- Cached `moq-net 0.2.21`, `src/version.rs`, `src/ietf/version.rs`, `src/client.rs`, `src/server.rs`, `src/model/origin.rs` and `src/ietf/publisher.rs`.
- Cached `moq-native 0.19.18`, `src/server.rs` and its Quinn integration.
- `web-transport-quinn 0.12.1`, raw QUIC session wrapper.
- Upstream: <https://github.com/moq-dev/moq>. Exact registry versions/checksums are in Cargo.lock. These are test-only dependencies until qualification closes.

The cached Git checkout and registry packages have different versions; do not treat them as the same source revision. The test uses registry packages, not a machine-specific checkout path.

## Findings

| Requirement | Evidence | Remaining gate |
| --- | --- | --- |
| IETF wire revision | Cached package declares drafts 14–21. Test pins `moq-transport-21`, ALPN `moqt-21`. | Independent implementation and published wire vectors |
| Avoid silent moq-lite fallback | Explicit single version in both builders and ALPN; resulting session version asserted. | Version mismatch/upgrade matrix |
| Native QUIC | Raw Quinn wrapper carries real encrypted QUIC packets through the test-only in-memory datagram link. | Native UDP, actual LAN and impaired routes |
| Authorization pause | Test inspects exact request path through `accept_request` before `ok()`. | Control-exporter/peer-key grant binding; unauthorized namespace/track rejection |
| Object delivery | One microsecond-timescale video-track object arrives byte-for-byte through a subscribed group. | LOC metadata, H.264/Opus, live-edge join and config transitions |
| Bounded model cache | Test configures a 4 MiB origin pool and 500 ms age ceiling. | Saturation, latest-group retention, per-object admission, cancellation and QUIC/socket/GPU residency |
| Structured driver lifecycle | Test starts returned drivers promptly and closes both sessions before awaiting them. | Fault cleanup, reset semantics and saturation |
| License | Declared MIT OR Apache-2.0 for the inspected MoQ packages. | Full transitive notices and codec distribution review |

Draft-17+ exchanges SETUP through its returned driver. Start polling that driver immediately after `connect` returns; waiting for the server's paused handshake before polling the client driver deadlocks startup. The qualification test covers this ordering.

The library exposes subgroup zero as its public model. Do not infer arbitrary subgroup/FEC support. Its model supports retained groups and fetch; a Janus adapter still needs explicit no-history/deadline policy and grant enforcement. The existence of a cache byte cap alone does not establish Janus's total memory bound.

## Reproduce

```sh
cargo test -p janus-session moq_qualification --locked
```

This verifies the same candidate implementation at both endpoints. It does not satisfy the independent-peer release gate. Cloudflare's current source and the PRD's draft-23 reference could not be checked through the local registry/network path; compare them before selecting the production adapter. Do not advertise draft-23 support based on this test.
