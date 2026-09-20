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

pub type QuestionId = u32;
pub type ChallengeId = u32;

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
	pub criterion_id: u32,
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
#[scale_info(skip_type_params(MaxWeights, MaxCredentials))]
pub struct TenderRecord<AccountId, BlockNumber, Balance, MaxWeights, MaxCredentials>
where
	MaxWeights: Get<u32>,
	MaxCredentials: Get<u32>,
{
	/// Procuring entity identity (Module 15).
	pub entity: AccountId,
	/// Procurement officer administering the lifecycle.
	pub officer: AccountId,
	pub kind: TenderKind,
	pub bid_mode: BidMode,
	/// Tender notice + specification bundle, DNC-anchored.
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
	pub item_id: u32,
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
	pub criterion_id: u32,
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
#[derive(Encode, Decode, Clone, PartialEq, Eq, RuntimeDebug, TypeInfo, MaxEncodedLen)]
pub struct ChallengeRecord<AccountId, BlockNumber> {
	pub challenger: AccountId,
	pub grounds_hash: Hash256,
	pub lodged_at: BlockNumber,
	pub state: ChallengeState,
	pub resolution_hash: Option<Hash256>,
	pub resolved_at: Option<BlockNumber>,
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
pub trait DeliveryInstantiator<AccountId, TenderId> {
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
impl<AccountId, TenderId> DeliveryInstantiator<AccountId, TenderId> for () {
	fn instantiate(
		_id: TenderId,
		_awardee: &AccountId,
		_hash: Hash256,
	) -> Result<Option<Hash256>, DispatchError> {
		Ok(None)
	}
}
