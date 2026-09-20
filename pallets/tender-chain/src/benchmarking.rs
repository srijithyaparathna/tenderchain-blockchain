//! Benchmarking for pallet-tender-chain (spec §9: "benchmarks complete").
//!
//! Each benchmark drives the tender through the real lifecycle rather than
//! writing storage directly, so the measured extrinsic sees the same state a
//! production call would: gates processed by `on_initialize`, bonds reserved,
//! evaluators activated behind lodged conflict declarations.
//!
//! Linear components are chosen to be the quantities the extrinsic actually
//! loops over:
//!
//! | component | meaning                                    |
//! |-----------|--------------------------------------------|
//! | `w`       | weighted criteria on the tender            |
//! | `p`       | price-schedule lines in a revealed bid     |
//! | `c`       | criteria scored in one scoresheet          |
//! | `b`       | bidders ranked / bonds released            |
//! | `n`       | gate transitions processed in one block    |
//!
//! Where the pallet's `#[pallet::weight]` annotation passes `MaxBidders` rather
//! than the runtime value (`award`, `execute_award`, `cancel_tender`), the
//! benchmark still varies `b` across the full range so the fitted per-bidder
//! coefficient is measured honestly; the annotation then charges the worst case.

#![cfg(feature = "runtime-benchmarks")]

use super::*;
use crate::Pallet as TenderChain;
use alloc::{vec, vec::Vec};
use frame_benchmarking::v2::*;
use frame_support::traits::Currency;
use frame_system::{pallet_prelude::BlockNumberFor, RawOrigin};

const SEED: u32 = 0;

/// A non-zero currency unit, whatever the runtime's existential deposit is.
fn unit<T: Config>() -> BalanceOf<T> {
	let m = T::Currency::minimum_balance();
	if m.is_zero() {
		One::one()
	} else {
		m
	}
}

/// Bid bond used throughout. Non-zero so the reserve/unreserve paths are
/// actually exercised.
fn bond_amount<T: Config>() -> BalanceOf<T> {
	unit::<T>().saturating_mul(10u32.into())
}

fn fund<T: Config>(who: &T::AccountId) {
	let big = unit::<T>().saturating_mul(1_000_000u32.into());
	let _ = T::Currency::make_free_balance_be(who, big);
}

fn funded_account<T: Config>(name: &'static str, index: u32) -> T::AccountId {
	let who: T::AccountId = account(name, index, SEED);
	fund::<T>(&who);
	who
}

/// `w` criteria whose weights sum to exactly 100, as `create_tender` demands.
fn criteria(w: u32) -> Vec<CriterionWeight> {
	let rest = w.saturating_sub(1);
	let head = 100u32.saturating_sub(rest);
	let mut v = vec![CriterionWeight { criterion_id: 0, weight_percent: head as u8 }];
	for i in 1..w {
		v.push(CriterionWeight { criterion_id: i, weight_percent: 1 });
	}
	v
}

/// Gate schedule shared by every benchmark:
/// publish 2 → questions close 10 → submissions close 20 → opening 20 →
/// opening ends 30, with a 10-block standstill after award.
fn gates<T: Config>() -> TenderGates<BlockNumberFor<T>> {
	let open_at: BlockNumberFor<T> = SUBMISSION_CLOSE.into();
	TenderGates {
		publish_at: 2u32.into(),
		questions_close_at: QUESTIONS_CLOSE.into(),
		submission_close_at: open_at,
		opening_at: open_at,
		opening_end_at: opening_end::<T>(),
	}
}

/// The reveal window has to clear `MinRevealWindow`, which is a runtime
/// constant, so the opening deadline is derived rather than hard-coded.
fn opening_end<T: Config>() -> BlockNumberFor<T> {
	let open_at: BlockNumberFor<T> = SUBMISSION_CLOSE.into();
	open_at.saturating_add(T::MinRevealWindow::get()).saturating_add(5u32.into())
}

const QUESTIONS_CLOSE: u32 = 10;
const SUBMISSION_CLOSE: u32 = 20;
const STANDSTILL: u32 = 10;

/// Run the chain forward, firing `on_initialize` for every block so the
/// deadline wheel actually drives the state machine (spec §8).
fn advance_to<T: Config>(n: u32) {
	let target: BlockNumberFor<T> = n.into();
	let mut cur = frame_system::Pallet::<T>::block_number();
	while cur < target {
		cur = cur.saturating_add(One::one());
		frame_system::Pallet::<T>::set_block_number(cur);
		TenderChain::<T>::on_initialize(cur);
	}
}

fn advance_to_bn<T: Config>(target: BlockNumberFor<T>) {
	let mut cur = frame_system::Pallet::<T>::block_number();
	while cur < target {
		cur = cur.saturating_add(One::one());
		frame_system::Pallet::<T>::set_block_number(cur);
		TenderChain::<T>::on_initialize(cur);
	}
}

fn start<T: Config>() {
	frame_system::Pallet::<T>::set_block_number(One::one());
}

/// Create a tender in `Draft` and return its id.
fn create<T: Config>(
	officer: &T::AccountId,
	kind: TenderKind,
	w: u32,
) -> Result<T::TenderId, BenchmarkError> {
	let raw = NextTenderId::<T>::get();
	TenderChain::<T>::create_tender(
		RawOrigin::Signed(officer.clone()).into(),
		officer.clone(),
		kind,
		BidMode::Sealed,
		[1u8; 32],
		[2u8; 32],
		criteria(w),
		gates::<T>(),
		STANDSTILL.into(),
		Vec::new(),
		0,
		BondTerms {
			amount: bond_amount::<T>(),
			forfeit_on_non_reveal: true,
			forfeit_on_withdrawal: false,
		},
		false,
		None,
	)?;
	Ok(raw.into())
}

fn create_and_publish<T: Config>(
	officer: &T::AccountId,
	kind: TenderKind,
	w: u32,
) -> Result<T::TenderId, BenchmarkError> {
	let id = create::<T>(officer, kind, w)?;
	TenderChain::<T>::publish_tender(RawOrigin::Signed(officer.clone()).into(), id)?;
	Ok(id)
}

fn price_lines<T: Config>(p: u32) -> Vec<PriceLine<BalanceOf<T>>> {
	(0..p)
		.map(|i| PriceLine { item_id: i, amount: unit::<T>().saturating_mul((i + 1).into()) })
		.collect()
}

fn bounded_prices<T: Config>(
	v: Vec<PriceLine<BalanceOf<T>>>,
) -> BoundedVec<PriceLine<BalanceOf<T>>, T::MaxPriceLines> {
	v.try_into().expect("benchmark price schedule within MaxPriceLines; qed")
}

/// Commit a bid that will reveal validly.
fn commit<T: Config>(
	tender_id: T::TenderId,
	bidder: &T::AccountId,
	p: u32,
) -> Result<(), BenchmarkError> {
	let prices = bounded_prices::<T>(price_lines::<T>(p));
	let hash = TenderChain::<T>::compute_commitment(bidder, &[7u8; 32], &prices, &[9u8; 32]);
	TenderChain::<T>::commit_bid(RawOrigin::Signed(bidder.clone()).into(), tender_id, hash)?;
	Ok(())
}

fn reveal<T: Config>(
	tender_id: T::TenderId,
	bidder: &T::AccountId,
	p: u32,
) -> Result<(), BenchmarkError> {
	TenderChain::<T>::reveal_bid(
		RawOrigin::Signed(bidder.clone()).into(),
		tender_id,
		[7u8; 32],
		price_lines::<T>(p),
		[9u8; 32],
	)?;
	Ok(())
}

/// Size of the evaluation panel: the runtime's minimum, never below one.
fn panel_size<T: Config>() -> u32 {
	let n = T::MinEvaluators::get();
	if n == 0 {
		1
	} else {
		n
	}
}

fn appoint_and_activate<T: Config>(
	tender_id: T::TenderId,
	officer: &T::AccountId,
	evaluator: &T::AccountId,
) -> Result<(), BenchmarkError> {
	TenderChain::<T>::appoint_evaluator(
		RawOrigin::Signed(officer.clone()).into(),
		tender_id,
		evaluator.clone(),
		[3u8; 32],
	)?;
	TenderChain::<T>::declare_conflict(
		RawOrigin::Signed(evaluator.clone()).into(),
		tender_id,
		[4u8; 32],
	)?;
	TenderChain::<T>::activate_evaluator(
		RawOrigin::Signed(officer.clone()).into(),
		tender_id,
		evaluator.clone(),
	)?;
	Ok(())
}

fn scoresheet(w: u32) -> Vec<CriterionScore> {
	(0..w).map(|i| CriterionScore { criterion_id: i, score: 50 }).collect()
}

/// Drive a tender all the way to `Evaluation` with `b` bidders holding valid
/// reveals and a full evaluator panel activated.
///
/// `scoring` controls how many of those evaluators have already submitted
/// scoresheets — `submit_scores` leaves one unscored so the measured call is a
/// genuine first-time insert that still walks the variance-comparison loop.
struct Setup<T: Config> {
	tender_id: T::TenderId,
	bidders: Vec<T::AccountId>,
	evaluators: Vec<T::AccountId>,
}

fn setup_to_evaluation<T: Config>(
	officer: &T::AccountId,
	kind: TenderKind,
	w: u32,
	b: u32,
	scoring: u32,
) -> Result<Setup<T>, BenchmarkError> {
	let tender_id = create_and_publish::<T>(officer, kind, w)?;

	let bidders: Vec<T::AccountId> = (0..b).map(|i| funded_account::<T>("bidder", i)).collect();
	for bidder in bidders.iter() {
		commit::<T>(tender_id, bidder, 1)?;
	}

	advance_to::<T>(SUBMISSION_CLOSE);
	TenderChain::<T>::open_tender(RawOrigin::Signed(officer.clone()).into(), tender_id)?;
	for bidder in bidders.iter() {
		reveal::<T>(tender_id, bidder, 1)?;
	}

	advance_to_bn::<T>(opening_end::<T>());

	let evaluators: Vec<T::AccountId> =
		(0..panel_size::<T>()).map(|i| funded_account::<T>("evaluator", i)).collect();
	for e in evaluators.iter() {
		appoint_and_activate::<T>(tender_id, officer, e)?;
	}

	for e in evaluators.iter().take(scoring as usize) {
		for bidder in bidders.iter() {
			TenderChain::<T>::submit_scores(
				RawOrigin::Signed(e.clone()).into(),
				tender_id,
				bidder.clone(),
				scoresheet(w),
				[5u8; 32],
			)?;
		}
	}

	Ok(Setup { tender_id, bidders, evaluators })
}

/// Award the tender to its first bidder and return the outcome's standstill end.
fn do_award<T: Config>(
	tender_id: T::TenderId,
	awardee: &T::AccountId,
) -> Result<(), BenchmarkError> {
	let origin = T::AwardOrigin::try_successful_origin()
		.map_err(|_| BenchmarkError::Stop("no successful AwardOrigin"))?;
	TenderChain::<T>::award(origin, tender_id, vec![awardee.clone()], [6u8; 32])?;
	Ok(())
}

#[benchmarks]
mod benchmarks {
	use super::*;

	#[benchmark]
	fn create_tender(w: Linear<1, { T::MaxWeights::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let raw = NextTenderId::<T>::get();

		#[extrinsic_call]
		_(
			RawOrigin::Signed(officer.clone()),
			officer.clone(),
			TenderKind::Rft,
			BidMode::Sealed,
			[1u8; 32],
			[2u8; 32],
			criteria(w),
			gates::<T>(),
			STANDSTILL.into(),
			Vec::new(),
			0,
			BondTerms {
				amount: bond_amount::<T>(),
				forfeit_on_non_reveal: true,
				forfeit_on_withdrawal: false,
			},
			false,
			None,
		);

		let id: T::TenderId = raw.into();
		assert!(Tenders::<T>::get(id).is_some());
		Ok(())
	}

	#[benchmark]
	fn publish_tender() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let id = create::<T>(&officer, TenderKind::Rft, T::MaxWeights::get())?;

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), id);

		let t = Tenders::<T>::get(id).expect("created; qed");
		assert!(t.published_at.is_some());
		Ok(())
	}

	#[benchmark]
	fn ask_question() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let bidder: T::AccountId = whitelisted_caller();
		fund::<T>(&bidder);

		#[extrinsic_call]
		_(RawOrigin::Signed(bidder), id, [11u8; 32], [12u8; 32]);

		assert!(Questions::<T>::contains_key(id, 0));
		Ok(())
	}

	#[benchmark]
	fn answer_question() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let bidder = funded_account::<T>("bidder", 0);
		TenderChain::<T>::ask_question(
			RawOrigin::Signed(bidder).into(),
			id,
			[11u8; 32],
			[12u8; 32],
		)?;

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), id, 0u32, [12u8; 32]);

		let q = Questions::<T>::get(id, 0u32).expect("asked; qed");
		assert!(q.answer_hash.is_some());
		Ok(())
	}

	#[benchmark]
	fn publish_addendum() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		// Worst case: the addendum also extends the close block, which touches
		// the deadline wheel as well as the addenda list.
		let extended: BlockNumberFor<T> = (SUBMISSION_CLOSE + 5).into();

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), id, [13u8; 32], Some(extended));

		assert_eq!(Addenda::<T>::get(id).len(), 1);
		Ok(())
	}

	#[benchmark]
	fn commit_bid() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let bidder: T::AccountId = whitelisted_caller();
		fund::<T>(&bidder);
		let prices = bounded_prices::<T>(price_lines::<T>(1));
		let hash = TenderChain::<T>::compute_commitment(&bidder, &[7u8; 32], &prices, &[9u8; 32]);

		#[extrinsic_call]
		_(RawOrigin::Signed(bidder.clone()), id, hash);

		assert!(BidCommitments::<T>::contains_key(id, &bidder));
		Ok(())
	}

	#[benchmark]
	fn withdraw_commitment() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let bidder: T::AccountId = whitelisted_caller();
		fund::<T>(&bidder);
		commit::<T>(id, &bidder, 1)?;

		#[extrinsic_call]
		_(RawOrigin::Signed(bidder.clone()), id);

		assert!(!BidCommitments::<T>::contains_key(id, &bidder));
		Ok(())
	}

	#[benchmark]
	fn open_tender() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let bidder = funded_account::<T>("bidder", 0);
		commit::<T>(id, &bidder, 1)?;
		advance_to::<T>(SUBMISSION_CLOSE);

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), id);

		let t = Tenders::<T>::get(id).expect("created; qed");
		assert!(matches!(t.state, TenderState::Opening));
		Ok(())
	}

	#[benchmark]
	fn reveal_bid(p: Linear<1, { T::MaxPriceLines::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let bidder: T::AccountId = whitelisted_caller();
		fund::<T>(&bidder);
		commit::<T>(id, &bidder, p)?;
		advance_to::<T>(SUBMISSION_CLOSE);
		TenderChain::<T>::open_tender(RawOrigin::Signed(officer).into(), id)?;

		#[extrinsic_call]
		_(RawOrigin::Signed(bidder.clone()), id, [7u8; 32], price_lines::<T>(p), [9u8; 32]);

		let r = Reveals::<T>::get(id, &bidder).expect("revealed; qed");
		assert!(r.valid);
		Ok(())
	}

	#[benchmark]
	fn appoint_evaluator() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let evaluator = funded_account::<T>("evaluator", 0);

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), id, evaluator.clone(), [3u8; 32]);

		assert!(EvaluatorSet::<T>::contains_key(id, &evaluator));
		Ok(())
	}

	#[benchmark]
	fn declare_conflict() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let evaluator: T::AccountId = whitelisted_caller();
		fund::<T>(&evaluator);
		TenderChain::<T>::appoint_evaluator(
			RawOrigin::Signed(officer).into(),
			id,
			evaluator.clone(),
			[3u8; 32],
		)?;

		#[extrinsic_call]
		_(RawOrigin::Signed(evaluator.clone()), id, [4u8; 32]);

		let rec = EvaluatorSet::<T>::get(id, &evaluator).expect("appointed; qed");
		assert!(rec.conflict_declaration.is_some());
		Ok(())
	}

	#[benchmark]
	fn activate_evaluator() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		let evaluator = funded_account::<T>("evaluator", 0);
		TenderChain::<T>::appoint_evaluator(
			RawOrigin::Signed(officer.clone()).into(),
			id,
			evaluator.clone(),
			[3u8; 32],
		)?;
		TenderChain::<T>::declare_conflict(
			RawOrigin::Signed(evaluator.clone()).into(),
			id,
			[4u8; 32],
		)?;

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), id, evaluator.clone());

		let rec = EvaluatorSet::<T>::get(id, &evaluator).expect("appointed; qed");
		assert!(rec.active);
		Ok(())
	}

	#[benchmark]
	fn submit_scores(c: Linear<1, { T::MaxWeights::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		// Every evaluator but the last has already scored, so the measured call
		// walks the full variance-comparison loop before inserting.
		let already = panel_size::<T>().saturating_sub(1);
		let setup = setup_to_evaluation::<T>(&officer, TenderKind::Rft, c, 1, already)?;
		let last = setup.evaluators.last().expect("panel non-empty; qed").clone();
		let bidder = setup.bidders[0].clone();

		#[extrinsic_call]
		_(RawOrigin::Signed(last.clone()), setup.tender_id, bidder.clone(), scoresheet(c), [5u8; 32]);

		assert!(Scores::<T>::contains_key((setup.tender_id, bidder, last)));
		Ok(())
	}

	#[benchmark]
	fn award(b: Linear<1, { T::MaxBidders::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let w = T::MaxWeights::get();
		let setup =
			setup_to_evaluation::<T>(&officer, TenderKind::Rft, w, b, panel_size::<T>())?;
		let awardee = setup.bidders[0].clone();
		let origin = T::AwardOrigin::try_successful_origin()
			.map_err(|_| BenchmarkError::Stop("no successful AwardOrigin"))?;

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, setup.tender_id, vec![awardee], [6u8; 32]);

		let outcome = Outcomes::<T>::get(setup.tender_id).expect("awarded; qed");
		assert_eq!(outcome.ranking.len() as u32, b);
		Ok(())
	}

	#[benchmark]
	fn lodge_challenge() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let setup =
			setup_to_evaluation::<T>(&officer, TenderKind::Rft, 2, 2, panel_size::<T>())?;
		do_award::<T>(setup.tender_id, &setup.bidders[0])?;
		// The unsuccessful bidder is the one with standing to challenge.
		let challenger = setup.bidders[1].clone();

		#[extrinsic_call]
		_(RawOrigin::Signed(challenger), setup.tender_id, [14u8; 32]);

		assert_eq!(OpenChallengeCount::<T>::get(setup.tender_id), 1);
		Ok(())
	}

	#[benchmark]
	fn resolve_challenge() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		let setup =
			setup_to_evaluation::<T>(&officer, TenderKind::Rft, 2, 2, panel_size::<T>())?;
		do_award::<T>(setup.tender_id, &setup.bidders[0])?;
		TenderChain::<T>::lodge_challenge(
			RawOrigin::Signed(setup.bidders[1].clone()).into(),
			setup.tender_id,
			[14u8; 32],
		)?;
		let origin = T::ChallengeResolverOrigin::try_successful_origin()
			.map_err(|_| BenchmarkError::Stop("no successful ChallengeResolverOrigin"))?;

		#[extrinsic_call]
		_(origin as T::RuntimeOrigin, setup.tender_id, 0u32, false, [15u8; 32]);

		assert_eq!(OpenChallengeCount::<T>::get(setup.tender_id), 0);
		Ok(())
	}

	#[benchmark]
	fn execute_award(b: Linear<1, { T::MaxBidders::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let w = T::MaxWeights::get();
		let setup =
			setup_to_evaluation::<T>(&officer, TenderKind::Rft, w, b, panel_size::<T>())?;
		do_award::<T>(setup.tender_id, &setup.bidders[0])?;
		// Standstill runs from the award, which happened at `opening_end`.
		let after = opening_end::<T>()
			.saturating_add(STANDSTILL.into())
			.saturating_add(One::one());
		advance_to_bn::<T>(after);

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), setup.tender_id, [16u8; 32]);

		let t = Tenders::<T>::get(setup.tender_id).expect("created; qed");
		assert!(matches!(t.state, TenderState::Contracted));
		Ok(())
	}

	#[benchmark]
	fn cancel_tender(b: Linear<1, { T::MaxBidders::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		let id = create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		// `b` reserved bonds to unwind — the loop cancellation actually walks.
		for i in 0..b {
			let bidder = funded_account::<T>("bidder", i);
			commit::<T>(id, &bidder, 1)?;
		}

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), id, [17u8; 32]);

		let t = Tenders::<T>::get(id).expect("created; qed");
		assert!(matches!(t.state, TenderState::Cancelled));
		Ok(())
	}

	#[benchmark]
	fn call_off() -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		// A Panel award admits its awardees to the standing-offer pool.
		let setup =
			setup_to_evaluation::<T>(&officer, TenderKind::Panel, 2, 1, panel_size::<T>())?;
		let supplier = setup.bidders[0].clone();
		do_award::<T>(setup.tender_id, &supplier)?;
		let panel_id: T::PanelId = NextTenderId::<T>::get().saturating_sub(1).into();

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), panel_id, supplier.clone(), [18u8; 32]);

		assert!(PanelPool::<T>::contains_key(panel_id, &supplier));
		Ok(())
	}

	#[benchmark]
	fn publish_shortlist(s: Linear<1, { T::MaxBidders::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer: T::AccountId = whitelisted_caller();
		fund::<T>(&officer);
		// An EOI in Evaluation with `s` valid responses, all of them shortlisted.
		let setup = setup_to_evaluation::<T>(&officer, TenderKind::Eoi, 2, s, 0)?;

		#[extrinsic_call]
		_(RawOrigin::Signed(officer), setup.tender_id, setup.bidders.clone());

		assert_eq!(ShortlistCount::<T>::get(setup.tender_id), s);
		Ok(())
	}

	#[benchmark]
	fn submit_open_bid(p: Linear<1, { T::MaxPriceLines::get() }>) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		// Open bidding is confined to RFQs (spec §1.2).
		let raw = NextTenderId::<T>::get();
		TenderChain::<T>::create_tender(
			RawOrigin::Signed(officer.clone()).into(),
			officer.clone(),
			TenderKind::Rfq,
			BidMode::Open,
			[1u8; 32],
			[2u8; 32],
			criteria(2),
			gates::<T>(),
			STANDSTILL.into(),
			Vec::new(),
			0,
			BondTerms {
				amount: bond_amount::<T>(),
				forfeit_on_non_reveal: true,
				forfeit_on_withdrawal: false,
			},
			false,
			None,
		)?;
		let id: T::TenderId = raw.into();
		TenderChain::<T>::publish_tender(RawOrigin::Signed(officer).into(), id)?;

		let bidder: T::AccountId = whitelisted_caller();
		fund::<T>(&bidder);

		#[extrinsic_call]
		_(RawOrigin::Signed(bidder.clone()), id, [7u8; 32], price_lines::<T>(p));

		let r = Reveals::<T>::get(id, &bidder).expect("open bid records its reveal; qed");
		assert!(r.valid);
		Ok(())
	}

	/// The deadline wheel itself: `n` gate transitions falling on one block.
	#[benchmark]
	fn on_initialize(
		n: Linear<0, { T::MaxTransitionsPerBlock::get() }>,
	) -> Result<(), BenchmarkError> {
		start::<T>();
		let officer = funded_account::<T>("officer", 0);
		// Every published tender lands a QuestionsClose gate on the same block.
		for _ in 0..n {
			create_and_publish::<T>(&officer, TenderKind::Rft, 2)?;
		}
		advance_to::<T>(QUESTIONS_CLOSE - 1);
		let target: BlockNumberFor<T> = QUESTIONS_CLOSE.into();
		frame_system::Pallet::<T>::set_block_number(target);
		assert_eq!(DeadlineWheel::<T>::get(target).len() as u32, n);

		#[block]
		{
			TenderChain::<T>::on_initialize(target);
		}

		assert!(DeadlineWheel::<T>::get(target).is_empty());
		Ok(())
	}

	impl_benchmark_test_suite!(TenderChain, crate::mock::new_test_ext(), crate::mock::Test);
}
