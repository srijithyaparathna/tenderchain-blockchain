#![cfg_attr(not(feature = "std"), no_std)]

//! # TenderChain — Public Tender & Procurement Pallet (Module 26)
//!
//! Puts the whole tendering lifecycle on chain: entities publish tenders, suppliers
//! submit sealed bids, evaluation runs against criteria locked before a single bid
//! arrives, awards are published and challengeable, and delivery hands off to
//! Module 25.
//!
//! ## What makes the integrity structural rather than procedural
//!
//! - **Criteria locked before bids.** `publish_tender` stamps `published_at` and
//!   from that block the criteria hash, weights and gates are immutable — there is
//!   no extrinsic that mutates them. Retrofitting criteria to a preferred bidder is
//!   not forbidden by policy, it is absent from the API.
//! - **Sealed until opening.** In `BidMode::Sealed` the chain holds only
//!   `blake2_256(bidder ‖ documents ‖ prices ‖ salt)`. There is nothing readable to
//!   leak before the reveal window. The bidder is bound into the preimage so one
//!   bidder cannot replay another's commitment as their own.
//! - **The chain closes the tender, not an official.** Every deadline is a
//!   `DeadlineWheel` entry processed in `on_initialize`.
//! - **Every id is a hash.** Tenders, questions, challenges and panels are
//!   identified by domain-separated blake2-256 hashes minted on chain
//!   ([`Pallet::derive_id`]); criteria and price lines by hashes of their own
//!   definitions. No sequential counter is exposed, so ids leak nothing about
//!   volume and cannot collide across entities.
//! - **Attributed evaluation.** Scores are keyed by evaluator and require a lodged
//!   conflict declaration; disagreement beyond a configured threshold auto-emits
//!   `ScoreVarianceFlagged` for probity review.
//!
//! ## Integration status
//!
//! Modules 2/8/10/13/15/16/19/25 do not exist in this runtime yet. Every point
//! where the spec's integration map (§7) touches this pallet is a `Config` seam,
//! so wiring a real module is a runtime config change, not a pallet rewrite:
//!
//! | Module | Seam | Stand-in |
//! |---|---|---|
//! | 8 Escrow | [`BondManager`] (`Bonds`) | [`ReserveBonds`] — reserves on the bidder's account |
//! | 15 Identity / 10 Reputation | [`EligibilityProvider`] (`Eligibility`) | `()` — everyone eligible |
//! | 10 Reputation | [`ReputationSink`] (`Reputation`) | `()` — facts dropped |
//! | 25 Work Task | [`DeliveryInstantiator`] (`Delivery`) | `()` — no project |
//! | 13 Email | [`Notifier`] (`Notices`) | `()` — events only |
//! | 2 DNC | [`DocumentAnchor`] (`Documents`) | `()` — every hash accepted |
//! | 16 Multisig | `AwardOrigin`, `ChallengeResolverOrigin` | root |
//!
//! Module 19 (Sentinel) needs no seam — it consumes this pallet's events — and
//! Module 12 (FastLane) enforces eligibility pre-consensus through the same
//! `EligibilityProvider` this pallet re-checks on chain. Spec §8 requires the
//! award origin never be a single key, which this pallet cannot enforce on the
//! runtime's behalf.

pub use pallet::*;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;
pub mod types;
pub mod weights;

pub use types::*;
pub use weights::WeightInfo;

extern crate alloc;

use alloc::vec::Vec;
use frame_support::pallet_prelude::*;
use frame_support::traits::Currency;
use sp_runtime::traits::{One, Saturating, Zero};

/// Fixed salt for open-bid commitments. Open bids are published at submission
/// time, so there is no secret to protect and no salt for a bidder to lose.
const OPEN_BID_SALT: Hash256 = [0u8; 32];

/// Blocks searched ahead for room when an over-budget gate is deferred.
const GATE_LOOKAHEAD: u32 = 16;

/// Domain tags for id derivation. Distinct tags keep a tender id from ever
/// equalling a question, challenge or panel id built from the same inputs.
const TENDER_ID_TAG: &[u8] = b"tenderchain/tender";
const QUESTION_ID_TAG: &[u8] = b"tenderchain/question";
const CHALLENGE_ID_TAG: &[u8] = b"tenderchain/challenge";
const PANEL_ID_TAG: &[u8] = b"tenderchain/panel";
const CALL_OFF_ID_TAG: &[u8] = b"tenderchain/calloff";

#[frame_support::pallet]
pub mod pallet {
	use super::*;
	use frame_system::pallet_prelude::*;

	#[pallet::pallet]
	pub struct Pallet<T>(_);

	#[pallet::config]
	pub trait Config: frame_system::Config {
		/// The overarching event type.
		type RuntimeEvent: From<Event<Self>> + IsType<<Self as frame_system::Config>::RuntimeEvent>;

		/// The currency bonds are denominated in.
		type Currency: Currency<Self::AccountId>;

		/// Bid-bond custody (Module 8, Escrow). [`ReserveBonds`] stands in until
		/// Escrow lands.
		type Bonds: BondManager<Self::AccountId, BalanceOf<Self>>;

		/// Origin permitted to make an award. Spec §8: this MUST be a governed
		/// origin (Multisig/governance), never a single key. The pallet cannot
		/// verify that property — it is the runtime's responsibility.
		type AwardOrigin: EnsureOrigin<Self::RuntimeOrigin>;

		/// Origin permitted to resolve a lodged challenge (internal review board,
		/// probity authority or external arbiter, per deployment).
		type ChallengeResolverOrigin: EnsureOrigin<Self::RuntimeOrigin>;

		/// Origin permitted to set the deployment's procurement policy (spec §8).
		/// Governed, like the award authority.
		type PolicyOrigin: EnsureOrigin<Self::RuntimeOrigin>;

		/// Bidder credential / reputation checks (Modules 15 + 10).
		type Eligibility: EligibilityProvider<Self::AccountId>;

		/// Award-to-delivery handoff (Module 25).
		type Delivery: DeliveryInstantiator<Self::AccountId>;

		/// Evidential system mail to bidders (Module 13).
		type Notices: Notifier<Self::AccountId>;

		/// Document anchoring check (Module 2, DNC).
		type Documents: DocumentAnchor;

		/// Reputation feedback (Module 10).
		type Reputation: ReputationSink<Self::AccountId>;

		/// Max weighted criteria per tender.
		#[pallet::constant]
		type MaxWeights: Get<u32>;
		/// Max eligibility credentials demanded by one tender.
		#[pallet::constant]
		type MaxCredentials: Get<u32>;
		/// Max byte length of a tender's readable title.
		#[pallet::constant]
		type MaxTitleLen: Get<u32>;
		/// Max byte length of a tender's readable summary.
		#[pallet::constant]
		type MaxSummaryLen: Get<u32>;
		/// Max byte length of a challenge's readable grounds.
		#[pallet::constant]
		type MaxGroundsLen: Get<u32>;
		/// Max byte length of a challenge resolution's readable reasoning.
		#[pallet::constant]
		type MaxResolutionLen: Get<u32>;
		/// Max addenda per tender.
		#[pallet::constant]
		type MaxAddenda: Get<u32>;
		/// Max bidders per tender. Bounds the award-time ranking loop.
		#[pallet::constant]
		type MaxBidders: Get<u32>;
		/// Max evaluators per tender.
		#[pallet::constant]
		type MaxEvaluators: Get<u32>;
		/// Max questions per tender. The Q&A window is open to any account, so
		/// this is what stops question spam growing state without bound.
		#[pallet::constant]
		type MaxQuestions: Get<u32>;
		/// Max challenges per tender. Each open challenge suspends execution, so
		/// this also bounds how long execution can be held up.
		#[pallet::constant]
		type MaxChallenges: Get<u32>;
		/// Max price-schedule lines in a revealed bid.
		#[pallet::constant]
		type MaxPriceLines: Get<u32>;
		/// Max call-off orders per standing-offer panel.
		#[pallet::constant]
		type MaxCallOffs: Get<u32>;
		/// Max deadlines that may fall on a single block.
		#[pallet::constant]
		type MaxDeadlinesPerBlock: Get<u32>;
		/// Max gate transitions processed per block; the remainder defer to the next
		/// block so a deadline pile-up cannot stall the chain (spec §8).
		#[pallet::constant]
		type MaxTransitionsPerBlock: Get<u32>;
		/// Minimum blocks that must remain between `open_tender` and
		/// `opening_end_at`. Stops an officer opening at the last block and
		/// leaving bidders no realistic chance to reveal.
		#[pallet::constant]
		type MinRevealWindow: Get<BlockNumberFor<Self>>;
		/// Minimum activated evaluators before an award may be made.
		#[pallet::constant]
		type MinEvaluators: Get<u32>;
		/// Highest permissible per-criterion score.
		#[pallet::constant]
		type MaxScore: Get<u8>;
		/// Score spread between two evaluators on one criterion that triggers
		/// `ScoreVarianceFlagged`.
		#[pallet::constant]
		type ScoreVarianceThreshold: Get<u8>;

		/// Weights for this pallet's dispatchables.
		type WeightInfo: WeightInfo;
	}

	pub type BalanceOf<T> = <<T as Config>::Currency as Currency<
		<T as frame_system::Config>::AccountId,
	>>::Balance;

	pub type TenderRecordOf<T> = TenderRecord<
		<T as frame_system::Config>::AccountId,
		BlockNumberFor<T>,
		BalanceOf<T>,
		<T as Config>::MaxWeights,
		<T as Config>::MaxCredentials,
		<T as Config>::MaxTitleLen,
		<T as Config>::MaxSummaryLen,
	>;

	pub type ChallengeRecordOf<T> = ChallengeRecord<
		<T as frame_system::Config>::AccountId,
		BlockNumberFor<T>,
		<T as Config>::MaxGroundsLen,
		<T as Config>::MaxResolutionLen,
	>;

	pub type OutcomeRecordOf<T> = OutcomeRecord<
		<T as frame_system::Config>::AccountId,
		BlockNumberFor<T>,
		<T as Config>::MaxBidders,
	>;

	// -------------------------------------------------------------
	// Storage (spec §3)
	// -------------------------------------------------------------

	/// Monotonic input to tender id derivation. Never used as an id itself —
	/// it only guarantees two tenders minted by the same officer in the same
	/// block still hash apart.
	#[pallet::storage]
	pub type TenderNonce<T: Config> = StorageValue<_, u64, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn tenders)]
	pub type Tenders<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, TenderRecordOf<T>, OptionQuery>;

	#[pallet::storage]
	#[pallet::getter(fn addenda)]
	pub type Addenda<T: Config> = StorageMap<
		_,
		Blake2_128Concat,
		TenderId,
		BoundedVec<AddendumRecord<BlockNumberFor<T>>, T::MaxAddenda>,
		ValueQuery,
	>;

	/// Questions asked per tender. Bounds `MaxQuestions` and feeds question id
	/// derivation; ids themselves are hashes.
	#[pallet::storage]
	pub type QuestionCount<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, u32, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn questions)]
	pub type Questions<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		TenderId,
		Blake2_128Concat,
		QuestionId,
		QaRecord<T::AccountId, BlockNumberFor<T>>,
		OptionQuery,
	>;

	#[pallet::storage]
	#[pallet::getter(fn bid_commitments)]
	pub type BidCommitments<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		TenderId,
		Blake2_128Concat,
		T::AccountId,
		CommitmentRecord<BalanceOf<T>, BlockNumberFor<T>>,
		OptionQuery,
	>;

	/// Every account holding a live commitment, so award-time ranking and
	/// bond settlement iterate a bounded set rather than an unbounded map prefix.
	#[pallet::storage]
	#[pallet::getter(fn participants)]
	pub type Participants<T: Config> = StorageMap<
		_,
		Blake2_128Concat,
		TenderId,
		BoundedVec<T::AccountId, T::MaxBidders>,
		ValueQuery,
	>;

	#[pallet::storage]
	#[pallet::getter(fn reveals)]
	pub type Reveals<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		TenderId,
		Blake2_128Concat,
		T::AccountId,
		RevealRecord<BalanceOf<T>, BlockNumberFor<T>, T::MaxPriceLines>,
		OptionQuery,
	>;

	#[pallet::storage]
	#[pallet::getter(fn evaluator_set)]
	pub type EvaluatorSet<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		TenderId,
		Blake2_128Concat,
		T::AccountId,
		EvaluatorRecord<BlockNumberFor<T>>,
		OptionQuery,
	>;

	/// Count of *activated* evaluators, so `award` can check `MinEvaluators`
	/// without iterating.
	#[pallet::storage]
	pub type ActiveEvaluatorCount<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, u32, ValueQuery>;

	/// Count of *appointed* evaluators, so `appoint_evaluator` can enforce
	/// `MaxEvaluators` without iterating the double map prefix.
	#[pallet::storage]
	pub type AppointedEvaluatorCount<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, u32, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn scores)]
	pub type Scores<T: Config> = StorageNMap<
		_,
		(
			NMapKey<Blake2_128Concat, TenderId>,
			NMapKey<Blake2_128Concat, T::AccountId>, // bidder
			NMapKey<Blake2_128Concat, T::AccountId>, // evaluator
		),
		ScoreSheet<BlockNumberFor<T>, T::MaxWeights>,
		OptionQuery,
	>;

	#[pallet::storage]
	#[pallet::getter(fn outcomes)]
	pub type Outcomes<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, OutcomeRecordOf<T>, OptionQuery>;

	/// Challenges lodged per tender. Bounds `MaxChallenges` and feeds challenge
	/// id derivation; ids themselves are hashes.
	#[pallet::storage]
	pub type ChallengeCount<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, u32, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn challenges)]
	pub type Challenges<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		TenderId,
		Blake2_128Concat,
		ChallengeId,
		ChallengeRecordOf<T>,
		OptionQuery,
	>;

	/// Number of challenges still `Open`; execution stays suspended while non-zero.
	#[pallet::storage]
	pub type OpenChallengeCount<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, u32, ValueQuery>;

	#[pallet::storage]
	#[pallet::getter(fn panel_pool)]
	pub type PanelPool<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		PanelId,
		Blake2_128Concat,
		T::AccountId,
		PanelMembership<BlockNumberFor<T>>,
		OptionQuery,
	>;

	/// The tender that established each panel, so `call_off` can authorise
	/// against that tender's officer.
	#[pallet::storage]
	#[pallet::getter(fn panel_tender)]
	pub type PanelTender<T: Config> =
		StorageMap<_, Blake2_128Concat, PanelId, TenderId, OptionQuery>;

	/// Suppliers shortlisted by an EOI (spec §2.2). Keyed by the EOI's own id.
	#[pallet::storage]
	#[pallet::getter(fn shortlist)]
	pub type Shortlist<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		TenderId,
		Blake2_128Concat,
		T::AccountId,
		BlockNumberFor<T>,
		OptionQuery,
	>;

	/// Size of each shortlist, so `publish_shortlist` stays bounded.
	#[pallet::storage]
	pub type ShortlistCount<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, u32, ValueQuery>;

	/// For a follow-on RFT, the EOI whose shortlist gates entry. Locked at
	/// publication with everything else, so the shortlist cannot be swapped
	/// once bidding is under way.
	#[pallet::storage]
	#[pallet::getter(fn shortlist_source)]
	pub type ShortlistSource<T: Config> =
		StorageMap<_, Blake2_128Concat, TenderId, TenderId, OptionQuery>;

	/// Call-off orders placed against each standing-offer panel.
	#[pallet::storage]
	#[pallet::getter(fn call_offs)]
	pub type CallOffs<T: Config> = StorageDoubleMap<
		_,
		Blake2_128Concat,
		PanelId,
		Blake2_128Concat,
		CallOffId,
		CallOffRecord<T::AccountId, BlockNumberFor<T>>,
		OptionQuery,
	>;

	/// Call-offs placed per panel. Bounds `MaxCallOffs` and feeds call-off id
	/// derivation; ids themselves are hashes.
	#[pallet::storage]
	pub type CallOffCount<T: Config> = StorageMap<_, Blake2_128Concat, PanelId, u32, ValueQuery>;

	/// The deployment's procurement policy (spec §8). Snapshotted into each
	/// tender at publication.
	#[pallet::storage]
	#[pallet::getter(fn policy)]
	pub type Policy<T: Config> = StorageValue<_, ProcurementPolicy<BlockNumberFor<T>>, ValueQuery>;

	/// Lifecycle transitions due at each block (spec §3, §8).
	#[pallet::storage]
	#[pallet::getter(fn deadline_wheel)]
	pub type DeadlineWheel<T: Config> = StorageMap<
		_,
		Twox64Concat,
		BlockNumberFor<T>,
		BoundedVec<(TenderId, Gate), T::MaxDeadlinesPerBlock>,
		ValueQuery,
	>;

	// -------------------------------------------------------------
	// Events (spec §4.2)
	// -------------------------------------------------------------

	#[pallet::event]
	#[pallet::generate_deposit(pub(super) fn deposit_event)]
	pub enum Event<T: Config> {
		TenderCreated { tender_id: TenderId, officer: T::AccountId, title: Vec<u8> },
		TenderPublished { tender_id: TenderId, entity: T::AccountId, close_block: BlockNumberFor<T> },
		AddendumPublished { tender_id: TenderId, content_hash: Hash256, extended_close_to: Option<BlockNumberFor<T>> },
		QuestionAsked { tender_id: TenderId, question_id: QuestionId },
		QuestionAnswered { tender_id: TenderId, question_id: QuestionId },
		BidCommitted { tender_id: TenderId, bidder: T::AccountId, block: BlockNumberFor<T> },
		CommitmentWithdrawn { tender_id: TenderId, bidder: T::AccountId },
		/// The Q&A window closed and submissions opened. Emitted by the deadline
		/// wheel so the whole state machine is reconstructible from events alone.
		SubmissionOpened { tender_id: TenderId },
		TenderClosed { tender_id: TenderId },
		OpeningStarted { tender_id: TenderId, participants: u32 },
		BidRevealed { tender_id: TenderId, bidder: T::AccountId, valid: bool },
		RevealMismatch { tender_id: TenderId, bidder: T::AccountId },
		EvaluationStarted { tender_id: TenderId },
		EvaluatorAppointed { tender_id: TenderId, evaluator: T::AccountId },
		ConflictDeclared { tender_id: TenderId, evaluator: T::AccountId },
		EvaluatorActivated { tender_id: TenderId, evaluator: T::AccountId },
		ScoresSubmitted { tender_id: TenderId, evaluator: T::AccountId, bidder: T::AccountId },
		/// Two evaluators disagreed on one criterion by more than the configured
		/// threshold. Routed to probity observers (spec §5.2).
		ScoreVarianceFlagged { tender_id: TenderId, bidder: T::AccountId, criterion_id: CriterionId, spread: u8 },
		Awarded { tender_id: TenderId, awardee: T::AccountId, rationale_hash: Hash256 },
		StandstillOpened { tender_id: TenderId, standstill_end: BlockNumberFor<T> },
		StandstillClosed { tender_id: TenderId },
		ChallengeLodged { tender_id: TenderId, challenge_id: ChallengeId, challenger: T::AccountId },
		ChallengeResolved { tender_id: TenderId, challenge_id: ChallengeId, state: ChallengeState },
		ContractExecuted { tender_id: TenderId, contract_hash: Hash256 },
		/// Module 25 delivery instantiated for one awardee. `delivery_project` is
		/// the reference spec §4.2 asks for; it is `None` while Work Task is stubbed.
		DeliveryInstantiated {
			tender_id: TenderId,
			awardee: T::AccountId,
			delivery_project: Option<Hash256>,
		},
		/// An EOI published its shortlist (spec §2.2).
		ShortlistPublished { tender_id: TenderId, suppliers: u32 },
		PanelMemberAdmitted { panel_id: PanelId, supplier: T::AccountId },
		CallOffPlaced {
			panel_id: PanelId,
			call_off_id: CallOffId,
			supplier: T::AccountId,
			order_hash: Hash256,
		},
		/// Draft criteria were replaced before publication.
		CriteriaAmended { tender_id: TenderId, criteria_hash: Hash256 },
		/// The deployment's procurement policy changed. Applies to tenders
		/// published from now on; live tenders keep their snapshot.
		PolicyUpdated { policy: ProcurementPolicy<BlockNumberFor<T>> },
		BondReturned { tender_id: TenderId, bidder: T::AccountId, amount: BalanceOf<T> },
		BondForfeited { tender_id: TenderId, bidder: T::AccountId, amount: BalanceOf<T> },
		TenderCancelled { tender_id: TenderId, reason_hash: Hash256 },
		/// A deadline could not be re-queued because every block in the
		/// look-ahead window was full. Recorded so the stall is visible to
		/// probity observers rather than silent.
		GateDropped { tender_id: TenderId, gate: Gate },
	}

	// -------------------------------------------------------------
	// Errors (spec §4.3)
	// -------------------------------------------------------------

	#[pallet::error]
	pub enum Error<T> {
		/// Gates are not strictly increasing.
		GateOrderInvalid,
		/// Criterion weights did not sum to 100%.
		WeightsInvalid,
		/// Too many weighted criteria.
		TooManyWeights,
		/// No such tender.
		TenderNotFound,
		/// The tender is not in a state that permits this action.
		BadState,
		/// Only the procurement officer (or entity) may do this.
		NotOfficer,
		/// The tender has not been published.
		TenderNotPublished,
		/// Submissions have closed.
		SubmissionClosed,
		/// Bidder does not satisfy the eligibility policy.
		NotEligible,
		/// This bidder already holds a commitment for this tender.
		CommitmentExists,
		/// No commitment found for this bidder.
		CommitmentNotFound,
		/// Revealed content does not hash to the commitment.
		RevealMismatch,
		/// Not inside the reveal window.
		RevealWindowClosed,
		/// This bid was already revealed.
		AlreadyRevealed,
		/// Evaluator has not lodged a conflict-of-interest declaration.
		ConflictNotDeclared,
		/// Caller is not an activated evaluator for this tender.
		NotEvaluator,
		/// A score exceeded `MaxScore`.
		ScoreOutOfRange,
		/// Scoresheet criteria do not match the tender's locked criteria.
		CriteriaMismatch,
		/// Fewer than `MinEvaluators` are activated.
		TooFewEvaluators,
		/// The standstill window is still open.
		StandstillActive,
		/// The standstill (challenge) window has passed.
		ChallengeWindowClosed,
		/// A challenge is unresolved; execution is suspended.
		ChallengeOpen,
		/// No such challenge, or it is already resolved.
		ChallengeNotFound,
		/// Supplier is not on the panel.
		NotPanelMember,
		/// This tender is not a panel/standing-offer.
		NotPanelTender,
		/// An addendum may extend the close block, never shorten it (spec §4.1).
		CloseCannotShorten,
		/// Bounded collection full.
		TooManyAddenda,
		TooManyBidders,
		TooManyEvaluators,
		TooManyPriceLines,
		/// This tender already holds `MaxQuestions` questions.
		TooManyQuestions,
		/// This tender already holds `MaxChallenges` challenges.
		TooManyChallenges,
		/// Only a bidder who lodged a commitment on this tender may challenge it.
		NotAParticipant,
		/// Spec §1.2 evaluator separation: an evaluator may not also bid on the
		/// tender they score, and a bidder may not be appointed to score it.
		EvaluatorIsBidder,
		/// Spec §1.2 evaluator separation: the officer or procuring entity may
		/// not sit on their own evaluation panel.
		OfficerCannotEvaluate,
		/// Opening this late would leave less than `MinRevealWindow` for bidders.
		RevealWindowTooShort,
		/// This tender draws its bidders from an EOI shortlist, and the caller is
		/// not on it.
		NotShortlisted,
		/// Shortlists may only be published from an evaluated EOI.
		NotAnEoi,
		/// A supplier proposed for the shortlist did not lodge a valid bid.
		InvalidShortlistEntry,
		/// Too many required eligibility credentials.
		TooManyCredentials,
		/// Title exceeds `MaxTitleLen`.
		TitleTooLong,
		/// Title is empty. A tender with no name is not a public notice.
		TitleEmpty,
		/// Summary exceeds `MaxSummaryLen`.
		SummaryTooLong,
		/// Challenge grounds exceed `MaxGroundsLen`.
		GroundsTooLong,
		/// Challenge grounds are empty. An allegation with no stated grounds
		/// suspends a lawful award on nothing and cannot be ruled on.
		GroundsEmpty,
		/// Challenge resolution exceeds `MaxResolutionLen`.
		ResolutionTooLong,
		/// Challenge resolution is empty. Every ruling carries written reasoning.
		ResolutionEmpty,
		/// This call does not apply to the tender's bid mode: sealed tenders use
		/// `commit_bid`/`reveal_bid`, open tenders use `submit_open_bid`.
		WrongBidMode,
		/// No panel exists for this id.
		PanelNotFound,
		/// Too many deadlines already scheduled on that block.
		DeadlineWheelFull,
		/// Awardee did not participate, or its bid was voided.
		InvalidAwardee,
		/// Internal nonce exhausted.
		IdOverflow,
		/// A derived id already exists. Practically unreachable; checked so a
		/// collision can never overwrite a live record.
		IdCollision,
		/// No such question.
		QuestionNotFound,
		/// Answers are part of the public record and cannot be rewritten.
		AlreadyAnswered,
		/// Criterion ids must be unique within a tender and within a scoresheet.
		DuplicateCriterion,
		/// The same account was named twice as an awardee.
		DuplicateAwardee,
		/// An EOI concludes with `publish_shortlist`, not `award`.
		EoiUsesShortlist,
		/// Awardees cannot challenge their own award (spec §6.3: challenges come
		/// from unsuccessful bidders).
		AwardeeCannotChallenge,
		/// Only the procuring entity may do this (spec §4.1: governed entity
		/// authority, not the officer).
		NotEntity,
		/// The tender's `publish_at` gate has not been reached.
		PublishTooEarly,
		/// A conflict declaration is locked once scoring rights are active.
		EvaluatorAlreadyActive,
		/// Open bidding is only permitted for `Rfq` tenders.
		OpenBidNotPermitted,
		/// Nothing to award — no valid reveals.
		NoValidBids,
		/// A document hash does not name anything DNC holds (Module 2).
		DocumentNotAnchored,
		/// The bid bond could not be locked.
		BondRequired,
		/// Criteria, weights and gates are locked once a tender is published.
		CriteriaLocked,
		/// The award must come from the governed award authority (spec §8).
		AwardAuthorityRequired,
		/// The standstill is shorter than the policy's mandatory minimum.
		StandstillTooShort,
		/// Publication leaves the market less than the policy's minimum
		/// submission period.
		SubmissionPeriodTooShort,
		/// The extension would push the close past the policy's cap.
		CloseExtensionTooLong,
		/// This addendum leaves bidders less than the policy's response window;
		/// it must also extend the close.
		AddendumNeedsExtension,
		/// This panel already holds `MaxCallOffs` call-off orders.
		TooManyCallOffs,
	}

	// -------------------------------------------------------------
	// Hooks — the deadline wheel (spec §8: "no officer-controlled clock")
	// -------------------------------------------------------------

	#[pallet::hooks]
	impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
		fn on_initialize(now: BlockNumberFor<T>) -> Weight {
			let max = T::MaxTransitionsPerBlock::get();
			let due = DeadlineWheel::<T>::take(now);
			let mut processed = 0u32;
			let mut deferred: Vec<(TenderId, Gate)> = Vec::new();

			for (tender_id, gate) in due.into_iter() {
				if processed >= max {
					// Over budget for this block — carry the rest to the next one
					// rather than doing unbounded work.
					deferred.push((tender_id, gate));
					continue;
				}
				Self::process_gate(tender_id, gate);
				processed = processed.saturating_add(1);
			}

			// Carry the overflow forward into the first following block with room.
			// Dropping a gate would strand its tender in the current state with
			// bonds reserved, so the look-ahead is walked before giving up, and a
			// gate that still cannot be placed is put on the public record.
			let mut probes: u64 = 0;
			for (tender_id, gate) in deferred.into_iter() {
				let mut at = now;
				let mut placed = false;
				for _ in 0..GATE_LOOKAHEAD {
					at = at.saturating_add(One::one());
					probes = probes.saturating_add(1);
					if DeadlineWheel::<T>::try_mutate(at, |v| v.try_push((tender_id, gate))).is_ok() {
						placed = true;
						break;
					}
				}
				if !placed {
					Self::deposit_event(Event::GateDropped { tender_id, gate });
				}
			}

			// The benchmark covers the gates processed; the deferral probes are
			// charged on top at one read and one write each.
			T::WeightInfo::on_initialize(processed)
				.saturating_add(T::DbWeight::get().reads_writes(probes, probes))
		}
	}

	// -------------------------------------------------------------
	// Calls (spec §4.1)
	// -------------------------------------------------------------

	#[pallet::call]
	impl<T: Config> Pallet<T> {
		/// Create a tender in `Draft`. Validates gate ordering and that weights sum
		/// to 100%, but locks nothing yet — that happens at `publish_tender`.
		#[pallet::call_index(0)]
		#[pallet::weight(T::WeightInfo::create_tender(weights.len() as u32))]
		pub fn create_tender(
			origin: OriginFor<T>,
			entity: T::AccountId,
			kind: TenderKind,
			bid_mode: BidMode,
			title: Vec<u8>,
			summary: Vec<u8>,
			notice_hash: Hash256,
			criteria_hash: Hash256,
			weights: Vec<CriterionWeight>,
			gates: TenderGates<BlockNumberFor<T>>,
			standstill_period: BlockNumberFor<T>,
			required_credentials: Vec<Hash256>,
			min_reputation: u32,
			bond: BondTerms<BalanceOf<T>>,
			blind_questions: bool,
			shortlist_from: Option<TenderId>,
		) -> DispatchResult {
			let officer = ensure_signed(origin)?;

			// Open bidding removes the pre-opening secrecy guarantee, so it is
			// confined to low-value RFQs (spec §1.2).
			ensure!(
				matches!(bid_mode, BidMode::Sealed) || matches!(kind, TenderKind::Rfq),
				Error::<T>::OpenBidNotPermitted
			);

			let total: u32 = weights.iter().map(|w| w.weight_percent as u32).sum();
			ensure!(total == 100, Error::<T>::WeightsInvalid);
			Self::ensure_anchored(&notice_hash)?;
			Self::ensure_anchored(&criteria_hash)?;
			// A criterion listed twice would carry two weights, and a scoresheet
			// could then score it twice while silently omitting another.
			Self::ensure_unique_criteria(weights.iter().map(|w| &w.criterion_id))?;

			Self::ensure_gate_order(&gates)?;
			// Checked again at publication, against the policy in force then.
			Self::ensure_policy(&Policy::<T>::get(), &gates, standstill_period, gates.publish_at)?;

			let bounded_weights: BoundedVec<CriterionWeight, T::MaxWeights> =
				weights.try_into().map_err(|_| Error::<T>::TooManyWeights)?;
			let bounded_creds: BoundedVec<Hash256, T::MaxCredentials> =
				required_credentials.try_into().map_err(|_| Error::<T>::TooManyCredentials)?;
			// Rejected rather than truncated: a silently cut title is a wrong
			// public record, and the officer cannot see that it happened.
			let bounded_title: BoundedVec<u8, T::MaxTitleLen> =
				title.try_into().map_err(|_| Error::<T>::TitleTooLong)?;
			ensure!(!bounded_title.is_empty(), Error::<T>::TitleEmpty);
			let bounded_summary: BoundedVec<u8, T::MaxSummaryLen> =
				summary.try_into().map_err(|_| Error::<T>::SummaryTooLong)?;

			let now = frame_system::Pallet::<T>::block_number();
			let nonce = TenderNonce::<T>::get();
			let tender_id = Self::tender_id_for(&officer, &entity, nonce, now);
			ensure!(!Tenders::<T>::contains_key(tender_id), Error::<T>::IdCollision);

			// Validated before anything is written: a failed check must not
			// leave a half-created tender behind.
			if let Some(eoi) = shortlist_from {
				let source = Tenders::<T>::get(eoi).ok_or(Error::<T>::TenderNotFound)?;
				ensure!(matches!(source.kind, TenderKind::Eoi), Error::<T>::NotAnEoi);
			}

			Tenders::<T>::insert(
				tender_id,
				TenderRecord {
					entity,
					officer: officer.clone(),
					kind,
					bid_mode,
					title: bounded_title.clone(),
					summary: bounded_summary,
					notice_hash,
					criteria_hash,
					weights: bounded_weights,
					gates,
					standstill_period,
					eligibility: EligibilityPolicy {
						required_credentials: bounded_creds,
						min_reputation,
					},
					bond,
					blind_questions,
					state: TenderState::Draft,
					created_at: now,
					policy: Default::default(),
					published_close_at: None,
					published_at: None,
				},
			);
			// Locked at publication along with criteria and gates: the shortlist a
			// tender draws from cannot be swapped once bidding is under way.
			if let Some(eoi) = shortlist_from {
				ShortlistSource::<T>::insert(tender_id, eoi);
			}
			TenderNonce::<T>::put(nonce.checked_add(1).ok_or(Error::<T>::IdOverflow)?);

			Self::deposit_event(Event::TenderCreated {
				tender_id,
				officer,
				title: bounded_title.into_inner(),
			});
			Ok(())
		}

		/// Publish, locking criteria, weights, gates and eligibility permanently.
		/// Schedules every downstream deadline onto the wheel.
		#[pallet::call_index(1)]
		#[pallet::weight(T::WeightInfo::publish_tender())]
		pub fn publish_tender(origin: OriginFor<T>, tender_id: TenderId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();

			Tenders::<T>::try_mutate(tender_id, |maybe| -> DispatchResult {
				let tender = maybe.as_mut().ok_or(Error::<T>::TenderNotFound)?;
				Self::ensure_officer(tender, &who)?;
				ensure!(matches!(tender.state, TenderState::Draft), Error::<T>::BadState);
				// Publication is a block gate like every other deadline (spec §1.2):
				// the notice cannot go out before the block it was scheduled for.
				ensure!(now >= tender.gates.publish_at, Error::<T>::PublishTooEarly);
				// There must still be a Q&A window left to publish into.
				ensure!(now < tender.gates.questions_close_at, Error::<T>::GateOrderInvalid);
				// The policy in force now governs, measured from the actual
				// publication block — publishing late eats into the market's
				// response time just as a short schedule would.
				let policy = Policy::<T>::get();
				Self::ensure_policy(&policy, &tender.gates, tender.standstill_period, now)?;

				tender.state = TenderState::QaWindow;
				tender.published_at = Some(now);
				tender.policy = policy;
				tender.published_close_at = Some(tender.gates.submission_close_at);

				Self::deposit_event(Event::TenderPublished {
					tender_id,
					entity: tender.entity.clone(),
					close_block: tender.gates.submission_close_at,
				});
				Ok(())
			})?;

			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			Self::schedule(tender.gates.questions_close_at, tender_id, Gate::QuestionsClose)?;
			Self::schedule(tender.gates.submission_close_at, tender_id, Gate::SubmissionClose)?;
			Self::schedule(tender.gates.opening_end_at, tender_id, Gate::OpeningEnd)?;
			// A public notice: nobody has bid yet, so there is no recipient list.
			T::Notices::notify(tender_id, TenderNotice::Published, &[]);
			Ok(())
		}

		/// Ask a question during the Q&A window.
		#[pallet::call_index(2)]
		#[pallet::weight(T::WeightInfo::ask_question())]
		///
		/// When the tender sets `blind_questions`, pass the `author_salt` used to
		/// build `blake2_256(asker ‖ salt)`; the account itself is never written to
		/// storage, so authorship is genuinely unreadable rather than merely
		/// omitted from the event (spec §5.3).
		pub fn ask_question(
			origin: OriginFor<T>,
			tender_id: TenderId,
			question_hash: Hash256,
			author_salt: Hash256,
		) -> DispatchResult {
			let asker = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			ensure!(matches!(tender.state, TenderState::QaWindow), Error::<T>::BadState);
			// The block number closes questions even if the wheel has not yet
			// processed the gate (e.g. it was deferred by a busy block).
			ensure!(now < tender.gates.questions_close_at, Error::<T>::BadState);

			// Questions are never removed, so the count bounds state growth. The
			// Q&A window is open to any account, so without this an attacker
			// could grow state for the price of fees alone.
			let count = QuestionCount::<T>::get(tender_id);
			ensure!(count < T::MaxQuestions::get(), Error::<T>::TooManyQuestions);
			// The asker is deliberately not an input: with blinded authorship, a
			// hash over (asker, tender, count) could be brute-forced against the
			// small set of plausible suppliers and would de-blind the question.
			let qid = Self::question_id_for(&tender_id, count);
			ensure!(!Questions::<T>::contains_key(tender_id, qid), Error::<T>::IdCollision);
			let author = if tender.blind_questions {
				QuestionAuthor::Blinded(Self::blind_author(&asker, &author_salt))
			} else {
				QuestionAuthor::Open(asker)
			};

			Questions::<T>::insert(
				tender_id,
				qid,
				QaRecord {
					author,
					question_hash,
					asked_at: now,
					answer_hash: None,
					answered_at: None,
				},
			);
			QuestionCount::<T>::insert(tender_id, count.checked_add(1).ok_or(Error::<T>::IdOverflow)?);

			Self::deposit_event(Event::QuestionAsked { tender_id, question_id: qid });
			Ok(())
		}

		/// Answer a question. Answers publish to all bidders at once — there is no
		/// call that delivers a private clarification (spec §6.1 step 3).
		#[pallet::call_index(3)]
		#[pallet::weight(T::WeightInfo::answer_question())]
		pub fn answer_question(
			origin: OriginFor<T>,
			tender_id: TenderId,
			question_id: QuestionId,
			answer_hash: Hash256,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			Self::ensure_officer(&tender, &who)?;
			// Clarifications are only useful while bids can still respond to them.
			ensure!(
				matches!(tender.state, TenderState::QaWindow | TenderState::Submission),
				Error::<T>::BadState
			);

			Questions::<T>::try_mutate(tender_id, question_id, |maybe| -> DispatchResult {
				let q = maybe.as_mut().ok_or(Error::<T>::QuestionNotFound)?;
				// A published answer is what every bidder priced against; letting it
				// be rewritten would be a private clarification by another name.
				ensure!(q.answer_hash.is_none(), Error::<T>::AlreadyAnswered);
				q.answer_hash = Some(answer_hash);
				q.answered_at = Some(frame_system::Pallet::<T>::block_number());
				Ok(())
			})?;

			Self::deposit_event(Event::QuestionAnswered { tender_id, question_id });
			Ok(())
		}

		/// Publish an addendum, optionally extending the submission close block.
		/// Extension only — an addendum can never shorten the window.
		#[pallet::call_index(4)]
		#[pallet::weight(T::WeightInfo::publish_addendum())]
		pub fn publish_addendum(
			origin: OriginFor<T>,
			tender_id: TenderId,
			content_hash: Hash256,
			extend_close_to: Option<BlockNumberFor<T>>,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			Self::ensure_anchored(&content_hash)?;

			let new_close = Tenders::<T>::try_mutate(
				tender_id,
				|maybe| -> Result<Option<BlockNumberFor<T>>, DispatchError> {
					let tender = maybe.as_mut().ok_or(Error::<T>::TenderNotFound)?;
					Self::ensure_officer(tender, &who)?;
					ensure!(
						matches!(tender.state, TenderState::QaWindow | TenderState::Submission),
						Error::<T>::BadState
					);

					// Governed rule for addenda (spec §2.3), from the policy snapshot
					// taken at publication. Bidders must be left enough time to
					// respond to what the addendum says.
					let close_after = extend_close_to.unwrap_or(tender.gates.submission_close_at);
					ensure!(
						close_after >= now.saturating_add(tender.policy.addendum_response_window),
						Error::<T>::AddendumNeedsExtension
					);

					if let Some(new_close) = extend_close_to {
						ensure!(
							new_close > tender.gates.submission_close_at,
							Error::<T>::CloseCannotShorten
						);
						if let Some(cap) = tender.policy.max_close_extension {
							let base = tender
								.published_close_at
								.unwrap_or(tender.gates.submission_close_at);
							ensure!(
								new_close <= base.saturating_add(cap),
								Error::<T>::CloseExtensionTooLong
							);
						}
						// Shift every downstream gate by the same amount, so any gap
						// the officer published between close and opening survives
						// and the reveal window keeps its length.
						let delta = new_close.saturating_sub(tender.gates.submission_close_at);
						tender.gates.submission_close_at = new_close;
						tender.gates.opening_at = tender.gates.opening_at.saturating_add(delta);
						tender.gates.opening_end_at = tender.gates.opening_end_at.saturating_add(delta);
						return Ok(Some(new_close));
					}
					Ok(None)
				},
			)?;

			Addenda::<T>::try_mutate(tender_id, |list| -> DispatchResult {
				list.try_push(AddendumRecord {
					content_hash,
					published_at: now,
					extended_close_to: new_close,
				})
				.map_err(|_| Error::<T>::TooManyAddenda)?;
				Ok(())
			})?;

			// The original SubmissionClose/OpeningEnd wheel entries stay put; the
			// gate handlers re-check the (possibly extended) block and re-schedule
			// themselves if they fire early.
			if let Some(new_close) = new_close {
				let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
				Self::schedule(new_close, tender_id, Gate::SubmissionClose)?;
				Self::schedule(tender.gates.opening_end_at, tender_id, Gate::OpeningEnd)?;
			}

			Self::notify_participants(tender_id, TenderNotice::AddendumPublished);
			Self::deposit_event(Event::AddendumPublished {
				tender_id,
				content_hash,
				extended_close_to: new_close,
			});
			Ok(())
		}

		/// Lodge a sealed bid commitment and reserve the bid bond.
		///
		/// `commitment_hash` must equal
		/// `blake2_256(bidder ‖ documents_hash ‖ price_schedule ‖ salt)` — see
		/// [`Pallet::compute_commitment`]. Binding the bidder into the preimage stops
		/// a rival copying a commitment and claiming the same bid.
		#[pallet::call_index(5)]
		#[pallet::weight(T::WeightInfo::commit_bid())]
		pub fn commit_bid(
			origin: OriginFor<T>,
			tender_id: TenderId,
			commitment_hash: Hash256,
		) -> DispatchResult {
			let bidder = ensure_signed(origin)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			// Sealed tenders only; open-bid RFQs publish their content at
			// submission time via `submit_open_bid`.
			ensure!(matches!(tender.bid_mode, BidMode::Sealed), Error::<T>::WrongBidMode);
			Self::ensure_can_submit(tender_id, &tender, &bidder)?;
			Self::record_submission(tender_id, &tender, &bidder, commitment_hash)?;
			Ok(())
		}

		/// Submit an open bid, publishing its content immediately.
		///
		/// Spec §1.2 allows an open-bid mode for low-value RFQs. There is no
		/// sealed phase and no separate reveal: the documents and price schedule
		/// are recorded and readable the moment the bid is lodged, so everyone
		/// sees the same thing at the same time.
		#[pallet::call_index(20)]
		#[pallet::weight(T::WeightInfo::submit_open_bid(price_schedule.len() as u32))]
		pub fn submit_open_bid(
			origin: OriginFor<T>,
			tender_id: TenderId,
			documents_hash: Hash256,
			price_schedule: Vec<PriceLine<BalanceOf<T>>>,
		) -> DispatchResult {
			let bidder = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			ensure!(matches!(tender.bid_mode, BidMode::Open), Error::<T>::WrongBidMode);
			Self::ensure_can_submit(tender_id, &tender, &bidder)?;

			let bounded_prices: BoundedVec<PriceLine<BalanceOf<T>>, T::MaxPriceLines> =
				price_schedule.try_into().map_err(|_| Error::<T>::TooManyPriceLines)?;

			// The commitment is still recorded, over a fixed all-zero salt, so bond
			// settlement and ranking treat open and sealed bids identically. It is
			// not secret — in open mode it is not meant to be.
			let commitment_hash =
				Self::compute_commitment(&bidder, &documents_hash, &bounded_prices, &OPEN_BID_SALT);
			Self::record_submission(tender_id, &tender, &bidder, commitment_hash)?;

			// Recorded as revealed straight away: there is nothing left to disclose,
			// so an open bid can never be treated as a non-reveal and forfeited.
			Reveals::<T>::insert(
				tender_id,
				&bidder,
				RevealRecord {
					documents_hash,
					price_schedule: bounded_prices,
					revealed_at: now,
					valid: true,
				},
			);

			Self::deposit_event(Event::BidRevealed { tender_id, bidder, valid: true });
			Ok(())
		}

		/// Withdraw a commitment before close. Bond treatment follows the published
		/// terms.
		#[pallet::call_index(6)]
		#[pallet::weight(T::WeightInfo::withdraw_commitment())]
		pub fn withdraw_commitment(
			origin: OriginFor<T>,
			tender_id: TenderId,
		) -> DispatchResult {
			let bidder = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			ensure!(
				matches!(tender.state, TenderState::QaWindow | TenderState::Submission),
				Error::<T>::SubmissionClosed
			);
			ensure!(now < tender.gates.submission_close_at, Error::<T>::SubmissionClosed);

			let commitment = BidCommitments::<T>::take(tender_id, &bidder)
				.ok_or(Error::<T>::CommitmentNotFound)?;

			Participants::<T>::mutate(tender_id, |list| list.retain(|a| a != &bidder));
			// An open bid is recorded as revealed at submission. Left behind, that
			// record would keep a withdrawn bidder scoreable and awardable.
			Reveals::<T>::remove(tender_id, &bidder);

			if !commitment.bond_reserved.is_zero() {
				if tender.bond.forfeit_on_withdrawal {
					Self::forfeit_bond(tender_id, &bidder, commitment.bond_reserved);
				} else {
					T::Bonds::release(&bidder, commitment.bond_reserved);
					Self::deposit_event(Event::BondReturned {
						tender_id,
						bidder: bidder.clone(),
						amount: commitment.bond_reserved,
					});
				}
			}

			Self::deposit_event(Event::CommitmentWithdrawn { tender_id, bidder });
			Ok(())
		}

		/// Move a closed tender into its public opening window.
		#[pallet::call_index(7)]
		#[pallet::weight(T::WeightInfo::open_tender())]
		pub fn open_tender(origin: OriginFor<T>, tender_id: TenderId) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();

			Tenders::<T>::try_mutate(tender_id, |maybe| -> DispatchResult {
				let tender = maybe.as_mut().ok_or(Error::<T>::TenderNotFound)?;
				Self::ensure_officer(tender, &who)?;
				ensure!(matches!(tender.state, TenderState::Closed), Error::<T>::BadState);
				ensure!(now >= tender.gates.opening_at, Error::<T>::BadState);
				// Opening at the last moment would shrink the reveal window to
				// nothing, which is the same lever as never opening at all.
				ensure!(
					tender.gates.opening_end_at.saturating_sub(now) >= T::MinRevealWindow::get(),
					Error::<T>::RevealWindowTooShort
				);
				tender.state = TenderState::Opening;
				Ok(())
			})?;

			let count = Participants::<T>::get(tender_id).len() as u32;
			Self::deposit_event(Event::OpeningStarted { tender_id, participants: count });
			Ok(())
		}

		/// Reveal a sealed bid. A hash mismatch voids the bid but is recorded
		/// permanently rather than silently rejected.
		#[pallet::call_index(8)]
		#[pallet::weight(T::WeightInfo::reveal_bid(price_schedule.len() as u32))]
		pub fn reveal_bid(
			origin: OriginFor<T>,
			tender_id: TenderId,
			documents_hash: Hash256,
			price_schedule: Vec<PriceLine<BalanceOf<T>>>,
			salt: Hash256,
		) -> DispatchResult {
			let bidder = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;

			// An open bid was published at submission time; there is nothing to reveal.
			ensure!(matches!(tender.bid_mode, BidMode::Sealed), Error::<T>::WrongBidMode);
			ensure!(matches!(tender.state, TenderState::Opening), Error::<T>::RevealWindowClosed);
			ensure!(now <= tender.gates.opening_end_at, Error::<T>::RevealWindowClosed);
			ensure!(!Reveals::<T>::contains_key(tender_id, &bidder), Error::<T>::AlreadyRevealed);

			let commitment = BidCommitments::<T>::get(tender_id, &bidder)
				.ok_or(Error::<T>::CommitmentNotFound)?;

			let bounded_prices: BoundedVec<PriceLine<BalanceOf<T>>, T::MaxPriceLines> =
				price_schedule.try_into().map_err(|_| Error::<T>::TooManyPriceLines)?;

			let expected =
				Self::compute_commitment(&bidder, &documents_hash, &bounded_prices, &salt);
			let valid = expected == commitment.commitment_hash;

			Reveals::<T>::insert(
				tender_id,
				&bidder,
				RevealRecord {
					documents_hash,
					price_schedule: bounded_prices,
					revealed_at: now,
					valid,
				},
			);

			if valid {
				Self::deposit_event(Event::BidRevealed { tender_id, bidder, valid: true });
			} else {
				Self::deposit_event(Event::RevealMismatch {
					tender_id,
					bidder: bidder.clone(),
				});
				Self::deposit_event(Event::BidRevealed { tender_id, bidder, valid: false });
			}
			Ok(())
		}

		/// Appoint an evaluator. Scoring rights stay inactive until a conflict
		/// declaration is lodged.
		#[pallet::call_index(9)]
		#[pallet::weight(T::WeightInfo::appoint_evaluator())]
		pub fn appoint_evaluator(
			origin: OriginFor<T>,
			tender_id: TenderId,
			evaluator: T::AccountId,
			credential_ref: Hash256,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			Self::ensure_officer(&tender, &who)?;
			// The panel is part of the evaluation record; it cannot be reshaped
			// once the tender has been contracted, shortlisted or cancelled.
			ensure!(
				!matches!(
					tender.state,
					TenderState::Contracted | TenderState::Cancelled | TenderState::Shortlisted
				),
				Error::<T>::BadState
			);

			// Spec §1.2 evaluator separation. Scoring your own bid, or scoring a
			// tender you administer, defeats attributed evaluation regardless of
			// what the conflict declaration says.
			ensure!(
				!BidCommitments::<T>::contains_key(tender_id, &evaluator),
				Error::<T>::EvaluatorIsBidder
			);
			ensure!(
				evaluator != tender.officer && evaluator != tender.entity,
				Error::<T>::OfficerCannotEvaluate
			);

			// Re-appointing someone already on the panel refreshes their record
			// rather than consuming another slot.
			let existing = EvaluatorSet::<T>::get(tender_id, &evaluator);
			if existing.is_none() {
				ensure!(
					AppointedEvaluatorCount::<T>::get(tender_id) < T::MaxEvaluators::get(),
					Error::<T>::TooManyEvaluators
				);
				AppointedEvaluatorCount::<T>::mutate(tender_id, |c| *c = c.saturating_add(1));
			}

			// Re-appointment clears the conflict declaration and deactivates, so an
			// evaluator who was already active must leave the active tally too —
			// otherwise `MinEvaluators` could be satisfied by evaluators who are no
			// longer activated.
			if existing.map_or(false, |rec| rec.active) {
				ActiveEvaluatorCount::<T>::mutate(tender_id, |c| *c = c.saturating_sub(1));
			}

			EvaluatorSet::<T>::insert(
				tender_id,
				&evaluator,
				EvaluatorRecord {
					appointed_at: frame_system::Pallet::<T>::block_number(),
					credential_ref,
					conflict_declaration: None,
					active: false,
				},
			);

			Self::deposit_event(Event::EvaluatorAppointed { tender_id, evaluator });
			Ok(())
		}

		/// Lodge a conflict-of-interest declaration (spec §2.1).
		#[pallet::call_index(10)]
		#[pallet::weight(T::WeightInfo::declare_conflict())]
		pub fn declare_conflict(
			origin: OriginFor<T>,
			tender_id: TenderId,
			declaration_hash: Hash256,
		) -> DispatchResult {
			let evaluator = ensure_signed(origin)?;

			EvaluatorSet::<T>::try_mutate(tender_id, &evaluator, |maybe| -> DispatchResult {
				let rec = maybe.as_mut().ok_or(Error::<T>::NotEvaluator)?;
				// Scoring rights were granted against this declaration; swapping it
				// afterwards would rewrite the basis on which they were granted.
				ensure!(!rec.active, Error::<T>::EvaluatorAlreadyActive);
				rec.conflict_declaration = Some(declaration_hash);
				Ok(())
			})?;

			Self::deposit_event(Event::ConflictDeclared { tender_id, evaluator });
			Ok(())
		}

		/// Activate an evaluator's scoring rights. Refuses without a declaration.
		#[pallet::call_index(11)]
		#[pallet::weight(T::WeightInfo::activate_evaluator())]
		pub fn activate_evaluator(
			origin: OriginFor<T>,
			tender_id: TenderId,
			evaluator: T::AccountId,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			Self::ensure_officer(&tender, &who)?;

			let newly_active =
				EvaluatorSet::<T>::try_mutate(tender_id, &evaluator, |maybe| -> Result<bool, DispatchError> {
					let rec = maybe.as_mut().ok_or(Error::<T>::NotEvaluator)?;
					ensure!(rec.conflict_declaration.is_some(), Error::<T>::ConflictNotDeclared);
					if rec.active {
						return Ok(false);
					}
					rec.active = true;
					Ok(true)
				})?;

			if newly_active {
				ActiveEvaluatorCount::<T>::mutate(tender_id, |c| *c = c.saturating_add(1));
			}

			Self::deposit_event(Event::EvaluatorActivated { tender_id, evaluator });
			Ok(())
		}

		/// Submit an attributed scoresheet for one bidder.
		#[pallet::call_index(12)]
		#[pallet::weight(T::WeightInfo::submit_scores(scores.len() as u32))]
		pub fn submit_scores(
			origin: OriginFor<T>,
			tender_id: TenderId,
			bidder: T::AccountId,
			scores: Vec<CriterionScore>,
			comment_hash: Hash256,
		) -> DispatchResult {
			let evaluator = ensure_signed(origin)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			ensure!(matches!(tender.state, TenderState::Evaluation), Error::<T>::BadState);

			let rec = EvaluatorSet::<T>::get(tender_id, &evaluator).ok_or(Error::<T>::NotEvaluator)?;
			ensure!(rec.active, Error::<T>::ConflictNotDeclared);

			// A voided or absent bid cannot be scored.
			let reveal = Reveals::<T>::get(tender_id, &bidder).ok_or(Error::<T>::InvalidAwardee)?;
			ensure!(reveal.valid, Error::<T>::InvalidAwardee);

			// Scores must cover exactly the locked criteria — no adding a criterion
			// at scoring time, no quietly dropping one.
			ensure!(scores.len() == tender.weights.len(), Error::<T>::CriteriaMismatch);
			// Equal length plus membership is not enough on its own: without
			// uniqueness a sheet could score one criterion twice and skip another.
			Self::ensure_unique_criteria(scores.iter().map(|s| &s.criterion_id))?;
			let max_score = T::MaxScore::get();
			for s in scores.iter() {
				ensure!(s.score <= max_score, Error::<T>::ScoreOutOfRange);
				ensure!(
					tender.weights.iter().any(|w| w.criterion_id == s.criterion_id),
					Error::<T>::CriteriaMismatch
				);
			}

			// Flag disagreement against every *other* evaluator who already scored
			// this bidder, before writing our own sheet. Skipping self matters:
			// resubmitting overwrites the caller's previous sheet, and comparing a
			// revision against the version it replaces would raise a variance flag
			// for inter-evaluator disagreement that never happened.
			let threshold = T::ScoreVarianceThreshold::get();
			for (other, sheet) in Scores::<T>::iter_prefix((tender_id, bidder.clone())) {
				if other == evaluator {
					continue;
				}
				for mine in scores.iter() {
					if let Some(theirs) =
						sheet.scores.iter().find(|s| s.criterion_id == mine.criterion_id)
					{
						let spread = mine.score.abs_diff(theirs.score);
						if spread > threshold {
							Self::deposit_event(Event::ScoreVarianceFlagged {
								tender_id,
								bidder: bidder.clone(),
								criterion_id: mine.criterion_id,
								spread,
							});
						}
					}
				}
			}

			let bounded: BoundedVec<CriterionScore, T::MaxWeights> =
				scores.try_into().map_err(|_| Error::<T>::TooManyWeights)?;

			Scores::<T>::insert(
				(tender_id, bidder.clone(), evaluator.clone()),
				ScoreSheet {
					scores: bounded,
					comment_hash,
					submitted_at: frame_system::Pallet::<T>::block_number(),
				},
			);

			Self::deposit_event(Event::ScoresSubmitted { tender_id, evaluator, bidder });
			Ok(())
		}

		/// Make the award. Computes and publishes the full ranking, then opens the
		/// standstill window.
		///
		/// Gated on `AwardOrigin`, which the runtime must bind to a Multisig or
		/// governance origin (spec §8).
		#[pallet::call_index(13)]
		#[pallet::weight(T::WeightInfo::award(T::MaxBidders::get()))]
		pub fn award(
			origin: OriginFor<T>,
			tender_id: TenderId,
			awardees: Vec<T::AccountId>,
			rationale_hash: Hash256,
		) -> DispatchResult {
			T::AwardOrigin::ensure_origin(origin).map_err(|_| Error::<T>::AwardAuthorityRequired)?;
			let now = frame_system::Pallet::<T>::block_number();
			Self::ensure_anchored(&rationale_hash)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			ensure!(matches!(tender.state, TenderState::Evaluation), Error::<T>::BadState);
			ensure!(!matches!(tender.kind, TenderKind::Eoi), Error::<T>::EoiUsesShortlist);
			ensure!(
				ActiveEvaluatorCount::<T>::get(tender_id) >= T::MinEvaluators::get(),
				Error::<T>::TooFewEvaluators
			);
			ensure!(!awardees.is_empty(), Error::<T>::InvalidAwardee);

			// Only a bidder with a valid reveal may be awarded, and only once: a
			// repeated awardee would be admitted and instantiated twice.
			for (i, a) in awardees.iter().enumerate() {
				ensure!(!awardees[..i].contains(a), Error::<T>::DuplicateAwardee);
				let reveal = Reveals::<T>::get(tender_id, a).ok_or(Error::<T>::InvalidAwardee)?;
				ensure!(reveal.valid, Error::<T>::InvalidAwardee);
			}

			let ranking = Self::compute_ranking(tender_id, &tender);
			ensure!(!ranking.is_empty(), Error::<T>::NoValidBids);

			let bounded_ranking: BoundedVec<(T::AccountId, u32), T::MaxBidders> =
				ranking.try_into().map_err(|_| Error::<T>::TooManyBidders)?;
			let bounded_awardees: BoundedVec<T::AccountId, T::MaxBidders> =
				awardees.clone().try_into().map_err(|_| Error::<T>::TooManyBidders)?;

			let standstill_end = now.saturating_add(tender.standstill_period);

			Outcomes::<T>::insert(
				tender_id,
				OutcomeRecord {
					ranking: bounded_ranking,
					awardees: bounded_awardees,
					rationale_hash,
					awarded_at: now,
					standstill_end,
					contract_hash: None,
				},
			);
			Tenders::<T>::mutate(tender_id, |m| {
				if let Some(t) = m.as_mut() {
					t.state = TenderState::Awarded;
				}
			});

			// Panel admission happens at `execute_award`, not here: an award is
			// still challengeable during standstill, and suppliers admitted now
			// would survive an upheld challenge that overturned their award.

			Self::schedule(standstill_end, tender_id, Gate::StandstillEnd)?;

			for a in awardees.into_iter() {
				Self::deposit_event(Event::Awarded { tender_id, awardee: a, rationale_hash });
			}
			Self::deposit_event(Event::StandstillOpened { tender_id, standstill_end });
			// Spec §4.1: "notify all bidders via system mail".
			Self::notify_participants(tender_id, TenderNotice::Awarded);
			Ok(())
		}

		/// Lodge a challenge inside the standstill window. Suspends execution.
		#[pallet::call_index(14)]
		#[pallet::weight(T::WeightInfo::lodge_challenge())]
		pub fn lodge_challenge(
			origin: OriginFor<T>,
			tender_id: TenderId,
			grounds: Vec<u8>,
			evidence_hash: Option<Hash256>,
		) -> DispatchResult {
			let challenger = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			// Only a standing award can be challenged. An outcome record outlives
			// cancellation and upheld remittal, and challenging it there would
			// flip a cancelled or re-evaluating tender back to `Challenged`.
			ensure!(
				matches!(tender.state, TenderState::Awarded | TenderState::Challenged),
				Error::<T>::BadState
			);
			let outcome = Outcomes::<T>::get(tender_id).ok_or(Error::<T>::BadState)?;
			ensure!(now <= outcome.standstill_end, Error::<T>::ChallengeWindowClosed);

			// Spec §4.1/§6.3: challenges come from bidders. Without this any account
			// could lodge them, and since every open challenge suspends execution,
			// an outsider could hold a lawful award hostage indefinitely.
			ensure!(
				BidCommitments::<T>::contains_key(tender_id, &challenger),
				Error::<T>::NotAParticipant
			);
			ensure!(
				!outcome.awardees.contains(&challenger),
				Error::<T>::AwardeeCannotChallenge
			);

			// Rejected rather than truncated, as with `title`: a silently cut
			// allegation is a wrong public record the challenger never saw happen,
			// and it is the text a resolver later rules on.
			let bounded_grounds: BoundedVec<u8, T::MaxGroundsLen> =
				grounds.try_into().map_err(|_| Error::<T>::GroundsTooLong)?;
			ensure!(!bounded_grounds.is_empty(), Error::<T>::GroundsEmpty);
			if let Some(evidence) = evidence_hash.as_ref() {
				Self::ensure_anchored(evidence)?;
			}

			// Challenges are never removed, so the count bounds them.
			let count = ChallengeCount::<T>::get(tender_id);
			ensure!(count < T::MaxChallenges::get(), Error::<T>::TooManyChallenges);
			let cid = Self::challenge_id_for(&tender_id, count);
			ensure!(!Challenges::<T>::contains_key(tender_id, cid), Error::<T>::IdCollision);
			Challenges::<T>::insert(
				tender_id,
				cid,
				ChallengeRecord {
					challenger: challenger.clone(),
					grounds: bounded_grounds,
					evidence_hash,
					lodged_at: now,
					state: ChallengeState::Open,
					resolution: None,
					resolved_at: None,
				},
			);
			ChallengeCount::<T>::insert(tender_id, count.checked_add(1).ok_or(Error::<T>::IdOverflow)?);
			OpenChallengeCount::<T>::mutate(tender_id, |c| *c = c.saturating_add(1));

			Tenders::<T>::mutate(tender_id, |m| {
				if let Some(t) = m.as_mut() {
					t.state = TenderState::Challenged;
				}
			});

			Self::notify_participants(tender_id, TenderNotice::ChallengeLodged);
			Self::deposit_event(Event::ChallengeLodged {
				tender_id,
				challenge_id: cid,
				challenger,
			});
			Ok(())
		}

		/// Resolve a challenge. Dismissal releases execution; upholding remits the
		/// tender to re-evaluation. Either way the reasoning is permanent.
		#[pallet::call_index(15)]
		#[pallet::weight(T::WeightInfo::resolve_challenge())]
		pub fn resolve_challenge(
			origin: OriginFor<T>,
			tender_id: TenderId,
			challenge_id: ChallengeId,
			uphold: bool,
			resolution: Vec<u8>,
		) -> DispatchResult {
			T::ChallengeResolverOrigin::ensure_origin(origin)?;
			let now = frame_system::Pallet::<T>::block_number();

			let bounded_resolution: BoundedVec<u8, T::MaxResolutionLen> =
				resolution.try_into().map_err(|_| Error::<T>::ResolutionTooLong)?;
			ensure!(!bounded_resolution.is_empty(), Error::<T>::ResolutionEmpty);

			let new_state = if uphold { ChallengeState::Upheld } else { ChallengeState::Dismissed };

			Challenges::<T>::try_mutate(tender_id, challenge_id, |maybe| -> DispatchResult {
				let c = maybe.as_mut().ok_or(Error::<T>::ChallengeNotFound)?;
				ensure!(matches!(c.state, ChallengeState::Open), Error::<T>::ChallengeNotFound);
				c.state = new_state;
				c.resolution = Some(bounded_resolution.clone());
				c.resolved_at = Some(now);
				Ok(())
			})?;

			let remaining = OpenChallengeCount::<T>::mutate(tender_id, |c| {
				*c = c.saturating_sub(1);
				*c
			});

			Tenders::<T>::mutate(tender_id, |m| {
				if let Some(t) = m.as_mut() {
					// A tender cancelled while the challenge was open stays cancelled;
					// the ruling is still recorded on the challenge itself.
					if matches!(t.state, TenderState::Cancelled | TenderState::Contracted) {
						return;
					}
					if uphold {
						// Remitted for re-evaluation.
						t.state = TenderState::Evaluation;
					} else if remaining == 0 && matches!(t.state, TenderState::Challenged) {
						// Only restore the award if nothing else moved the tender on.
						// With concurrent challenges, an earlier upheld one may have
						// already remitted this to `Evaluation`; dismissing the last
						// remaining challenge must not silently undo that.
						t.state = TenderState::Awarded;
					}
				}
			});

			Self::notify_participants(tender_id, TenderNotice::ChallengeResolved);
			Self::deposit_event(Event::ChallengeResolved {
				tender_id,
				challenge_id,
				state: new_state,
			});
			Ok(())
		}

		/// Execute the award once the standstill has passed and no challenge is
		/// open: notarise the contract, hand off to delivery, release all bonds.
		#[pallet::call_index(16)]
		#[pallet::weight(T::WeightInfo::execute_award(T::MaxBidders::get()))]
		pub fn execute_award(
			origin: OriginFor<T>,
			tender_id: TenderId,
			contract_hash: Hash256,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			Self::ensure_officer(&tender, &who)?;
			// Checked before the state test: a lodged challenge moves the tender to
			// `Challenged`, and "a challenge is open" is the actionable error —
			// `BadState` would hide the real reason execution is suspended.
			ensure!(OpenChallengeCount::<T>::get(tender_id) == 0, Error::<T>::ChallengeOpen);
			ensure!(matches!(tender.state, TenderState::Awarded), Error::<T>::BadState);

			let outcome = Outcomes::<T>::get(tender_id).ok_or(Error::<T>::BadState)?;
			ensure!(now > outcome.standstill_end, Error::<T>::StandstillActive);
			// Spec §2.3: the contract is notarised via DNC.
			Self::ensure_anchored(&contract_hash)?;

			// Panel/standing-offer tenders admit every awardee to the pool now
			// that the award has survived standstill (spec §2.2).
			if matches!(tender.kind, TenderKind::Panel) {
				let panel_id = Self::panel_id_of(&tender_id);
				PanelTender::<T>::insert(panel_id, tender_id);
				for a in outcome.awardees.iter() {
					PanelPool::<T>::insert(
						panel_id,
						a,
						PanelMembership { admitted_at: now, terms_hash: contract_hash },
					);
					Self::deposit_event(Event::PanelMemberAdmitted {
						panel_id,
						supplier: a.clone(),
					});
				}
			}

			for a in outcome.awardees.iter() {
				let delivery_project = T::Delivery::instantiate(tender_id, a, contract_hash)?;
				Self::deposit_event(Event::DeliveryInstantiated {
					tender_id,
					awardee: a.clone(),
					delivery_project,
				});
			}

			Self::release_all_bonds(tender_id);

			Outcomes::<T>::mutate(tender_id, |m| {
				if let Some(o) = m.as_mut() {
					o.contract_hash = Some(contract_hash);
				}
			});
			Tenders::<T>::mutate(tender_id, |m| {
				if let Some(t) = m.as_mut() {
					t.state = TenderState::Contracted;
				}
			});

			for a in outcome.awardees.iter() {
				T::Reputation::record(tender_id, a, ReputationFact::ContractWon);
			}
			T::Reputation::record(tender_id, &tender.entity, ReputationFact::ContractAwarded);
			Self::notify_participants(tender_id, TenderNotice::ContractExecuted);
			Self::deposit_event(Event::ContractExecuted { tender_id, contract_hash });
			Ok(())
		}

		/// Cancel a tender. Terminal, reason permanently public, all bonds returned.
		///
		/// Spec §4.1 gives cancellation to the *entity authority*, not the
		/// officer: an officer who could cancel alone could quietly kill a tender
		/// that was going the wrong way for them. The entity account is expected
		/// to be the entity's governed (multisig) identity.
		#[pallet::call_index(17)]
		#[pallet::weight(T::WeightInfo::cancel_tender(T::MaxBidders::get()))]
		pub fn cancel_tender(
			origin: OriginFor<T>,
			tender_id: TenderId,
			reason_hash: Hash256,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			ensure!(who == tender.entity, Error::<T>::NotEntity);
			Self::ensure_anchored(&reason_hash)?;
			ensure!(
				!matches!(tender.state, TenderState::Contracted | TenderState::Cancelled),
				Error::<T>::BadState
			);

			// Cancellation never forfeits: bidders are not penalised for the
			// entity's decision.
			Self::release_all_bonds(tender_id);

			Tenders::<T>::mutate(tender_id, |m| {
				if let Some(t) = m.as_mut() {
					t.state = TenderState::Cancelled;
				}
			});

			// Withdrawing an unpublished draft affects nobody; cancelling a live
			// tender is the entity conduct spec §7 asks Reputation to see.
			if tender.published_at.is_some() {
				T::Reputation::record(tender_id, &tender.entity, ReputationFact::TenderCancelled);
			}
			Self::notify_participants(tender_id, TenderNotice::Cancelled);
			Self::deposit_event(Event::TenderCancelled { tender_id, reason_hash });
			Ok(())
		}

		/// Publish an EOI's shortlist, closing stage one of a multi-stage
		/// procurement (spec §2.2). A follow-on RFT created with
		/// `shortlist_from: Some(this_tender)` then admits only these suppliers.
		#[pallet::call_index(19)]
		#[pallet::weight(T::WeightInfo::publish_shortlist(suppliers.len() as u32))]
		pub fn publish_shortlist(
			origin: OriginFor<T>,
			tender_id: TenderId,
			suppliers: Vec<T::AccountId>,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			let now = frame_system::Pallet::<T>::block_number();
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			Self::ensure_officer(&tender, &who)?;

			ensure!(matches!(tender.kind, TenderKind::Eoi), Error::<T>::NotAnEoi);
			ensure!(matches!(tender.state, TenderState::Evaluation), Error::<T>::BadState);
			ensure!(!suppliers.is_empty(), Error::<T>::InvalidShortlistEntry);
			ensure!(
				suppliers.len() as u32 <= T::MaxBidders::get(),
				Error::<T>::TooManyBidders
			);

			// Only a supplier who actually responded, with a bid that survived
			// reveal validation, may be shortlisted.
            for s in suppliers.iter() {
				let reveal = Reveals::<T>::get(tender_id, s)
					.ok_or(Error::<T>::InvalidShortlistEntry)?;
				ensure!(reveal.valid, Error::<T>::InvalidShortlistEntry);
			}

			for s in suppliers.iter() {
				Shortlist::<T>::insert(tender_id, s, now);
			}
			ShortlistCount::<T>::insert(tender_id, suppliers.len() as u32);

			Tenders::<T>::mutate(tender_id, |m| {
				if let Some(t) = m.as_mut() {
					t.state = TenderState::Shortlisted;
				}
			});

			Self::deposit_event(Event::ShortlistPublished {
				tender_id,
				suppliers: suppliers.len() as u32,
			});
			Ok(())
		}

		/// Place a call-off order against a standing-offer panel.
		#[pallet::call_index(18)]
		#[pallet::weight(T::WeightInfo::call_off())]
		pub fn call_off(
			origin: OriginFor<T>,
			panel_id: PanelId,
			supplier: T::AccountId,
			order_hash: Hash256,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			// Spec §4.1 lists call_off as an Officer call. Without this any signed
			// account could place orders against someone else's standing offer.
			let tender_id = PanelTender::<T>::get(panel_id).ok_or(Error::<T>::PanelNotFound)?;
			let tender = Tenders::<T>::get(tender_id).ok_or(Error::<T>::TenderNotFound)?;
			Self::ensure_officer(&tender, &who)?;
			ensure!(
				PanelPool::<T>::contains_key(panel_id, &supplier),
				Error::<T>::NotPanelMember
			);
			Self::ensure_anchored(&order_hash)?;

			// Stored, not just emitted: the order book of a standing offer is part
			// of the public record a probity observer reads from state.
			let count = CallOffCount::<T>::get(panel_id);
			ensure!(count < T::MaxCallOffs::get(), Error::<T>::TooManyCallOffs);
			let call_off_id = Self::call_off_id_for(&panel_id, count);
			ensure!(!CallOffs::<T>::contains_key(panel_id, call_off_id), Error::<T>::IdCollision);
			CallOffs::<T>::insert(
				panel_id,
				call_off_id,
				CallOffRecord {
					supplier: supplier.clone(),
					order_hash,
					placed_by: who,
					placed_at: frame_system::Pallet::<T>::block_number(),
				},
			);
			CallOffCount::<T>::insert(panel_id, count.checked_add(1).ok_or(Error::<T>::IdOverflow)?);

			T::Notices::notify(tender_id, TenderNotice::CallOffPlaced, core::slice::from_ref(&supplier));
			Self::deposit_event(Event::CallOffPlaced { panel_id, call_off_id, supplier, order_hash });
			Ok(())
		}

		/// Replace a draft's criteria and weights. Refused once the tender is
		/// published (`CriteriaLocked`) — spec §1.1: criteria are locked before a
		/// single bid arrives.
		#[pallet::call_index(21)]
		#[pallet::weight(T::WeightInfo::amend_criteria(weights.len() as u32))]
		pub fn amend_criteria(
			origin: OriginFor<T>,
			tender_id: TenderId,
			criteria_hash: Hash256,
			weights: Vec<CriterionWeight>,
		) -> DispatchResult {
			let who = ensure_signed(origin)?;
			Tenders::<T>::try_mutate(tender_id, |maybe| -> DispatchResult {
				let tender = maybe.as_mut().ok_or(Error::<T>::TenderNotFound)?;
				Self::ensure_officer(tender, &who)?;
				ensure!(matches!(tender.state, TenderState::Draft), Error::<T>::CriteriaLocked);

				let total: u32 = weights.iter().map(|w| w.weight_percent as u32).sum();
				ensure!(total == 100, Error::<T>::WeightsInvalid);
				Self::ensure_unique_criteria(weights.iter().map(|w| &w.criterion_id))?;
				Self::ensure_anchored(&criteria_hash)?;

				tender.weights = weights.try_into().map_err(|_| Error::<T>::TooManyWeights)?;
				tender.criteria_hash = criteria_hash;
				Ok(())
			})?;
			Self::deposit_event(Event::CriteriaAmended { tender_id, criteria_hash });
			Ok(())
		}

		/// Set the deployment's procurement policy (spec §8). Governed origin
		/// only. Applies to tenders published from now on; every live tender
		/// keeps the snapshot it was published under.
		#[pallet::call_index(22)]
		#[pallet::weight(T::WeightInfo::set_policy())]
		pub fn set_policy(
			origin: OriginFor<T>,
			policy: ProcurementPolicy<BlockNumberFor<T>>,
		) -> DispatchResult {
			T::PolicyOrigin::ensure_origin(origin)?;
			Policy::<T>::put(policy);
			Self::deposit_event(Event::PolicyUpdated { policy });
			Ok(())
		}
	}

	// -------------------------------------------------------------
	// Internal helpers
	// -------------------------------------------------------------

	impl<T: Config> Pallet<T> {
		/// The canonical bid commitment preimage.
		///
		/// Public so bidders can compute the identical value off-chain before
		/// committing — the reveal is an exact-match check against this.
		pub fn compute_commitment(
			bidder: &T::AccountId,
			documents_hash: &Hash256,
			price_schedule: &BoundedVec<PriceLine<BalanceOf<T>>, T::MaxPriceLines>,
			salt: &Hash256,
		) -> Hash256 {
			let mut preimage = Vec::new();
			// The bidder is bound in so a commitment cannot be lifted and replayed
			// by another account.
			preimage.extend_from_slice(&bidder.encode());
			preimage.extend_from_slice(documents_hash);
			preimage.extend_from_slice(&price_schedule.encode());
			preimage.extend_from_slice(salt);
			sp_io::hashing::blake2_256(&preimage)
		}

		/// Entry conditions common to sealed and open submissions.
		fn ensure_can_submit(
			tender_id: TenderId,
			tender: &TenderRecordOf<T>,
			bidder: &T::AccountId,
		) -> DispatchResult {
			let now = frame_system::Pallet::<T>::block_number();
			ensure!(
				matches!(tender.state, TenderState::QaWindow | TenderState::Submission),
				Error::<T>::TenderNotPublished
			);
			// Belt and braces alongside the wheel: even if a gate has not yet been
			// processed, the block number itself closes submissions.
			ensure!(now < tender.gates.submission_close_at, Error::<T>::SubmissionClosed);
			ensure!(
				!BidCommitments::<T>::contains_key(tender_id, bidder),
				Error::<T>::CommitmentExists
			);
			// Spec §1.2 evaluator separation: whoever scores a tender cannot bid on it.
			ensure!(
				!EvaluatorSet::<T>::contains_key(tender_id, bidder),
				Error::<T>::EvaluatorIsBidder
			);
			// Multi-stage (spec §2.2): a follow-on RFT admits only the EOI's shortlist.
			if let Some(eoi) = ShortlistSource::<T>::get(tender_id) {
				ensure!(Shortlist::<T>::contains_key(eoi, bidder), Error::<T>::NotShortlisted);
			}
			ensure!(
				T::Eligibility::is_eligible(
					bidder,
					&tender.eligibility.required_credentials,
					tender.eligibility.min_reputation
				),
				Error::<T>::NotEligible
			);
			Ok(())
		}

		/// Register participation, reserve the bond, store the commitment.
		fn record_submission(
			tender_id: TenderId,
			tender: &TenderRecordOf<T>,
			bidder: &T::AccountId,
			commitment_hash: Hash256,
		) -> DispatchResult {
			let now = frame_system::Pallet::<T>::block_number();
			Participants::<T>::try_mutate(tender_id, |list| -> DispatchResult {
				list.try_push(bidder.clone()).map_err(|_| Error::<T>::TooManyBidders)?;
				Ok(())
			})?;

			let bond = tender.bond.amount;
			if !bond.is_zero() {
				T::Bonds::lock(bidder, bond).map_err(|_| Error::<T>::BondRequired)?;
			}

			BidCommitments::<T>::insert(
				tender_id,
				bidder,
				CommitmentRecord { commitment_hash, committed_at: now, bond_reserved: bond },
			);

			Self::deposit_event(Event::BidCommitted {
				tender_id,
				bidder: bidder.clone(),
				block: now,
			});
			Ok(())
		}

		/// `blake2_256(asker ‖ salt)` — the blinded question author (spec §5.3).
		///
		/// Public so an asker can reproduce it off-chain to prove authorship to a
		/// probity observer without publishing it to everyone else.
		pub fn blind_author(asker: &T::AccountId, salt: &Hash256) -> Hash256 {
			let mut preimage = Vec::new();
			preimage.extend_from_slice(&asker.encode());
			preimage.extend_from_slice(salt);
			sp_io::hashing::blake2_256(&preimage)
		}

		fn ensure_gate_order(gates: &TenderGates<BlockNumberFor<T>>) -> DispatchResult {
			ensure!(
				gates.publish_at < gates.questions_close_at
					&& gates.questions_close_at < gates.submission_close_at
					&& gates.submission_close_at <= gates.opening_at
					&& gates.opening_at < gates.opening_end_at,
				Error::<T>::GateOrderInvalid
			);
			Ok(())
		}

		fn ensure_officer(tender: &TenderRecordOf<T>, who: &T::AccountId) -> DispatchResult {
			ensure!(who == &tender.officer || who == &tender.entity, Error::<T>::NotOfficer);
			Ok(())
		}

		fn schedule(
			at: BlockNumberFor<T>,
			tender_id: TenderId,
			gate: Gate,
		) -> DispatchResult {
			DeadlineWheel::<T>::try_mutate(at, |v| -> DispatchResult {
				v.try_push((tender_id, gate)).map_err(|_| Error::<T>::DeadlineWheelFull)?;
				Ok(())
			})
		}

		/// Apply one scheduled lifecycle transition.
		fn process_gate(tender_id: TenderId, gate: Gate) {
			let Some(tender) = Tenders::<T>::get(tender_id) else { return };
			// A cancelled tender ignores every remaining deadline.
			if matches!(tender.state, TenderState::Cancelled | TenderState::Contracted) {
				return;
			}
			let now = frame_system::Pallet::<T>::block_number();

			match gate {
				Gate::QuestionsClose => {
					if matches!(tender.state, TenderState::QaWindow) {
						Tenders::<T>::mutate(tender_id, |m| {
							if let Some(t) = m.as_mut() {
								t.state = TenderState::Submission;
							}
						});
						Self::deposit_event(Event::SubmissionOpened { tender_id });
					}
				},
				Gate::SubmissionClose => {
					// An addendum may have pushed the close out; if so this entry
					// fired early and the later one will do the work.
					if now < tender.gates.submission_close_at {
						return;
					}
					if matches!(tender.state, TenderState::QaWindow | TenderState::Submission) {
						Tenders::<T>::mutate(tender_id, |m| {
							if let Some(t) = m.as_mut() {
								t.state = TenderState::Closed;
							}
						});
						Self::deposit_event(Event::TenderClosed { tender_id });
					}
				},
				Gate::OpeningEnd => {
					if now < tender.gates.opening_end_at {
						return;
					}
					if matches!(tender.state, TenderState::Opening | TenderState::Closed) {
						// Forfeiting a bond punishes a bidder for not revealing. That
						// is only fair if revealing was actually possible: `reveal_bid`
						// requires `Opening`, so if the officer never opened the
						// tender, nobody *could* reveal. Forfeiting there would let an
						// officer confiscate every bond into their own entity by simply
						// doing nothing — so the un-opened case returns bonds instead.
						if matches!(tender.state, TenderState::Opening) {
							Self::settle_non_reveals(tender_id, &tender);
						} else {
							Self::release_all_bonds(tender_id);
						}
						Tenders::<T>::mutate(tender_id, |m| {
							if let Some(t) = m.as_mut() {
								t.state = TenderState::Evaluation;
							}
						});
						Self::deposit_event(Event::EvaluationStarted { tender_id });
					}
				},
				Gate::StandstillEnd => {
					Self::deposit_event(Event::StandstillClosed { tender_id });
					// Mail only while the award stands: after an upheld challenge the
					// tender is back in evaluation and "standstill closed" would
					// mislead bidders.
					if matches!(tender.state, TenderState::Awarded) {
						Self::notify_participants(tender_id, TenderNotice::StandstillClosed);
					}
				},
			}
		}

		/// At the end of the reveal window, forfeit bonds for sealed bids that were
		/// committed but never validly revealed, where the published terms say so.
		///
		/// A mismatched reveal counts as a non-reveal (spec §8: "non-reveal and
		/// mismatch handling per published bond terms"). Otherwise a bidder who
		/// saw rivals' prices in the opening window could escape the forfeit by
		/// revealing garbage instead of nothing, and `forfeit_on_non_reveal`
		/// would bind no one.
		///
		/// Bounded by `MaxBidders` and reflected in `WeightInfo::on_initialize`.
		fn settle_non_reveals(tender_id: TenderId, tender: &TenderRecordOf<T>) {
			if !tender.bond.forfeit_on_non_reveal {
				return;
			}
			for bidder in Participants::<T>::get(tender_id).into_iter() {
				let validly_revealed =
					Reveals::<T>::get(tender_id, &bidder).map_or(false, |r| r.valid);
				if validly_revealed {
					continue;
				}
				if let Some(c) = BidCommitments::<T>::get(tender_id, &bidder) {
					if !c.bond_reserved.is_zero() {
						Self::forfeit_bond(tender_id, &bidder, c.bond_reserved);
						BidCommitments::<T>::mutate(tender_id, &bidder, |m| {
							if let Some(rec) = m.as_mut() {
								rec.bond_reserved = Zero::zero();
							}
						});
					}
				}
			}
		}

		/// Return every still-reserved bond on a tender.
		fn release_all_bonds(tender_id: TenderId) {
			for bidder in Participants::<T>::get(tender_id).into_iter() {
				if let Some(c) = BidCommitments::<T>::get(tender_id, &bidder) {
					if !c.bond_reserved.is_zero() {
						T::Bonds::release(&bidder, c.bond_reserved);
						Self::deposit_event(Event::BondReturned {
							tender_id,
							bidder: bidder.clone(),
							amount: c.bond_reserved,
						});
						BidCommitments::<T>::mutate(tender_id, &bidder, |m| {
							if let Some(rec) = m.as_mut() {
								rec.bond_reserved = Zero::zero();
							}
						});
					}
				}
			}
		}

		/// Forfeit a locked bond to the procuring entity.
		fn forfeit_bond(tender_id: TenderId, bidder: &T::AccountId, amount: BalanceOf<T>) {
			let Some(tender) = Tenders::<T>::get(tender_id) else { return };
			// `forfeit` reports what actually moved, so the event never claims a
			// bond was taken when custody failed to transfer it.
			let forfeited = T::Bonds::forfeit(bidder, &tender.entity, amount);
			if !forfeited.is_zero() {
				T::Reputation::record(tender_id, bidder, ReputationFact::BondForfeited);
			}
			Self::deposit_event(Event::BondForfeited {
				tender_id,
				bidder: bidder.clone(),
				amount: forfeited,
			});
		}

		/// Weighted ranking over averaged per-criterion scores, best first.
		///
		/// Score scale is `0 ..= MaxScore * 100` (percent weights are not divided
		/// out, so integer division never discards precision).
		fn compute_ranking(
			tender_id: TenderId,
			tender: &TenderRecordOf<T>,
		) -> Vec<(T::AccountId, u32)> {
			let mut out: Vec<(T::AccountId, u32)> = Vec::new();

			for bidder in Participants::<T>::get(tender_id).into_iter() {
				match Reveals::<T>::get(tender_id, &bidder) {
					Some(r) if r.valid => {},
					// Voided and non-revealed bids are excluded from the ranking.
					_ => continue,
				}

				let mut total: u32 = 0;
				for w in tender.weights.iter() {
					let mut sum: u32 = 0;
					let mut count: u32 = 0;
					for (_evaluator, sheet) in Scores::<T>::iter_prefix((tender_id, bidder.clone()))
					{
						if let Some(s) =
							sheet.scores.iter().find(|s| s.criterion_id == w.criterion_id)
						{
							sum = sum.saturating_add(s.score as u32);
							count = count.saturating_add(1);
						}
					}
					if count > 0 {
						let avg = sum / count;
						total = total.saturating_add(avg.saturating_mul(w.weight_percent as u32));
					}
				}
				out.push((bidder, total));
			}

			// Highest weighted score first.
			out.sort_by(|a, b| b.1.cmp(&a.1));
			out
		}

		/// `blake2_256(tag ‖ inputs)` — how every chain-minted id is formed.
		///
		/// Public so portals and auditors can recompute an id from the inputs
		/// the corresponding event and storage already expose.
		pub fn derive_id(tag: &[u8], inputs: &[u8]) -> Hash256 {
			let mut preimage = Vec::with_capacity(tag.len() + inputs.len());
			preimage.extend_from_slice(tag);
			preimage.extend_from_slice(inputs);
			sp_io::hashing::blake2_256(&preimage)
		}

		/// `blake2_256("tenderchain/tender" ‖ officer ‖ entity ‖ nonce ‖ block)`.
		///
		/// `nonce` is `TenderNonce` at creation time, so a portal can predict the
		/// id of the tender it is about to create.
		pub fn tender_id_for(
			officer: &T::AccountId,
			entity: &T::AccountId,
			nonce: u64,
			at: BlockNumberFor<T>,
		) -> TenderId {
			Self::derive_id(TENDER_ID_TAG, &(officer, entity, nonce, at).encode())
		}

		/// `blake2_256("tenderchain/question" ‖ tender_id ‖ index)`, where `index`
		/// is how many questions the tender already held.
		pub fn question_id_for(tender_id: &TenderId, index: u32) -> QuestionId {
			Self::derive_id(QUESTION_ID_TAG, &(tender_id, index).encode())
		}

		/// `blake2_256("tenderchain/challenge" ‖ tender_id ‖ index)`, where `index`
		/// is how many challenges the tender already held.
		pub fn challenge_id_for(tender_id: &TenderId, index: u32) -> ChallengeId {
			Self::derive_id(CHALLENGE_ID_TAG, &(tender_id, index).encode())
		}

		/// `blake2_256("tenderchain/calloff" ‖ panel_id ‖ index)`, where `index`
		/// is how many call-offs the panel already held.
		pub fn call_off_id_for(panel_id: &PanelId, index: u32) -> CallOffId {
			Self::derive_id(CALL_OFF_ID_TAG, &(panel_id, index).encode())
		}

		/// A panel's id is derived from the tender that established it, so the
		/// relationship is checkable without a lookup.
		pub fn panel_id_of(tender_id: &TenderId) -> PanelId {
			Self::derive_id(PANEL_ID_TAG, tender_id)
		}

		/// Spec §8 jurisdictional rules that bind a tender's schedule. `from` is
		/// the block the market starts responding: the scheduled `publish_at` at
		/// creation, the actual publication block at publish.
		fn ensure_policy(
			policy: &ProcurementPolicy<BlockNumberFor<T>>,
			gates: &TenderGates<BlockNumberFor<T>>,
			standstill_period: BlockNumberFor<T>,
			from: BlockNumberFor<T>,
		) -> DispatchResult {
			ensure!(standstill_period >= policy.min_standstill, Error::<T>::StandstillTooShort);
			ensure!(
				gates.submission_close_at.saturating_sub(from) >= policy.min_submission_period,
				Error::<T>::SubmissionPeriodTooShort
			);
			Ok(())
		}

		/// Spec §1.2: documents are referenced by hash, and the hash must name
		/// something DNC actually holds.
		fn ensure_anchored(hash: &Hash256) -> DispatchResult {
			ensure!(T::Documents::is_anchored(hash), Error::<T>::DocumentNotAnchored);
			Ok(())
		}

		/// Mail every current participant (Module 13).
		fn notify_participants(tender_id: TenderId, notice: TenderNotice) {
			let recipients = Participants::<T>::get(tender_id);
			T::Notices::notify(tender_id, notice, &recipients);
		}

		fn ensure_unique_criteria<'a>(ids: impl Iterator<Item = &'a CriterionId>) -> DispatchResult {
			let mut seen: Vec<&CriterionId> = Vec::new();
			for id in ids {
				ensure!(!seen.contains(&id), Error::<T>::DuplicateCriterion);
				seen.push(id);
			}
			Ok(())
		}
	}
}
