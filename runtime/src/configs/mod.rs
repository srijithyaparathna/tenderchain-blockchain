// This is free and unencumbered software released into the public domain.
//
// Anyone is free to copy, modify, publish, use, compile, sell, or
// distribute this software, either in source code form or as a compiled
// binary, for any purpose, commercial or non-commercial, and by any
// means.
//
// In jurisdictions that recognize copyright laws, the author or authors
// of this software dedicate any and all copyright interest in the
// software to the public domain. We make this dedication for the benefit
// of the public at large and to the detriment of our heirs and
// successors. We intend this dedication to be an overt act of
// relinquishment in perpetuity of all present and future rights to this
// software under copyright law.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
// IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR
// OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
// ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
// OTHER DEALINGS IN THE SOFTWARE.
//
// For more information, please refer to <http://unlicense.org>

// Substrate and Polkadot dependencies
use frame_support::{
	derive_impl, parameter_types,
	traits::{ConstBool, ConstU128, ConstU32, ConstU64, ConstU8, VariantCountOf},
	weights::{
		constants::{RocksDbWeight, WEIGHT_REF_TIME_PER_SECOND},
		IdentityFee, Weight,
	},
};
use frame_system::limits::{BlockLength, BlockWeights};
use pallet_transaction_payment::{ConstFeeMultiplier, FungibleAdapter, Multiplier};
use sp_consensus_aura::sr25519::AuthorityId as AuraId;
use sp_runtime::{traits::One, Perbill};
use sp_version::RuntimeVersion;

// Local module imports
use super::{
	AccountId, Aura, Balance, Balances, Block, BlockNumber, Hash, Nonce, PalletInfo, Runtime,
	RuntimeCall, RuntimeEvent, RuntimeFreezeReason, RuntimeHoldReason, RuntimeOrigin, RuntimeTask,
	System, EXISTENTIAL_DEPOSIT, SLOT_DURATION, VERSION,
};

const NORMAL_DISPATCH_RATIO: Perbill = Perbill::from_percent(75);

parameter_types! {
	pub const BlockHashCount: BlockNumber = 2400;
	pub const Version: RuntimeVersion = VERSION;

	/// We allow for 2 seconds of compute with a 6 second average block time.
	pub RuntimeBlockWeights: BlockWeights = BlockWeights::with_sensible_defaults(
		Weight::from_parts(2u64 * WEIGHT_REF_TIME_PER_SECOND, u64::MAX),
		NORMAL_DISPATCH_RATIO,
	);
	pub RuntimeBlockLength: BlockLength = BlockLength::max_with_normal_ratio(5 * 1024 * 1024, NORMAL_DISPATCH_RATIO);
	pub const SS58Prefix: u8 = 42;
}

/// The default types are being injected by [`derive_impl`](`frame_support::derive_impl`) from
/// [`SoloChainDefaultConfig`](`struct@frame_system::config_preludes::SolochainDefaultConfig`),
/// but overridden as needed.
#[derive_impl(frame_system::config_preludes::SolochainDefaultConfig)]
impl frame_system::Config for Runtime {
	/// The block type for the runtime.
	type Block = Block;
	/// Block & extrinsics weights: base values and limits.
	type BlockWeights = RuntimeBlockWeights;
	/// The maximum length of a block (in bytes).
	type BlockLength = RuntimeBlockLength;
	/// The identifier used to distinguish between accounts.
	type AccountId = AccountId;
	/// The type for storing how many extrinsics an account has signed.
	type Nonce = Nonce;
	/// The type for hashing blocks and tries.
	type Hash = Hash;
	/// Maximum number of block number to block hash mappings to keep (oldest pruned first).
	type BlockHashCount = BlockHashCount;
	/// The weight of database operations that the runtime can invoke.
	type DbWeight = RocksDbWeight;
	/// Version of the runtime.
	type Version = Version;
	/// The data to be stored in an account.
	type AccountData = pallet_balances::AccountData<Balance>;
	/// This is used as an identifier of the chain. 42 is the generic substrate prefix.
	type SS58Prefix = SS58Prefix;
	type MaxConsumers = frame_support::traits::ConstU32<16>;
}

impl pallet_aura::Config for Runtime {
	type AuthorityId = AuraId;
	type DisabledValidators = ();
	type MaxAuthorities = ConstU32<32>;
	type AllowMultipleBlocksPerSlot = ConstBool<false>;
	type SlotDuration = pallet_aura::MinimumPeriodTimesTwo<Runtime>;
}

impl pallet_grandpa::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;

	type WeightInfo = ();
	type MaxAuthorities = ConstU32<32>;
	type MaxNominators = ConstU32<0>;
	type MaxSetIdSessionEntries = ConstU64<0>;

	type KeyOwnerProof = sp_core::Void;
	type EquivocationReportSystem = ();
}

impl pallet_timestamp::Config for Runtime {
	/// A timestamp: milliseconds since the unix epoch.
	type Moment = u64;
	type OnTimestampSet = Aura;
	type MinimumPeriod = ConstU64<{ SLOT_DURATION / 2 }>;
	type WeightInfo = ();
}

impl pallet_balances::Config for Runtime {
	type MaxLocks = ConstU32<50>;
	type MaxReserves = ();
	type ReserveIdentifier = [u8; 8];
	/// The type for recording an account's balance.
	type Balance = Balance;
	/// The ubiquitous event type.
	type RuntimeEvent = RuntimeEvent;
	type DustRemoval = ();
	type ExistentialDeposit = ConstU128<EXISTENTIAL_DEPOSIT>;
	type AccountStore = System;
	type WeightInfo = pallet_balances::weights::SubstrateWeight<Runtime>;
	type FreezeIdentifier = RuntimeFreezeReason;
	type MaxFreezes = VariantCountOf<RuntimeFreezeReason>;
	type RuntimeHoldReason = RuntimeHoldReason;
	type RuntimeFreezeReason = RuntimeFreezeReason;
	type DoneSlashHandler = ();
}

parameter_types! {
	pub FeeMultiplier: Multiplier = Multiplier::one();
}

impl pallet_transaction_payment::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type OnChargeTransaction = FungibleAdapter<Balances, ()>;
	type OperationalFeeMultiplier = ConstU8<5>;
	type WeightToFee = IdentityFee<Balance>;
	type LengthToFee = IdentityFee<Balance>;
	type FeeMultiplierUpdate = ConstFeeMultiplier<FeeMultiplier>;
	type WeightInfo = pallet_transaction_payment::weights::SubstrateWeight<Runtime>;
}

impl pallet_sudo::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type RuntimeCall = RuntimeCall;
	type WeightInfo = pallet_sudo::weights::SubstrateWeight<Runtime>;
}

/// Configure the pallet-template in pallets/template.
impl pallet_template::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type WeightInfo = pallet_template::weights::SubstrateWeight<Runtime>;
}

parameter_types! {
	/// Bounds for Module 26 (TenderChain). Every collection in the pallet is
	/// bounded — spec §8 forbids unbounded storage.
	pub const TenderMaxWeights: u32 = 20;
	pub const TenderMaxCredentials: u32 = 16;
	pub const TenderMaxAddenda: u32 = 32;
	pub const TenderMaxBidders: u32 = 128;
	pub const TenderMaxEvaluators: u32 = 16;
	pub const TenderMaxQuestions: u32 = 256;
	pub const TenderMaxChallenges: u32 = 32;
	pub const TenderMaxPriceLines: u32 = 64;
	pub const TenderMaxDeadlinesPerBlock: u32 = 64;
	pub const TenderMaxTransitionsPerBlock: u32 = 16;
	/// Spec §5.2: ghost evaluation is defeated by attribution, which needs a
	/// panel rather than a single scorer.
	pub const TenderMinEvaluators: u32 = 3;
	/// Blocks that must remain for bidders to reveal after the officer opens.
	/// At 6s blocks this is ~10 minutes.
	pub const TenderMinRevealWindow: BlockNumber = 100;
	pub const TenderMaxScore: u8 = 100;
	/// Spread between two evaluators on one criterion that auto-flags for
	/// probity review (spec §5.2).
	pub const TenderScoreVarianceThreshold: u8 = 30;
}

/// Configure Module 26 (TenderChain) in pallets/tender-chain.
///
/// NOTE FOR PRODUCTION DEPLOYMENT — spec §8 requires that the award authority be
/// a *governed* origin (Multisig/governance), never a single key. `EnsureRoot`
/// satisfies "not an ordinary signed officer" and is the correct stand-in until
/// Module 16 (Multisig) lands, at which point both origins below should be
/// repointed at the deployment's governance origin. The pallet cannot verify
/// this property itself; it is the runtime's responsibility.
impl pallet_tender_chain::Config for Runtime {
	type RuntimeEvent = RuntimeEvent;
	type Currency = Balances;
	type TenderId = u32;
	type PanelId = u32;
	type AwardOrigin = frame_system::EnsureRoot<AccountId>;
	type ChallengeResolverOrigin = frame_system::EnsureRoot<AccountId>;
	// Modules 15 (Identity) / 10 (Reputation) and 25 (Work Task) are not in this
	// runtime yet; the permissive no-op impls stand in. Wiring them later is a
	// change to these two lines only.
	type Eligibility = ();
	type Delivery = ();
	type MaxWeights = TenderMaxWeights;
	type MaxCredentials = TenderMaxCredentials;
	type MaxAddenda = TenderMaxAddenda;
	type MaxBidders = TenderMaxBidders;
	type MaxEvaluators = TenderMaxEvaluators;
	type MaxQuestions = TenderMaxQuestions;
	type MaxChallenges = TenderMaxChallenges;
	type MaxPriceLines = TenderMaxPriceLines;
	type MaxDeadlinesPerBlock = TenderMaxDeadlinesPerBlock;
	type MaxTransitionsPerBlock = TenderMaxTransitionsPerBlock;
	type MinRevealWindow = TenderMinRevealWindow;
	type MinEvaluators = TenderMinEvaluators;
	type MaxScore = TenderMaxScore;
	type ScoreVarianceThreshold = TenderScoreVarianceThreshold;
	// Real benchmarked weights, measured against this runtime's own bounds.
	type WeightInfo = crate::weights::pallet_tender_chain::WeightInfo<Runtime>;
}
