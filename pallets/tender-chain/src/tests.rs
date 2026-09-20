//! Tests for TenderChain.
//!
//! Coverage targets spec §9: state-machine gate transitions, commit/reveal
//! validation including mismatch voiding, bond lifecycle, score-variance
//! flagging, and challenge suspension — plus the three end-to-end flows in §6.

use crate::{mock::*, types::*, Error, Event};
use frame_support::{assert_noop, assert_ok, BoundedVec};

const ENTITY: AccountId = 1;
const OFFICER: AccountId = 2;
const BIDDER_A: AccountId = 10;
const BIDDER_B: AccountId = 11;
const BIDDER_C: AccountId = 12;
const EVAL_1: AccountId = 20;
const EVAL_2: AccountId = 21;

const NOTICE: Hash256 = [1u8; 32];
const CRITERIA: Hash256 = [2u8; 32];
const SALT_A: Hash256 = [0xAAu8; 32];
const SALT_B: Hash256 = [0xBBu8; 32];
const DOCS_A: Hash256 = [0xA1u8; 32];
const DOCS_B: Hash256 = [0xB1u8; 32];
const RATIONALE: Hash256 = [9u8; 32];
const CONTRACT: Hash256 = [8u8; 32];

fn weights() -> Vec<CriterionWeight> {
	vec![
		CriterionWeight { criterion_id: 1, weight_percent: 60 },
		CriterionWeight { criterion_id: 2, weight_percent: 40 },
	]
}

/// publish=2, questions close=5, submissions close=10, opening 10..15
fn gates() -> TenderGates<u64> {
	TenderGates {
		publish_at: 2,
		questions_close_at: 5,
		submission_close_at: 10,
		opening_at: 10,
		opening_end_at: 15,
	}
}

fn bond(amount: Balance) -> BondTerms<Balance> {
	BondTerms { amount, forfeit_on_non_reveal: true, forfeit_on_withdrawal: false }
}

fn prices(v: Vec<(u32, Balance)>) -> Vec<PriceLine<Balance>> {
	v.into_iter().map(|(item_id, amount)| PriceLine { item_id, amount }).collect()
}

fn bounded_prices(v: Vec<PriceLine<Balance>>) -> BoundedVec<PriceLine<Balance>, MaxPriceLines> {
	v.try_into().unwrap()
}

fn create(bond_amount: Balance) -> u32 {
	assert_ok!(TenderChain::create_tender(
		RuntimeOrigin::signed(OFFICER),
		ENTITY,
		TenderKind::Rft,
		BidMode::Sealed,
		NOTICE,
		CRITERIA,
		weights(),
		gates(),
		20, // standstill period
		vec![],
		0,
		bond(bond_amount),
		false,
		None,
	));
	0
}

fn create_and_publish(bond_amount: Balance) -> u32 {
	let id = create(bond_amount);
	assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
	id
}

fn commitment_for(bidder: AccountId, docs: Hash256, p: Vec<PriceLine<Balance>>, salt: Hash256) -> Hash256 {
	TenderChain::compute_commitment(&bidder, &docs, &bounded_prices(p), &salt)
}

/// Take the tender through to `Evaluation` with two valid sealed bids revealed.
fn to_evaluation_with_two_bids(bond_amount: Balance) -> u32 {
	let id = create_and_publish(bond_amount);
	let pa = prices(vec![(1, 500)]);
	let pb = prices(vec![(1, 600)]);

	assert_ok!(TenderChain::commit_bid(
		RuntimeOrigin::signed(BIDDER_A),
		id,
		commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)
	));
	assert_ok!(TenderChain::commit_bid(
		RuntimeOrigin::signed(BIDDER_B),
		id,
		commitment_for(BIDDER_B, DOCS_B, pb.clone(), SALT_B)
	));

	run_to_block(11); // past submission close
	assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
	assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, pa, SALT_A));
	assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_B), id, DOCS_B, pb, SALT_B));

	run_to_block(16); // past opening end -> Evaluation
	id
}

fn appoint_and_activate(id: u32, who: AccountId) {
	assert_ok!(TenderChain::appoint_evaluator(RuntimeOrigin::signed(OFFICER), id, who, [7u8; 32]));
	assert_ok!(TenderChain::declare_conflict(RuntimeOrigin::signed(who), id, [3u8; 32]));
	assert_ok!(TenderChain::activate_evaluator(RuntimeOrigin::signed(OFFICER), id, who));
}

fn score(id: u32, evaluator: AccountId, bidder: AccountId, s1: u8, s2: u8) {
	assert_ok!(TenderChain::submit_scores(
		RuntimeOrigin::signed(evaluator),
		id,
		bidder,
		vec![
			CriterionScore { criterion_id: 1, score: s1 },
			CriterionScore { criterion_id: 2, score: s2 },
		],
		[4u8; 32],
	));
}

// ---------------------------------------------------------------
// Creation & validation
// ---------------------------------------------------------------

#[test]
fn create_tender_works() {
	new_test_ext().execute_with(|| {
		let id = create(0);
		let t = TenderChain::tenders(id).unwrap();
		assert_eq!(t.officer, OFFICER);
		assert_eq!(t.entity, ENTITY);
		assert!(matches!(t.state, TenderState::Draft));
		assert_eq!(t.published_at, None);
		System::assert_has_event(Event::TenderCreated { tender_id: id, officer: OFFICER }.into());
	});
}

#[test]
fn create_tender_rejects_weights_not_summing_to_100() {
	new_test_ext().execute_with(|| {
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				NOTICE,
				CRITERIA,
				vec![CriterionWeight { criterion_id: 1, weight_percent: 90 }],
				gates(),
				20,
				vec![],
				0,
				bond(0),
				false,
				None,
			),
			Error::<Test>::WeightsInvalid
		);
	});
}

#[test]
fn create_tender_rejects_out_of_order_gates() {
	new_test_ext().execute_with(|| {
		let bad = TenderGates {
			publish_at: 50,
			questions_close_at: 5,
			submission_close_at: 10,
			opening_at: 10,
			opening_end_at: 15,
		};
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				NOTICE,
				CRITERIA,
				weights(),
				bad,
				20,
				vec![],
				0,
				bond(0),
				false,
				None,
			),
			Error::<Test>::GateOrderInvalid
		);
	});
}

#[test]
fn open_bidding_confined_to_rfq() {
	new_test_ext().execute_with(|| {
		// Sealed is the default guarantee; open bidding on a full RFT is refused.
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Open,
				NOTICE,
				CRITERIA,
				weights(),
				gates(),
				20,
				vec![],
				0,
				bond(0),
				false,
				None,
			),
			Error::<Test>::OpenBidNotPermitted
		);
		// The same thing on an RFQ is fine.
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Rfq,
			BidMode::Open,
			NOTICE,
			CRITERIA,
			weights(),
			gates(),
			20,
			vec![],
			0,
			bond(0),
			false,
			None,
		));
	});
}

// ---------------------------------------------------------------
// Gate transitions driven by on_initialize (spec §8)
// ---------------------------------------------------------------

#[test]
fn deadline_wheel_drives_the_state_machine() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::QaWindow));

		run_to_block(5); // questions close
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Submission));

		run_to_block(10); // submissions close
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Closed));
		System::assert_has_event(Event::TenderClosed { tender_id: id }.into());

		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Opening));

		run_to_block(15); // reveal window ends
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));
		System::assert_has_event(Event::EvaluationStarted { tender_id: id }.into());
	});
}

#[test]
fn commits_rejected_after_close_block_even_before_gate_runs() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		// Jump the block number without running on_initialize, so the gate has not
		// fired. The block-number check must still close submissions.
		System::set_block_number(11);
		assert_noop!(
			TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, [0u8; 32]),
			Error::<Test>::SubmissionClosed
		);
	});
}

#[test]
fn addendum_may_extend_close_but_never_shorten_it() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);

		assert_noop!(
			TenderChain::publish_addendum(RuntimeOrigin::signed(OFFICER), id, [5u8; 32], Some(8)),
			Error::<Test>::CloseCannotShorten
		);

		assert_ok!(TenderChain::publish_addendum(
			RuntimeOrigin::signed(OFFICER),
			id,
			[5u8; 32],
			Some(20)
		));
		let t = TenderChain::tenders(id).unwrap();
		assert_eq!(t.gates.submission_close_at, 20);
		// Reveal window keeps its original 5-block length.
		assert_eq!(t.gates.opening_at, 20);
		assert_eq!(t.gates.opening_end_at, 25);

		// The original close gate at 10 must not close the extended tender.
		run_to_block(11);
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Submission));
		run_to_block(21);
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Closed));
	});
}

// ---------------------------------------------------------------
// Commit / reveal (spec §5.2)
// ---------------------------------------------------------------

#[test]
fn valid_reveal_is_accepted() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		let p = prices(vec![(1, 500), (2, 250)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, p.clone(), SALT_A)
		));

		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, p, SALT_A));

		assert!(TenderChain::reveals(id, BIDDER_A).unwrap().valid);
		System::assert_has_event(
			Event::BidRevealed { tender_id: id, bidder: BIDDER_A, valid: true }.into(),
		);
	});
}

#[test]
fn reveal_mismatch_voids_bid_and_is_recorded_publicly() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		let committed = prices(vec![(1, 500)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, committed, SALT_A)
		));

		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));

		// Reveal a cheaper price than was committed.
		assert_ok!(TenderChain::reveal_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			DOCS_A,
			prices(vec![(1, 400)]),
			SALT_A
		));

		// Recorded, not silently dropped — and voided.
		let r = TenderChain::reveals(id, BIDDER_A).unwrap();
		assert!(!r.valid);
		System::assert_has_event(Event::RevealMismatch { tender_id: id, bidder: BIDDER_A }.into());
	});
}

#[test]
fn commitment_cannot_be_replayed_by_another_bidder() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		let p = prices(vec![(1, 500)]);
		let a_commitment = commitment_for(BIDDER_A, DOCS_A, p.clone(), SALT_A);

		// B copies A's commitment off the public chain.
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, a_commitment));
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_B), id, a_commitment));

		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));

		// A reveals fine. B reveals the identical content and is voided, because the
		// bidder account is bound into the preimage.
		assert_ok!(TenderChain::reveal_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			DOCS_A,
			p.clone(),
			SALT_A
		));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_B), id, DOCS_A, p, SALT_A));

		assert!(TenderChain::reveals(id, BIDDER_A).unwrap().valid);
		assert!(!TenderChain::reveals(id, BIDDER_B).unwrap().valid);
	});
}

#[test]
fn duplicate_commitment_rejected() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, [1u8; 32]));
		assert_noop!(
			TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, [2u8; 32]),
			Error::<Test>::CommitmentExists
		);
	});
}

// ---------------------------------------------------------------
// Bond lifecycle (spec §9)
// ---------------------------------------------------------------

#[test]
fn bond_is_reserved_on_commit_and_returned_on_execute() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = to_evaluation_with_two_bids(bond_amount);
		assert_eq!(Balances::reserved_balance(BIDDER_A), bond_amount);
		assert_eq!(Balances::reserved_balance(BIDDER_B), bond_amount);

		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 78, 72);
		score(id, EVAL_1, BIDDER_B, 50, 60);
		score(id, EVAL_2, BIDDER_B, 52, 58);

		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		run_to_block(100); // past standstill
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));

		// Every bond released, winner and losers alike.
		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		assert_eq!(Balances::reserved_balance(BIDDER_B), 0);
		System::assert_has_event(
			Event::BondReturned { tender_id: id, bidder: BIDDER_B, amount: bond_amount }.into(),
		);
	});
}

#[test]
fn non_reveal_forfeits_bond_when_terms_say_so() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = create_and_publish(bond_amount);
		let entity_before = Balances::free_balance(ENTITY);

		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, [1u8; 32]));
		assert_eq!(Balances::reserved_balance(BIDDER_A), bond_amount);

		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		// Never reveals.
		run_to_block(16);

		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		assert_eq!(Balances::free_balance(ENTITY), entity_before + bond_amount);
		System::assert_has_event(
			Event::BondForfeited { tender_id: id, bidder: BIDDER_A, amount: bond_amount }.into(),
		);
	});
}

#[test]
fn withdrawal_returns_bond_when_terms_do_not_forfeit() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = create_and_publish(bond_amount);
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, [1u8; 32]));
		assert_eq!(Balances::reserved_balance(BIDDER_A), bond_amount);

		assert_ok!(TenderChain::withdraw_commitment(RuntimeOrigin::signed(BIDDER_A), id));
		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		assert!(TenderChain::bid_commitments(id, BIDDER_A).is_none());
		assert!(TenderChain::participants(id).is_empty());
	});
}

#[test]
fn cancellation_returns_all_bonds() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = create_and_publish(bond_amount);
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), id, [1u8; 32]));
		assert_ok!(TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_B), id, [2u8; 32]));

		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(OFFICER), id, [6u8; 32]));

		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		assert_eq!(Balances::reserved_balance(BIDDER_B), 0);
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Cancelled));
		System::assert_has_event(
			Event::TenderCancelled { tender_id: id, reason_hash: [6u8; 32] }.into(),
		);
	});
}

// ---------------------------------------------------------------
// Evaluation & probity (spec §5.2)
// ---------------------------------------------------------------

#[test]
fn evaluator_cannot_score_without_conflict_declaration() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		assert_ok!(TenderChain::appoint_evaluator(
			RuntimeOrigin::signed(OFFICER),
			id,
			EVAL_1,
			[7u8; 32]
		));

		// Activation refused before a declaration is lodged...
		assert_noop!(
			TenderChain::activate_evaluator(RuntimeOrigin::signed(OFFICER), id, EVAL_1),
			Error::<Test>::ConflictNotDeclared
		);
		// ...and scoring refused while inactive.
		assert_noop!(
			TenderChain::submit_scores(
				RuntimeOrigin::signed(EVAL_1),
				id,
				BIDDER_A,
				vec![
					CriterionScore { criterion_id: 1, score: 50 },
					CriterionScore { criterion_id: 2, score: 50 }
				],
				[4u8; 32]
			),
			Error::<Test>::ConflictNotDeclared
		);
	});
}

#[test]
fn non_evaluator_cannot_score() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		assert_noop!(
			TenderChain::submit_scores(
				RuntimeOrigin::signed(BIDDER_C),
				id,
				BIDDER_A,
				vec![
					CriterionScore { criterion_id: 1, score: 99 },
					CriterionScore { criterion_id: 2, score: 99 }
				],
				[4u8; 32]
			),
			Error::<Test>::NotEvaluator
		);
	});
}

#[test]
fn scores_must_match_locked_criteria_and_stay_in_range() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);

		// A criterion that was never in the locked set.
		assert_noop!(
			TenderChain::submit_scores(
				RuntimeOrigin::signed(EVAL_1),
				id,
				BIDDER_A,
				vec![
					CriterionScore { criterion_id: 1, score: 50 },
					CriterionScore { criterion_id: 99, score: 50 }
				],
				[4u8; 32]
			),
			Error::<Test>::CriteriaMismatch
		);

		// Dropping a criterion.
		assert_noop!(
			TenderChain::submit_scores(
				RuntimeOrigin::signed(EVAL_1),
				id,
				BIDDER_A,
				vec![CriterionScore { criterion_id: 1, score: 50 }],
				[4u8; 32]
			),
			Error::<Test>::CriteriaMismatch
		);

		// Above MaxScore.
		assert_noop!(
			TenderChain::submit_scores(
				RuntimeOrigin::signed(EVAL_1),
				id,
				BIDDER_A,
				vec![
					CriterionScore { criterion_id: 1, score: 200 },
					CriterionScore { criterion_id: 2, score: 50 }
				],
				[4u8; 32]
			),
			Error::<Test>::ScoreOutOfRange
		);
	});
}

#[test]
fn score_variance_beyond_threshold_is_flagged() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);

		score(id, EVAL_1, BIDDER_A, 90, 50);
		// Threshold is 30; a 60-point spread on criterion 1 must flag.
		score(id, EVAL_2, BIDDER_A, 30, 55);

		System::assert_has_event(
			Event::ScoreVarianceFlagged {
				tender_id: id,
				bidder: BIDDER_A,
				criterion_id: 1,
				spread: 60,
			}
			.into(),
		);
	});
}

#[test]
fn voided_bid_cannot_be_scored_or_awarded() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, prices(vec![(1, 500)]), SALT_A)
		));
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		// Mismatched reveal -> voided.
		assert_ok!(TenderChain::reveal_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			DOCS_A,
			prices(vec![(1, 400)]),
			SALT_A
		));
		run_to_block(16);

		appoint_and_activate(id, EVAL_1);
		assert_noop!(
			TenderChain::submit_scores(
				RuntimeOrigin::signed(EVAL_1),
				id,
				BIDDER_A,
				vec![
					CriterionScore { criterion_id: 1, score: 50 },
					CriterionScore { criterion_id: 2, score: 50 }
				],
				[4u8; 32]
			),
			Error::<Test>::InvalidAwardee
		);
		appoint_and_activate(id, EVAL_2);
		assert_noop!(
			TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE),
			Error::<Test>::InvalidAwardee
		);
	});
}

// ---------------------------------------------------------------
// Award (spec §8: governed origin, minimum evaluators)
// ---------------------------------------------------------------

#[test]
fn award_requires_governed_origin() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 78, 72);

		// A single signed key cannot award.
		assert_noop!(
			TenderChain::award(RuntimeOrigin::signed(OFFICER), id, vec![BIDDER_A], RATIONALE),
			sp_runtime::DispatchError::BadOrigin
		);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
	});
}

#[test]
fn award_requires_minimum_evaluators() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1); // MinEvaluators is 2
		score(id, EVAL_1, BIDDER_A, 80, 70);

		assert_noop!(
			TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE),
			Error::<Test>::TooFewEvaluators
		);
	});
}

#[test]
fn ranking_is_weighted_and_ordered_best_first() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);

		// A averages 80 on c1 (weight 60) and 70 on c2 (weight 40) -> 80*60+70*40 = 7600
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		// B averages 50 and 60 -> 50*60+60*40 = 5400
		score(id, EVAL_1, BIDDER_B, 50, 60);
		score(id, EVAL_2, BIDDER_B, 50, 60);

		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		let outcome = TenderChain::outcomes(id).unwrap();
		assert_eq!(outcome.ranking[0], (BIDDER_A, 7600));
		assert_eq!(outcome.ranking[1], (BIDDER_B, 5400));
		assert_eq!(outcome.awardees.to_vec(), vec![BIDDER_A]);
		assert_eq!(outcome.rationale_hash, RATIONALE);
	});
}

// ---------------------------------------------------------------
// Challenge (spec §6.3)
// ---------------------------------------------------------------

#[test]
fn challenge_suspends_execution_until_resolved() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		// Losing bidder challenges inside the standstill.
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, [0xCCu8; 32]));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Challenged));

		run_to_block(100); // standstill elapsed, but the challenge is open
		assert_noop!(
			TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT),
			Error::<Test>::ChallengeOpen
		);

		// Dismissed -> execution released.
		assert_ok!(TenderChain::resolve_challenge(
			RuntimeOrigin::root(),
			id,
			0,
			false,
			[0xDDu8; 32]
		));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Awarded));
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Contracted));
	});
}

#[test]
fn upheld_challenge_remits_to_evaluation() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, [0xCCu8; 32]));

		assert_ok!(TenderChain::resolve_challenge(
			RuntimeOrigin::root(),
			id,
			0,
			true,
			[0xDDu8; 32]
		));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));

		let c = TenderChain::challenges(id, 0).unwrap();
		assert!(matches!(c.state, ChallengeState::Upheld));
		assert_eq!(c.resolution_hash, Some([0xDDu8; 32]));
	});
}

#[test]
fn challenge_rejected_after_standstill_expires() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		run_to_block(100);
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, [0xCCu8; 32]),
			Error::<Test>::StandstillExpired
		);
	});
}

#[test]
fn execute_award_blocked_during_standstill() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		assert_noop!(
			TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT),
			Error::<Test>::StandstillActive
		);
	});
}

// ---------------------------------------------------------------
// Flow A end-to-end (spec §6.1) and panel mode
// ---------------------------------------------------------------

#[test]
fn flow_a_government_sealed_rft_end_to_end() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = create_and_publish(bond_amount);

		// 3. Questions and answers publish to everyone.
		assert_ok!(TenderChain::ask_question(RuntimeOrigin::signed(BIDDER_A), id, [0x11u8; 32], SALT_A));
		assert_ok!(TenderChain::answer_question(
			RuntimeOrigin::signed(OFFICER),
			id,
			0,
			[0x12u8; 32]
		));

		// 4. Three bidders commit; one will mismatch on reveal.
		let pa = prices(vec![(1, 500)]);
		let pb = prices(vec![(1, 600)]);
		let pc = prices(vec![(1, 700)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)
		));
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			commitment_for(BIDDER_B, DOCS_B, pb.clone(), SALT_B)
		));
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_C),
			id,
			commitment_for(BIDDER_C, DOCS_B, pc, SALT_B)
		));

		// 5. Close, open, reveal.
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::reveal_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			DOCS_A,
			pa,
			SALT_A
		));
		assert_ok!(TenderChain::reveal_bid(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			DOCS_B,
			pb,
			SALT_B
		));
		// C reveals different prices than committed -> voided.
		assert_ok!(TenderChain::reveal_bid(
			RuntimeOrigin::signed(BIDDER_C),
			id,
			DOCS_B,
			prices(vec![(1, 100)]),
			SALT_B
		));
		System::assert_has_event(Event::RevealMismatch { tender_id: id, bidder: BIDDER_C }.into());

		run_to_block(16);

		// 6. Evaluation with a variance flag.
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 90, 80);
		score(id, EVAL_2, BIDDER_A, 40, 78); // 50-point spread -> flagged
		score(id, EVAL_1, BIDDER_B, 60, 60);
		score(id, EVAL_2, BIDDER_B, 62, 58);
		System::assert_has_event(
			Event::ScoreVarianceFlagged {
				tender_id: id,
				bidder: BIDDER_A,
				criterion_id: 1,
				spread: 50,
			}
			.into(),
		);

		// 7. Award by governed origin; standstill opens.
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		let outcome = TenderChain::outcomes(id).unwrap();
		// The voided bid is excluded from the ranking entirely.
		assert_eq!(outcome.ranking.len(), 2);
		assert!(!outcome.ranking.iter().any(|(who, _)| *who == BIDDER_C));

		// 8. No challenge; execute after standstill, bonds return automatically.
		run_to_block(100);
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Contracted));
		assert_eq!(TenderChain::outcomes(id).unwrap().contract_hash, Some(CONTRACT));
		for b in [BIDDER_A, BIDDER_B, BIDDER_C] {
			assert_eq!(Balances::reserved_balance(b), 0);
		}
		System::assert_has_event(
			Event::ContractExecuted { tender_id: id, contract_hash: CONTRACT }.into(),
		);
	});
}

// ---------------------------------------------------------------
// Bounding (spec §8: bounded questions, evaluators and challenges
// per tender; §9: no unbounded storage)
// ---------------------------------------------------------------

#[test]
fn questions_are_bounded_per_tender() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(500);
		// The Q&A window is open to any account, so the bound is what stops
		// state growing for the price of fees alone.
		for i in 0..MaxQuestions::get() {
			assert_ok!(TenderChain::ask_question(
				RuntimeOrigin::signed(BIDDER_A),
				id,
				[i as u8; 32],
				SALT_A
			));
		}
		assert_noop!(
			TenderChain::ask_question(RuntimeOrigin::signed(BIDDER_B), id, [0xFFu8; 32], SALT_B),
			Error::<Test>::TooManyQuestions
		);
	});
}

#[test]
fn evaluators_are_bounded_per_tender() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(500);
		let max = MaxEvaluators::get();
		for i in 0..max {
			assert_ok!(TenderChain::appoint_evaluator(
				RuntimeOrigin::signed(OFFICER),
				id,
				100 + i as AccountId,
				[7u8; 32]
			));
		}
		assert_noop!(
			TenderChain::appoint_evaluator(RuntimeOrigin::signed(OFFICER), id, 999, [7u8; 32]),
			Error::<Test>::TooManyEvaluators
		);
	});
}

#[test]
fn reappointing_an_existing_evaluator_does_not_consume_a_slot() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(500);
		let max = MaxEvaluators::get();
		for i in 0..max {
			assert_ok!(TenderChain::appoint_evaluator(
				RuntimeOrigin::signed(OFFICER),
				id,
				100 + i as AccountId,
				[7u8; 32]
			));
		}
		// Refreshing someone already on the panel is not a new appointment.
		assert_ok!(TenderChain::appoint_evaluator(
			RuntimeOrigin::signed(OFFICER),
			id,
			100,
			[8u8; 32]
		));
	});
}

/// Re-appointment clears the conflict declaration and deactivates. The active
/// tally has to follow, or `MinEvaluators` could be satisfied by evaluators who
/// are no longer activated.
#[test]
fn reappointment_deactivates_and_decrements_the_active_tally() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(500);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);

		// Re-appoint one of the two activated evaluators.
		assert_ok!(TenderChain::appoint_evaluator(
			RuntimeOrigin::signed(OFFICER),
			id,
			EVAL_2,
			[7u8; 32]
		));
		let rec = TenderChain::evaluator_set(id, EVAL_2).unwrap();
		assert!(!rec.active);
		assert!(rec.conflict_declaration.is_none());

		// Only one evaluator is genuinely active now, so the award must fail.
		assert_noop!(
			TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE),
			Error::<Test>::TooFewEvaluators
		);
	});
}

// ---------------------------------------------------------------
// Challenge standing and bounding
// ---------------------------------------------------------------

/// Spec §4.1/§6.3: challenges come from bidders. Since every open challenge
/// suspends execution, letting outsiders lodge them would let anyone hold a
/// lawful award hostage.
#[test]
fn only_a_participating_bidder_may_challenge() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(500);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		// BIDDER_C never lodged a commitment on this tender.
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_C), id, [0xCCu8; 32]),
			Error::<Test>::NotAParticipant
		);
		// The losing bidder has standing.
		assert_ok!(TenderChain::lodge_challenge(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			[0xCCu8; 32]
		));
	});
}

#[test]
fn challenges_are_bounded_per_tender() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(500);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		for i in 0..MaxChallenges::get() {
			assert_ok!(TenderChain::lodge_challenge(
				RuntimeOrigin::signed(BIDDER_B),
				id,
				[i as u8; 32]
			));
		}
		assert_noop!(
			TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, [0xFFu8; 32]),
			Error::<Test>::TooManyChallenges
		);
	});
}

// ---------------------------------------------------------------
// Authorisation, state-machine and bid-mode regressions
// ---------------------------------------------------------------

/// Spec §4.1 lists `call_off` as an Officer call. It previously only checked
/// panel membership, so any signed account could order against any panel.
#[test]
fn call_off_requires_the_panel_officer() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Panel,
			BidMode::Sealed,
			NOTICE,
			CRITERIA,
			weights(),
			gates(),
			20,
			vec![],
			0,
			bond(0),
			false,
			None,
		));
		let id = 0;
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
		let pa = prices(vec![(1, 500)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)
		));
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, pa, SALT_A));
		run_to_block(16);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		// A stranger cannot order against the agency's standing offer.
		assert_noop!(
			TenderChain::call_off(RuntimeOrigin::signed(BIDDER_C), 0, BIDDER_A, [0x77u8; 32]),
			Error::<Test>::NotOfficer
		);
		// The officer can.
		assert_ok!(TenderChain::call_off(RuntimeOrigin::signed(OFFICER), 0, BIDDER_A, [0x77u8; 32]));
	});
}

/// An evaluator revising their own scoresheet is not inter-evaluator
/// disagreement, so it must not raise a variance flag.
#[test]
fn revising_your_own_scores_does_not_raise_a_variance_flag() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);

		score(id, EVAL_1, BIDDER_A, 90, 80);
		System::reset_events();
		// Same evaluator, a 70-point revision downward. Compared against their own
		// prior sheet this would trip the 30-point threshold.
		score(id, EVAL_1, BIDDER_A, 20, 80);

		assert!(!System::events().iter().any(|e| matches!(
			e.event,
			RuntimeEvent::TenderChain(Event::ScoreVarianceFlagged { .. })
		)));

		// A genuine disagreement from a different evaluator still flags.
		score(id, EVAL_2, BIDDER_A, 90, 80);
		System::assert_has_event(
			Event::ScoreVarianceFlagged {
				tender_id: id,
				bidder: BIDDER_A,
				criterion_id: 1,
				spread: 70,
			}
			.into(),
		);
	});
}

/// With two challenges open, upholding one remits the tender to `Evaluation`.
/// Dismissing the other must not drag it back to `Awarded` and silently undo
/// the re-evaluation.
#[test]
fn dismissing_a_second_challenge_does_not_undo_an_upheld_one() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(0);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		// Both losing-side bidders challenge.
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_B), id, [0xC1u8; 32]));
		assert_ok!(TenderChain::lodge_challenge(RuntimeOrigin::signed(BIDDER_A), id, [0xC2u8; 32]));

		// First upheld -> remitted for re-evaluation.
		assert_ok!(TenderChain::resolve_challenge(RuntimeOrigin::root(), id, 0, true, [0xE1u8; 32]));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));

		// Second dismissed -> must stay remitted, not snap back to Awarded.
		assert_ok!(TenderChain::resolve_challenge(RuntimeOrigin::root(), id, 1, false, [0xE2u8; 32]));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));
	});
}

#[test]
fn too_many_credentials_reports_its_own_error() {
	new_test_ext().execute_with(|| {
		let creds: Vec<Hash256> = (0..(MaxCredentials::get() + 1)).map(|i| [i as u8; 32]).collect();
		assert_noop!(
			TenderChain::create_tender(
				RuntimeOrigin::signed(OFFICER),
				ENTITY,
				TenderKind::Rft,
				BidMode::Sealed,
				NOTICE,
				CRITERIA,
				weights(),
				gates(),
				20,
				creds,
				0,
				bond(0),
				false,
				None,
			),
			Error::<Test>::TooManyCredentials
		);
	});
}

// ---------------------------------------------------------------
// Open-bid mode (spec §1.2)
// ---------------------------------------------------------------

fn create_open_rfq() -> u32 {
	assert_ok!(TenderChain::create_tender(
		RuntimeOrigin::signed(OFFICER),
		ENTITY,
		TenderKind::Rfq,
		BidMode::Open,
		NOTICE,
		CRITERIA,
		weights(),
		gates(),
		20,
		vec![],
		0,
		bond(500),
		false,
		None,
	));
	assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), 0));
	0
}

/// An open bid publishes its content at submission time: readable immediately,
/// with no separate reveal step.
#[test]
fn open_bid_is_readable_immediately_and_needs_no_reveal() {
	new_test_ext().execute_with(|| {
		let id = create_open_rfq();
		assert_ok!(TenderChain::submit_open_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			DOCS_A,
			prices(vec![(1, 500)])
		));

		// The bid is already on the record as revealed and valid.
		let r = TenderChain::reveals(id, BIDDER_A).unwrap();
		assert!(r.valid);
		assert_eq!(r.documents_hash, DOCS_A);
		assert_eq!(r.price_schedule[0].amount, 500);
		assert_eq!(Balances::reserved_balance(BIDDER_A), 500);
	});
}

/// A sealed commitment is meaningless on an open tender.
#[test]
fn commit_bid_is_rejected_on_an_open_tender() {
	new_test_ext().execute_with(|| {
		let open = create_open_rfq();
		assert_noop!(
			TenderChain::commit_bid(RuntimeOrigin::signed(BIDDER_A), open, [0u8; 32]),
			Error::<Test>::WrongBidMode
		);
	});
}

/// And an open submission is not accepted on a sealed tender.
#[test]
fn open_submission_is_rejected_on_a_sealed_tender() {
	new_test_ext().execute_with(|| {
		let sealed = create_and_publish(0);
		assert_noop!(
			TenderChain::submit_open_bid(
				RuntimeOrigin::signed(BIDDER_A),
				sealed,
				DOCS_A,
				prices(vec![(1, 500)])
			),
			Error::<Test>::WrongBidMode
		);
	});
}

/// An open bid has nothing left to disclose, so it can never be treated as a
/// non-reveal and forfeited at the end of the opening window.
#[test]
fn open_bids_are_never_forfeited_as_non_reveals() {
	new_test_ext().execute_with(|| {
		let id = create_open_rfq();
		assert_ok!(TenderChain::submit_open_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			DOCS_A,
			prices(vec![(1, 500)])
		));
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		// Nobody calls reveal_bid — there is nothing to reveal.
		assert_noop!(
			TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, prices(vec![(1, 500)]), SALT_A),
			Error::<Test>::WrongBidMode
		);
		run_to_block(16);

		// The bond is held through evaluation, as for any bidder — but never
		// forfeited, because the bid was disclosed all along.
		assert_eq!(Balances::reserved_balance(BIDDER_A), 500);
		assert!(!System::events().iter().any(|e| matches!(
			e.event,
			RuntimeEvent::TenderChain(Event::BondForfeited { .. })
		)));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));
	});
}

// ---------------------------------------------------------------
// Officer inaction must not confiscate bonds
// ---------------------------------------------------------------

/// If the officer never calls `open_tender`, no bidder *can* reveal —
/// `reveal_bid` requires `Opening`. Forfeiting on non-reveal there would let an
/// officer confiscate every bond into their own entity by doing nothing at all.
#[test]
fn never_opening_the_tender_returns_bonds_instead_of_forfeiting_them() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = create_and_publish(bond_amount);
		let entity_before = Balances::free_balance(ENTITY);

		let pa = prices(vec![(1, 500)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa, SALT_A)
		));
		assert_eq!(Balances::reserved_balance(BIDDER_A), bond_amount);

		// Officer never calls open_tender; the wheel runs past opening_end_at.
		run_to_block(16);

		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Evaluation));
		// Bond returned, not forfeited, and the entity gained nothing.
		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		assert_eq!(Balances::free_balance(ENTITY), entity_before);
		System::assert_has_event(
			Event::BondReturned { tender_id: id, bidder: BIDDER_A, amount: bond_amount }.into(),
		);
	});
}

/// The same lever with a smaller handle: opening so late that the reveal window
/// is nearly zero. `MinRevealWindow` is 2 in the mock.
#[test]
fn officer_cannot_open_too_late_to_leave_a_reveal_window() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(500);
		let pa = prices(vec![(1, 500)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa, SALT_A)
		));

		// opening_end_at is 15; at block 14 only one block would remain.
		run_to_block(14);
		assert_noop!(
			TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id),
			Error::<Test>::RevealWindowTooShort
		);
	});
}

/// Genuine non-reveal — the tender *was* opened and the bidder simply did not
/// show — still forfeits, as the published terms say.
#[test]
fn genuine_non_reveal_still_forfeits() {
	new_test_ext().execute_with(|| {
		let bond_amount: Balance = 5_000;
		let id = create_and_publish(bond_amount);
		let pa = prices(vec![(1, 500)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa, SALT_A)
		));

		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		// BIDDER_A never reveals.
		run_to_block(16);

		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		System::assert_has_event(
			Event::BondForfeited { tender_id: id, bidder: BIDDER_A, amount: bond_amount }.into(),
		);
	});
}

// ---------------------------------------------------------------
// Evaluator separation (spec §1.2)
// ---------------------------------------------------------------

#[test]
fn a_bidder_cannot_be_appointed_evaluator() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(500);
		let pa = prices(vec![(1, 500)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa, SALT_A)
		));
		assert_noop!(
			TenderChain::appoint_evaluator(RuntimeOrigin::signed(OFFICER), id, BIDDER_A, [7u8; 32]),
			Error::<Test>::EvaluatorIsBidder
		);
	});
}

#[test]
fn an_evaluator_cannot_bid_on_the_tender_they_score() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(500);
		assert_ok!(TenderChain::appoint_evaluator(
			RuntimeOrigin::signed(OFFICER),
			id,
			EVAL_1,
			[7u8; 32]
		));
		let p = prices(vec![(1, 500)]);
		assert_noop!(
			TenderChain::commit_bid(
				RuntimeOrigin::signed(EVAL_1),
				id,
				commitment_for(EVAL_1, DOCS_A, p, SALT_A)
			),
			Error::<Test>::EvaluatorIsBidder
		);
	});
}

#[test]
fn the_officer_and_entity_cannot_sit_on_their_own_panel() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(500);
		for who in [OFFICER, ENTITY] {
			assert_noop!(
				TenderChain::appoint_evaluator(RuntimeOrigin::signed(OFFICER), id, who, [7u8; 32]),
				Error::<Test>::OfficerCannotEvaluate
			);
		}
	});
}

// ---------------------------------------------------------------
// Q&A identity blinding (spec §5.3)
// ---------------------------------------------------------------

/// Chain state is world-readable, so blinding has to mean not storing the
/// account at all — omitting it from the event is not blinding.
#[test]
fn blinded_questions_do_not_store_the_asker() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Rft,
			BidMode::Sealed,
			NOTICE,
			CRITERIA,
			weights(),
			gates(),
			20,
			vec![],
			0,
			bond(0),
			true, // blind_questions
			None,
		));
		let id = 0;
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::ask_question(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			[0x11u8; 32],
			SALT_A
		));

		let q = TenderChain::questions(id, 0).unwrap();
		match q.author {
			QuestionAuthor::Blinded(h) => {
				// The asker can reproduce it to prove authorship; nobody can invert it.
				assert_eq!(h, TenderChain::blind_author(&BIDDER_A, &SALT_A));
				assert_ne!(h, TenderChain::blind_author(&BIDDER_B, &SALT_A));
			},
			QuestionAuthor::Open(_) => panic!("author should be blinded"),
		}
	});
}

#[test]
fn unblinded_questions_keep_public_authorship() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_ok!(TenderChain::ask_question(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			[0x11u8; 32],
			SALT_A
		));
		let q = TenderChain::questions(id, 0).unwrap();
		assert_eq!(q.author, QuestionAuthor::Open(BIDDER_A));
	});
}

// ---------------------------------------------------------------
// Multi-stage: EOI -> shortlist -> RFT (spec §2.2)
// ---------------------------------------------------------------

/// Take an EOI to Evaluation with two responders, then shortlist one.
fn eoi_to_shortlist() -> u32 {
	assert_ok!(TenderChain::create_tender(
		RuntimeOrigin::signed(OFFICER),
		ENTITY,
		TenderKind::Eoi,
		BidMode::Sealed,
		NOTICE,
		CRITERIA,
		weights(),
		gates(),
		20,
		vec![],
		0,
		bond(0),
		false,
		None,
	));
	let eoi = 0;
	assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), eoi));

	let pa = prices(vec![(1, 500)]);
	let pb = prices(vec![(1, 600)]);
	assert_ok!(TenderChain::commit_bid(
		RuntimeOrigin::signed(BIDDER_A),
		eoi,
		commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)
	));
	assert_ok!(TenderChain::commit_bid(
		RuntimeOrigin::signed(BIDDER_B),
		eoi,
		commitment_for(BIDDER_B, DOCS_B, pb.clone(), SALT_B)
	));
	run_to_block(11);
	assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), eoi));
	assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), eoi, DOCS_A, pa, SALT_A));
	assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_B), eoi, DOCS_B, pb, SALT_B));
	run_to_block(16);
	eoi
}

#[test]
fn eoi_shortlist_credentials_bidders_into_the_follow_on_rft() {
	new_test_ext().execute_with(|| {
		let eoi = eoi_to_shortlist();

		// Only BIDDER_A makes the shortlist.
		assert_ok!(TenderChain::publish_shortlist(
			RuntimeOrigin::signed(OFFICER),
			eoi,
			vec![BIDDER_A]
		));
		assert!(matches!(TenderChain::tenders(eoi).unwrap().state, TenderState::Shortlisted));
		System::assert_has_event(
			Event::ShortlistPublished { tender_id: eoi, suppliers: 1 }.into(),
		);

		// Stage two: an RFT drawing its bidders from that shortlist.
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Rft,
			BidMode::Sealed,
			NOTICE,
			CRITERIA,
			weights(),
			TenderGates {
				publish_at: 17,
				questions_close_at: 20,
				submission_close_at: 25,
				opening_at: 25,
				opening_end_at: 30,
			},
			20,
			vec![],
			0,
			bond(0),
			false,
			Some(eoi),
		));
		let rft = 1;
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), rft));

		let p = prices(vec![(1, 400)]);
		// Shortlisted supplier gets in.
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			rft,
			commitment_for(BIDDER_A, DOCS_A, p.clone(), SALT_A)
		));
		// The one who responded to the EOI but missed the cut does not.
		assert_noop!(
			TenderChain::commit_bid(
				RuntimeOrigin::signed(BIDDER_B),
				rft,
				commitment_for(BIDDER_B, DOCS_B, p.clone(), SALT_B)
			),
			Error::<Test>::NotShortlisted
		);
		// Nor does someone who never took part in stage one.
		assert_noop!(
			TenderChain::commit_bid(
				RuntimeOrigin::signed(BIDDER_C),
				rft,
				commitment_for(BIDDER_C, DOCS_B, p, SALT_B)
			),
			Error::<Test>::NotShortlisted
		);
	});
}

#[test]
fn only_an_eoi_can_publish_a_shortlist_and_only_from_valid_bids() {
	new_test_ext().execute_with(|| {
		// An RFT is not a stage-one tender.
		let rft = to_evaluation_with_two_bids(0);
		assert_noop!(
			TenderChain::publish_shortlist(RuntimeOrigin::signed(OFFICER), rft, vec![BIDDER_A]),
			Error::<Test>::NotAnEoi
		);
	});
}

#[test]
fn shortlisting_a_non_responder_is_rejected() {
	new_test_ext().execute_with(|| {
		let eoi = eoi_to_shortlist();
		assert_noop!(
			TenderChain::publish_shortlist(RuntimeOrigin::signed(OFFICER), eoi, vec![BIDDER_C]),
			Error::<Test>::InvalidShortlistEntry
		);
	});
}

// ---------------------------------------------------------------
// Public verification: every transition is event-backed
// ---------------------------------------------------------------

#[test]
fn questions_close_transition_emits_an_event() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		run_to_block(6); // past questions_close_at = 5
		System::assert_has_event(Event::SubmissionOpened { tender_id: id }.into());
	});
}

/// Flow B (spec §6.2) — organisation job & task tendering.
///
/// The lightweight work-package mode. What distinguishes it from Flow A is the
/// afterlife: the award must hand off to Module 25 so delivery is tracked on the
/// same ledger. `RecordingDelivery` in the mock stands in for Work Task and
/// records the handoff, so this asserts the instantiation genuinely happens
/// rather than passing against a no-op.
#[test]
fn flow_b_job_task_award_instantiates_module_25_delivery() {
	new_test_ext().execute_with(|| {
		// 1. The organisation publishes a work-package tender: Job & Task mode,
		//    short timeline, eligibility gated on a minimum Reputation score.
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::JobTask,
			BidMode::Sealed,
			NOTICE,
			CRITERIA,
			weights(),
			gates(),
			20, // standstill period
			vec![],
			5, // minimum reputation
			bond(500),
			false,
			None,
		));
		let id = 0;
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));
		assert!(DeliveryLog::get().is_empty());

		// 2. Two contractors bid on the work package.
		let pa = prices(vec![(1, 500)]);
		let pb = prices(vec![(1, 600)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)
		));
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			commitment_for(BIDDER_B, DOCS_B, pb.clone(), SALT_B)
		));

		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, pa, SALT_A));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_B), id, DOCS_B, pb, SALT_B));

		run_to_block(16);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 90, 85);
		score(id, EVAL_2, BIDDER_A, 88, 84);
		score(id, EVAL_1, BIDDER_B, 60, 55);
		score(id, EVAL_2, BIDDER_B, 58, 57);

		// 3. Award to the contractor. Delivery must NOT be instantiated yet — the
		//    standstill window still has to run, so an award alone hands nothing
		//    to Module 25.
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Awarded));
		assert!(DeliveryLog::get().is_empty());

		// 4. Standstill passes, the officer executes, and the tasks instantiate to
		//    the winner as assignee.
		run_to_block(100);
		assert_ok!(TenderChain::execute_award(RuntimeOrigin::signed(OFFICER), id, CONTRACT));

		assert_eq!(DeliveryLog::get(), vec![(id, BIDDER_A, CONTRACT)]);
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Contracted));

		// The unsuccessful contractor's bond comes back automatically; the
		// winner's does too, since delivery funds move under Module 8/16.
		assert_eq!(Balances::reserved_balance(BIDDER_A), 0);
		assert_eq!(Balances::reserved_balance(BIDDER_B), 0);
	});
}

/// A cancelled Job & Task tender must hand nothing to Module 25 — the delivery
/// seam is reachable only through `execute_award`.
#[test]
fn cancelled_tender_never_instantiates_delivery() {
	new_test_ext().execute_with(|| {
		let id = to_evaluation_with_two_bids(500);
		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 90, 85);
		score(id, EVAL_2, BIDDER_A, 88, 84);
		assert_ok!(TenderChain::award(RuntimeOrigin::root(), id, vec![BIDDER_A], RATIONALE));

		assert_ok!(TenderChain::cancel_tender(RuntimeOrigin::signed(OFFICER), id, [0x5Eu8; 32]));
		assert!(DeliveryLog::get().is_empty());
		assert!(matches!(TenderChain::tenders(id).unwrap().state, TenderState::Cancelled));
	});
}

#[test]
fn panel_award_admits_members_and_permits_call_off() {
	new_test_ext().execute_with(|| {
		assert_ok!(TenderChain::create_tender(
			RuntimeOrigin::signed(OFFICER),
			ENTITY,
			TenderKind::Panel,
			BidMode::Sealed,
			NOTICE,
			CRITERIA,
			weights(),
			gates(),
			20,
			vec![],
			0,
			bond(0),
			false,
			None,
		));
		let id = 0u32;
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id));

		let pa = prices(vec![(1, 500)]);
		let pb = prices(vec![(1, 600)]);
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_A),
			id,
			commitment_for(BIDDER_A, DOCS_A, pa.clone(), SALT_A)
		));
		assert_ok!(TenderChain::commit_bid(
			RuntimeOrigin::signed(BIDDER_B),
			id,
			commitment_for(BIDDER_B, DOCS_B, pb.clone(), SALT_B)
		));
		run_to_block(11);
		assert_ok!(TenderChain::open_tender(RuntimeOrigin::signed(OFFICER), id));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_A), id, DOCS_A, pa, SALT_A));
		assert_ok!(TenderChain::reveal_bid(RuntimeOrigin::signed(BIDDER_B), id, DOCS_B, pb, SALT_B));
		run_to_block(16);

		appoint_and_activate(id, EVAL_1);
		appoint_and_activate(id, EVAL_2);
		score(id, EVAL_1, BIDDER_A, 80, 70);
		score(id, EVAL_2, BIDDER_A, 80, 70);
		score(id, EVAL_1, BIDDER_B, 70, 70);
		score(id, EVAL_2, BIDDER_B, 70, 70);

		// Multi-award establishes the pool.
		assert_ok!(TenderChain::award(
			RuntimeOrigin::root(),
			id,
			vec![BIDDER_A, BIDDER_B],
			RATIONALE
		));
		assert!(TenderChain::panel_pool(id, BIDDER_A).is_some());
		assert!(TenderChain::panel_pool(id, BIDDER_B).is_some());

		assert_ok!(TenderChain::call_off(
			RuntimeOrigin::signed(OFFICER),
			id,
			BIDDER_A,
			[0xEEu8; 32]
		));
		// A non-member cannot receive a call-off.
		assert_noop!(
			TenderChain::call_off(RuntimeOrigin::signed(OFFICER), id, BIDDER_C, [0xEEu8; 32]),
			Error::<Test>::NotPanelMember
		);
	});
}

// ---------------------------------------------------------------
// Authorisation
// ---------------------------------------------------------------

#[test]
fn only_officer_or_entity_may_administer() {
	new_test_ext().execute_with(|| {
		let id = create(0);
		assert_noop!(
			TenderChain::publish_tender(RuntimeOrigin::signed(BIDDER_A), id),
			Error::<Test>::NotOfficer
		);
		// The procuring entity may also administer.
		assert_ok!(TenderChain::publish_tender(RuntimeOrigin::signed(ENTITY), id));
	});
}

#[test]
fn cannot_publish_twice() {
	new_test_ext().execute_with(|| {
		let id = create_and_publish(0);
		assert_noop!(
			TenderChain::publish_tender(RuntimeOrigin::signed(OFFICER), id),
			Error::<Test>::BadState
		);
	});
}
