//! Data model for the TenderChain pallet (spec §3).
//!
//! Everything confidential lives off-chain and is referenced here by hash only —
//! the pallet stores structure, proof and public metadata, never readable content
//! (spec §1.2, §8).

use codec::{Decode, DecodeWithMemTracking, Encode, MaxEncodedLen};
use frame_support::pallet_prelude::*;
use scale_info::TypeInfo;

/// A 32-byte content commitment (blake2-256 of a DNC-anchored document, spec §1.2).
pub type Hash256 = [u8; 32];

// ---------------------------------------------------------------------
// Identifiers
//
// Every identifier in this pallet is a 32-byte hash, never a sequential
// counter. A counter leaks how many tenders an entity has run, how many
// questions a tender drew and how many challenges it faced, and it invites
// off-by-one reuse across forks and restarts. The chain-minted ids
// (tender, question, challenge, panel) are derived by `Pallet::derive_id`
// from a domain tag plus inputs that make them unique; criterion and price
// line ids are supplied by the caller as hashes of their own definitions.
// ---------------------------------------------------------------------

/// A tender, minted by `create_tender`.
pub type TenderId = Hash256;
/// A standing-offer panel, derived from the tender that established it.
pub type PanelId = Hash256;
/// A question, minted by `ask_question`.
pub type QuestionId = Hash256;
/// A challenge, minted by `lodge_challenge`.
pub type ChallengeId = Hash256;
/// An evaluation criterion — the hash of its definition in the criteria
/// document committed to by `criteria_hash`.
pub type CriterionId = Hash256;
/// A price-schedule line item — the hash of its definition in the bid.
pub type ItemId = Hash256;
/// A call-off order against a standing-offer panel, minted by `call_off`.
pub type CallOffId = Hash256;

/// Jurisdictional procurement rules, configured per deployment (spec §8:
/// "thresholds, mandatory standstill durations, publication requirements
/// expressed as workspace policy objects").
///
/// Set by the governed `PolicyOrigin` through `set_policy`. Each tender takes a
/// snapshot at publication, so a later policy change never moves the rules of
/// a tender that is already live (spec §5.1: "the rules never moved"). The
/// default is fully permissive, which is the behaviour before policy existed.
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, Default, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct ProcurementPolicy<BlockNumber> {
	/// Mandatory minimum standstill (challenge window) after award.
	pub min_standstill: BlockNumber,
	/// Minimum blocks between publication and submission close — the time the
	/// market gets to respond to a notice.
	pub min_submission_period: BlockNumber,
	/// Bidders must have at least this many blocks between an addendum and the
	/// submission close. An addendum issued later than that must extend the
	/// close far enough to restore the window (the spec's "governed rule" for
	/// addenda, §2.3).
	pub addendum_response_window: BlockNumber,
	/// Cap on how far addenda may push the close beyond the block published
	/// with the notice. `None` means no cap.
	pub max_close_extension: Option<BlockNumber>,
}

/// Tender procurement types (spec §2.2).
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub enum TenderKind {
	/// Request for Quotation — low value, optionally open-bid, price-weighted.
	Rfq,
	/// Request for Tender — full sealed-bid with weighted criteria.
	Rft,
	/// Expression of Interest — stage one of a multi-stage procurement.
	Eoi,
	/// Panel / Standing Offer — multi-award supplier pool with call-offs.
	Panel,
	/// Lightweight work-package tender; award instantiates Module 25 tasks.
	JobTask,
}

/// Submission mechanism (spec §1.2: commit-reveal default, open-bid for low-value RFQ).
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub enum BidMode {
	/// Commit-reveal. Nothing readable exists on chain before opening.
	Sealed,
	/// Bid content hash published at commit time. Only valid for `Rfq`.
	Open,
}

/// Lifecycle states (spec §2.3).
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub enum TenderState {
	Draft,
	/// Published; the Q&A window is open. (Spec §2.3 lists "Published" and
	/// "Q&A Window" separately, but publication opens Q&A in the same call, so
	/// they are one state rather than a state that is never observable.)
	QaWindow,
	Submission,
	Closed,
	Opening,
	Evaluation,
	Awarded,
	/// Terminal state of an EOI: its shortlist is published and can now
	/// credential bidders into a follow-on RFT (spec §2.2).
	Shortlisted,
	Challenged,
	Contracted,
	Cancelled,
}

/// A scheduled lifecycle transition processed by `on_initialize` (spec §8: "no
/// officer-controlled clock").
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub enum Gate {
	/// Q&A window ends; tender moves to `Submission`.
	QuestionsClose,
	/// Submissions close; tender moves to `Closed`. No further commits.
	SubmissionClose,
	/// Reveal window ends; tender moves to `Evaluation`.
	OpeningEnd,
	/// Standstill (challenge) window ends; `execute_award` becomes callable.
	StandstillEnd,
}

/// Block-number gates locked at publication (spec §2.3, §5.1).
///
/// `standstill_end` is deliberately absent: the standstill window opens at *award*,
/// which is not a knowable block at creation time. Its duration is carried
/// separately as `TenderRecord::standstill_period` and the concrete end block is
/// computed and recorded in the `OutcomeRecord` when the award is made.
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct TenderGates<BlockNumber> {
	pub publish_at: BlockNumber,
	pub questions_close_at: BlockNumber,
	pub submission_close_at: BlockNumber,
	pub opening_at: BlockNumber,
	pub opening_end_at: BlockNumber,
}

/// One evaluation criterion and its weight (percent; the set must sum to 100).
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct CriterionWeight {
	pub criterion_id: CriterionId,
	pub weight_percent: u8,
}

/// Bid bond terms (spec §4.1, §8). Bonds are reserved on the bidder's account.
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct BondTerms<Balance> {
	pub amount: Balance,
	/// Forfeit the bond if a sealed bid is committed but never revealed.
	pub forfeit_on_non_reveal: bool,
	/// Forfeit the bond if the bidder withdraws before close.
	pub forfeit_on_withdrawal: bool,
}

/// Eligibility policy checked at `commit_bid` (spec §2.1, §6.2).
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
#[scale_info(skip_type_params(MaxCredentials))]
pub struct EligibilityPolicy<MaxCredentials: Get<u32>> {
	/// Credential type hashes the bidder must hold (licence class, insurance, tax
	/// clearance, jurisdiction — resolved by Module 15).
	pub required_credentials: BoundedVec<Hash256, MaxCredentials>,
	/// Minimum Module 10 reputation score.
	pub min_reputation: u32,
}

/// The main tender record (spec §3, `Tenders`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
#[scale_info(skip_type_params(MaxWeights, MaxCredentials, MaxTitleLen, MaxSummaryLen))]
pub struct TenderRecord<AccountId, BlockNumber, Balance, MaxWeights, MaxCredentials, MaxTitleLen, MaxSummaryLen>
where
	MaxWeights: Get<u32>,
	MaxCredentials: Get<u32>,
	MaxTitleLen: Get<u32>,
	MaxSummaryLen: Get<u32>,
{
	/// Procuring entity identity (Module 15).
	pub entity: AccountId,
	/// Procurement officer administering the lifecycle.
	pub officer: AccountId,
	pub kind: TenderKind,
	pub bid_mode: BidMode,
	/// Tender name, readable on the public record.
	///
	/// Spec §1.2 confines hashing to *confidential* content. A tender notice is
	/// published in order to be read, so hashing its name bought nothing: it
	/// left an auditor reading raw chain state with a bare commitment, and made
	/// the readable text depend on whoever holds the off-chain store. Keeping it
	/// on chain makes the subject matter of a procurement permanent public
	/// record.
	pub title: BoundedVec<u8, MaxTitleLen>,
	/// Short public description, same reasoning as `title`. The full
	/// specification bundle stays off-chain under `notice_hash`.
	pub summary: BoundedVec<u8, MaxSummaryLen>,
	/// Full tender notice + specification bundle, DNC-anchored. Still a hash:
	/// drawings, terms and schedules are far too large for chain storage.
	pub notice_hash: Hash256,
	/// Hash commitment over the evaluation criteria. Locked at publication.
	pub criteria_hash: Hash256,
	pub weights: BoundedVec<CriterionWeight, MaxWeights>,
	pub gates: TenderGates<BlockNumber>,
	pub standstill_period: BlockNumber,
	pub eligibility: EligibilityPolicy<MaxCredentials>,
	pub bond: BondTerms<Balance>,
	/// When set, question authorship is stored as a hash commitment rather than
	/// an account, so smaller suppliers can ask without exposing interest
	/// (spec §5.3).
	pub blind_questions: bool,
	pub state: TenderState,
	/// Block the draft was created at. Ids are hashes and carry no order, so
	/// this is what lists and audits sort by.
	pub created_at: BlockNumber,
	/// The procurement policy in force when the tender was published, frozen
	/// with the rest of the rules. Default (permissive) while in `Draft`.
	pub policy: ProcurementPolicy<BlockNumber>,
	/// Submission close as published, before any addendum extended it — the
	/// base `max_close_extension` is measured from.
	pub published_close_at: Option<BlockNumber>,
	/// Block at which `publish_tender` locked the rules. `None` while in `Draft`.
	pub published_at: Option<BlockNumber>,
}

/// A published clarification or change (spec §3, `Addenda`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct AddendumRecord<BlockNumber> {
	pub content_hash: Hash256,
	pub published_at: BlockNumber,
	/// New submission close block, if this addendum extended it.
	pub extended_close_to: Option<BlockNumber>,
}

/// Who asked a question (spec §5.3).
///
/// Chain state is world-readable, so storing an `AccountId` and merely omitting
/// it from the event is not blinding — anyone can read the storage map. Real
/// blinding therefore means never writing the identity at all: `Blinded` holds
/// `blake2_256(asker ‖ salt)`, which the asker (and anyone they hand the salt
/// to, such as a probity observer) can verify but nobody else can invert.
#[derive(Encode, Decode, DecodeWithMemTracking, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub enum QuestionAuthor<AccountId> {
	/// Authorship is public, as on most government tenders.
	Open(AccountId),
	/// `blake2_256(asker ‖ salt)`. Nothing on chain reveals the account.
	Blinded(Hash256),
}

/// A bidder question and its official answer (spec §3, `Questions`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct QaRecord<AccountId, BlockNumber> {
	/// Author, public or blinded per the tender's `blind_questions` policy.
	pub author: QuestionAuthor<AccountId>,
	pub question_hash: Hash256,
	pub asked_at: BlockNumber,
	pub answer_hash: Option<Hash256>,
	pub answered_at: Option<BlockNumber>,
}

/// A sealed bid commitment (spec §3, `BidCommitments`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct CommitmentRecord<Balance, BlockNumber> {
	pub commitment_hash: Hash256,
	pub committed_at: BlockNumber,
	/// Amount reserved as the bid bond.
	pub bond_reserved: Balance,
}

/// One line of a declared price schedule.
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct PriceLine<Balance> {
	pub item_id: ItemId,
	pub amount: Balance,
}

/// A revealed bid, validated against its commitment (spec §3, `Reveals`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
#[scale_info(skip_type_params(MaxPriceLines))]
pub struct RevealRecord<Balance, BlockNumber, MaxPriceLines: Get<u32>> {
	pub documents_hash: Hash256,
	pub price_schedule: BoundedVec<PriceLine<Balance>, MaxPriceLines>,
	pub revealed_at: BlockNumber,
	/// False when the reveal did not hash-match the commitment. The bid is voided
	/// but the mismatch stays on the public record (spec §4.1, §6.1 step 5).
	pub valid: bool,
}

/// An appointed evaluator (spec §3, `EvaluatorSet`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct EvaluatorRecord<BlockNumber> {
	pub appointed_at: BlockNumber,
	/// Module 15 credential reference.
	pub credential_ref: Hash256,
	/// Conflict-of-interest declaration. Scoring rights stay inactive until lodged.
	pub conflict_declaration: Option<Hash256>,
	pub active: bool,
}

/// A single criterion score within a scoresheet.
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub struct CriterionScore {
	pub criterion_id: CriterionId,
	pub score: u8,
}

/// An individually attributed scoresheet (spec §3, `Scores`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
#[scale_info(skip_type_params(MaxWeights))]
pub struct ScoreSheet<BlockNumber, MaxWeights: Get<u32>> {
	pub scores: BoundedVec<CriterionScore, MaxWeights>,
	pub comment_hash: Hash256,
	pub submitted_at: BlockNumber,
}

/// The award outcome (spec §3, `Outcomes`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
#[scale_info(skip_type_params(MaxBidders))]
pub struct OutcomeRecord<AccountId, BlockNumber, MaxBidders: Get<u32>> {
	/// Every ranked bidder and its weighted score, best first.
	pub ranking: BoundedVec<(AccountId, u32), MaxBidders>,
	pub awardees: BoundedVec<AccountId, MaxBidders>,
	pub rationale_hash: Hash256,
	pub awarded_at: BlockNumber,
	/// Challenges lodged before this block suspend execution (spec §2.3).
	pub standstill_end: BlockNumber,
	/// Contract notarisation reference, set by `execute_award`.
	pub contract_hash: Option<Hash256>,
}

/// Resolution state of a challenge.
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub enum ChallengeState {
	Open,
	/// Dismissed; execution may proceed.
	Dismissed,
	/// Upheld; the tender is remitted for re-evaluation or cancellation.
	Upheld,
}

/// A lodged procurement challenge (spec §3, `Challenges`; §6.3).
///
/// Grounds and resolution are held on chain in readable form, not as hashes,
/// for the same reason as `TenderRecord::title` — see `docs/offchain-content-store.md`
/// §8. A challenge is an allegation that a public award was made improperly, and
/// its resolution is the ruling on that allegation. Both are published in order
/// to be read: hashing them bought no confidentiality (the challenge suspends
/// execution in public either way) and cost the property that matters, which is
/// that an auditor reading raw chain state years later can see what was alleged
/// and why it was upheld or dismissed. Under the old hash-only form that text
/// survived only in whichever browser happened to hold the preimage.
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
#[scale_info(skip_type_params(MaxGroundsLen, MaxResolutionLen))]
pub struct ChallengeRecord<AccountId, BlockNumber, MaxGroundsLen, MaxResolutionLen>
where
	MaxGroundsLen: Get<u32>,
	MaxResolutionLen: Get<u32>,
{
	pub challenger: AccountId,
	/// The written grounds of the challenge, readable on the public record.
	pub grounds: BoundedVec<u8, MaxGroundsLen>,
	/// Supporting evidence too large for chain storage, DNC-anchored. `None` when
	/// the challenger lodged grounds alone, which is the common case.
	pub evidence_hash: Option<Hash256>,
	pub lodged_at: BlockNumber,
	pub state: ChallengeState,
	/// The written ruling. `None` while the challenge is still `Open`.
	pub resolution: Option<BoundedVec<u8, MaxResolutionLen>>,
	pub resolved_at: Option<BlockNumber>,
}

/// A call-off order placed against a standing offer (spec §2.2, §4.1).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct CallOffRecord<AccountId, BlockNumber> {
	pub supplier: AccountId,
	/// The order document, DNC-anchored.
	pub order_hash: Hash256,
	/// The officer who placed it.
	pub placed_by: AccountId,
	pub placed_at: BlockNumber,
}

/// Standing-offer panel membership (spec §3, `PanelPool`).
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct PanelMembership<BlockNumber> {
	pub admitted_at: BlockNumber,
	/// Call-off terms reference.
	pub terms_hash: Hash256,
}

// ---------------------------------------------------------------------
// Integration seams (spec §7)
//
// None of Modules 2/8/10/13/15/16/19/25 exist in this runtime yet. Rather than
// hard-code assumptions about them, each integration point is a trait with a
// permissive no-op `()` implementation. Wiring a real module later is a change
// to the runtime's `Config` impl, not to any logic in this pallet.
// ---------------------------------------------------------------------

/// Bidder eligibility, backed by Module 15 (Identity) credentials and Module 10
/// (Reputation) scores. Enforced pre-consensus by Module 12 (FastLane) in
/// production; re-checked here so the guarantee holds on chain regardless.
pub trait EligibilityProvider<AccountId> {
	fn is_eligible(who: &AccountId, required_credentials: &[Hash256], min_reputation: u32) -> bool;
}

/// Permissive default: every account is eligible. Replace with the Module 15
/// adapter once Identity lands.
impl<AccountId> EligibilityProvider<AccountId> for () {
	fn is_eligible(_who: &AccountId, _required: &[Hash256], _min_reputation: u32) -> bool {
		true
	}
}

/// Award-to-delivery handoff into Module 25 (Work Task) — "the tender's afterlife"
/// (spec §7). Called by `execute_award` once the standstill has passed.
pub trait DeliveryInstantiator<AccountId> {
	/// Returns the delivery project reference Module 25 created, if any. Spec
	/// §4.2 wants this surfaced on the execution event as `delivery_project`.
	fn instantiate(
		tender_id: TenderId,
		awardee: &AccountId,
		contract_hash: Hash256,
	) -> Result<Option<Hash256>, DispatchError>;
}

/// No-op default: the award is recorded on chain but instantiates no delivery
/// project. Replace with the Module 25 adapter once Work Task lands.
impl<AccountId> DeliveryInstantiator<AccountId> for () {
	fn instantiate(
		_id: TenderId,
		_awardee: &AccountId,
		_hash: Hash256,
	) -> Result<Option<Hash256>, DispatchError> {
		Ok(None)
	}
}

/// Bid-bond custody, backed by Module 8 (Escrow) — spec §1.2 "bid bonds via
/// Escrow with automatic return on non-award", §7.
///
/// The pallet decides *when* a bond is locked, returned or forfeited; this trait
/// decides *where the money sits* while that happens. Swapping reserves for
/// Escrow is then a runtime config change, not a rewrite of the bond logic.
pub trait BondManager<AccountId, Balance> {
	/// Take custody of `amount` from `who` for the life of the bid.
	fn lock(who: &AccountId, amount: Balance) -> DispatchResult;
	/// Hand a locked bond back to `who` in full.
	fn release(who: &AccountId, amount: Balance);
	/// Move a locked bond from `who` to `beneficiary`. Returns the amount that
	/// actually moved; anything that could not move is released back to `who`,
	/// so a bond is never left stuck in custody.
	fn forfeit(who: &AccountId, beneficiary: &AccountId, amount: Balance) -> Balance;
}

/// Stand-in until Escrow lands: bonds are *reserved* on the bidder's own
/// account, so an un-forfeited bond never leaves the bidder's custody.
pub struct ReserveBonds<Currency>(core::marker::PhantomData<Currency>);

impl<AccountId, C> BondManager<AccountId, C::Balance> for ReserveBonds<C>
where
	C: frame_support::traits::ReservableCurrency<AccountId>,
{
	fn lock(who: &AccountId, amount: C::Balance) -> DispatchResult {
		C::reserve(who, amount)
	}

	fn release(who: &AccountId, amount: C::Balance) {
		C::unreserve(who, amount);
	}

	fn forfeit(who: &AccountId, beneficiary: &AccountId, amount: C::Balance) -> C::Balance {
		use sp_runtime::traits::{Saturating, Zero};
		// Moved straight out of the reserve, never via the free balance: an
		// unreserve-then-transfer could fail halfway and hand the bond back
		// while the pallet recorded it as forfeited.
		let unmoved = C::repatriate_reserved(
			who,
			beneficiary,
			amount,
			frame_support::traits::BalanceStatus::Free,
		)
		.unwrap_or(amount);
		if !unmoved.is_zero() {
			C::unreserve(who, unmoved);
		}
		amount.saturating_sub(unmoved)
	}
}

/// A lifecycle notice sent to bidders as evidential system mail (Module 13,
/// spec §4.1 `award`, §7).
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub enum TenderNotice {
	/// Public notice: the tender is open. Sent with no recipients — anyone may
	/// bid, so there is no list yet.
	Published,
	AddendumPublished,
	/// Outcome and ranking published; standstill has opened.
	Awarded,
	StandstillClosed,
	ChallengeLodged,
	ChallengeResolved,
	ContractExecuted,
	Cancelled,
	/// A call-off order was placed with the recipient.
	CallOffPlaced,
}

/// Bidder notification, backed by Module 13 (Email).
///
/// Infallible by design: a mail outage must never block a tender's lifecycle,
/// and the chain's own events remain the authoritative record either way.
pub trait Notifier<AccountId> {
	/// `recipients` is every current participant; empty means a public notice.
	fn notify(tender_id: TenderId, notice: TenderNotice, recipients: &[AccountId]);
}

/// No-op until Module 13 lands. Events carry the same information.
impl<AccountId> Notifier<AccountId> for () {
	fn notify(_tender_id: TenderId, _notice: TenderNotice, _recipients: &[AccountId]) {}
}

/// Document anchoring, backed by Module 2 (DNC) — spec §1.2 "documents by
/// reference", §7.
///
/// The pallet stores document hashes; this checks that a hash names a document
/// DNC actually holds, so an officer cannot publish a notice, addendum,
/// rationale, contract or cancellation reason that resolves to nothing.
pub trait DocumentAnchor {
	fn is_anchored(hash: &Hash256) -> bool;
}

/// Accept-all until Module 2 lands.
impl DocumentAnchor for () {
	fn is_anchored(_hash: &Hash256) -> bool {
		true
	}
}

/// A procurement fact that bears on reputation (Module 10, spec §6.2 step 3,
/// §7: "supplier delivery history … entity payment conduct equally recorded").
#[derive(
	Encode, Decode, DecodeWithMemTracking, Clone, Copy, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen,
)]
pub enum ReputationFact {
	/// Supplier: won a contract that survived standstill and was executed.
	ContractWon,
	/// Supplier: bond forfeited — non-reveal, mismatched reveal, or withdrawal
	/// under forfeiting terms.
	BondForfeited,
	/// Entity: executed a contract it tendered.
	ContractAwarded,
	/// Entity: cancelled a tender after publishing it.
	TenderCancelled,
}

/// Reputation feedback into Module 10. The pallet reports facts; how they are
/// weighted is Reputation's business.
pub trait ReputationSink<AccountId> {
	fn record(tender_id: TenderId, who: &AccountId, fact: ReputationFact);
}

/// No-op until Module 10 lands.
impl<AccountId> ReputationSink<AccountId> for () {
	fn record(_tender_id: TenderId, _who: &AccountId, _fact: ReputationFact) {}
}
