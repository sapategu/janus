# Licensing and third-party code

**Updated 2026-08-01**, when this repository gained a `LICENSE` for the first
time. It had none while already vendoring LGPL-3.0-derived code, which was an
exposure rather than an oversight worth leaving standing.
## The short version

**This work as a whole is distributed under LGPL-3.0-or-later**, because
`fpsapcore` is derived from
[omarroth/doubletake](https://github.com/omarroth/doubletake) and that is
doubletake's licence. The strongest copyleft actually present sets the terms for
the combined work.

Two of the three packages are **not** encumbered and are independently reusable
under a permissive licence. Which is which:
| package | licence | why |
|---|---|---|
| `fpsapcore` | **LGPL-3.0-or-later** | derived from doubletake — see below |
| `fpbridge` | **mostly Blue Oak 1.0.0** | independent reverse engineering, with four named exceptions below |
| `fairplayhash` | **Blue Oak 1.0.0** | independent reverse engineering |
| `ports/*/fairplay_sapcore.*` | **LGPL-3.0-or-later** | ports of `fpsapcore` — see below |
| `ports/*/fairplay_bridge.*`, `fairplaycore.*` | **Blue Oak 1.0.0** | independent bridge primitive; carries no `fpsapcore` code |
| `airplay/` (separate module) | **Blue Oak 1.0.0** | independently written AirPlay 2 pairing layer; depends on `golang.org/x/crypto` |
`fpbridge` imports `fpsapcore`, so any binary built from this module is a
combined work under LGPL-3.0-or-later regardless. The per-package split matters
only if you lift a package out on its own.

Licence texts: [`LICENSE`](LICENSE) (LGPL-3.0), [`COPYING.GPL-3.0`](COPYING.GPL-3.0)
— which LGPL-3.0 incorporates by reference and cannot stand without — and
[`LICENSE.BlueOak-1.0.0`](LICENSE.BlueOak-1.0.0).
## What is derived from doubletake

At commit `8ccea5f`. Everything in `fpsapcore` is covered, either as
their code or as a modification of it:
| file | relationship |
|---|---|
| `fairplay_sap.go` | taken from their `internal/airplay` |
| `fairplay_md5.go` | taken from their `internal/airplay` |
| `descriptor.go` | their descriptor function and constants |
| `bridge.go` | our `gp ^ 0x0f` and word swap around their function |
| `fast.go` | our precompute of the payload-independent prefix blocks |
| `ring.go`, `ring_swar.go` | our tabulation and SWAR form of their scramble loop |
| `scramble.go` | our GF(2)-matrix collapse of their scramble |
| `fairplay_md5_unrolled.go` | our unrolling of their round loop |
| `message_encrypt.go` | our forward-AES m3 body encryption, on their message layout |
An earlier revision of this file listed only `bridge.go`, `fast.go` and `ring.go`
as local work. That was accurate when written and went stale as the package grew;
the table above is the current state. **Treat the whole package as LGPL-3.0.**
### The four files in `fpbridge` that are not independent

Added 2026-08-01, when the m3 framing layer was rewritten. These were written
here, but while reading doubletake's `exchangeM3` and `validateFPSAPRecord`, and
that shows in their shape. **Treat them as LGPL-3.0-or-later like `fpsapcore`,
not as Blue Oak.**
| file | what came from where |
|---|---|
| `fp_sap_session.go` | the session's structure — build the record, encrypt the local SAP into bytes 16..144, fold it into the descriptor — follows their `exchangeM3` |
| `fp_sap_m3.go` (`parseFPSAPM2` only) | field-by-field record validation, modelled on their `validateFPSAPRecord` |
| `mode_identity_test.go` | four response constants produced by running their code |
| `session_xcheck_test.go` | a local SAP and a 164-byte m3 produced by running their code |
| `fp_sap_m1.go`, `fp_sap_m1_test.go` | the m1 payload bytes `02 00 <caps> bb`, and the reading that the third is a capability mask rather than a message mode |
Being precise about what is and is not owed here, because a licence claim that
overstates independence is worse than one that understates it:
- **Every m3 constant is independently present in our own captured data.** The
  magic `FPLY`, the version/type bytes `03 01 03 00`, the declared length 152,
  the mode byte 3, the label `8f 1a 9c` and the local SAP's `00 01` head are all
  readable straight out of `m3Prefix`, which was captured here from an emulator
  snapshot years apart from doubletake. None of them was copied.
- **`fp_sap_m1.go` is the exception, and is more derived than the rest.** This
  project captured an m3 and never an m1, so the payload bytes
  `02 00 <caps> bb` are not present anywhere in our own data. They came from
  doubletake's `fpsapM1Payload`, as did the observation that the third byte is a
  capability mask and not the message mode. Nothing about that file is
  independent.
- **What was taken is the reading of the layout** — that byte 12 is the mode,
  that 13..16 is a label, that a sender randomises the local SAP from byte 2.
  Knowing where to look is the contribution, and it came from their source.
- **The test constants are measurements**, produced by executing their program
  rather than copied from it. They are listed anyway, because the point of this
  file is that a reader can check the claim rather than take it.
The rest of `fpbridge` — Phase 1, the white-box tables, the framing constants
themselves — predates any contact with doubletake and is unaffected.
### The ports carry the same obligation
Added 2026-08-03. An earlier revision of this file claimed the snippets directory
was Blue Oak throughout and "carries no `fpsapcore` code". **That was true when
written and is now false**, in the same way the `fpsapcore` table above went
stale: each language later gained a `fairplay_sapcore.*` file, and those are
ports of `fpsapcore` — which makes them derived from doubletake at one further
remove. They say so in their own headers, and they are **LGPL-3.0-or-later**:
| file | licence |
|---|---|
| `ports/{c,rust,python}/fairplay_sapcore.*`, `ports/csharp/FairPlaySapCore.cs`, `ports/kotlin/FairPlaySapCore.kt` (and their tests) | **LGPL-3.0-or-later** |
| `ports/{c,python}/fairplay_bridge.*`, `ports/rust/fairplaycore.rs`, `ports/csharp/FairPlayBridge.cs`, `ports/kotlin/FairPlayBridge.kt` (and their tests) | **Blue Oak 1.0.0** |
So "vendor a single file and you are unencumbered" holds only for the bridge
*primitive* files in the second row. Vendoring a `fairplay_sapcore.*` carries
copyleft into your project. Every file states its own licence in an
`SPDX-License-Identifier` header; **that header is authoritative** if it ever
disagrees with this document again.
That code is the closed form of the FairPlay Phase-1 bridge. This project had
independently reached the same 20 bytes through 7.2 MB of generated
straight-line code, and the two were verified byte-for-byte equal over thousands
of payloads before the closed form was adopted — see [`docs/10-history.md`](docs/10-history.md).
The independent version is not what ships; theirs is smaller and faster, so it
replaced ours.
## If you redistribute this

LGPL-3.0 §4 lets you convey a work that uses the library provided the recipient
can relink it against a modified `fpsapcore`. A statically linked Go binary has
no shared-library escape hatch, so satisfying that means shipping either the
corresponding source or the object files needed to relink.
The practical route for a Go project: vendor the Go packages (`fpbridge`,
`fpsapcore`, `fairplayhash`) as source, keep this file and the licence texts
alongside them, and pin the commit you took. If you need only the
permissively-licensed parts, use only the bridge-primitive files listed above —
the `fairplay_sapcore.*` ports do carry `fpsapcore` code.
## Apple

No Apple source code is present. The white-box tables in `fpbridge` and
`fairplayhash` are recovered *data* — for white-box cryptography the key is
dissolved into the tables, so the tables are the cipher and there is no smaller
form. No Apple binary, no ARM64 interpreter, no code-segment addresses. See
[`docs/09-limitations.md`](docs/09-limitations.md) and
[`docs/05-whitebox-crypto.md`](docs/05-whitebox-crypto.md) for what that claim
does and does not cover.
This implements an authentication handshake. It is not FairPlay Streaming DRM,
decrypts no content, and extracts no content keys.
