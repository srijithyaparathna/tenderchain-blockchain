use crate as pallet_tender_chain;
use frame_support::{derive_impl, parameter_types};
use frame_system::EnsureRoot;
use pallet_tender_chain::types::{
	DeliveryInstantiator, DocumentAnchor, Hash256, Notifier, ReputationFact, ReputationSink,
	ReserveBonds, TenderId, TenderNotice,
};
use sp_runtime::BuildStorage;

type Block = frame_system::mocking::MockBlock<Test>;
pub type Balance = u64;
pub type AccountId = u64;

#[frame_support::runtime]
mod runtime {
	#[runtime::runtime]
	#[runtime::derive(
		RuntimeCall,
		RuntimeEvent,
		RuntimeError,
		RuntimeOrigin,
		RuntimeFreezeReason,
		RuntimeHoldReason,
		RuntimeSlashReason,
		RuntimeLockId,
		RuntimeTask,
		RuntimeViewFunction
	)]
	pub struct Test;

	#[runtime::pallet_index(0)]
	pub type System = frame_system::Pallet<Test>;

	#[runtime::pallet_index(1)]
	pub type Balances = pallet_balances::Pallet<Test>;

	#[runtime::pallet_index(2)]
	pub type TenderChain = pallet_tender_chain::Pallet<Test>;
}

#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
	type Block = Block;
	type AccountData = pallet_balances::AccountData<Balance>;
}

#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
	type AccountStore = System;
	type Balance = Balance;
	// Bonds are reserved, so the mock needs a non-zero reserve capability.
	type ExistentialDeposit = ConstU64<1>;
}

use frame_support::traits::ConstU64;

parameter_types! {
	pub const MaxWeights: u32 = 10;
	pub const MaxCredentials: u32 = 8;
	pub const MaxTitleLen: u32 = 128;
	pub const MaxSummaryLen: u32 = 512;
	pub const MaxGroundsLen: u32 = 2048;
	pub const MaxResolutionLen: u32 = 2048;
	pub const MaxAddenda: u32 = 8;
	pub const MaxBidders: u32 = 16;
	pub const MaxEvaluators: u32 = 8;
	pub const MaxQuestions: u32 = 4;
	pub const MaxChallenges: u32 = 3;
	pub const MaxPriceLines: u32 = 16;
	pub const MaxCallOffs: u32 = 2;
	pub const MaxDeadlinesPerBlock: u32 = 32;
	pub const MaxTransitionsPerBlock: u32 = 8;
	pub const MinEvaluators: u32 = 2;
	pub const MinRevealWindow: u64 = 2;
	pub const MaxScore: u8 = 100;
	pub const ScoreVarianceThreshold: u8 = 30;
}

parameter_types! {
	/// Every award-to-delivery handoff the pallet requests, in order.
	pub storage DeliveryLog: Vec<(TenderId, AccountId, Hash256)> = Vec::new();
}

/// Stand-in for Module 25 (Work Task). Behaviourally identical to the `()`
/// impl — it always succeeds — but it records the handoff so Flow B can assert
/// that `execute_award` really does instantiate delivery, rather than passing
/// against a no-op that could never fail.
pub struct RecordingDelivery;

impl DeliveryInstantiator<AccountId> for RecordingDelivery {
	fn instantiate(
		tender_id: TenderId,
		awardee: &AccountId,
		contract_hash: Hash256,
	) -> Result<Option<Hash256>, sp_runtime::DispatchError> {
		let mut log = DeliveryLog::get();
		log.push((tender_id, *awardee, contract_hash));
		DeliveryLog::set(&log);
		// Stand-in Module 25 project reference, so the delivery_project field on
		// `DeliveryInstantiated` is exercised rather than always `None`.
		Ok(Some([0xD1u8; 32]))
	}
}

parameter_types! {
	/// Every notice sent, as (tender, notice, recipients).
	pub storage NoticeLog: Vec<(TenderId, TenderNotice, Vec<AccountId>)> = Vec::new();
	/// Every reputation fact reported, as (tender, account, fact).
	pub storage ReputationLog: Vec<(TenderId, AccountId, ReputationFact)> = Vec::new();
}

/// Stand-in for Module 13: records instead of mailing.
pub struct RecordingNotifier;

impl Notifier<AccountId> for RecordingNotifier {
	fn notify(tender_id: TenderId, notice: TenderNotice, recipients: &[AccountId]) {
		let mut log = NoticeLog::get();
		log.push((tender_id, notice, recipients.to_vec()));
		NoticeLog::set(&log);
	}
}

/// Stand-in for Module 10: records instead of scoring.
pub struct RecordingReputation;

impl ReputationSink<AccountId> for RecordingReputation {
	fn record(tender_id: TenderId, who: &AccountId, fact: ReputationFact) {
		let mut log = ReputationLog::get();
		log.push((tender_id, *who, fact));
		ReputationLog::set(&log);
	}
}

/// A hash DNC does not hold. Everything else counts as anchored.
pub const UNANCHORED: Hash256 = [0xBAu8; 32];

/// Stand-in for Module 2: rejects exactly `UNANCHORED`, so tests can prove the
/// anchoring check is wired without changing any other hash.
pub struct MockDnc;

impl DocumentAnchor for MockDnc {
	fn is_anchored(hash: &Hash256) -> bool {
		hash != &UNANCHORED
	}
}

impl pallet_tender_chain::Config for Test {
	type RuntimeEvent = RuntimeEvent;
	type Currency = Balances;
	type Bonds = ReserveBonds<Balances>;
	// Spec §8 demands a governed origin; root stands in for a Multisig here.
	type AwardOrigin = EnsureRoot<AccountId>;
	type ChallengeResolverOrigin = EnsureRoot<AccountId>;
	type PolicyOrigin = EnsureRoot<AccountId>;
	// Permissive stubs — Modules 15/10 and 25 do not exist yet.
	type Eligibility = ();
	type Delivery = RecordingDelivery;
	type Notices = RecordingNotifier;
	type Documents = MockDnc;
	type Reputation = RecordingReputation;
	type MaxWeights = MaxWeights;
	type MaxCredentials = MaxCredentials;
	type MaxTitleLen = MaxTitleLen;
	type MaxSummaryLen = MaxSummaryLen;
	type MaxGroundsLen = MaxGroundsLen;
	type MaxResolutionLen = MaxResolutionLen;
	type MaxAddenda = MaxAddenda;
	type MaxBidders = MaxBidders;
	type MaxEvaluators = MaxEvaluators;
	type MaxQuestions = MaxQuestions;
	type MaxChallenges = MaxChallenges;
	type MaxPriceLines = MaxPriceLines;
	type MaxCallOffs = MaxCallOffs;
	type MaxDeadlinesPerBlock = MaxDeadlinesPerBlock;
	type MaxTransitionsPerBlock = MaxTransitionsPerBlock;
	type MinRevealWindow = MinRevealWindow;
	type MinEvaluators = MinEvaluators;
	type MaxScore = MaxScore;
	type ScoreVarianceThreshold = ScoreVarianceThreshold;
	type WeightInfo = ();
}

/// Build genesis storage, funding a spread of accounts so bonds can be reserved.
pub fn new_test_ext() -> sp_io::TestExternalities {
	let mut t = frame_system::GenesisConfig::<Test>::default().build_storage().unwrap();
	pallet_balances::GenesisConfig::<Test> {
		balances: (1..=20u64).map(|i| (i, 1_000_000)).collect(),
		..Default::default()
	}
	.assimilate_storage(&mut t)
	.unwrap();
	let mut ext: sp_io::TestExternalities = t.into();
	ext.execute_with(|| System::set_block_number(1));
	ext
}

/// Advance to `n`, running `on_initialize` for every block in between so the
/// deadline wheel actually fires.
pub fn run_to_block(n: u64) {
	use frame_support::traits::OnInitialize;
	while System::block_number() < n {
		let next = System::block_number() + 1;
		System::set_block_number(next);
		TenderChain::on_initialize(next);
	}
}
