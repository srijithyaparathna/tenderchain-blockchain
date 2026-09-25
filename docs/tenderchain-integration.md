# TenderChain (Module 26) — Event Schema & Integration Guide

Audience: procurement portal teams, audit/probity tooling, bidder front ends and
agency front ends. Companion to the Module 26 work assignment; this document
records what the pallet as built actually emits and expects.

Pallet index in the runtime: **8** (`TenderChain`). Source:
`pallets/tender-chain/`.

---

## 1. Reconstructing a tender from events alone

Spec §9 requires that an independent observer be able to rebuild the whole
trail from events and state. Every lifecycle fact is emitted; nothing that
matters to probity is inferable only from a storage diff.

The minimal reconstruction is: filter all events by `tender_id`, order by
`(block_number, extrinsic_index)`, and fold:

```
TenderCreated        -> Draft
TenderPublished      -> QaWindow          (close_block is now locked)
SubmissionOpened     -> Submission        (Q&A closed; wheel-emitted)
TenderClosed         -> Closed            (emitted by the deadline wheel)
OpeningStarted       -> Opening           (participants count becomes public)
EvaluationStarted    -> Evaluation        (emitted by the deadline wheel)
Awarded              -> Awarded
StandstillOpened     -> standstill running until standstill_end
ChallengeLodged      -> Challenged        (execution suspended)
ChallengeResolved    -> back to Awarded, or remitted
ContractExecuted     -> Contracted
TenderCancelled      -> Cancelled         (terminal)
```

Every transition is event-backed, including the questions-close gate, which
emits `SubmissionOpened`. A tender's full lifecycle is reconstructible from
events alone.

`TenderClosed`, `EvaluationStarted` and `StandstillClosed` originate in
`on_initialize`, **not** in an extrinsic. Indexers that only scan extrinsic
events will miss the three transitions that prove the chain — not an official —
closed the tender. Scan block events too.

## 2. Event schema

Field types: every id — `tender_id`, `question_id`, `challenge_id`, `panel_id`,
`call_off_id`, `criterion_id` and a price line's `item_id` — is a 32-byte hash (`[u8; 32]`,
rendered as 0x-hex), never a counter; see §2a. `Hash256` is a 32-byte
blake2-256 content commitment (hex, DNC-resolvable); `block` is `BlockNumber`;
`amount` is `Balance`.

### Publication and rules

| Event | Fields | Meaning for the record |
|---|---|---|
| `TenderCreated` | `tender_id`, `officer`, `title` | Draft exists. Nothing is locked yet. `title` is readable UTF-8, so an indexer can list tenders from events alone. |
| `TenderPublished` | `tender_id`, `entity`, `close_block` | **The lock point.** From this block criteria hash, weights, gates and eligibility are immutable. `close_block` is the consensus submission deadline. |
| `AddendumPublished` | `tender_id`, `content_hash`, `extended_close_to` | A clarification or change. `extended_close_to` is `Some` only when the close moved — and it can only ever move later. |
| `QuestionAsked` | `tender_id`, `question_id` | Omits the asker. Whether authorship is resolvable at all depends on the tender's `blind_questions` policy — see §3a. |
| `QuestionAnswered` | `tender_id`, `question_id` | Answers publish to everyone at once; there is no private-clarification call. |

### Submission and opening

| Event | Fields | Meaning for the record |
|---|---|---|
| `BidCommitted` | `tender_id`, `bidder`, `block` | Consensus proof a bid existed before close. The content is not readable — only `blake2_256(bidder ‖ documents ‖ prices ‖ salt)` is on chain. |
| `CommitmentWithdrawn` | `tender_id`, `bidder` | Pre-close withdrawal. Pair with `BondReturned`/`BondForfeited` to see bond treatment. |
| `TenderClosed` | `tender_id` | Wheel-emitted. The block gate closed submissions. |
| `OpeningStarted` | `tender_id`, `participants` | Participation count becomes public at opening, not before. |
| `BidRevealed` | `tender_id`, `bidder`, `valid` | Emitted for every reveal, valid or not. |
| `RevealMismatch` | `tender_id`, `bidder` | Revealed content did not hash to the commitment. The bid is voided but **stays on the public record** — it is never silently dropped. Always accompanied by `BidRevealed { valid: false }`. |

### Evaluation

| Event | Fields | Meaning for the record |
|---|---|---|
| `EvaluationStarted` | `tender_id` | Wheel-emitted at the end of the opening window. |
| `EvaluatorAppointed` | `tender_id`, `evaluator` | Appointed, but cannot score yet. |
| `ConflictDeclared` | `tender_id`, `evaluator` | Conflict-of-interest declaration lodged (hash in `EvaluatorSet`). |
| `EvaluatorActivated` | `tender_id`, `evaluator` | Scoring rights live. Cannot occur before `ConflictDeclared`. |
| `ScoresSubmitted` | `tender_id`, `evaluator`, `bidder` | Attribution. Per-criterion values are in the `Scores` map, keyed by the same triple. |
| `ScoreVarianceFlagged` | `tender_id`, `bidder`, `criterion_id`, `spread` | **Probity signal.** Two evaluators differed by more than the configured threshold. Emitted once per disagreeing pair per criterion, at the moment the later sheet is submitted. Route to probity observers and to Sentinel (Module 19). |

### Award, challenge, delivery

| Event | Fields | Meaning for the record |
|---|---|---|
| `Awarded` | `tender_id`, `awardee`, `rationale_hash` | One event **per awardee** — a panel award emits several. Full ranking is in `Outcomes`. |
| `StandstillOpened` | `tender_id`, `standstill_end` | Challenge window runs until `standstill_end` inclusive. |
| `StandstillClosed` | `tender_id` | Wheel-emitted. `execute_award` becomes callable. |
| `ChallengeLodged` | `tender_id`, `challenge_id`, `challenger` | Execution suspends immediately. |
| `ChallengeResolved` | `tender_id`, `challenge_id`, `state` | `state` is `Dismissed` (execution may proceed) or `Upheld` (remitted to re-evaluation or cancellation). |
| `ContractExecuted` | `tender_id`, `contract_hash` | Contract notarised. |
| `DeliveryInstantiated` | `tender_id`, `awardee`, `delivery_project` | One per awardee. `delivery_project` is the Module 25 reference (spec §4.2 names it on the execution event; it is split out here because a panel award has several awardees). `None` while Work Task is stubbed. |
| `ShortlistPublished` | `tender_id`, `suppliers` | An EOI closed stage one. Those suppliers can now bid on an RFT created with `shortlist_from` pointing at this tender. |
| `PanelMemberAdmitted` | `panel_id`, `supplier` | Panel/standing-offer award admitted a supplier to the pool. |
| `CallOffPlaced` | `panel_id`, `call_off_id`, `supplier`, `order_hash` | An order against a standing offer, stored in `CallOffs`. |
| `CriteriaAmended` | `tender_id`, `criteria_hash` | A draft's criteria were replaced. Impossible after publication (`CriteriaLocked`). |
| `PolicyUpdated` | `policy` | The deployment's procurement policy changed. Binds tenders published from now on only. |
| `BondReturned` / `BondForfeited` | `tender_id`, `bidder`, `amount` | Bond settlement. Returns happen automatically on execute and on cancellation. |
| `TenderCancelled` | `tender_id`, `reason_hash` | Terminal, and the reason is permanently public — silent cancellation is not possible. |
| `GateDropped` | `tender_id`, `gate` | A deadline overflowed its block and every block in the 16-block look-ahead was also full. Should never occur with sane `MaxDeadlinesPerBlock`; if it does, the tender is stuck at that gate and needs probity attention. |

## 2a. Hashed identifiers

No id in this pallet is a counter. A counter leaks volume (how many tenders an
entity has run, how many questions or challenges a tender drew) and invites
reuse across forks and restarts. Chain-minted ids are domain-separated
blake2-256 hashes, and each has a public helper so a portal or auditor can
recompute it:

| Id | Preimage | Helper |
|---|---|---|
| `tender_id` | `"tenderchain/tender" ‖ SCALE(officer) ‖ SCALE(entity) ‖ SCALE(nonce: u64) ‖ SCALE(created_at)` | `Pallet::tender_id_for` |
| `question_id` | `"tenderchain/question" ‖ tender_id ‖ SCALE(index: u32)` | `Pallet::question_id_for` |
| `challenge_id` | `"tenderchain/challenge" ‖ tender_id ‖ SCALE(index: u32)` | `Pallet::challenge_id_for` |
| `panel_id` | `"tenderchain/panel" ‖ tender_id` | `Pallet::panel_id_of` |
| `call_off_id` | `"tenderchain/calloff" ‖ panel_id ‖ SCALE(index: u32)` | `Pallet::call_off_id_for` |

`nonce` is the `TenderNonce` storage value at creation; `index` is the number of
questions (`QuestionCount`), challenges (`ChallengeCount`) or call-offs
(`CallOffCount`) the tender or panel already held.

Every id, where it is stored, and who mints it:

| Name | Type alias | Stored in | Minted by |
|---|---|---|---|
| Tender id | `TenderId` | `Tenders` and every per-tender map | chain, `create_tender` |
| Question id | `QuestionId` | `Questions` | chain, `ask_question` |
| Challenge id | `ChallengeId` | `Challenges` | chain, `lodge_challenge` |
| Panel id | `PanelId` | `PanelPool`, `PanelTender`, `CallOffs` | chain, derived from the tender |
| Call-off id | `CallOffId` | `CallOffs` | chain, `call_off` |
| Criterion id | `CriterionId` | `TenderRecord.weights`, `ScoreSheet.scores` | caller, hash of the criterion's definition |
| Price-line item id | `ItemId` | `RevealRecord.price_schedule` | caller, hash of the line's definition | `created_at` is also stored on the tender record, and is what lists
should sort by, since hashes carry no order.

The asker is deliberately **not** part of a question's preimage: with blinded
authorship, a hash over the asker could be brute-forced against the handful of
plausible suppliers and would undo the blinding.

`criterion_id` and `item_id` are supplied by the caller as hashes of their own
definitions. The portal uses
`blake2_256(JSON{index, name, description, weight})` for a criterion (so the id
points at one entry in the document `criteria_hash` commits to) and
`blake2_256("item:" ‖ index ‖ ":" ‖ description)` for a price line. Criterion
ids must be unique within a tender and within a scoresheet
(`DuplicateCriterion`).

### Panel id derivation

A panel is established by the tender that awarded it: `panel_id ==
panel_id_of(tender_id)` of the establishing `Panel` tender, and `PanelTender`
maps it back. Members are admitted by `execute_award`, not `award`, so an award
overturned during standstill never reaches the pool; `PanelMemberAdmitted`
correlates with that tender's `ContractExecuted` in the same block.

## 3a. Question authorship and blinding

Chain state is world-readable, so omitting the asker from an event is not
blinding — anyone can read the storage map. The tender therefore carries a
`blind_questions` flag set at creation and locked at publication:

- **`blind_questions: false`** — `QaRecord.author` is `Open(AccountId)`.
  Authorship is public, as on most government tenders.
- **`blind_questions: true`** — `QaRecord.author` is
  `Blinded(blake2_256(asker ‖ salt))`. The account is never written to storage.
  The asker passes their `author_salt` to `ask_question` and keeps it; they can
  later reproduce the hash to prove authorship to a probity observer, and nobody
  else can invert it.

`Pallet::blind_author` is the canonical hasher, so a front end can reproduce it
off-chain. A blinded tender gives up the ability to prove *non*-authorship, and
loses authorship entirely if the salt is lost — treat the salt like the bid salt.

## 3b. Multi-stage: EOI to RFT

Stage one is a `TenderKind::Eoi` tender run like any other through to
`Evaluation`. The officer then calls `publish_shortlist(eoi_id, suppliers)`;
only suppliers with a valid reveal on that EOI may be listed, and the EOI moves
to the terminal `Shortlisted` state.

Stage two is an ordinary RFT created with `shortlist_from: Some(eoi_id)`. That
link is written at creation and locked at publication alongside criteria and
gates, so the shortlist a tender draws from cannot be swapped once bidding is
under way. `commit_bid` then rejects anyone not on the list with
`NotShortlisted`.

## 3. Bidder front-end guide

### Computing a commitment

The commitment preimage is exact and includes the bidder's own account, so a
rival cannot lift a commitment and replay it as theirs:

```
commitment = blake2_256( SCALE(bidder_account_id)
                       ‖ documents_hash            (32 bytes)
                       ‖ SCALE(price_schedule)     (BoundedVec<PriceLine>)
                       ‖ salt )                    (32 bytes)
```

`PriceLine` is `{ item_id: [u8; 32], amount: Balance }`. The price schedule must be
SCALE-encoded **as the `BoundedVec` it becomes on chain**, in the same order
that will later be passed to `reveal_bid`. Reordering the lines changes the
hash and voids the bid. `Pallet::compute_commitment` is the canonical
implementation; front ends must match it byte for byte.

Generate a fresh random `salt` per bid and store it with the documents. **Losing
the salt makes the bid unrevealable**, which forfeits the bond when
`forfeit_on_non_reveal` is set.

### Sealed vs open

The tender's `bid_mode` decides which call you use, and they are not
interchangeable — the wrong one returns `WrongBidMode`.

- **`BidMode::Sealed`** — `commit_bid` then `reveal_bid`, as below.
- **`BidMode::Open`** (RFQs only) — a single `submit_open_bid(tender_id,
  documents_hash, price_schedule)`. The content is recorded and readable
  immediately, so everyone sees the same thing at the same time. There is no
  hash to compute, no salt to keep, and no reveal step; the bid is stored as
  already-revealed, so it can never be forfeited as a non-reveal.

### Submission sequence

1. `commit_bid(tender_id, commitment_hash)` before `close_block`. The bond is
   reserved on the bidder's own account — never transferred away — so an
   un-forfeited bond never leaves the bidder's custody.
2. Wait for `OpeningStarted`.
3. `reveal_bid(tender_id, documents_hash, price_schedule, salt)` inside the
   opening window, with byte-identical inputs.

A reveal outside the window fails with `RevealWindowClosed`; the bid is then
treated as a non-reveal and the bond is forfeited if the terms say so. A
**mismatched** reveal is treated the same way at the end of the window (spec
§8): otherwise a bidder who saw rivals' prices could escape the forfeit by
revealing garbage instead of nothing. Front
ends should surface the opening window as a hard countdown, not a soft reminder.

**A mismatching reveal succeeds as a transaction.** `reveal_bid` does not error
on a hash mismatch — it writes the reveal with `valid: false`, emits
`RevealMismatch`, and returns `Ok`. A front end that only checks for extrinsic
failure will report a voided bid as accepted. Check the emitted events, not the
dispatch result. (The pallet declares an unused `RevealMismatch` *error*
variant that shares the event's name; it is never returned, and it is the event
that carries the meaning.)

### Errors worth handling explicitly

| Error | Front-end handling |
|---|---|
| `BondRequired` | The bidder cannot cover the bid bond. Show the bond amount and the free balance. |
| `SubmissionClosed` | The block gate closed the tender. Not recoverable — do not offer a retry. |
| `NotEligible` | The bidder lacks a required credential or the reputation floor. Link to Module 15 credential remediation. |
| `CommitmentExists` | One commitment per bidder per tender; offer `withdraw_commitment` instead. |
| `TooManyQuestions` | The tender hit `MaxQuestions`. Disable the ask-a-question control. |
| `NotShortlisted` | This RFT draws from an EOI shortlist and the bidder is not on it. |
| `EvaluatorIsBidder` | The account is on this tender's evaluation panel and so cannot bid on it. |
| `WrongBidMode` | Sealed call on an open tender, or vice versa. Read `bid_mode` and route accordingly. |
| `AlreadyRevealed` | Reveals are once-only. |
| `TooManyPriceLines` | Exceeds `MaxPriceLines`; validate before submitting. |

## 4. Agency front-end guide

### Lifecycle calls the officer drives

`create_tender` → `publish_tender` → (`answer_question`, `publish_addendum`) →
`open_tender` (after the close block) → `appoint_evaluator` /
`activate_evaluator` → [award by governed origin] → `execute_award` (after
standstill).

The officer does **not** close the tender or start evaluation — the deadline
wheel does. A portal that shows a "close tender" button is misrepresenting the
system.

### Validation to enforce before submitting `create_tender`

- Criterion weights must sum to exactly 100, else `WeightsInvalid`.
- Gates must satisfy
  `publish_at < questions_close_at < submission_close_at <= opening_at < opening_end_at`,
  else `GateOrderInvalid`.
- `publish_tender` must be called while `publish_at <= now < questions_close_at`
  — publication is a block gate (`PublishTooEarly` before it), and a draft that
  misses its own Q&A window can never be published.
- Criterion ids must be distinct (`DuplicateCriterion`).
- `BidMode::Open` is only valid for `TenderKind::Rfq` (`OpenBidNotPermitted`).

### Things the API deliberately does not allow

There is no extrinsic to amend criteria, weights or eligibility after
publication, and none to shorten a submission window. These are absent from the
API rather than blocked by a permission check, so a portal cannot expose them
and should not imply they exist. An addendum may extend the close block only.

### Things only the entity may do

`cancel_tender` is the procuring entity's call (spec §4.1: "entity authority
(governed)"), not the officer's (`NotEntity`). The entity account should be the
entity's governed (multisig) identity. The portal creates tenders with the
officer as entity, so for those the two coincide.

### Draft criteria can be amended; published criteria cannot

`amend_criteria(tender_id, criteria_hash, weights)` replaces a draft's criteria,
validated exactly as at creation. After `publish_tender` it fails with
`CriteriaLocked` — criteria are locked before a single bid arrives (spec §1.1).

### Jurisdictional policy (spec §8)

`set_policy` (governed `PolicyOrigin`) sets the deployment's
`ProcurementPolicy`. The default is fully permissive.

| Field | Rule | Error |
|---|---|---|
| `min_standstill` | A tender's `standstill_period` must be at least this | `StandstillTooShort` |
| `min_submission_period` | Blocks from publication to submission close — checked at creation against `publish_at`, and again at publication against the actual block, so a late publish cannot squeeze the market | `SubmissionPeriodTooShort` |
| `addendum_response_window` | After any addendum, bidders must have at least this many blocks before close; a late addendum must extend the close to restore it | `AddendumNeedsExtension` |
| `max_close_extension` | Addenda may push the close at most this far past the **published** close (so it cannot be ratcheted); `None` means no cap | `CloseExtensionTooLong` |

Each tender snapshots the policy at publication (`TenderRecord.policy`, with the
original close in `published_close_at`). Changing the policy never moves the
rules of a tender that is already live.

An extension shifts `opening_at` and `opening_end_at` by the same amount as the
close, so any gap published between close and opening, and the reveal window's
length, are preserved.

### Answers are final

`answer_question` is accepted during `QaWindow` and `Submission` only, and a
question can be answered once (`AlreadyAnswered`). Correct a published answer
with an addendum so the correction is itself on the record.

### Panel call-offs

`call_off` is an officer call, authorised against the officer of the tender that
established the panel (`PanelTender` maps panel id to that tender). A signed
account that is not that officer or entity gets `NotOfficer`.

### Award authority

`award` is gated on `AwardOrigin`, which spec §8 requires be a governed origin.
In the current runtime this is `EnsureRoot` as a stand-in; it must be repointed
at Module 16 (Multisig) or the deployment's governance origin before production.
Agency front ends should drive awards through the governance flow, not a signed
officer call.

## 5. Audit / probity tooling guide

Everything below is answerable from public data with no privileged access.

- **Were the rules fixed before bids arrived?** Take the `TenderPublished` block
  as the lock point; the criteria hash and weights read from `Tenders` are the
  published ones, and no extrinsic exists that could have changed them since.
  Every `AddendumPublished` is itself timestamped and public.
- **Did any bid arrive late?** Compare each `BidCommitted.block` against
  `TenderPublished.close_block`. The runtime rejects late commits, so a
  violation would be an event that cannot exist — which is the point.
- **Was anything readable before opening?** No. Only commitment hashes are
  stored before the reveal window.
- **Who evaluated, and did they declare?** `EvaluatorActivated` cannot precede
  `ConflictDeclared` for the same evaluator. Scores are attributed by the
  `Scores` key triple.
- **Was there disagreement?** Every `ScoreVarianceFlagged` event, with the
  criterion and the spread.
- **Does the ranking follow the locked weights?** Recompute: per criterion,
  average the scores across evaluators (integer division), multiply by
  `weight_percent`, sum. Compare against `Outcomes.ranking`. Voided and
  non-revealed bids are excluded from the ranking by construction.
- **Did delivery follow?** `ContractExecuted` carries the notarised contract
  hash and `DeliveryInstantiated` the Module 25 project reference per awardee.
- **Were the roles separated?** No account appears both in this tender's
  `EvaluatorSet` and its `BidCommitments`, and neither the officer nor the
  entity sits on the panel — the runtime rejects both (spec §1.2).
- **Could the officer have pocketed the bonds?** No. Forfeiture on non-reveal
  only applies if the tender actually reached `Opening`; if the officer never
  opened it, every bond is returned. And the officer cannot open so late that
  the reveal window falls below `MinRevealWindow`.

## 5a. Challenge standing and bounds

`lodge_challenge` is restricted to accounts that lodged a commitment on that
tender (`NotAParticipant` otherwise) and did not win it
(`AwardeeCannotChallenge`), and only while the award stands — the tender must
be `Awarded` or `Challenged`. A cancelled tender, or one already remitted to
re-evaluation, cannot be challenged, and ruling on a challenge never moves a
cancelled tender out of `Cancelled`. This matters because every open challenge
suspends `execute_award`: without the restriction any account could hold a
lawful award hostage indefinitely. Challenges are additionally capped at
`MaxChallenges` per tender, so the suspension window is bounded even from a
bidder with standing.

Both sides of a challenge are readable on chain, not hashed: `lodge_challenge`
takes `grounds: Vec<u8>` plus an optional `evidence_hash` for exhibits, and
`resolve_challenge` takes `resolution: Vec<u8>`. Neither may be empty, and
over-length input is rejected rather than truncated (`GroundsTooLong`,
`GroundsEmpty`, `ResolutionTooLong`, `ResolutionEmpty`). A portal therefore
renders a challenge log in full from chain state alone, with no content-store
lookup — see `offchain-content-store.md` §8.1 for why.

Questions are capped at `MaxQuestions` and evaluators at `MaxEvaluators` per
tender. Re-appointing an evaluator already on the panel refreshes their record
rather than consuming a slot — and, because re-appointment clears the conflict
declaration and deactivates them, it also decrements the activated tally that
`MinEvaluators` is checked against.

## 6. Integration seams (spec §7)

The pallet is complete and self-contained, but Modules 2/8/10/13/15/16/19/25 do
not exist in this runtime yet. Every point where the spec's integration map
touches TenderChain is a `Config` seam with a stand-in, so wiring a real module
is a change to the runtime `Config` impl only — no pallet logic, storage or
events change:

| Module | Config item | Trait | Currently | Should become |
|---|---|---|---|---|
| 8 Escrow | `Bonds` | `BondManager` (`lock` / `release` / `forfeit`) | `ReserveBonds<Balances>` — reserved on the bidder's own account, forfeits repatriated to the entity | Escrow lock / return / forfeit |
| 15 Identity + 10 Reputation | `Eligibility` | `EligibilityProvider` | `()` — everyone eligible | Credentials + reputation floor |
| 10 Reputation | `Reputation` | `ReputationSink` | `()` — facts dropped | Records `ContractWon`, `BondForfeited` (supplier) and `ContractAwarded`, `TenderCancelled` (entity) |
| 25 Work Task | `Delivery` | `DeliveryInstantiator` | `()` — no delivery project | Module 25 instantiation |
| 13 Email | `Notices` | `Notifier` | `()` — events only | System mail: public notice on publish; every participant on addendum, award, standstill closed, challenge lodged/resolved, contract executed, cancellation |
| 2 DNC | `Documents` | `DocumentAnchor` | `()` — every hash accepted | Rejects (`DocumentNotAnchored`) a notice, criteria, addendum, rationale, contract, cancellation reason or challenge evidence hash DNC does not hold |
| 16 Multisig | `AwardOrigin` | `EnsureOrigin` | `EnsureRoot` | Multisig / governance origin |
| Review board | `ChallengeResolverOrigin` | `EnsureOrigin` | `EnsureRoot` | Probity authority / arbiter |
| 19 Sentinel | — | — | — | Consumes this pallet's events; no seam needed |
| 12 FastLane | — | `EligibilityProvider` | — | Pre-consensus filter using the same check the pallet re-runs on chain |

`Notifier` and `ReputationSink` are infallible on purpose: a mail or reputation
outage must never block a tender's lifecycle, and the pallet's events remain the
authoritative record. When a real implementation is wired, its cost must be
added to the benchmarks of the calls that invoke it.

Until eligibility is wired, `NotEligible` can never fire and eligibility policy
is recorded but not enforced. Portals should not present eligibility as
enforced until Module 15 lands.
