# TenderChain (Module 26) — Off-chain Content Store

Audience: portal implementers and audit/probity tooling. Companion to
`tenderchain-integration.md`, which covers what the pallet emits. This document
covers the other half: where the readable content behind each on-chain hash
lives, and how an auditor verifies the two agree.

Pallet source: `pallets/tender-chain/`. Type definitions: `src/types.rs`.

---

## 1. Why this exists

The pallet stores structure and proof, never readable content (spec §1.2, §8).
A tender's evaluation criteria, the text of a bidder question, an award
rationale — each is on chain only as `Hash256`, a blake2-256 commitment:

```rust
/// A 32-byte content commitment (blake2-256 of a DNC-anchored document, spec §1.2).
pub type Hash256 = [u8; 32];
```

That is correct pallet design, but it leaves an open question the pallet
deliberately does not answer: *where does the preimage live?*

**Title, summary, challenge grounds and challenge resolutions are the
exception.** They are held on chain in readable form — see §8. Everything else
in the table below still needs the store this document specifies.

The portal currently answers it with `localStorage`
(`frontend/tenderchain/src/services/contentStore.ts`). That is adequate for a
single-browser demo and inadequate for anything else. Two consequences follow
directly:

- A tender created in one browser renders in another as `0x7a3c1f…9f1b` — the
  chain has the hash, but the only copy of the text was local to the creator.
- Clearing browser data destroys the content permanently. The hash is one-way;
  nothing on chain can reconstruct it.

This document specifies the minimal service that closes that gap.

---

## 2. Storage choice

**SQLite.** Every write is the same shape — `hash → blob` — and every read is an
exact-match lookup by primary key. There is no nested structure to model, no
flexible schema, and no query predicate beyond equality on `hash`.

A document store (MongoDB) would add a server process and a network hop while
providing nothing this access pattern uses. A relational table additionally
enforces the content-addressing invariant in the schema itself, via `PRIMARY KEY`
on `hash`, rather than in application code.

Postgres becomes the right answer when this service grows into a read model with
search and filtering across tenders. It is not the right answer for a key-value
table that fits in one file. See §7.

---

## 3. Schema

```sql
CREATE TABLE content (
  hash          TEXT PRIMARY KEY,   -- blake2-256 hex; MUST equal blake2_256(body)
  content_type  TEXT NOT NULL CHECK (content_type IN (
                   'notice', 'criteria', 'addendum',
                   'question', 'answer',
                   'cancel_reason', 'conflict_declaration',
                   'award_rationale', 'challenge_grounds', 'challenge_resolution',
                   'contract'
                 )),
  tender_id     INTEGER NOT NULL,
  body          TEXT NOT NULL,      -- raw text, or a JSON string for notice/criteria
  mime_type     TEXT NOT NULL DEFAULT 'text/plain',
  created_by    TEXT,               -- SS58 account, for the audit trail
  created_at    INTEGER NOT NULL    -- unix seconds
);

CREATE INDEX idx_content_tender ON content (tender_id);
```

One table is the right granularity. The service stores and serves opaque
preimages; it does not parse them. Splitting `body` into typed, queryable
columns is the indexer's job, not this service's — see §7.

`hash` is the primary key rather than a surrogate id because the content *is*
its identity. Two writes of identical content are the same row by construction,
and deduplication is free.

---

## 4. Content types and their on-chain anchors

Each `content_type` corresponds to exactly one hash field in chain state or in
an emitted event. This mapping is the verification contract: given a
`content_type` and a `tender_id`, an auditor knows precisely which on-chain
value to compare against.

| `content_type` | On-chain anchor | Shape |
|---|---|---|
| `notice` | `TenderRecord.notice_hash` | JSON — the full specification bundle |
| `criteria` | `TenderRecord.criteria_hash` | JSON — array of criteria, `id` renumbered to index |
| `addendum` | `AddendumRecord.content_hash` | text |
| `question` | `QaRecord.question_hash` | text |
| `answer` | `QaRecord.answer_hash` (`Option`) | text |
| `cancel_reason` | `Event::TenderCancelled.reason_hash` | text |
| `conflict_declaration` | `EvaluatorRecord.conflict_declaration` (`Option`) | text |
| `award_rationale` | `OutcomeRecord.rationale_hash` | text |
| `challenge_evidence` | `ChallengeRecord.evidence_hash` (`Option`) | binary/text — supporting exhibits |
| `contract` | `OutcomeRecord.contract_hash` (`Option`) | text |

`challenge_grounds` and `challenge_resolution` were rows in this table and are
not any more: both are readable on chain (§8). `challenge_evidence` replaces
them for the case this store is still right for — exhibits too large to put in
state.

`criteria` is renumbered before hashing: the wizard labels criteria `c1`, `c2`…
but `criterion_id` is a `u32` on chain, and scores are submitted against those
ids. The stored JSON must carry the renumbered ids, or the stored criteria and
the on-chain weights will disagree.

---

## 5. Interface

```
PUT /content
    body: { content_type, tender_id, body, created_by? }
    -> { hash }

GET /content/:hash
    -> { hash, content_type, tender_id, body, mime_type, created_at }
    -> 404 when the preimage was never submitted
```

The server computes `hash = blake2_256(body)` itself and does not accept a
client-supplied hash. If a row already exists for that hash with a differing
body, the write is rejected — under blake2-256 that indicates a bug or an
attack, never ordinary operation.

The client still computes the hash locally before the write confirms, because
it needs the value to put on chain. The point is that the server's copy is
derived independently, so a client cannot cause the store to serve content that
does not match its own key.

**404 is a normal response, not an error condition.** Content submitted by
another portal instance, or never submitted at all, is simply absent. Callers
render the short hash as a fallback, exactly as `shortHash()` does today.

---

## 6. What must not be stored here

**Sealed bid content, before the reveal deadline.** In `BidMode::Sealed` the
chain holds only:

```
blake2_256(bidder ‖ documents_hash ‖ prices ‖ salt)
```

There is nothing readable on chain to leak before opening — that is the property
the module's design rests on. Writing bid prices, documents or salts to a
server-side database before the reveal window closes reintroduces exactly the
leak the commit-reveal scheme exists to prevent, and does so at a single point
that a database administrator can read.

Those values stay client-side, in `localBidStore.ts`, until reveal. Their
durability is the bidder's responsibility, as with a private key; the portal
should offer an export so a bidder can back up their salts rather than silently
depending on one browser profile.

`RevealRecord.documents_hash` is therefore absent from the §4 table. After a
valid reveal the documents are public and *may* be admitted as a twelfth content
type. That is a deliberate later decision, not an omission.

---

## 7. Relationship to a future indexer

This service is not an indexer and should not grow into one by accretion. It
answers exactly one question: *given a hash, what was the preimage?*

An indexer answers a different question — *which tenders match these criteria?* —
and needs machinery this service has no use for: event subscription, fork and
reorg handling, backfill from genesis, and a relational read model whose columns
are parsed out of the bodies stored here.

Build it when there is a query that the chain plus this table cannot answer
efficiently: full-text search over notices, cross-tender analytics, or a tender
list too large to enumerate client-side. Until then the chain is the index, and
`content` is the dereference.

---

## 8. What is *not* in this store: readable public record

`TenderRecord` carries the tender's name and short description on chain as
readable `BoundedVec<u8, _>` fields, not hashes:

```rust
pub title: BoundedVec<u8, MaxTitleLen>,       // 128 bytes in the runtime
pub summary: BoundedVec<u8, MaxSummaryLen>,   // 512 bytes in the runtime
```

The reasoning, in short: spec §1.2 confines hashing to *confidential* content,
and a tender notice is published in order to be read. Hashing its name bought
no confidentiality and cost real properties — an auditor reading raw chain
state saw a bare commitment, and the readable text depended on whoever held the
off-chain store. On chain, the subject matter of every procurement is permanent
public record.

`notice_hash` is unchanged and still anchors the full specification bundle:
drawings, schedules and terms are far too large for chain storage. The split is
deliberate — a short readable name on chain, the heavy document off it.

Over-length input is **rejected, not truncated** (`TitleTooLong`,
`SummaryTooLong`), and an empty title is rejected (`TitleEmpty`). A silently
truncated title would be a wrong public record that the officer never saw
happen.

`Event::TenderCreated` carries `title`, so an indexer can build a tender list
from the event stream alone without a state query per tender.

### 8.1 Challenge grounds and resolutions

`ChallengeRecord` follows the same rule, for the same reason:

```rust
pub grounds: BoundedVec<u8, MaxGroundsLen>,          // 2048 bytes in the runtime
pub resolution: Option<BoundedVec<u8, MaxResolutionLen>>, // 2048 bytes, set on resolve
pub evidence_hash: Option<Hash256>,                  // exhibits stay off chain
```

A challenge is an allegation that a public award was made improperly, and it
suspends execution of that award in public while it stands. There was never
confidentiality for hashing to protect, so the hash bought nothing and cost the
property that matters: an auditor reading raw chain state could see *that* an
award was challenged and upheld, but not what was alleged or why it was upheld.
The readable text existed only in whichever browser held the preimage, which
for the single record a procurement dispute turns on is the wrong place.

`resolve_challenge`'s doc comment already claimed "either way the reasoning is
permanent". Under the hash-only form that was not true. It is now.

Both are **rejected, not truncated**, over length (`GroundsTooLong`,
`ResolutionTooLong`), and neither may be empty (`GroundsEmpty`,
`ResolutionEmpty`) — a ruling with no stated reasoning is not a ruling, and an
allegation with no stated grounds suspends a lawful award on nothing.

`evidence_hash` is `Option` and stays off chain: exhibits are drawings,
correspondence and bid comparisons, far too large for state, exactly as
`notice_hash` is for a tender.
